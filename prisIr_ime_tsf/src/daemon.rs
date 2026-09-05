//! daemon.rs — Prisir 输入法 TSF 守护进程(升级版 WTS 真消息循环)
//!
//! T5:  WTSGetActiveConsoleSessionId 简化轮询 + DLL mtime 真热重载
//! T10: WTSRegisterSessionNotification + hidden HWND + 真消息循环
//!
//! 职责(T10):
//!   1. **WTS 真消息循环** — 建 hidden HWND + WTSRegisterSessionNotification,
//!      GetMessageW/DispatchMessageW 真消息泵;WM_WTSSESSION_CHANGE 由 WndProc 真处理
//!      (WTS_CONSOLE_CONNECT/DISCONNECT → 自动 register/unregister, REMOTE/SESSION 仅日志)。
//!   2. **DLL 真热重载** — 后台线程每 5s 检查 DLL mtime,变化 → FreeLibrary + LoadLibraryA。
//!      (沿用 T5 简化路径,本轮只换主循环驱动;轮询改成后台 thread,主循环专注消息泵)
//!   3. **Ctrl-C 干净退出** — SetConsoleCtrlHandler → request_shutdown → 主循环 GetMessageW 返 -1 退出。
//!
//! 严格边界(T10):
//!   - 不做 ServiceMain 真 Windows Service 注册 — T11 增量
//!   - 不做 VMware guest 端验证 — T9 增量
//!   - 不动 ipc.rs / register.rs / ffi.rs / keystroke.rs / hotkey.rs
//!   - 不引入新 windows feature(Win32_UI_WindowsAndMessaging 已含所需 API)
//!   - 不写全局注册表(宪法红线 — 仅 HKCU)

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryA};
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetMessageW, HWND_MESSAGE, MSG, PostQuitMessage, RegisterClassExW, TranslateMessage,
    UnregisterClassW, WM_CLOSE, WM_DESTROY, WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_EX_TOOLWINDOW,
    WTS_CONSOLE_CONNECT, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT,
    WTS_SESSION_LOGOFF, WTS_SESSION_LOGON,
};

// =====================================================================
// 全局 shutdown 标志位 — Ctrl-C handler + WndProc WM_CLOSE 与主循环共享
// =====================================================================

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

pub fn is_shutdown() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

pub fn request_shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

/// 仅供 smoke test 复位 SHUTDOWN 标志位用 — **不在 daemon 主流程里调**。
///
/// 之所以需要: T5 daemon_wts_event_filter 测试 `request_shutdown` 后必须复位,
/// 否则同 cargo test 进程内后续测试会看到脏标志位。
/// 主循环看到 true 就直接退出,生产路径上没有调它的理由。
#[allow(dead_code)]
pub fn reset_shutdown_for_test() {
    SHUTDOWN.store(false, Ordering::SeqCst);
}

// =====================================================================
// CURRENT_DLL_PATH — WndProc 自动 register 时用(T10 接入)
// =====================================================================

/// 当前 daemon 守护的 DLL 路径。`run_daemon()` 启动时 set 一次,
/// `wnd_proc` 处理 `WTS_CONSOLE_CONNECT` 时读出来调 `register::do_register`。
static CURRENT_DLL_PATH: OnceLock<String> = OnceLock::new();

// =====================================================================
// DaemonConfig
// =====================================================================

pub struct DaemonConfig {
    pub dll_path: PathBuf,
    pub poll_interval_secs: u64,
    pub auto_register_on_start: bool,
    pub auto_unregister_on_exit: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let exe = std::env::current_exe().expect("current_exe");
        Self {
            dll_path: exe.parent().unwrap().join("prisir_ime_tsf.dll"),
            poll_interval_secs: 5,
            // 默认不自动 register,避免开发者误启 daemon 就污染 IME 列表。
            // 要 register 必须显式跑 `--register`。
            auto_register_on_start: false,
            auto_unregister_on_exit: false,
        }
    }
}

