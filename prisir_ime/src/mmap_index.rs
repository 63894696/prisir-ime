//! mmap 紧凑索引(2026-09-02,根治切换卡顿/按键被吃)。
//!
//! **为什么**:bincode 反序列化 `MemoryIndex`(HashMap 递归树)是 CPU 密集(逐节点重建
//! HashMap),322MB → VM 上几秒,且每进程独立建一份。本模块把同一份逻辑索引编译成
//! **单块连续字节布局**,加载 = `memmap2` 只读映射(微秒级、零解析零分配),多进程共享
//! 同一物理页 —— 对齐微软拼音/搜狗的离线编译紧凑二进制 + mmap 共享工作方式。
//!
//! **结果一致性(关键路径不许假 PASS)**:构建器从同一份 `MemoryIndex` 导出,严格保留
//! top/top1/桶的既有顺序;4 个查询方法返回值与 `MemoryIndex` 逐字节一致(由
//! `tests/mmap_parity.rs` 对代表性输入断言)。查询语义与 MemoryIndex 完全对齐,上层透明。
//!
//! **安全**:所有读取走带边界检查的访问器;`from_bytes` 校验 magic/version/db 指纹 +
//! 各段偏移长度在文件范围内,任一不符即 `Err`,调用方回退 bincode/SQLite(不影响功能)。
//!
//! ### 文件布局(小端;偏移均为相对文件开头的字节位置)
//! ```text
//! [Header 固定 96B]
//!   magic[4]="MIDX" version:u32 db_len:u64 db_mtime:u64
//!   node_off:u32 node_count:u32
//!   child_off:u32 child_count:u32
//!   top_off:u32  top_count:u32
//!   jpmap_off:u32  jpmap_count:u32
//!   fjbucket_off:u32 fjbucket_count:u32
//!   fjmap_off:u32 fjmap_count:u32
//!   strpool_off:u32 strpool_len:u32
//!   (reserved 到 96B)
//! [Node[]      24B] child_start:u32 child_len:u32
//!                   top_start:u32 top_len:u32 top1_start:u32 top1_len:u32
//! [ChildEnt[]   8B] byte:u8 _pad[3] node_idx:u32        (按 byte 升序,查询二分)
//! [TopEnt[]    16B] str_off:u32 _pad:u32 weight:i64     (str_off→StrPool,'\0' 结尾)
//! [JpMapEnt[]  12B] jp_str_off:u32 bucket_start:u32 bucket_len:u32 (按 jp 串升序,二分;
//!                   bucket 是 TopEnt 段)
//! [FjBucketEnt[]16B] key_str_off:u32 val_str_off:u32 weight:i64 (桶内按 weight 降序;
//!                   查询 filter key.ends_with(rest),与 MemoryIndex 一致)
//! [FjMapEnt[]  12B] first_str_off:u32 bucket_start:u32 bucket_len:u32 (按 first 串升序,二分)
//! [StrPool]         所有字符串 UTF-8 拼接,每个以 '\0' 结尾,off 指向起始
//! ```

use memmap2::Mmap;
use std::path::Path;

use crate::trie::MemoryIndex;

pub const MMAP_MAGIC: &[u8; 4] = b"MIDX";
/// mmap 格式版本(布局变更 +1,旧缓存自动失效重建)。
pub const MMAP_FORMAT_VERSION: u32 = 1;

const HEADER_LEN: usize = 96;
const NODE_SIZE: usize = 24;
const CHILD_SIZE: usize = 8;
const TOP_SIZE: usize = 16;
const JPMAP_SIZE: usize = 12;
const FJMAP_SIZE: usize = 12;
const FJBUCKET_SIZE: usize = 16;

// ──────────────────────────────────────────────────────────────────────
// 字符串池(构建期):去重 + 记录每个串的起始偏移('\0' 结尾)
// ──────────────────────────────────────────────────────────────────────
struct StrPool {
    buf: Vec<u8>,
    map: std::collections::HashMap<String, u32>,
}
impl StrPool {
    fn new() -> Self {
        StrPool { buf: Vec::new(), map: std::collections::HashMap::new() }
    }
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&off) = self.map.get(s) {
            return off;
        }
        let off = self.buf.len() as u32;
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
        self.map.insert(s.to_string(), off);
        off
    }
}

/// 读字节池中某偏移处的 '\0' 结尾串(构建期排序比较 + 查询期读串共用)。
fn pool_str(pool: &[u8], off: u32) -> &[u8] {
    let start = off as usize;
    if start >= pool.len() {
        return &[];
    }
    let end = pool[start..]
        .iter()
        .position(|&b| b == 0)
        .map(|p| start + p)
        .unwrap_or(pool.len());
    &pool[start..end]
}

