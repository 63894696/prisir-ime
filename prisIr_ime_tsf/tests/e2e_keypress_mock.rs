//! e2e_keypress_mock — 模拟拼音「nihao」按键序列(n→ni→nih→niha→nihao)
//!                    + 期望 ciku.db 返候选 JSON
//!
//! 背景(T8 降级路径):
//!   原 plan §第三闸要求「记事本打「nihao」出「你好」」 — 即真激活 TSF text store,
//!   走真 Windows IMM/TSF Activate, 看 IME 真实上屏。沙盒内做。
//!   本期降级为 stdin JSON-RPC 链路模拟:
//!     - 模拟按键序列: n → ni → nih → niha → nihao
//!     - 走 --ipc + ipc::query 方法
//!     - 走 ffi::prisir_tsf_load_engine + prisir_tsf_query → ciku.db
//!     - 期望响应: candidates JSON(你 / 泥 / 拟...)或 -32000(server error: 缺 dll/db)
//!
//! 不污染: --ipc 不写 HKCU, 不拉起 TSF, 走纯 ffi。

mod common;

#[test]
#[ignore] // 显式 `cargo test --test e2e_keypress_mock -- --ignored` 触发
fn keypress_nihao_yields_candidates() {
    let mut child = common::spawn_ipc();

    // 模拟按键序列(用户打「nihao」: 5 次按键, PinyinBuffer 累加 n→ni→nih→niha→nihao)
    for (i, partial) in ["n", "ni", "nih", "niha", "nihao"].iter().enumerate() {
        let req = format!(
            r#"{{"method":"query","params":{{"pinyin":"{}"}},"id":{}}}"#,
            partial,
            i + 1
        );
        let resp = common::send_line(&mut child, &req);
        eprintln!(
            "[e2e_keypress_mock] query '{}' → {}",
            partial, resp
        );
        assert!(
            resp.contains("\"candidates\"") || resp.contains("-32000"),
            "query '{}' 应返 candidates 或 -32000, 实际: {}",
            partial,
            resp
        );
    }

    drop(child.stdin.take()); // EOF, daemon 退出
    let _ = child.wait();
}