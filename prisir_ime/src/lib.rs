//! prisir_ime — Prisir Browser 内置输入法引擎 (IME 线 P0)
//!
//! 逻辑移植自已验证的 Python lingxi_ime(逻辑同源,Python 保留为开发/测试主力)。
//! 词库复用 ciku.db(phrase 186万 + pinyin 33万 + wubi86 13.7万),纯本地零外发。
//!
//! 模块:
//!   syllable  — 标准普通话音节表 + 拼音切分
//!   db        — rusqlite 读 ciku.db(单字/词组/简拼/前缀/反向混拼)
//!   trie      — 全内存 Trie 前缀索引(移植 trie_index.py)
//!   engine    — 候选查询合并排序 + 混拼 + 模糊音 + DP 整句智能首选

pub mod syllable;
pub mod db;
pub mod trie;
pub mod mmap_index;
pub mod engine;
pub mod ffi;

#[cfg(target_os = "android")]
pub mod jni;

pub use engine::ImeEngine;
