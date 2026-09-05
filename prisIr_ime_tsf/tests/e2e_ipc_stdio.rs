//! e2e_ipc_stdio — 真 spawn --ipc + stdin 喂 9 个 JSON 请求 + stdout 验证
//!
//! 9 个请求 = 7 method (version / status / query / register / unregister / enable / disable)
//!            + 1 未知 method (foo)
//!            + 1 parse error (not json)
//!
//! 验收:
//!   - 7 method 响应要么 result 要么 -32000(server error, e.g. 缺 dll/db)
//!   - 未知 method → -32601 + method not found
//!   - parse error → -32700 + parse error
//!
//! 不污染验证:
//!   register/enable 路径会真写 HKCU, 末尾必 --unregister 清理。

mod common;

#[test]
#[ignore] // 显式 `cargo test --test e2e_ipc_stdio -- --ignored` 触发
fn ipc_9_requests_roundtrip() {
    let mut child = common::spawn_ipc();

    // version: 必含 crate + version + tsf_version
    let resp = common::send_line(
        &mut child,
        r#"{"method":"version","params":{},"id":1}"#,
    );
    assert!(
        resp.contains("\"crate\":\"prisir_ime_tsf\""),
        "version 响应应含 crate=prisir_ime_tsf, 实际: {}",
        resp
    );
    assert!(
        resp.contains("\"version\":\"0.6.0\""),
        "version 响应应含 version=0.6.0, 实际: {}",
        resp
    );

    // status: 必含 tip_key_exists + clsid + pid + version
    let resp = common::send_line(
        &mut child,
        r#"{"method":"status","params":{},"id":2}"#,
    );
    assert!(
        resp.contains("\"pid\"") && resp.contains("\"version\""),
        "status 响应应含 pid + version, 实际: {}",
        resp
    );

    // query nihao: 响应要么 candidates 要么 -32000(server error: 缺 dll/db)
    let resp = common::send_line(
        &mut child,
        r#"{"method":"query","params":{"pinyin":"nihao"},"id":3}"#,
    );
    assert!(
        resp.contains("\"candidates\"") || resp.contains("-32000"),
        "query 响应应含 candidates 或 -32000, 实际: {}",
        resp
    );

    // register: 必 result 或 -32000
    let resp = common::send_line(
        &mut child,
        r#"{"method":"register","params":{},"id":4}"#,
    );
    assert!(
        resp.contains("\"result\"") || resp.contains("-32000"),
        "register 响应应 result 或 -32000, 实际: {}",
        resp
    );

    // unregister: 必 result 或 -32000
    let resp = common::send_line(
        &mut child,
        r#"{"method":"unregister","params":{},"id":5}"#,
    );
    assert!(
        resp.contains("\"result\"") || resp.contains("-32000"),
        "unregister 响应应 result 或 -32000, 实际: {}",
        resp
    );

    // enable: 必 result 或 -32000
    let resp = common::send_line(
        &mut child,
        r#"{"method":"enable","params":{},"id":6}"#,
    );
    assert!(
        resp.contains("\"result\"") || resp.contains("-32000"),
        "enable 响应应 result 或 -32000, 实际: {}",
        resp
    );

    // disable: 必 result 或 -32000
    let resp = common::send_line(
        &mut child,
        r#"{"method":"disable","params":{},"id":7}"#,
    );
    assert!(
        resp.contains("\"result\"") || resp.contains("-32000"),
        "disable 响应应 result 或 -32000, 实际: {}",
        resp
    );

    // 未知 method: 必 -32601 + method not found
    let resp = common::send_line(
        &mut child,
        r#"{"method":"foo","params":{},"id":99}"#,
    );
    assert!(
        resp.contains("-32601") && resp.contains("method not found"),
        "未知 method 必 -32601 + method not found, 实际: {}",
        resp
    );

    // parse error: 必 -32700 + parse error
    let resp = common::send_line(&mut child, "not json");
    assert!(
        resp.contains("-32700") && resp.contains("parse error"),
        "非法 JSON 必 -32700 + parse error, 实际: {}",
        resp
    );

    // 不污染验证: 末尾 --unregister 清理(如果上面 register 成功了)
    let resp = common::send_line(
        &mut child,
        r#"{"method":"unregister","params":{},"id":100}"#,
    );
    eprintln!("[e2e_ipc_stdio] cleanup unregister response: {}", resp);

    drop(child.stdin.take()); // EOF, daemon 退出
    let _ = child.wait();
}