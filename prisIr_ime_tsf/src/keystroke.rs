//! PinyinBuffer 状态机 — 纯 Rust,无 TSF 依赖
//!
//! T3 阶段承担「键码 → 候选上屏」的核心状态机。
//! 真正的拼音查询通过 `ffi::prisir_tsf_query` 走动态加载的 `prisIr_ime.dll`,
//! 这里只负责:
//!   - 累加 a-z 输入
//!   - 退格 (BS) 删除
//!   - 数字键 (1-9) 选第 n 个候选上屏
//!   - 空格选首位候选上屏
//!   - ESC 清空缓冲区
//!
//! T7 阶段扩展:
//!   - 进程级活动输入法(AtomicU8: 0=Pinyin, 1=Wubi),由 `set_active_method` 切换
//!   - `KeystrokeBuffer::query_candidates` 按 method 分发(当前 Pinyin/Wubi 都走
//!     `_query`,但 method 字段已挂在 buffer 上,后续五笔根码查询真差异化时直接接)
//!
//! 上屏函数返回 `Some(String)` 表示有候选输出,调用方负责 `InsertTextAtSelection`。
//! 返回 `None` 表示未触发上屏(用户继续在缓冲区累积)。
#![allow(dead_code)]

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

use crate::hotkey::InputMethod;

/// 单个候选词(词 + 权重)。
/// 与 `prisIr_ime.dll` 暴露的 JSON `{"word": ..., "weight": ...}` 对齐。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Candidate {
    pub word: String,
    // weight 必须 i64(2026-09-04 修 shou=0):引擎权重是 i64,模糊音降级
    // (w - FUZZY_DEMOTE)会产生负权重;u64 接不住 → serde_json 反序列化失败
    // → 静默 unwrap_or_default 返空 → 候选清空字母上屏。
    pub weight: i64,
}

impl Candidate {
    pub fn new(word: impl Into<String>, weight: i64) -> Self {
        Self { word: word.into(), weight }
    }
}

/// 每行(页)候选数默认值 — 2026-09-03 改:横排单行,一页几个由 `cand_per_page()` 读
/// 环境变量 `PRISIR_CAND_PER_PAGE`(设置面板预留),缺省 5。
pub const DEFAULT_CAND_PER_PAGE: usize = 5;