// ──────────────────────────────────────────────────────────────────────
// 构建器:从 &MemoryIndex 导出紧凑字节
// ──────────────────────────────────────────────────────────────────────

/// 从 `MemoryIndex` 构建紧凑 mmap 字节(含 Header)。
/// 严格保留 top/top1/桶的既有顺序(逐字节一致性的来源)。
pub fn build_from_memory_index(
    mem: &MemoryIndex,
    db_len: u64,
    db_mtime: u64,
) -> Result<Vec<u8>, String> {
    let mut strpool = StrPool::new();
    let mut nodes: Vec<u8> = Vec::new();
    let mut children: Vec<u8> = Vec::new();
    let mut tops: Vec<u8> = Vec::new();
    let mut jpmap: Vec<u8> = Vec::new();
    let mut fjmap: Vec<u8> = Vec::new();
    let mut fjbucket: Vec<u8> = Vec::new();

    // 递归拍平 trie(深度优先,先孩子后回填本节点),返回节点索引。
    fn flatten(
        node: &crate::trie::Node,
        nodes: &mut Vec<u8>,
        children: &mut Vec<u8>,
        tops: &mut Vec<u8>,
        strpool: &mut StrPool,
    ) -> u32 {
        let my_idx = (nodes.len() / NODE_SIZE) as u32;
        nodes.extend_from_slice(&[0u8; NODE_SIZE]); // 占位

        // top(词组)段
        let top_start = (tops.len() / TOP_SIZE) as u32;
        for (v, w) in node.top_slice() {
            let off = strpool.intern(v);
            tops.extend_from_slice(&off.to_le_bytes());
            tops.extend_from_slice(&0u32.to_le_bytes());
            tops.extend_from_slice(&w.to_le_bytes());
        }
        let top_len = (tops.len() / TOP_SIZE) as u32 - top_start;

        // top1(单字)段
        let top1_start = (tops.len() / TOP_SIZE) as u32;
        for (v, w) in node.top1_slice() {
            let off = strpool.intern(v);
            tops.extend_from_slice(&off.to_le_bytes());
            tops.extend_from_slice(&0u32.to_le_bytes());
            tops.extend_from_slice(&w.to_le_bytes());
        }
        let top1_len = (tops.len() / TOP_SIZE) as u32 - top1_start;

        // children:递归后按 byte 排序写 ChildEnt
        let mut kids: Vec<(u8, u32)> = Vec::new();
        for (b, child) in node.children_iter() {
            let ci = flatten(child, nodes, children, tops, strpool);
            kids.push((*b, ci));
        }
        kids.sort_by_key(|(b, _)| *b);
        let child_start = (children.len() / CHILD_SIZE) as u32;
        for (b, ci) in &kids {
            children.push(*b);
            children.extend_from_slice(&[0u8; 3]);
            children.extend_from_slice(&ci.to_le_bytes());
        }
        let child_len = (children.len() / CHILD_SIZE) as u32 - child_start;

        // 回填本节点
        let base = my_idx as usize * NODE_SIZE;
        nodes[base..base + 4].copy_from_slice(&child_start.to_le_bytes());
        nodes[base + 4..base + 8].copy_from_slice(&child_len.to_le_bytes());
        nodes[base + 8..base + 12].copy_from_slice(&top_start.to_le_bytes());
        nodes[base + 12..base + 16].copy_from_slice(&top_len.to_le_bytes());
        nodes[base + 16..base + 20].copy_from_slice(&top1_start.to_le_bytes());
        nodes[base + 20..base + 24].copy_from_slice(&top1_len.to_le_bytes());
        my_idx
    }

    flatten(mem.root_node(), &mut nodes, &mut children, &mut tops, &mut strpool);

    // jp_map:桶写进 tops 段,映射按 jp 串内容排序(二分前提)
    let mut jp_entries: Vec<(u32, u32, u32)> = Vec::new();
    for (jp, bucket) in mem.jp_map_iter() {
        let jp_off = strpool.intern(jp);
        let bstart = (tops.len() / TOP_SIZE) as u32;
        for (v, w) in bucket {
            let off = strpool.intern(v);
            tops.extend_from_slice(&off.to_le_bytes());
            tops.extend_from_slice(&0u32.to_le_bytes());
            tops.extend_from_slice(&w.to_le_bytes());
        }
        let blen = (tops.len() / TOP_SIZE) as u32 - bstart;
        jp_entries.push((jp_off, bstart, blen));
    }
    jp_entries.sort_by(|a, b| pool_str(&strpool.buf, a.0).cmp(pool_str(&strpool.buf, b.0)));
    for (jp_off, bstart, blen) in &jp_entries {
        jpmap.extend_from_slice(&jp_off.to_le_bytes());
        jpmap.extend_from_slice(&bstart.to_le_bytes());
        jpmap.extend_from_slice(&blen.to_le_bytes());
    }

    // first_jp_map:桶写进 fjbucket 段,映射按 first 串内容排序
    let mut fj_entries: Vec<(u32, u32, u32)> = Vec::new();
    for (first, bucket) in mem.first_jp_map_iter() {
        let first_off = strpool.intern(first);
        let bstart = (fjbucket.len() / FJBUCKET_SIZE) as u32;
        for (k, v, w) in bucket {
            let koff = strpool.intern(k);
            let voff = strpool.intern(v);
            fjbucket.extend_from_slice(&koff.to_le_bytes());
            fjbucket.extend_from_slice(&voff.to_le_bytes());
            fjbucket.extend_from_slice(&w.to_le_bytes());
        }
        let blen = (fjbucket.len() / FJBUCKET_SIZE) as u32 - bstart;
        fj_entries.push((first_off, bstart, blen));
    }
    fj_entries.sort_by(|a, b| pool_str(&strpool.buf, a.0).cmp(pool_str(&strpool.buf, b.0)));
    for (first_off, bstart, blen) in &fj_entries {
        fjmap.extend_from_slice(&first_off.to_le_bytes());
        fjmap.extend_from_slice(&bstart.to_le_bytes());
        fjmap.extend_from_slice(&blen.to_le_bytes());
    }

    // 拼 Header + 各段
    let node_off = HEADER_LEN as u32;
    let node_count = (nodes.len() / NODE_SIZE) as u32;
    let child_off = node_off + nodes.len() as u32;
    let child_count = (children.len() / CHILD_SIZE) as u32;
    let top_off = child_off + children.len() as u32;
    let top_count = (tops.len() / TOP_SIZE) as u32;
    let jpmap_off = top_off + tops.len() as u32;
    let jpmap_count = (jpmap.len() / JPMAP_SIZE) as u32;
    let fjbucket_off = jpmap_off + jpmap.len() as u32;
    let fjbucket_count = (fjbucket.len() / FJBUCKET_SIZE) as u32;
    let fjmap_off = fjbucket_off + fjbucket.len() as u32;
    let fjmap_count = (fjmap.len() / FJMAP_SIZE) as u32;
    let strpool_off = fjmap_off + fjmap.len() as u32;
    let strpool_len = strpool.buf.len() as u32;

    let mut out = vec![0u8; HEADER_LEN];
    out[0..4].copy_from_slice(MMAP_MAGIC);
    out[4..8].copy_from_slice(&MMAP_FORMAT_VERSION.to_le_bytes());
    out[8..16].copy_from_slice(&db_len.to_le_bytes());
    out[16..24].copy_from_slice(&db_mtime.to_le_bytes());
    let mut put = |slot: usize, v: u32| out[slot..slot + 4].copy_from_slice(&v.to_le_bytes());
    put(24, node_off); put(28, node_count);
    put(32, child_off); put(36, child_count);
    put(40, top_off); put(44, top_count);
    put(48, jpmap_off); put(52, jpmap_count);
    put(56, fjbucket_off); put(60, fjbucket_count);
    put(64, fjmap_off); put(68, fjmap_count);
    put(72, strpool_off); put(76, strpool_len);
    // 80..96 reserved

    out.extend_from_slice(&nodes);
    out.extend_from_slice(&children);
    out.extend_from_slice(&tops);
    out.extend_from_slice(&jpmap);
    out.extend_from_slice(&fjbucket);
    out.extend_from_slice(&fjmap);
    out.extend_from_slice(&strpool.buf);
    Ok(out)
}

