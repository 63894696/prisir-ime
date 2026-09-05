//! 候选词引擎(移植 engine.py):切分 → 单字+词组+混拼合并排序 → DP 整句智能首选。
//! 逻辑同源 Python,词库经 db.rs 读 ciku.db,前缀查询走 trie.rs 内存索引。

use crate::db::CikuDb;
use crate::syllable::Syllables;
use crate::trie::MemoryIndex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 索引缓存格式版本:序列化结构变更时 +1,旧缓存自动失效重建。
/// v2: trie 节点增加 top1(单字)通道,旧 v1 缓存无此字段,必须重建。
/// v3: MemoryIndex 增加混拼索引 jp_map/first_jp_map,必须重建。
const INDEX_CACHE_VERSION: u32 = 3;

/// 模糊音扩展字的权重降级量(2026-09-02):把 zhi 等模糊字压到原音字权重之下,
/// 保证打 zi 时原音字(自/字/…/耔/缁)排在模糊 zhi 字(只/指/值)之前。
/// 取值大于词库权重动态范围(实测 max≈26000),降级后模糊字恒为负,原音字(含 weight=0)恒在前。
const FUZZY_DEMOTE: i64 = 1_000_000;

/// 索引缓存头部(自描述,校验用)。后接 bincode 序列化的 MemoryIndex。
#[derive(serde::Serialize, serde::Deserialize)]
struct IndexHeader {
    magic: [u8; 4], // b"PIXC"
    version: u32,
    db_len: u64,
    db_mtime_secs: u64,
}

/// 词库指纹:文件大小 + mtime。变了即认为词库更新,缓存失效重建。
fn db_fingerprint(db_path: &str) -> Option<(u64, u64)> {
    let md = std::fs::metadata(db_path).ok()?;
    let mtime = md
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((md.len(), mtime))
}

/// 缓存文件路径:与词库同目录,`<name>.idx`。
fn cache_path_for(db_path: &str) -> PathBuf {
    let p = Path::new(db_path);
    p.with_extension("idx")
}

/// mmap 缓存路径:与词库同目录,`<name>.midx`(2026-09-02)。
fn mmap_path_for(db_path: &str) -> PathBuf {
    let p = Path::new(db_path);
    p.with_extension("midx")
}

/// (候选词, 权重)
pub type Candidate = (String, i64);

pub struct ImeEngine {
    pub db: CikuDb,
    pub syll: Syllables,
    pub mem: Option<MemoryIndex>,
    /// 模糊音规则集(对齐 ime_config.FUZZY_RULES)
    pub fuzzy_rules: Vec<&'static str>,
}

/// 模糊音映射:规则名 -> [(源,目标)](对齐 Python _FUZZY_MAP)
fn fuzzy_map(rule: &str) -> &'static [(&'static str, &'static str)] {
    match rule {
        "z_zh" => &[("zh", "z"), ("z", "zh")],
        "c_ch" => &[("ch", "c"), ("c", "ch")],
        "s_sh" => &[("sh", "s"), ("s", "sh")],
        "an_ang" => &[("an", "ang"), ("ang", "an")],
        "en_eng" => &[("en", "eng"), ("eng", "en")],
        "in_ing" => &[("in", "ing"), ("ing", "in")],
        "ian_iang" => &[("ian", "iang"), ("iang", "ian")],
        "uan_uang" => &[("uan", "uang"), ("uang", "uan")],
        "ai_an" => &[("ai", "an"), ("an", "ai")],
        "un_ong" => &[("un", "ong"), ("ong", "un")],
        _ => &[],
    }
}
/// 声母规则(起始替换);其余按结尾替换
fn fuzzy_is_initial(rule: &str) -> bool {
    matches!(rule, "z_zh" | "c_ch" | "s_sh")
}