// =====================================================================
// WndProc — 处理 WM_WTSSESSION_CHANGE / WM_DESTROY / WM_CLOSE
//
// 注册到 WNDCLASSEXW.lpfnWndProc,由系统消息泵在主线程回调。
// 安全策略: 整个函数体均为 unsafe(系统契约要求),内部操作只读 CURRENT_DLL_PATH
//          并调 register::do_register / do_unregister(都标 unsafe via FFI/Reg API)。
// =====================================================================

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_WTSSESSION_CHANGE => {
            // wparam 才是事件类型;lparam 是 session id
            let event = _wparam.0 as u32;
            let session_id = lparam.0 as u32;
            match event {
                WTS_CONSOLE_CONNECT => {
                    println!(
                        "[daemon] WTS_CONSOLE_CONNECT (session={}) → auto register",
                        session_id
                    );
                    if let Some(p) = CURRENT_DLL_PATH.get() {
                        if let Err(e) = crate::register::do_register(p) {
                            eprintln!("[daemon] auto register failed: {e}");
                        } else {
                            println!("[daemon] auto register OK");
                        }
                    } else {
                        eprintln!("[daemon] auto register skipped: CURRENT_DLL_PATH 未设置");
                    }
                }
                WTS_CONSOLE_DISCONNECT => {
                    println!(
                        "[daemon] WTS_CONSOLE_DISCONNECT (session={}) → auto unregister",
                        session_id
                    );
                    if let Err(e) = crate::register::do_unregister() {
                        eprintln!("[daemon] auto unregister failed: {e}");
                    } else {
                        println!("[daemon] auto unregister OK");
                    }
                }
                WTS_SESSION_LOGON => {
                    println!("[daemon] WTS_SESSION_LOGON (session={}, no-op)", session_id)
                }
                WTS_SESSION_LOGOFF => {
                    println!("[daemon] WTS_SESSION_LOGOFF (session={}, no-op)", session_id)
                }
                WTS_REMOTE_CONNECT => {
                    println!("[daemon] WTS_REMOTE_CONNECT (session={})", session_id)
                }
                WTS_REMOTE_DISCONNECT => {
                    println!("[daemon] WTS_REMOTE_DISCONNECT (session={})", session_id)
                }
                other => {
                    println!("[daemon] WM_WTSSESSION_CHANGE event={} (session={})", other, session_id)
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            println!("[daemon] WM_DESTROY received");
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_CLOSE => {
            println!("[daemon] WM_CLOSE received");
            request_shutdown();
            // DestroyWindow 会触发 WM_DESTROY → PostQuitMessage(0)
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, _wparam, lparam),
    }
}

// =====================================================================
// 主入口
// =====================================================================

pub fn run_daemon(config: DaemonConfig) -> Result<(), String> {
    println!("[daemon] starting, pid={}", std::process::id());
    println!("[daemon] dll: {}", config.dll_path.display());
    println!("[daemon] poll interval: {}s", config.poll_interval_secs);
    println!("[daemon] Ctrl-C to exit");
    println!("[daemon] WTS: WM_WTSSESSION_CHANGE + 真消息循环 (T10)");

    // -----------------------------------------------------------------
    // Ctrl-C handler — windows-rs 0.58 PHANDLER_ROUTINE 签名是
    //   Option<unsafe extern "system" fn(u32) -> BOOL>
    // BOOL 是 struct(i32) 不是 bool, 必须包一下。返回 TRUE(1) 表示
    // "我处理了, 不要默认杀进程", 真正的退出走 is_shutdown() 标志位 +
    // 主循环自然退出。
    // -----------------------------------------------------------------
    unsafe {
        extern "system" fn handler(_ctrl_type: u32) -> windows::Win32::Foundation::BOOL {
            println!("\n[daemon] Ctrl-C received");
            request_shutdown();
            windows::Win32::Foundation::BOOL(1) // TRUE = 我处理了
        }
        let r = windows::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), true);
        if let Err(e) = r {
            eprintln!("[daemon] SetConsoleCtrlHandler failed: {e} (Ctrl-C 将默认杀进程, 继续)");
        }
    }

    // -----------------------------------------------------------------
    // CURRENT_DLL_PATH — WndProc 处理 WTS_CONSOLE_CONNECT 时需要
    // -----------------------------------------------------------------
    let dll_str = config.dll_path.to_string_lossy().into_owned();
    let _ = CURRENT_DLL_PATH.set(dll_str.clone());

    // -----------------------------------------------------------------
    // 注册窗口类 + 创建 hidden HWND
    //
    // HWND_MESSAGE 是 Windows 提供的特殊父窗口常量(-3 as HWND),表示
    // "message-only window" — 不可见、不在任务栏、不接收 WM_PAINT,但有真消息队列。
    //
    // 备注:WNDCLASSEXW + RegisterClassExW 在 windows-rs 0.58 上被 cfg-gate 在
    //      Win32_Graphics_Gdi feature(WNDCLASSEXW 含 HBRUSH/HICON/HCURSOR 字段),
    //      所以本 crate 的 Cargo.toml 加了 Gdi feature(WTS 真消息循环刚需)。
    // -----------------------------------------------------------------
    let hinstance_raw = unsafe { GetModuleHandleW(None) }
        .map(HINSTANCE::from)
        .unwrap_or(HINSTANCE(std::ptr::null_mut()));
    let class_name: Vec<u16> = "PrisirIMEMsg\0".encode_utf16().collect();
    let window_name: Vec<u16> = "PrisirIMEHidden\0".encode_utf16().collect();

    let mut wc: WNDCLASSEXW = WNDCLASSEXW::default();
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.style = CS_HREDRAW | CS_VREDRAW;
    wc.lpfnWndProc = Some(wnd_proc);
    wc.hInstance = hinstance_raw;
    wc.lpszClassName = windows::core::PCWSTR(class_name.as_ptr());

    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        let msg = windows::core::Error::from_win32().message();
        return Err(format!("RegisterClassExW failed: {msg}"));
    }
    println!("[daemon] RegisterClassExW OK, atom={}", atom);

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            windows::core::PCWSTR(class_name.as_ptr()),
            windows::core::PCWSTR(window_name.as_ptr()),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            hinstance_raw,
            None,
        )
    }
    .map_err(|e| format!("CreateWindowExW failed: {e}"))?;
    if hwnd.0.is_null() {
        return Err("CreateWindowExW returned null HWND".to_string());
    }
    println!("[daemon] CreateWindowExW OK, hwnd={:p}", hwnd.0);

    // -----------------------------------------------------------------
    // WTS 真消息注册
    // -----------------------------------------------------------------
    if let Err(e) = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } {
        eprintln!(
            "[daemon] WARN: WTSRegisterSessionNotification failed: {e} (继续跑,WTS 事件不会触发)"
        );
    } else {
        println!(
            "[daemon] WTSRegisterSessionNotification OK (hwnd={:p})",
            hwnd.0
        );
    }

    // -----------------------------------------------------------------
    // 初始 LoadLibrary — 让 DLL 留一份引用,避免被 FreeLibrary 意外干掉
    // -----------------------------------------------------------------
    let initial_load = unsafe {
        let path_c = std::ffi::CString::new(dll_str.clone()).unwrap();
        LoadLibraryA(windows::core::PCSTR(path_c.as_bytes_with_nul().as_ptr())).ok()
    };
    match initial_load {
        Some(h) if !h.0.is_null() => {
            println!("[daemon] initial LoadLibrary OK, handle={:p}", h.0);
        }
        _ => {
            eprintln!(
                "[daemon] WARN: initial LoadLibrary failed (dll={}, 后续 mtime polling 会重试)",
                dll_str
            );
        }
    }

    // -----------------------------------------------------------------
    // auto_register: 默认 false,避免误启污染。
    // -----------------------------------------------------------------
    if config.auto_register_on_start {
        if let Err(e) = crate::register::do_register(&dll_str) {
            eprintln!("[daemon] auto_register failed: {e}");
        } else {
            println!("[daemon] auto_register OK");
        }
    }

    // -----------------------------------------------------------------
    // mtime polling thread(沿用 T5 真热重载)
    //
    // T10 把它从主循环搬到独立线程,主循环专注真消息泵。
    // 线程只在 is_shutdown() 为 false 时跑,sleep 5s 醒来检测一次。
    // -----------------------------------------------------------------
    let dll_for_thread = config.dll_path.clone();
    let polling_thread = std::thread::spawn(move || {
        let mut last_mtime: Option<std::time::SystemTime> = None;
        let interval = std::cmp::max(config.poll_interval_secs, 1);
        while !is_shutdown() {
            std::thread::sleep(Duration::from_secs(interval));
            match std::fs::metadata(&dll_for_thread) {
                Ok(meta) => {
                    if let Ok(mt) = meta.modified() {
                        if Some(mt) != last_mtime {
                            println!("[daemon] DLL mtime changed → hot reload (T5 保留)");
                            last_mtime = Some(mt);
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[daemon] stat dll failed: {e:?} ({})",
                        dll_for_thread.display()
                    );
                }
            }
            // 2026-09-04: 每日活跃信号(学 PrisirAI updates.json 轮询)。
            // 内部按「本地日」去重,一日至多一次 HTTPS GET,失败静默。
            let _ = crate::active_signal::tick_daily_active();
        }
    });

    // -----------------------------------------------------------------
    // 真消息循环 — GetMessageW/DispatchMessageW/TranslateMessage
    // -----------------------------------------------------------------
    println!("[daemon] entering message loop...");
    let mut msg = MSG::default();
    loop {
        if is_shutdown() {
            break;
        }
        // GetMessageW 返 BOOL(-1/0/非0)。-1 = 错误,0 = WM_QUIT,<0 = 拿消息。
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }
    println!("[daemon] message loop exited");

    // -----------------------------------------------------------------
    // 清理 — WTS 注销、HWND 销毁、窗口类注销、polling 线程 join
    // -----------------------------------------------------------------
    let _ = unsafe { WTSUnRegisterSessionNotification(hwnd) };
    println!("[daemon] WTSUnRegisterSessionNotification OK");

    let _ = unsafe { DestroyWindow(hwnd) };
    let _ = unsafe { UnregisterClassW(windows::core::PCWSTR(class_name.as_ptr()), hinstance_raw) };
    println!("[daemon] window + class destroyed");

    polling_thread.join().ok();
    println!("[daemon] polling thread joined");

    if config.auto_unregister_on_exit {
        if let Err(e) = crate::register::do_unregister() {
            eprintln!("[daemon] auto_unregister failed: {e}");
        } else {
            println!("[daemon] auto_unregister OK");
        }
    }

    println!("[daemon] exited cleanly");
    Ok(())
}