/// 读每行候选数配置。环境变量 PRISIR_CAND_PER_PAGE ∈ [3, 9],非法/未设 → 默认 5。
/// 设置面板落地后改成读配置文件/注册表,这里先留 env 通道。
pub fn cand_per_page() -> usize {
    std::env::var("PRISIR_CAND_PER_PAGE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| (3..=9).contains(&n))
        .unwrap_or(DEFAULT_CAND_PER_PAGE)
}

/// 兼容旧代码:仍有一些地方用 CAND_PER_PAGE 常量(测试/日志)。运行时翻页用 `cand_per_page()`。
pub const CAND_PER_PAGE: usize = DEFAULT_CAND_PER_PAGE;

/// 拼音缓冲区 + 候选列表状态机。
pub struct PinyinBuffer {
    /// 已累积的 a-z 输入(不含数字键 / 退格 / 候选)
    pub buf: String,
    /// 由 FFI 查询注入的候选列表(全量,翻页在其上切片)
    pub candidates: Vec<Candidate>,
    /// 当前高亮候选在**当前可视区**内的索引(空格 = 默认 0)
    pub selected: usize,
    /// 当前页码(0 起)。横排模式 = 整页;派生自 row_offset。
    pub page: usize,
    /// 横排整页翻页的可视区首(全量候选索引)。row_offset = page * cand_per_page()。
    pub row_offset: usize,
    /// T7: 当前缓冲区所属输入法(拼音 / 五笔)。纯 Rust,不影响 FFI 链路
    /// (DLL `_query` 当前对 method 不敏感,字段挂这为 T8 真差异化预留)。
    pub method: InputMethod,
}

impl Default for PinyinBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl PinyinBuffer {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            candidates: Vec::new(),
            selected: 0,
            page: 0,
            row_offset: 0,
            method: active_method(),
        }
    }

    /// T7: 显式构造时指定 method(给五笔测试用)。
    pub fn new_with_method(method: InputMethod) -> Self {
        Self {
            buf: String::new(),
            candidates: Vec::new(),
            selected: 0,
            page: 0,
            row_offset: 0,
            method,
        }
    }

    /// 处理 ASCII 字符键 — 仅接受 a-z,其他字符一律忽略(拼音不会含其它符号)。
    /// 返回 `true` 表示接受了字符,`false` 表示忽略。
    pub fn on_char(&mut self, ch: char) -> bool {
        if ch.is_ascii_lowercase() {
            self.buf.push(ch);
            true
        } else {
            false
        }
    }

    /// 退格 — 弹出缓冲区最后一个字符,空缓冲时无操作。
    /// 返回 `true` 表示实际删除了一字符,`false` 表示空缓冲无操作。
    /// 任何对 buf 的修改都应该清空候选列表(候选是针对旧 buf 的)。
    pub fn on_backspace(&mut self) -> bool {
        if self.buf.is_empty() {
            return false;
        }
        self.buf.pop();
        self.candidates.clear();
        self.selected = 0;
        self.row_offset = 0;
        self.page = 0;
        true
    }

    /// 数字键 1-9 上屏 — 选当前可视区第 n 个候选(可视区首 = row_offset)。
    /// 全量索引 = row_offset + (n-1)。调用后清空缓冲区与候选。越界返回 None。
    pub fn on_digit(&mut self, n: u8) -> Option<String> {
        if !(1..=9).contains(&n) {
            return None;
        }
        let idx = self.row_offset + (n - 1) as usize;
        if idx >= self.candidates.len() {
            return None;
        }
        let chosen = self.candidates[idx].word.clone();
        self.reset();
        Some(chosen)
    }

    /// 兼容旧签名(翻页选错字修复遗留):数字选词以可视区 row_offset 为准,与候选窗一致。
    pub fn on_digit_at_page(&mut self, n: u8, _page: usize) -> Option<String> {
        self.on_digit(n)
    }

    /// 空格上屏 — 默认选可视区 `selected` 位置(默认 0 = 首行),候选为空时直接
    /// 上屏缓冲区(简拼模式无候选时,把整个拼音按字面量提交,符合大多数 IME 行为)。
    pub fn on_space(&mut self) -> Option<String> {
        let chosen = if self.candidates.is_empty() {
            std::mem::take(&mut self.buf)
        } else {
            let idx = (self.row_offset + self.selected).min(self.candidates.len() - 1);
            std::mem::replace(&mut self.candidates[idx].word, String::new())
        };
        self.reset();
        if chosen.is_empty() { None } else { Some(chosen) }
    }

    /// ESC — 清空缓冲区与候选,无上屏。
    pub fn on_escape(&mut self) {
        self.reset();
    }

    /// 重新查询 — FFI 调用完后注入新候选列表。重置 selected / row_offset 回首页。
    pub fn set_candidates(&mut self, cands: Vec<Candidate>) {
        self.candidates = cands;
        self.selected = 0;
        self.row_offset = 0;
        self.page = 0;
    }

    /// 可视区候选切片(供候选窗渲染)——可视区首 = row_offset,横排单行一页 cand_per_page() 个。
    pub fn page_slice(&self) -> &[Candidate] {
        let start = self.row_offset;
        if start >= self.candidates.len() {
            return &[];
        }
        let end = (start + cand_per_page()).min(self.candidates.len());
        &self.candidates[start..end]
    }

    /// 是否还能向下翻 / 向上翻(纯横排整页)。
    pub fn has_next_page(&self) -> bool {
        self.row_offset + cand_per_page() < self.candidates.len()
    }
    pub fn has_prev_page(&self) -> bool {
        self.row_offset > 0
    }

    /// 向下翻(横排整页,2026-09-03):row_offset += cand_per_page(),有下一页才翻。
    /// 返回是否真翻了(到末页没动返 false,翻页键透传)。
    pub fn page_down(&mut self) -> bool {
        if !self.has_next_page() {
            return false;
        }
        self.row_offset += cand_per_page();
        self.page = self.row_offset / cand_per_page();
        self.selected = 0;
        true
    }
    /// 向上翻(横排整页):row_offset -= cand_per_page(),到首页停。
    pub fn page_up(&mut self) -> bool {
        if !self.has_prev_page() {
            return false;
        }
        self.row_offset = self.row_offset.saturating_sub(cand_per_page());
        self.page = self.row_offset / cand_per_page();
        self.selected = 0;
        true
    }

    /// 重查快捷键(Ctrl+R 等)由调用方解析后调用本函数 — 触发上屏旧候选然后调用方再走 FFI 重查。
    /// 这里只给出工具方法,把当前选中候选上屏返回。
    pub fn commit_selected(&mut self) -> Option<String> {
        if self.candidates.is_empty() {
            return None;
        }
        let idx = (self.row_offset + self.selected).min(self.candidates.len() - 1);
        let chosen = self.candidates[idx].word.clone();
        self.reset();
        Some(chosen)
    }

    /// T7: 按 `self.method` 走 FFI 查询(拼音 / 五笔 当前都走 `_query`,DLL 端 method
    /// 真差异化留给 T8)。返回的 Vec 不会自动写入 `self.candidates`,调用方需 `set_candidates`。
    ///
    /// `engine_handle` 是 `prisir_tsf_load_engine` 返的句柄,null 时直接返空 Vec
    /// (开发机无 dll/db 时静默失败,不污染用户态)。
    pub fn query_candidates(&self, engine_handle: *mut std::ffi::c_void) -> Vec<Candidate> {
        match self.method {
            InputMethod::Pinyin | InputMethod::Wubi => self.query_via_ffi(engine_handle),
        }
    }

    fn query_via_ffi(&self, h: *mut std::ffi::c_void) -> Vec<Candidate> {
        use std::ffi::{CStr, CString};
        if h.is_null() { return Vec::new(); }
        let c = match CString::new(self.buf.clone()) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let json_ptr = crate::ffi::prisir_tsf_query(h, c.as_ptr());
        if json_ptr.is_null() { return Vec::new(); }
        let json_str = unsafe { CStr::from_ptr(json_ptr) }.to_string_lossy().into_owned();
        crate::ffi::prisir_tsf_free_string(json_ptr);
        serde_json::from_str(&json_str).unwrap_or_default()
    }

    /// 整句首选(smart_sentence)— 多音节拼音经语言模型转最优整句,返拼接字符串。
    /// 与 query(逐词候选)不同:输入整段拼音,引擎做分词+句级解码,直接给最优句。
    /// null 句柄 / 空 buffer / dll 未加载 → 返 None(静默,不污染用户态)。
    pub fn smart_sentence(&self, engine_handle: *mut std::ffi::c_void) -> Option<String> {
        use std::ffi::{CStr, CString};
        let h = engine_handle;
        if h.is_null() || self.buf.len() < 4 { return None; } // 少于4字母没必要整句
        let c = CString::new(self.buf.clone()).ok()?;
        let ptr = crate::ffi::prisir_tsf_smart_sentence(h, c.as_ptr());
        if ptr.is_null() { return None; }
        let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        crate::ffi::prisir_tsf_free_string(ptr);
        let s = s.trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }

    /// v0.8: 公开 reset —— TSF 模块在 Activate/Deactivate 时清空缓冲区。
    pub fn reset(&mut self) {
        self.buf.clear();
        self.candidates.clear();
        self.selected = 0;
        self.row_offset = 0;
        self.page = 0;
    }

    /// 当前缓冲区状态快照(供 UI 渲染或日志)。
    pub fn snapshot(&self) -> BufferSnapshot<'_> {
        BufferSnapshot { buf: &self.buf, candidates: &self.candidates, selected: self.selected }
    }
}