/// 简拼 = 每个音节首字母拼接(供 db.add_user_word 用)
pub fn to_jianpin(pinyin: &str) -> String {
    // pinyin 形如 "nihao" / "zhongguo";按音节切分取首字母
    let syll = Syllables::new();
    let segs = syll.segment(pinyin);
    if segs.is_empty() {
        // 兜底:空格/逐字符
        return pinyin.chars().filter(|c| c.is_ascii_alphabetic()).take(1).collect();
    }
    segs.iter().filter_map(|s| s.chars().next()).collect()
}

impl ImeEngine {
    pub fn new(db_path: &str) -> Result<Self, String> {
        let db = CikuDb::open(db_path).map_err(|e| format!("open db: {e}"))?;
        Ok(ImeEngine {
            db,
            syll: Syllables::new(),
            mem: None,
            fuzzy_rules: Vec::new(),
        })
    }

    /// 构建内存索引(一次性灌 phrase+pinyin 全量)。失败保留 None 走 SQLite。
    pub fn build_memory_index(&mut self) -> Result<(), String> {
        let mem = Self::rebuild_index(&self.db)?;
        self.mem = Some(mem);
        Ok(())
    }

    /// 从 DB 全量重建内存索引(不落盘)。
    fn rebuild_index(db: &CikuDb) -> Result<MemoryIndex, String> {
        let phrase = db.all_phrase_rows().map_err(|e| format!("phrase: {e}"))?;
        let phrase_jp = db.all_phrase_rows_with_jp().map_err(|e| format!("phrase_jp: {e}"))?;
        let pinyin = db.all_single_char_rows().map_err(|e| format!("pinyin: {e}"))?;
        let mut mem = MemoryIndex::new();
        for (key, value, weight) in &phrase {
            mem.insert(key, value, *weight);
        }
        // 混拼索引(jp 精确 + 反混),对齐原 SQLite query_phrase_mixed 三条路径。
        for (jp, key, value, weight) in &phrase_jp {
            mem.insert_mixed(jp, key, value, *weight);
        }
        for (key, value, weight) in &pinyin {
            mem.insert(key, value, *weight);
        }
        mem.finalize(); // 反混桶预排序(权重降序),查询 early-exit
        Ok(mem)
    }

    /// 加载或构建内存索引(索引持久化:第一次建好后存盘,之后启动直接反序列化)。
    ///
    /// 流程(2026-09-02 加 mmap 优先):
    ///   1. 先试 `<db>.midx`(mmap 紧凑格式):mmap 映射微秒级、零解析、多进程共享物理页,
    ///      校验 magic/version/db 指纹通过即用之(来源 "mmap")。
    ///   2. 否则读 `<db>.idx`(bincode):校验 指纹+格式版本,对上则用之(来源 "cache")。
    ///   3. 否则从 DB 重建,并(原子写)同时落盘 .idx 与 .midx(双写灰度,来源 "rebuilt")。
    /// 任何一步失败都回退到纯 SQLite(mem=None),不影响功能。
    /// 返回 (是否走内存索引, 来源描述) 供日志/诊断。
    pub fn load_or_build_index(&mut self, db_path: &str) -> (bool, &'static str) {
        // 2026-09-02 残页/跳页/回删卡顿 根因定位结论:mmap 内存索引层在某些时刻返回
        // 与 midx 实际内容不一致的数据,导致候选窗渲染出残页(位置4空白)、自动跳最后一页、
        // 回删卡顿。二分已证:强制走 SQLite 时这些现象全部消失。故彻底旁路内存索引,
        // 固定走 SQLite(唯一已验证干净的路径)。后续只剩「扩充词库」让 SQLite 查询够用。
        // mmap/.idx/trie 三层全部停用;mmap_index 模块保留但不再进入加载链。
        let _ = db_path;
        self.mem = None;
        (false, "sqlite")
    }

