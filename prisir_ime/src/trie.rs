//! 全内存 Trie 前缀索引(移植 trie_index.py MemoryIndex)。
//! 启动时把 phrase/pinyin 全量灌进来,每个节点保留权重 top-N 结果,
//! 前缀查询 = 走到节点直接拿缓存,O(前缀长),避免每次 SQL。
//!
//! 2026-09-02:节点增加 `top1`(单字)通道。原 `top` 对所有词(含多字词组)混排,
//! 高频多字词把单字挤出 top-N → 模糊音扩展出的翘舌单字(zi→zhi 的「之/知/只」)
//! 在构建期就丢失,query 期补不回。拆两个通道:单字进 `top1`,词组进 `top`,
//! query 时按需取,既不丢单字、又不增加内存查询耗时。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const NODE_TOP: usize = 12; // 每节点缓存的词组 top 结果数(对齐 Python)
const NODE_TOP1: usize = 24; // 每节点缓存的单字 top 结果数(单字歧义多,给足:一页5个够翻4页)

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Node {
    children: HashMap<u8, Node>,
    // (value, weight),按 weight 降序保留 top NODE_TOP;多字词组通道
    top: Vec<(String, i64)>,
    // (value, weight),按 weight 降序保留 top NODE_TOP1;单字通道(LENGTH(value)=1)
    top1: Vec<(String, i64)>,
}

#[derive(Serialize, Deserialize)]
pub struct MemoryIndex {
    root: Node,
    // ---- 混拼内存索引(2026-09-02):原 query_phrase_mixed 走 SQLite,非全拼音节前缀
    // (zo/nih/删字中途) 每击键 10-15ms 卡顿。构建时灌全量 phrase 的 jp/key,查询内存命中。
    /// jp 精确 → [(value,weight)] 降序(sj→世界/时间)。对齐 query_phrase_by_jp。
    jp_map: HashMap<String, Vec<(String, i64)>>,
    /// jp 首字母 → [(key,value,weight)],反混查询按 first 取桶再 filter key.ends_with(rest)。
    /// 对齐 query_phrase_reverse_mixed。桶内按 weight 降序。
    first_jp_map: HashMap<String, Vec<(String, String, i64)>>,
    /// mmap 后端(2026-09-02):有 Some 时 4 个查询方法直接走 mmap(微秒加载、多进程共享),
    /// 不参与 bincode 序列化(skip)。加载路径见 engine::load_or_build_index。
    /// root/jp_map/first_jp_map 在 mmap 模式下为空(不占用内存,数据全在映射页里)。
    #[serde(skip)]
    mmap: Option<crate::mmap_index::MmapIndex>,
}