/// 不可变快照(借用,生命周期跟随 PinyinBuffer)。
#[derive(Debug, Clone)]
pub struct BufferSnapshot<'a> {
    pub buf: &'a str,
    pub candidates: &'a [Candidate],
    pub selected: usize,
}

// ============================================================
// T7: 进程级活动输入法 — AtomicU8(0=Pinyin, 1=Wubi)
// ============================================================
//
// 仅做「当前输入法」标记,F5 切换 / 激活键切换都是写这个。
// 真按 method 走不同码表是 T8 沙盒 E2E 才接,本阶段 method 字段已挂在 PinyinBuffer 上。

const METHOD_PINYIN: u8 = 0;
const METHOD_WUBI: u8 = 1;

static ACTIVE_METHOD: AtomicU8 = AtomicU8::new(METHOD_PINYIN);

/// 切换进程级活动输入法。`--method` 子命令调它。
pub fn set_active_method(m: InputMethod) {
    let v = match m {
        InputMethod::Pinyin => METHOD_PINYIN,
        InputMethod::Wubi   => METHOD_WUBI,
    };
    ACTIVE_METHOD.store(v, Ordering::SeqCst);
}

/// 读取进程级活动输入法。
pub fn active_method() -> InputMethod {
    match ACTIVE_METHOD.load(Ordering::SeqCst) {
        METHOD_WUBI => InputMethod::Wubi,
        _           => InputMethod::Pinyin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_char_only_accepts_lowercase() {
        let mut b = PinyinBuffer::new();
        assert!(b.on_char('n'));
        assert!(b.on_char('i'));
        assert!(!b.on_char('N')); // 大写拒绝
        assert!(!b.on_char('1')); // 数字拒绝
        assert!(!b.on_char(' ')); // 空格拒绝
        assert_eq!(b.buf, "ni");
    }

    #[test]
    fn on_backspace_pops_last() {
        let mut b = PinyinBuffer::new();
        for c in "nihao".chars() { b.on_char(c); }
        assert_eq!(b.buf, "nihao");
        assert!(b.on_backspace());
        assert_eq!(b.buf, "niha");
        assert!(!b.on_backspace() && b.buf.is_empty() || b.on_backspace()); // 第二次还能再删
        for _ in 0..10 { b.on_backspace(); }
        assert!(b.buf.is_empty());
        assert!(!b.on_backspace());
    }

    #[test]
    fn paging_selects_across_pages() {
        // 12 个候选 → 3 页(每页 5)。横排翻页后数字键选可视区的候选。
        let cands: Vec<Candidate> = (0..12).map(|i| Candidate::new(format!("w{i}"), i as u64)).collect();
        let mut b = PinyinBuffer::new();
        b.buf = "ni".to_string();
        b.set_candidates(cands);
        assert_eq!(b.page_slice().len(), 5);
        assert!(b.has_next_page() && !b.has_prev_page());
        // 横排翻到第 2 页(可视区 = 索引 5..10)
        assert!(b.page_down());
        assert_eq!(b.row_offset, 5);
        assert_eq!(b.page_slice()[0].word, "w5");
        // 数字 2 → 可视区第 2 个 = 全量索引 6 = w6
        assert_eq!(b.on_digit(2).as_deref(), Some("w6"));
        assert!(b.candidates.is_empty() && b.row_offset == 0);
    }

    #[test]
    fn paging_bounds() {
        // 7 个候选:首页满 5,余 2。纯横排整页:第 2 页只有 2 个。
        let cands: Vec<Candidate> = (0..7).map(|i| Candidate::new(format!("w{i}"), i as u64)).collect();
        let mut b = PinyinBuffer::new();
        b.set_candidates(cands);
        assert!(b.page_down());          // → off=5,残页 2 个
        assert_eq!(b.row_offset, 5);
        assert!(!b.has_next_page());     // 没有第 3 页
        assert!(!b.page_down());         // 翻不动
        assert_eq!(b.page_slice().len(), 2);
        assert!(b.page_up());            // → off=0
        assert!(!b.page_up());           // 已在首页
        // 末页数字越界:残页只 2 个,按 3 无效;按 2 = 索引 6 = w6
        b.page_down();
        assert_eq!(b.on_digit(3), None);
        b.page_down(); // 重新翻到末页(上一个 on_digit 越界不清候选,但保险起见)
        assert_eq!(b.on_digit(2).as_deref(), Some("w6"));
    }

    #[test]
    fn set_candidates_resets_page() {
        let cands: Vec<Candidate> = (0..12).map(|i| Candidate::new(format!("w{i}"), i as u64)).collect();
        let mut b = PinyinBuffer::new();
        b.set_candidates(cands.clone());
        b.page_down();
        assert_eq!(b.page, 1);
        assert_eq!(b.row_offset, 5);
        b.set_candidates(cands);
        assert_eq!(b.page, 0);
        assert_eq!(b.row_offset, 0);
        assert_eq!(b.selected, 0);
    }

    #[test]
    fn on_digit_selects_candidate() {
        let mut b = PinyinBuffer::new();
        b.set_candidates(vec![
            Candidate::new("你好", 100),
            Candidate::new("泥灰", 50),
            Candidate::new("拟或", 30),
        ]);
        assert_eq!(b.on_digit(1).as_deref(), Some("你好"));
        // 选完清空
        assert!(b.candidates.is_empty());
        assert!(b.buf.is_empty());
        // 数字 0 越界
        let mut b2 = PinyinBuffer::new();
        b2.set_candidates(vec![Candidate::new("你", 1)]);
        assert_eq!(b2.on_digit(0), None);
        // 数字 9 越界
        assert_eq!(b2.on_digit(9), None);
    }

    #[test]
    fn on_space_selects_first_or_submits_buf() {
        let mut b = PinyinBuffer::new();
        b.set_candidates(vec![
            Candidate::new("你", 100),
            Candidate::new("泥", 50),
        ]);
        assert_eq!(b.on_space().as_deref(), Some("你"));

        // 无候选时空格提交缓冲区
        let mut b2 = PinyinBuffer::new();
        for c in "ni".chars() { b2.on_char(c); }
        assert_eq!(b2.on_space().as_deref(), Some("ni"));
    }

    #[test]
    fn on_escape_clears() {
        let mut b = PinyinBuffer::new();
        for c in "nihao".chars() { b.on_char(c); }
        b.set_candidates(vec![Candidate::new("你好", 1)]);
        b.on_escape();
        assert!(b.buf.is_empty());
        assert!(b.candidates.is_empty());
    }

    #[test]
    fn set_candidates_resets_selected() {
        let mut b = PinyinBuffer::new();
        b.set_candidates(vec![Candidate::new("a", 1)]);
        // 模拟 selected=2 后再次 set
        b.set_candidates(vec![Candidate::new("b", 2), Candidate::new("c", 1)]);
        assert_eq!(b.selected, 0);
    }

    // ===== T7 新增:method 字段 + 全局 active_method =====

    #[test]
    fn new_buffer_inherits_active_method() {
        // 默认是 Pinyin
        assert_eq!(active_method(), InputMethod::Pinyin);
        let b = PinyinBuffer::new();
        assert_eq!(b.method, InputMethod::Pinyin);

        // 切换到 Wubi
        set_active_method(InputMethod::Wubi);
        assert_eq!(active_method(), InputMethod::Wubi);
        let b2 = PinyinBuffer::new();
        assert_eq!(b2.method, InputMethod::Wubi);

        // new_with_method 显式指定
        let b3 = PinyinBuffer::new_with_method(InputMethod::Pinyin);
        assert_eq!(b3.method, InputMethod::Pinyin);

        // 复位,避免污染同进程后续测试
        set_active_method(InputMethod::Pinyin);
        assert_eq!(active_method(), InputMethod::Pinyin);
    }

    #[test]
    fn query_candidates_with_null_handle_returns_empty() {
        // 句柄为 null 时,query_candidates 静默返空(开发机无 dll/db 不污染)
        let mut b = PinyinBuffer::new_with_method(InputMethod::Pinyin);
        for c in "nihao".chars() { b.on_char(c); }
        let result = b.query_candidates(std::ptr::null_mut());
        assert!(result.is_empty(), "null handle 必须返空 Vec");

        // Wubi 同上
        let mut bw = PinyinBuffer::new_with_method(InputMethod::Wubi);
        for c in "aaaa".chars() { bw.on_char(c); }
        let result_w = bw.query_candidates(std::ptr::null_mut());
        assert!(result_w.is_empty(), "null handle 五笔也必须返空 Vec");
    }
}