    /// 尝试 mmap 加载;文件缺失/损坏/词库已变 返回 None。
    fn try_load_mmap(&self, db_path: &str) -> Option<MemoryIndex> {
        let path = mmap_path_for(db_path);
        if !path.exists() {
            return None;
        }
        let (db_len, db_mtime) = db_fingerprint(db_path)?;
        let m = crate::mmap_index::MmapIndex::map(&path, db_len, db_mtime).ok()?;
        Some(MemoryIndex::from_mmap(m))
    }

    /// 原子写 mmap 缓存(先写临时文件再 rename)。
    fn save_mmap(db_path: &str, mem: &MemoryIndex) -> Result<(), String> {
        let path = mmap_path_for(db_path);
        let (db_len, db_mtime) = db_fingerprint(db_path).ok_or("db stat")?;
        let bytes = crate::mmap_index::build_from_memory_index(mem, db_len, db_mtime)?;
        let af = atomicwrites::AtomicFile::new(&path, atomicwrites::OverwriteBehavior::AllowOverwrite);
        af.write(|f| std::io::Write::write_all(f, &bytes))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 尝试从缓存加载内存索引;缓存缺失/损坏/词库已变 返回 None。
    fn try_load_cached(&self, db_path: &str) -> Option<MemoryIndex> {
        let cache = cache_path_for(db_path);
        let bytes = std::fs::read(&cache).ok()?;
        let (db_len, db_mtime) = db_fingerprint(db_path)?;
        // 头部定长解码(bincode 对定长结构是定长的),先解析头部校验再解 body。
        let header_len = bincode::serialized_size(&IndexHeader {
            magic: *b"PIXC",
            version: 0,
            db_len: 0,
            db_mtime_secs: 0,
        })
        .ok()? as usize;
        if bytes.len() < header_len {
            return None;
        }
        let header: IndexHeader = bincode::deserialize(&bytes[..header_len]).ok()?;
        if header.magic != *b"PIXC"
            || header.version != INDEX_CACHE_VERSION
            || header.db_len != db_len
            || header.db_mtime_secs != db_mtime
        {
            return None; // 词库变了或版本不符 → 触发重建
        }
        MemoryIndex::from_bytes(&bytes[header_len..]).ok()
    }

    /// 原子写缓存(先写临时文件再 rename,避免中途崩溃留半个坏文件)。
    fn save_cached(db_path: &str, mem: &MemoryIndex) -> Result<(), String> {
        let cache = cache_path_for(db_path);
        let (db_len, db_mtime) = db_fingerprint(db_path).ok_or("db stat")?;
        let header = IndexHeader {
            magic: *b"PIXC",
            version: INDEX_CACHE_VERSION,
            db_len,
            db_mtime_secs: db_mtime,
        };
        let mut bytes = bincode::serialize(&header).map_err(|e| e.to_string())?;
        bytes.extend_from_slice(&mem.to_bytes()?);
        let af = atomicwrites::AtomicFile::new(&cache, atomicwrites::OverwriteBehavior::AllowOverwrite);
        af.write(|f| std::io::Write::write_all(f, &bytes))
            .map_err(|e| format!("atomic write: {e}"))
    }

    pub fn set_fuzzy_rules(&mut self, rules: Vec<&'static str>) {
        self.fuzzy_rules = rules;
    }

    pub fn segment(&self, input: &str) -> Vec<String> {
        self.syll.segment(input)
    }

    fn is_full_pinyin(&self, s: &str) -> bool {
        self.syll.is_full_pinyin(s)
    }

    /// 五笔候选查询(wubi86 表,精确编码匹配,权重降序)。
    /// 与拼音 `query` 并列,供 FFI `prisir_ime_query_wubi` 调用。
    pub fn query_wubi(&self, input: &str) -> Vec<Candidate> {
        let key = input.trim().to_lowercase();
        if key.is_empty() {
            return Vec::new();
        }
        self.db.query_wubi(&key, 50).unwrap_or_default()
    }

    /// 拼音候选查询(对齐 Python _query_pinyin)
    pub fn query(&self, input: &str) -> Vec<Candidate> {
        // 单字母(2026-09-02):直接走前缀高权单字桶,先于一切合并逻辑返回。
        // 对齐外挂内存路径 trie z→在/张/中/这/再…(前缀节点高权桶)。
        // 放在最前是因为「z」会被 query_phrase_by_key_prefix 命中 phrase 表 z 开头的词,
        // 使 seen 非空而提前返回窄结果;外挂语义是单字母就给前缀高权字,不做整词/混拼。
        if input.chars().count() == 1 {
            return self.db.query_pinyin_prefix_top(input, 50).unwrap_or_default();
        }
        let n_syll = self.segment(input).len().max(1);
        let mut seen: HashMap<String, i64> = HashMap::new();

        // 模糊音扩展(仅单音节单字查询)
        let fuzzy_inputs = if n_syll == 1 { self.fuzzy_expand(input) } else { vec![input.to_string()] };

        if let Some(mem) = &self.mem {
            for fp in &fuzzy_inputs {
                // 单字通道:模糊音扩展出的翘舌音(fp != input)单字常被高频词组挤出
                // 词组 top-N,必须走独立单字通道才拿得到(zi→zhi 的「之/知/只」)。
                for (word, w) in mem.query_prefix_single(fp) {
                    upsert(&mut seen, word, w);
                }
                // 词组通道:多字词须字数==音节数(沿用原错配过滤)
                for (word, w) in mem.query_prefix(fp) {
                    let wlen = word.chars().count();
                    if wlen > 1 && wlen != n_syll {
                        continue;
                    }
                    upsert(&mut seen, word, w);
                }
            }
        } else {
            // 2026-09-02 候选覆盖/排序修复(对齐外挂内存路径语义):
            // 原音(input 本身)与模糊扩展音(zhi)分开排序——原音字按权重排前,模糊字殿后,
            // 不全局混排。外挂内存路径之所以 zi 出「自/字/子/资…」且含耔/缁/赀,正是因为
            // zi 原音字在 zi 节点单字桶里天然排在模糊 zhi 字之前;混排会让高权 zhi 字
            // 反超低频 zi 原音字(耔 被挤出前页)。这里复刻该语义。
            // fuzzy_expand 保证原音在前,fuzzy_inputs[0]==input,其余为模糊扩展。
            let orig = input.to_string();
            // 1) 原音字:按权重降序,先占满
            if let Ok(rows) = self.db.query_pinyin(&orig, 50) {
                for (word, w) in rows {
                    upsert(&mut seen, word, w);
                }
            }
            // 2) 模糊扩展音字:殿后(仅当模糊音开启且与原音不同才查)
            for fp in fuzzy_inputs.iter().filter(|fp| **fp != orig) {
                if let Ok(rows) = self.db.query_pinyin(fp, 50) {
                    for (word, w) in rows {
                        // 模糊字权重压到最低档之下,确保不反超原音字(保留相对顺序)
                        upsert(&mut seen, word, w - FUZZY_DEMOTE);
                    }
                }
            }
            // 3) 整词词组(错配过滤)
            if let Ok(rows) = self.db.query_phrase(input, 50) {
                for (word, w) in rows {
                    if word.chars().count() != n_syll {
                        continue;
                    }
                    upsert(&mut seen, word, w);
                }
            }
        }

        // 混拼词组(正反向)
        for (word, w) in self.query_phrase_mixed(input) {
            upsert(&mut seen, word, w);
        }

        // 学习权重合并(2026-09-03 引入,2026-09-04 改学成置顶+位置锁定):
        // user.db 里用户学过的词提到最前。学成词给固定权重 LEARN_PIN_WEIGHT(>词库 max≈26000),
        // **不再叠加 base+lw、不再 +1 累积、不做时间衰减** → 学成词一旦学会位置永久锁定,
        // 解决「偶尔用的词位置老变、无法固化调用」(用户 2026-09-04 明确不要衰减)。
        // 多个学成词按 user_words_for 返回的 seq(学习先后)升序稳定排序,先学在前。
        // 学习库独有的新词(主库没有的自造词)也补进来。user.db 只读查询,微秒级。
        // seen 是 HashMap<String, i64>,为保住学成词的 seq 次序,单独收集学成词再前置。
        let user_words = self.db.user_words_for(input);
        if !user_words.is_empty() {
            // 学成词从 seen 移除(若在),稍后按 seq 序前置,避免被 HashMap 随机序打乱。
            for (word, _seq) in &user_words {
                seen.remove(word);
            }
        }

        if !seen.is_empty() || !user_words.is_empty() {
            // 多字真整词优先于单字,同权重整词靠前
            let mut merged: Vec<Candidate> = seen.into_iter().collect();
            // 排序必须完全确定(2026-09-02 修「翻页候选顺序漂移」):seen 是 HashMap,
            // into_iter 顺序每次随机;若 sort 只比多字/权重,同权重单字的相对顺序由
            // HashMap 随机顺序决定 → 每次 query 候选顺序都不同 → 翻页页码/内容漂移。
            // 加决胜键:权重相同再比字符串本身(字典序),保证同输入必得同序。
            merged.sort_by(|a, b| {
                let a_multi = a.0.chars().count() > 1;
                let b_multi = b.0.chars().count() > 1;
                b_multi
                    .cmp(&a_multi)
                    .then(b.1.cmp(&a.1))
                    .then(a.0.cmp(&b.0))
            });
            // 学成词按 seq 升序前置到候选最前(位置锁定:固定权重 + 学习次序)。
            // user_words_for 已按 seq ASC 返回,直接前插即得稳定次序。
            let pinned: Vec<Candidate> = user_words
                .into_iter()
                .map(|(word, _seq)| (word, crate::db::LEARN_PIN_WEIGHT))
                .collect();
            let mut out = pinned;
            out.extend(merged);
            out.truncate(50);
            return out;
        }

        // 多字母无整匹配:取首音节单字
        let segs = self.segment(input);
        if !segs.is_empty() {
            return self.db.query_pinyin(&segs[0], 50).unwrap_or_default();
        }
        Vec::new()
    }

    /// 模糊音扩展(对齐 Python _fuzzy_expand),原音在前。
    fn fuzzy_expand(&self, inp: &str) -> Vec<String> {
        let valid = |cand: &str| {
            let segs = self.segment(cand);
            segs.len() == 1 && segs[0] == cand
        };
        let mut results: Vec<String> = vec![inp.to_string()];
        for rule in self.fuzzy_rules.clone() {
            for (src, dst) in fuzzy_map(rule) {
                if fuzzy_is_initial(rule) {
                    if let Some(rest) = inp.strip_prefix(*src) {
                        let cand = format!("{dst}{rest}");
                        if valid(&cand) && !results.contains(&cand) {
                            results.push(cand);
                        }
                    }
                } else if let Some(head) = inp.strip_suffix(*src) {
                    let cand = format!("{head}{dst}");
                    if valid(&cand) && !results.contains(&cand) {
                        results.push(cand);
                    }
                }
            }
        }
        results
    }

    /// 混拼词组查询(正反向),对齐 Python _query_phrase_mixed。
    fn query_phrase_mixed(&self, inp: &str) -> Vec<Candidate> {
        if inp.len() < 2 {
            return Vec::new();
        }
        let segs = self.segment(inp);
        if self.is_full_pinyin(inp) && segs.len() == 1 {
            return Vec::new();
        }
        let mut seen: HashMap<String, i64> = HashMap::new();
        let mut add = |rows: Vec<Candidate>| {
            for (val, w) in rows {
                if val.chars().count() < 2 {
                    continue;
                }
                upsert(&mut seen, val, w);
            }
        };
        if !self.is_full_pinyin(inp) {
            if let Some(mem) = &self.mem {
                add(mem.query_jp(inp, 20));
                let rest = &inp[1..];
                if self.is_full_pinyin(rest) {
                    let first = inp.chars().next().unwrap().to_string();
                    add(mem.query_reverse_mixed(&first, rest, 15));
                }
            } else {
                add(self.db.query_phrase_by_jp(inp, 20).unwrap_or_default());
                let rest = &inp[1..];
                if self.is_full_pinyin(rest) {
                    let first = inp.chars().next().unwrap().to_string();
                    add(self.db.query_phrase_reverse_mixed(&first, rest, 15).unwrap_or_default());
                }
            }
        }
        // key 前缀:有 mem 走 trie(内存),无则 SQLite 范围查询
        if let Some(mem) = &self.mem {
            let mut kpref: Vec<Candidate> = mem
                .query_prefix(inp)
                .into_iter()
                .filter(|(v, _)| v.chars().count() >= 2)
                .collect();
            kpref.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            kpref.truncate(15);
            add(kpref);
        } else {
            add(self.db.query_phrase_by_key_prefix(inp, 15).unwrap_or_default());
        }
        let mut out: Vec<Candidate> = seen.into_iter().collect();
        // 同 query():加字典序决胜键,消除 HashMap 随机顺序导致的同权重漂移。
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out.truncate(15);
        out
    }

    /// 整句智能首选(DP/Viterbi),对齐 Python smart_sentence。
    pub fn smart_sentence(&self, input: &str) -> Option<String> {
        let segs = self.segment(input);
        let n = segs.len();
        if n < 2 {
            return None;
        }

        let best_word = |pseq: &[String]| -> Option<Candidate> {
            let key = pseq.concat();
            if let Ok(rows) = self.db.query_phrase(&key, 1) {
                if let Some(r) = rows.into_iter().next() {
                    return Some(r);
                }
            }
            if pseq.len() == 1 {
                if let Ok(rows) = self.db.query_pinyin(&key, 5) {
                    let singles: Vec<Candidate> =
                        rows.into_iter().filter(|(w, _)| w.chars().count() == 1).collect();
                    if let Some(r) = singles.into_iter().next() {
                        return Some(r);
                    }
                }
            }
            None
        };

        // 整句命中完整整词优先(如 shurufa->输入法)
        if let Some(full) = best_word(&segs) {
            if full.0.chars().count() == n {
                return Some(full.0);
            }
        }

        let neg = f64::NEG_INFINITY;
        let mut dp: Vec<(f64, Vec<String>)> = vec![(neg, Vec::new()); n + 1];
        dp[0] = (0.0, Vec::new());
        for i in 1..=n {
            let start = if i >= 4 { i - 4 } else { 0 };
            for j in start..i {
                let pseq = &segs[j..i];
                let bw = best_word(pseq);
                let bw = match bw {
                    Some(x) if dp[j].0 != neg => x,
                    _ => continue,
                };
                let (word, weight) = bw;
                let bonus = if pseq.len() >= 2 { 20.0 } else { 0.0 };
                let score = dp[j].0 + (weight.max(1) as f64).ln() + bonus;
                if score > dp[i].0 {
                    let mut path = dp[j].1.clone();
                    path.push(word);
                    dp[i] = (score, path);
                }
            }
        }
        if dp[n].0 == neg || dp[n].1.is_empty() {
            return None;
        }
        Some(dp[n].1.concat())
    }

    /// 学习用户选择(更新词频)
    pub fn learn(&self, input: &str, selected: &str) {
        if input.is_empty() || selected.is_empty() {
            return;
        }
        let _ = self.db.add_user_word(input, selected, 1);
    }
}

fn upsert(map: &mut HashMap<String, i64>, word: String, w: i64) {
    match map.get(&word) {
        Some(&existing) if existing >= w => {}
        _ => {
            map.insert(word, w);
        }
    }
}