impl MemoryIndex {
    // ---- 序列化(索引持久化:第一次建好后存盘,之后启动直接加载) ----
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        bincode::serialize(self).map_err(|e| format!("serialize: {e}"))
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        bincode::deserialize(bytes).map_err(|e| format!("deserialize: {e}"))
    }
    pub fn new() -> Self {
        MemoryIndex {
            root: Node::default(),
            jp_map: HashMap::new(),
            first_jp_map: HashMap::new(),
            mmap: None,
        }
    }

    /// 构造 mmap 后端的 MemoryIndex(2026-09-02):root/桶为空,查询全走 mmap。
    /// 内存占用 ≈ 映射页(多进程共享),不再每进程重建 HashMap 树。
    pub fn from_mmap(m: crate::mmap_index::MmapIndex) -> Self {
        MemoryIndex {
            root: Node::default(),
            jp_map: HashMap::new(),
            first_jp_map: HashMap::new(),
            mmap: Some(m),
        }
    }

    /// 灌一条词组的混拼索引 (jp,key,value,weight)。jp 可能为空(脏数据),跳过空 jp。
    pub fn insert_mixed(&mut self, jp: &str, key: &str, value: &str, weight: i64) {
        if jp.is_empty() {
            return;
        }
        // jp 精确桶(去重 + 降序 + 截断,上限放宽到 64:jp 命中通常很少)
        let bucket = self.jp_map.entry(jp.to_string()).or_default();
        insert_top(bucket, value, weight, 64);
        // 反混桶:按 jp 首字母归桶,存 (key,value,weight),查询时 filter key 后缀。
        if let Some(first) = jp.chars().next() {
            let fb = self.first_jp_map.entry(first.to_string()).or_default();
            // 反混桶大(同首字母词组多,z 桶约 76k),去重 (key,value) 保留高权重。
            // 不在插入期排序(每插一条排一次是 O(n^2 log n)),查询前统一排序一次(见 finalize)。
            if let Some(ex) = fb.iter_mut().find(|(k, v, _)| k == key && v == value) {
                if weight > ex.2 {
                    ex.2 = weight;
                }
            } else {
                fb.push((key.to_string(), value.to_string(), weight));
            }
        }
    }

    /// 构建收尾:把反混桶按 weight 降序排一次。反序列化旧缓存若无序也调用一次。
    /// 预排序后查询可 early-exit:取前 limit 个命中即是最高权重,无需全扫+全排。
    pub fn finalize(&mut self) {
        for bucket in self.first_jp_map.values_mut() {
            bucket.sort_by(|a, b| b.2.cmp(&a.2));
        }
    }

    /// jp 精确查询(内存)。对齐 db.query_phrase_by_jp。
    pub fn query_jp(&self, jp: &str, limit: usize) -> Vec<(String, i64)> {
        if let Some(m) = &self.mmap {
            return m.query_jp(jp, limit);
        }
        match self.jp_map.get(jp) {
            Some(v) => v.iter().take(limit).cloned().collect(),
            None => Vec::new(),
        }
    }

    /// 反混查询(内存):jp 首字母=first,结果 filter key 以 rest 结尾,按权重降序取 limit。
    /// 对齐 db.query_phrase_reverse_mixed。
    /// 桶已 finalize 预排序(权重降序),故扫描取前 limit 个命中即是最高权重,可 early-exit,
    /// 不必扫满整个大桶(z 桶约 76k)→ 稳态亚毫秒。
    pub fn query_reverse_mixed(&self, first: &str, rest: &str, limit: usize) -> Vec<(String, i64)> {
        if let Some(m) = &self.mmap {
            return m.query_reverse_mixed(first, rest, limit);
        }
        let bucket = match self.first_jp_map.get(first) {
            Some(b) => b,
            None => return Vec::new(),
        };
        bucket
            .iter()
            .filter(|(key, _, _)| key.ends_with(rest))
            .take(limit)
            .map(|(_, val, w)| (val.clone(), *w))
            .collect()
    }

    /// 插入一条 (key,value,weight);沿路径每个节点按字数分流更新 top1/top。
    pub fn insert(&mut self, key: &str, value: &str, weight: i64) {
        let single = value.chars().count() == 1;
        let mut node = &mut self.root;
        for &b in key.as_bytes() {
            node = node.children.entry(b).or_default();
            if single {
                insert_top(&mut node.top1, value, weight, NODE_TOP1);
            } else {
                insert_top(&mut node.top, value, weight, NODE_TOP);
            }
        }
    }

    /// 前缀查询(词组通道):返回该前缀下 top 多字词(权重降序)。
    pub fn query_prefix(&self, prefix: &str) -> Vec<(String, i64)> {
        if let Some(m) = &self.mmap {
            return m.query_prefix(prefix);
        }
        match self.find_node(prefix) {
            Some(n) => n.top.clone(),
            None => Vec::new(),
        }
    }

    /// 前缀查询(单字通道):返回该前缀下 top 单字(权重降序)。
    /// 模糊音扩展依赖它拿到翘舌单字(否则被词组挤出)。
    pub fn query_prefix_single(&self, prefix: &str) -> Vec<(String, i64)> {
        if let Some(m) = &self.mmap {
            return m.query_prefix_single(prefix);
        }
        match self.find_node(prefix) {
            Some(n) => n.top1.clone(),
            None => Vec::new(),
        }
    }

    fn find_node(&self, prefix: &str) -> Option<&Node> {
        let mut node = &self.root;
        for &b in prefix.as_bytes() {
            match node.children.get(&b) {
                Some(n) => node = n,
                None => return None,
            }
        }
        Some(node)
    }

    // ---- 只读访问器(2026-09-02,供 mmap_index builder 遍历导出) ----
    // 不暴露可变引用,保证 builder 只读、不影响索引内容。
    pub(crate) fn root_node(&self) -> &Node {
        &self.root
    }
    pub(crate) fn jp_map_iter(&self) -> impl Iterator<Item = (&String, &Vec<(String, i64)>)> {
        self.jp_map.iter()
    }
    pub(crate) fn first_jp_map_iter(
        &self,
    ) -> impl Iterator<Item = (&String, &Vec<(String, String, i64)>)> {
        self.first_jp_map.iter()
    }
}

impl Node {
    pub(crate) fn children_iter(&self) -> impl Iterator<Item = (&u8, &Node)> {
        self.children.iter()
    }
    pub(crate) fn top_slice(&self) -> &[(String, i64)] {
        &self.top
    }
    pub(crate) fn top1_slice(&self) -> &[(String, i64)] {
        &self.top1
    }
}

/// 往 top 列表插一条并保持按 weight 降序、去重 value、截断到 cap。
fn insert_top(top: &mut Vec<(String, i64)>, value: &str, weight: i64, cap: usize) {
    // 去重:同 value 保留更高权重
    if let Some(existing) = top.iter_mut().find(|(v, _)| v == value) {
        if weight > existing.1 {
            existing.1 = weight;
        }
    } else {
        top.push((value.to_string(), weight));
    }
    top.sort_by(|a, b| b.1.cmp(&a.1));
    top.truncate(cap);
}
