//! e2e_method_switch — --method pinyin/wubi 切换 + --method foo 报错 + --status 验证默认
//!
//! 验证:
//!   - --method pinyin → stdout 含 "[method] active: pinyin" (进程内立即生效, 由 stdout 自证)
//!   - --method wubi   → stdout 含 "[method] active: wubi"
//!   - --method foo    → 退出码 ≠ 0 + stderr 含 "unknown method" / "FAIL"
//!   - --status (新进程) → JSON active_method=pinyin(默认, 因 process-local, 跨进程回归默认)
//!
//! 设计注:
//!   main.rs 明确写出 "下次启动回归默认(拼音)。进程内立即生效" —
//!   active_method 是进程内 AtomicU8, 不持久化。所以跨进程的 --status 永远看到
//!   默认值(pinyin)。每个 test 步骤都是新 spawn 的进程, 在每个进程内 --method
//!   立即生效, 由 stdout 自证。

mod common;

use std::process::Command;

#[test]
#[ignore] // 显式 `cargo test --test e2e_method_switch -- --ignored` 触发
fn method_switch_pinyin_wubi() {
    let e = common::exe_path();

    // (1) --method pinyin → stdout 自证
    let o = Command::new(&e)
        .args(&["--method", "pinyin"])
        .output()
        .expect("--method pinyin");
    assert!(
        o.status.success(),
        "--method pinyin 失败: stderr={}",
        String::from_utf8_lossy(&o.stderr)
    );
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.contains("[method] active: pinyin"),
        "method pinyin 输出应含 '[method] active: pinyin', 实际: {}",
        stdout
    );

    // (2) --method wubi → stdout 自证
    let o = Command::new(&e)
        .args(&["--method", "wubi"])
        .output()
        .expect("--method wubi");
    assert!(
        o.status.success(),
        "--method wubi 失败: stderr={}",
        String::from_utf8_lossy(&o.stderr)
    );
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.contains("[method] active: wubi"),
        "method wubi 输出应含 '[method] active: wubi', 实际: {}",
        stdout
    );

    // (3) --method foo → exit ≠ 0 + stderr 报错
    let o = Command::new(&e)
        .args(&["--method", "foo"])
        .output()
        .expect("--method foo");
    assert!(
        !o.status.success(),
        "--method foo 应 exit≠0, 实际 exit={:?}",
        o.status.code()
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains("unknown method") || stderr.contains("FAIL"),
        "--method foo stderr 应含 unknown method / FAIL, 实际: {}",
        stderr
    );

    // (4) 跨进程 --status: 新进程默认 active_method=pinyin(因 process-local, 不持久化)
    let o = Command::new(&e).arg("--status").output().expect("--status default");
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.contains("\"active_method\": \"pinyin\""),
        "新进程 --status 应见 active_method=pinyin (默认), 实际: {}",
        stdout
    );

    // (5) 回归默认 pinyin(双保险)
    let o = Command::new(&e)
        .args(&["--method", "pinyin"])
        .output()
        .expect("--method pinyin reset");
    assert!(
        o.status.success(),
        "复位 pinyin 失败: stderr={}",
        String::from_utf8_lossy(&o.stderr)
    );
}