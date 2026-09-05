//! ipc.rs — stdio JSON-RPC server for chrome extension / settings UI
//!
//! 通信: stdin/stdout 一行一 JSON
//!   请求: {"method": "...", "params": {...}, "id": <int|null>}
//!   响应: {"result": ..., "id": ...} 或 {"error": {"code": N, "message": "..."}, "id": ...}
//!
//! 7 个 method:
//!   version     — crate 版本 + prisir_tsf_version_string()
//!   status      — HKCU 注册状态 + pid + version
//!   register    — 写 HKCU CTF TIP + COM InprocServer32
//!   unregister  — 删 HKCU 两个 key
//!   enable      — HKCU Enable DWORD = 1
//!   disable     — HKCU Enable DWORD = 0
//!   query       — 拼音查询(走 ffi::prisir_tsf_load_engine + query)
//!
//! 错误码(JSON-RPC 2.0 风格):
//!   -32700  parse error(请求体不是合法 JSON)
//!   -32601  method not found
//!   -32602  invalid params
//!   -32603  internal error
//!   -32000  server error(走 ffi / register 时失败)
//!
//! T6 设计原则:
//!   - 纯 stdio,不起 socket、不起 pipe,chrome extension 用 child_process 调起
//!   - 不调 WTS / 不拉 dll(register / query 路径才拉 dll,query 走 ffi::prisir_tsf_*)
//!   - --ipc-test 不调 register/unregister/enable/disable,避免污染开发机
//!   - --status 改 JSON 输出(破坏性变化,见 main.rs)

#![allow(unused_unsafe)] // FFI 调用在 `unsafe extern "C" fn` 同模块内已隐式 unsafe,逐个 `unsafe { }` block 会被 lint 多算

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct IpcRequest {
    pub method: String,
    #[serde(default)]
    pub params: Value,
    pub id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct IpcResponse {
    pub result: Value,
    pub id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct IpcError {
    pub error: IpcErrorBody,
    pub id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct IpcErrorBody {
    pub code: i32,
    pub message: String,
}

fn ok(id: Option<Value>, result: Value) -> String {
    serde_json::to_string(&IpcResponse { result, id })
        .unwrap_or_else(|_| r#"{"error":{"code":-32603,"message":"internal serialize failed"}}"#.to_string())
}

fn err(id: Option<Value>, code: i32, message: impl Into<String>) -> String {
    serde_json::to_string(&IpcError {
        error: IpcErrorBody { code, message: message.into() },
        id,
    })
    .unwrap_or_else(|_| r#"{"error":{"code":-32603,"message":"internal serialize failed"}}"#.to_string())
}

/// 单条 JSON-RPC 请求 → 单条 JSON-RPC 响应字符串。
///
/// 拆成独立函数(ipc-test 也调)便于:
///   - smoke 单测只验字符串内容
///   - 未来若起 socket / pipe 也直接复用
pub fn handle_request(line: &str) -> String {
    let req: IpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => return err(None, -32700, format!("parse error: {e}")),
    };
    let id = req.id.clone();
    match req.method.as_str() {
        "version" => ok(
            id.clone(),
            json!({
                "crate": env!("CARGO_PKG_NAME"),
                "version": env!("CARGO_PKG_VERSION"),
                "tsf_version": crate::ffi::prisir_tsf_version_string(),
            }),
        ),
        "status" => match crate::register::do_status() {
            Ok(s) => ok(
                id.clone(),
                json!({
                    "tip_key_exists": s.tip_key_exists,
                    "clsid_key_exists": s.clsid_key_exists,
                    "clsid": s.clsid,
                    "version": env!("CARGO_PKG_VERSION"),
                    "pid": std::process::id(),
                }),
            ),
            Err(e) => err(id, -32000, format!("status failed: {e}")),
        },
        "register" => {
            let dll_path = req
                .params
                .get("dll_path")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| {
                    let exe = std::env::current_exe().expect("current_exe");
                    exe.parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .join("prisir_ime_tsf.dll")
                        .to_string_lossy()
                        .into_owned()
                });
            match crate::register::do_register(&dll_path) {
                Ok(r) => ok(
                    id,
                    json!({
                        "status": "registered",
                        "dll_path": dll_path,
                        "entries_written": r.entries_written,
                    }),
                ),
                Err(e) => err(id, -32000, format!("register failed: {e}")),
            }
        }
        "unregister" => match crate::register::do_unregister() {
            Ok(()) => ok(id, json!({"status": "unregistered"})),
            Err(e) => err(id, -32000, format!("unregister failed: {e}")),
        },
        "enable" => match crate::register::do_enable() {
            Ok(()) => ok(id, json!({"status": "enabled", "enable_dword": 1})),
            Err(e) => err(id, -32000, format!("enable failed: {e}")),
        },
        "disable" => match crate::register::do_disable() {
            Ok(()) => ok(id, json!({"status": "disabled", "enable_dword": 0})),
            Err(e) => err(id, -32000, format!("disable failed: {e}")),
        },
        "query" => {
            let pinyin = match req.params.get("pinyin").and_then(|v| v.as_str()) {
                Some(p) => p.to_string(),
                None => return err(id, -32602, "missing params.pinyin"),
            };
            let db_path = req
                .params
                .get("db_path")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| {
                    std::env::var("PRISIR_CIKU_DB").unwrap_or_else(|_| {
                        std::env::var("LOCALAPPDATA")
                            .map(|d| format!("{}\\Prisir\\Browser\\models\\ciku.db", d))
                            .unwrap_or_else(|_| {
                                "voice_input\\lingxi_ime\\backend\\ciku.db".to_string()
                            })
                    })
                });
            let db_c = match std::ffi::CString::new(db_path.clone()) {
                Ok(c) => c,
                Err(e) => return err(id, -32602, format!("bad db_path: {e}")),
            };
            let pinyin_c = match std::ffi::CString::new(pinyin.clone()) {
                Ok(c) => c,
                Err(e) => return err(id, -32602, format!("bad pinyin: {e}")),
            };
            let h = unsafe { crate::ffi::prisir_tsf_load_engine(db_c.as_ptr()) };
            if h.is_null() {
                return err(id, -32000, "load engine failed");
            }
            let json_ptr = unsafe { crate::ffi::prisir_tsf_query(h, pinyin_c.as_ptr()) };
            if json_ptr.is_null() {
                unsafe { crate::ffi::prisir_tsf_free_engine(h) };
                return err(id, -32000, "query returned null");
            }
            let json_str = unsafe { std::ffi::CStr::from_ptr(json_ptr) }.to_string_lossy().into_owned();
            unsafe { crate::ffi::prisir_tsf_free_string(json_ptr) };
            unsafe { crate::ffi::prisir_tsf_free_engine(h) };
            let candidates: Value = serde_json::from_str(&json_str).unwrap_or(json!([]));
            ok(id, json!({"pinyin": pinyin, "candidates": candidates}))
        }
        other => err(id, -32601, format!("method not found: {other}")),
    }
}

/// 启动 stdio JSON-RPC server: 循环读 stdin 行 → handle_request → 写 stdout 行。
///
/// 退出条件: stdin EOF(pipe 关闭 / 子进程退出)。
pub fn run_ipc_server() -> Result<(), String> {
    eprintln!("[ipc] server started, pid={}", std::process::id());
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("read stdin: {e}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = handle_request(trimmed);
        writeln!(stdout, "{}", response).map_err(|e| format!("write stdout: {e}"))?;
        stdout.flush().ok();
    }
    eprintln!("[ipc] server stopped (stdin EOF)");
    Ok(())
}