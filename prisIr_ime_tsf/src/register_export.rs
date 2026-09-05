//! register_export.rs — T24 P3: 标准 COM DllRegisterServer / DllUnregisterServer 导出
//!
//! 这是 Windows COM 标准的注册入口 — regsvr32.exe 调 DllRegisterServer 时走这里。
//! 我们把它实现为调 crate::register 的 do_register / do_unregister。
//!
//! DLL 路径策略:
//!   - 用 GetModuleFileNameW 拿当前 DLL 路径,而不是 hard-code `C:\Program Files\PrisirIME\prisir_ime_tsf.dll`。
//!   - 这样 regsvr32 在任何路径调我们的 DLL,都能拿到正确 DLL 路径写注册表。
//!
//! 失败处理:
//!   - do_register 返 Err (T24 hotfix: admin 缺失会返 Err) → 返 E_ACCESSDENIED。
//!   - do_unregister 返 Err → 返 E_FAIL。
//!
//! 出参:
//!   - 这是 `extern "system"` C ABI 函数,直接返 HRESULT(0 = S_OK)。
//!   - 主流程用 `co_create_and_register` 等路径不会调它,只有 regsvr32 / rundll32 调。

use windows::core::HRESULT;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::Foundation::MAX_PATH;

use crate::register::{do_register, do_unregister};

/// 拿当前 DLL 的完整路径 — 给 do_register 当 dll_path 用。
fn current_dll_path() -> Result<String, String> {
    unsafe {
        let mut buf = [0u16; MAX_PATH as usize];
        let len = GetModuleFileNameW(None, &mut buf);
        if len == 0 {
            return Err(format!(
                "GetModuleFileNameW 失败 hr=0x{:08X}",
                windows::Win32::Foundation::GetLastError().0
            ));
        }
        if (len as usize) >= buf.len() {
            return Err(format!(
                "DLL 路径长度 {} >= MAX_PATH({}),路径被截断 — 不可能(我们的路径没那么长)",
                len,
                buf.len()
            ));
        }
        Ok(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// 标准 COM 注册入口。
///
/// regsvr32 调这个调起我们的 DLL 注册流程。
/// 在非 admin 进程里跑时,do_register 会返 Err (T24 hotfix),
/// 我们把它映射成 E_ACCESSDENIED 让 regsvr32 给用户清晰错误。
#[no_mangle]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    let dll_path = match current_dll_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[register_export] DllRegisterServer: {e}");
            return HRESULT(0x80070005u32 as i32); // E_ACCESSDENIED
        }
    };
    eprintln!("[register_export] DllRegisterServer: dll_path={dll_path}");
    match do_register(&dll_path) {
        Ok(r) => {
            eprintln!("[register_export] DllRegisterServer OK ({} entries)", r.entries_written);
            HRESULT(0) // S_OK
        }
        Err(e) => {
            eprintln!("[register_export] DllRegisterServer FAIL: {e}");
            eprintln!("[register_export] 常见原因: 非 admin context。regsvr32 应已自动触发 UAC,");
            eprintln!("[register_export] 如失败请用管理员 PowerShell: Start-Process ... -Verb RunAs");
            HRESULT(0x80070005u32 as i32) // E_ACCESSDENIED
        }
    }
}

/// 标准 COM 注销入口。
///
/// regsvr32 /u 调。
/// do_unregister 只动 HKCU, 不需要 admin。
#[no_mangle]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    eprintln!("[register_export] DllUnregisterServer");
    match do_unregister() {
        Ok(()) => {
            eprintln!("[register_export] DllUnregisterServer OK");
            HRESULT(0)
        }
        Err(e) => {
            eprintln!("[register_export] DllUnregisterServer FAIL: {e}");
            HRESULT(0x80004005u32 as i32) // E_FAIL
        }
    }
}

/// 标准 COM 安装入口 — regsvr32 /i 调。
///
/// Windows 推荐把"install"和"register"做成同一动作(Sogou/微软都这样)。
/// regsvr32 /i 不带额外参数 → lpCmdLine = NULL → 走默认 install = register。
#[no_mangle]
pub extern "system" fn DllInstall(_binstall: u32, _pcmdline: *mut u16) -> HRESULT {
    // 默认 install 行为 = register
    DllRegisterServer()
}