// ──────────────────────────────────────────────────────────────────────
// MmapIndex:只读映射 + 4 个查询方法(语义对齐 MemoryIndex)
// ──────────────────────────────────────────────────────────────────────

/// 解析后的段视图(偏移+计数),从 Header 读出并校验。
struct Sections {
    node_off: usize, node_count: usize,
    child_off: usize, child_count: usize,
    top_off: usize, top_count: usize,
    jpmap_off: usize, jpmap_count: usize,
    fjbucket_off: usize, fjbucket_count: usize,
    fjmap_off: usize, fjmap_count: usize,
    strpool_off: usize, strpool_len: usize,
}

fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn rd_u64(b: &[u8], off: usize) -> Option<u64> {
    b.get(off..off + 8).map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

fn parse_sections(b: &[u8]) -> Result<Sections, String> {
    if b.len() < HEADER_LEN {
        return Err("file too small for header".into());
    }
    if &b[0..4] != MMAP_MAGIC {
        return Err("bad magic".into());
    }
    let version = rd_u32(b, 4).ok_or("version")?;
    if version != MMAP_FORMAT_VERSION {
        return Err(format!("version mismatch {version}"));
    }
    let g = |o: usize| rd_u32(b, o).map(|v| v as usize).ok_or_else(|| format!("hdr@{o}"));
    let s = Sections {
        node_off: g(24)?, node_count: g(28)?,
        child_off: g(32)?, child_count: g(36)?,
        top_off: g(40)?, top_count: g(44)?,
        jpmap_off: g(48)?, jpmap_count: g(52)?,
        fjbucket_off: g(56)?, fjbucket_count: g(60)?,
        fjmap_off: g(64)?, fjmap_count: g(68)?,
        strpool_off: g(72)?, strpool_len: g(76)?,
    };
    // 边界校验:每段 [off, off+count*size) 必须在文件内
    let total = b.len();
    let chk = |off: usize, count: usize, size: usize, name: &str| -> Result<(), String> {
        let end = off.checked_add(count.checked_mul(size).ok_or("overflow")?).ok_or("overflow")?;
        if end > total {
            return Err(format!("section {name} out of range"));
        }
        Ok(())
    };
    chk(s.node_off, s.node_count, NODE_SIZE, "node")?;
    chk(s.child_off, s.child_count, CHILD_SIZE, "child")?;
    chk(s.top_off, s.top_count, TOP_SIZE, "top")?;
    chk(s.jpmap_off, s.jpmap_count, JPMAP_SIZE, "jpmap")?;
    chk(s.fjbucket_off, s.fjbucket_count, FJBUCKET_SIZE, "fjbucket")?;
    chk(s.fjmap_off, s.fjmap_count, FJMAP_SIZE, "fjmap")?;
    chk(s.strpool_off, s.strpool_len, 1, "strpool")?;
    Ok(s)
}

/// mmap 只读索引。查询语义与 `MemoryIndex` 逐字节一致。
pub struct MmapIndex {
    data: Mmap,
    sec: Sections,
}

impl MmapIndex {
    /// 映射 `.midx` 文件并校验 magic/version/db 指纹 + 段边界。
    pub fn map(path: &Path, db_len: u64, db_mtime: u64) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
        // SAFETY:文件以只读打开并映射;索引文件由本进程/构建器原子写,运行期不被修改。
        let data = unsafe { Mmap::map(&file) }.map_err(|e| format!("mmap: {e}"))?;
        let sec = parse_sections(&data)?;
        // db 指纹校验(词库变了缓存失效)
        let flen = rd_u64(&data, 8).ok_or("db_len")?;
        let fmtime = rd_u64(&data, 16).ok_or("db_mtime")?;
        if flen != db_len || fmtime != db_mtime {
            return Err("db fingerprint mismatch".into());
        }
        Ok(MmapIndex { data, sec })
    }

    // ---- 内部读取访问器(带边界检查) ----
    fn node(&self, idx: usize) -> Option<(usize, usize, usize, usize, usize, usize)> {
        let b = &self.data[..];
        let base = self.sec.node_off.checked_add(idx.checked_mul(NODE_SIZE)?)?;
        Some((
            rd_u32(b, base)? as usize,
            rd_u32(b, base + 4)? as usize,
            rd_u32(b, base + 8)? as usize,
            rd_u32(b, base + 12)? as usize,
            rd_u32(b, base + 16)? as usize,
            rd_u32(b, base + 20)? as usize,
        ))
    }
    fn child_at(&self, i: usize) -> Option<(u8, usize)> {
        let b = &self.data[..];
        let base = self.sec.child_off.checked_add(i.checked_mul(CHILD_SIZE)?)?;
        let byte = *b.get(base)?;
        let idx = rd_u32(b, base + 4)? as usize;
        Some((byte, idx))
    }
    fn top_at(&self, i: usize) -> Option<(u32, i64)> {
        let b = &self.data[..];
        let base = self.sec.top_off.checked_add(i.checked_mul(TOP_SIZE)?)?;
        let str_off = rd_u32(b, base)?;
        // TopEnt 16B: str_off(0..4) + pad(4..8) + weight(8..16)
        let weight = rd_u64(b, base + 8)? as i64;
        Some((str_off, weight))
    }
    fn pool(&self, off: u32) -> &[u8] {
        let b = &self.data[..];
        let start = self.sec.strpool_off + off as usize;
        if start >= self.sec.strpool_off + self.sec.strpool_len {
            return &[];
        }
        let region = &b[self.sec.strpool_off..self.sec.strpool_off + self.sec.strpool_len];
        pool_str(region, off)
    }

    /// 沿前缀找节点索引(二分 children)。
    fn find_node(&self, prefix: &str) -> Option<usize> {
        let mut node_idx = 0usize; // root 是 flatten 的第一个节点
        for &byte in prefix.as_bytes() {
            let (cs, cl, _, _, _, _) = self.node(node_idx)?;
            // 二分 children[cs..cs+cl] 找 byte
            let (mut lo, mut hi) = (cs, cs + cl);
            let mut found = None;
            while lo < hi {
                let mid = (lo + hi) / 2;
                let (b, ci) = self.child_at(mid)?;
                if b < byte {
                    lo = mid + 1;
                } else if b > byte {
                    hi = mid;
                } else {
                    found = Some(ci);
                    break;
                }
            }
            node_idx = found?;
        }
        Some(node_idx)
    }

    fn read_top_range(&self, start: usize, len: usize) -> Vec<(String, i64)> {
        let mut out = Vec::with_capacity(len);
        for i in start..start + len {
            if let Some((str_off, weight)) = self.top_at(i) {
                let s = self.pool(str_off);
                if let Ok(txt) = std::str::from_utf8(s) {
                    out.push((txt.to_string(), weight));
                }
            }
        }
        out
    }

    /// 前缀查询(词组通道)——对齐 MemoryIndex::query_prefix。
    pub fn query_prefix(&self, prefix: &str) -> Vec<(String, i64)> {
        match self.find_node(prefix).and_then(|i| self.node(i)) {
            Some((_, _, ts, tl, _, _)) => self.read_top_range(ts, tl),
            None => Vec::new(),
        }
    }

    /// 前缀查询(单字通道)——对齐 MemoryIndex::query_prefix_single。
    pub fn query_prefix_single(&self, prefix: &str) -> Vec<(String, i64)> {
        match self.find_node(prefix).and_then(|i| self.node(i)) {
            Some((_, _, _, _, t1s, t1l)) => self.read_top_range(t1s, t1l),
            None => Vec::new(),
        }
    }

    /// jp 精确查询——对齐 MemoryIndex::query_jp。
    pub fn query_jp(&self, jp: &str, limit: usize) -> Vec<(String, i64)> {
        // 二分 jpmap 找 jp 串
        let (mut lo, mut hi) = (0usize, self.sec.jpmap_count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let base = self.sec.jpmap_off + mid * JPMAP_SIZE;
            let (Some(joff), Some(bs), Some(bl)) = (
                rd_u32(&self.data, base),
                rd_u32(&self.data, base + 4),
                rd_u32(&self.data, base + 8),
            ) else { return Vec::new() };
            let key = self.pool(joff);
            match key.cmp(jp.as_bytes()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let mut out = self.read_top_range(bs as usize, bl as usize);
                    out.truncate(limit);
                    return out;
                }
            }
        }
        Vec::new()
    }

    /// 反混查询——对齐 MemoryIndex::query_reverse_mixed。
    /// first 命中桶后,filter key.ends_with(rest),取前 limit(桶已按 weight 降序)。
    pub fn query_reverse_mixed(&self, first: &str, rest: &str, limit: usize) -> Vec<(String, i64)> {
        let (mut lo, mut hi) = (0usize, self.sec.fjmap_count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let base = self.sec.fjmap_off + mid * FJMAP_SIZE;
            let (Some(foff), Some(bs), Some(bl)) = (
                rd_u32(&self.data, base),
                rd_u32(&self.data, base + 4),
                rd_u32(&self.data, base + 8),
            ) else { return Vec::new() };
            let key = self.pool(foff);
            match key.cmp(first.as_bytes()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let mut out = Vec::new();
                    for i in bs as usize..(bs + bl) as usize {
                        let fb = self.sec.fjbucket_off + i * FJBUCKET_SIZE;
                        let (Some(koff), Some(voff), Some(w)) = (
                            rd_u32(&self.data, fb),
                            rd_u32(&self.data, fb + 4),
                            rd_u64(&self.data, fb + 8),
                        ) else { break };
                        let k = self.pool(koff);
                        if k.ends_with(rest.as_bytes()) {
                            if let Ok(v) = std::str::from_utf8(self.pool(voff)) {
                                out.push((v.to_string(), w as i64));
                                if out.len() >= limit {
                                    break;
                                }
                            }
                        }
                    }
                    return out;
                }
            }
        }
        Vec::new()
    }
}
