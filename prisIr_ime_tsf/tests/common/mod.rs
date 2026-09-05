//! tests/common/mod.rs — e2e test 公共 helper
//!
//! T8 范围: 4 个 e2e test 全 #[ignore], 显式 `cargo test -- --ignored` 触发,
//! 默认 smoke 不跑(避免每跑 cargo test 都污染开发机)。
//!
//! helper 职责:
//!   - exe_path(): 拿 release build 出来的 prisir_tsfsvc.exe 路径
//!   - spawn_ipc(): 起 --ipc 子进程, stdin/stdout 用 pipe 接
//!   - send_line(): 喂一行 + 读一行(stdout 一行一 JSON)
//!   - kill_daemon(): taskkill /F /IM prisir_tsfsvc.exe(Windows only)

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

pub const EXE_REL: &str = "target/release/prisir_tsfsvc.exe";

pub fn exe_path() -> PathBuf {
    PathBuf::from("C:/Users/Administrator/oi_enhancements/prisIr_ime_tsf").join(EXE_REL)
}

pub fn dll_path() -> PathBuf {
    PathBuf::from("C:/Users/Administrator/oi_enhancements/prisIr_ime_tsf").join("target/release/prisir_ime_tsf.dll")
}

/// 起 `--ipc` 子进程, stdin/stdout/stderr 全 pipe。
pub fn spawn_ipc() -> Child {
    Command::new(exe_path())
        .arg("--ipc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn --ipc")
}

/// 喂一行 + 读一行(stdout 一行一 JSON-RPC)。
///
/// `ChildStdout` 没实现 `BufRead`, 必须用 `BufReader` 包一层。
pub fn send_line(child: &mut Child, line: &str) -> String {
    {
        let stdin = child.stdin.as_mut().expect("stdin pipe");
        writeln!(stdin, "{}", line).expect("write stdin");
        stdin.flush().expect("flush stdin");
    }
    let mut buf = String::new();
    {
        let stdout = child.stdout.as_mut().expect("stdout pipe");
        let mut reader = BufReader::new(stdout);
        reader.read_line(&mut buf).expect("read stdout");
    }
    buf.trim_end_matches(['\r', '\n']).to_string()
}

/// `taskkill /F /IM prisir_tsfsvc.exe` — 仅 Windows 上有意义。
///
/// 非 Windows 平台啥都不做(开发机就是 Windows,非 Windows 平台只是 cross-compile)。
pub fn kill_daemon() {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(&["/F", "/IM", "prisir_tsfsvc.exe"])
            .output();
    }
    #[cfg(not(windows))]
    {
        // 非 Windows: 不真杀(本测试主要在 Windows 开发机跑)
    }
}

/// 起 `--daemon` 子进程并返回 Child。
pub fn spawn_daemon() -> Child {
    Command::new(exe_path())
        .arg("--daemon")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn --daemon")
}