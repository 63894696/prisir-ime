//! IMM32 `.ime` HKL 注册 — 让 Prisir 进系统搜索框(SearchApp 只加载 IMM32 双栈 IME)。
//!
//! 与 TSF 注册(`register.rs` 走 ITfInputProcessorProfiles / CTF TIP 树)**完全独立**:
//! IMM32 走 `ImmInstallIMEW(.ime 路径, 名称)` 拿 HKL,再写 `HKLM\SYSTEM\CurrentControlSet\
//! Control\Keyboard Layouts\<HKL>` 与 `Keyboard Layout\Preload` 让用户可切。
//!
//! 参考:ReactOS imm32/utils.c、katahiromz/ImeStudy、记忆 prisirtip-imm32-research-2026-09-01。
//! 纪律(用户约束):**只在 VM 操作**,host 不注册 IMM(改系统键盘布局/Preload)。
#![allow(non_snake_case)]

use windows::core::PCWSTR;
use windows::Win32::UI::Input::Ime::ImmInstallIMEW;
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;

/// `.ime` 部署路径(与 TSF DLL 同内容,扩展名改 .ime 才被 IMM32 认)。
pub const IME_DEPLOY_PATH: &str = r"C:\PrisirIME\prisir_ime_tsf.ime";
/// 布局显示名。
pub const IME_LAYOUT_TEXT: &str = "Prisir 拼音";

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 注册 IMM IME:ImmInstallIMEW 拿 HKL。返回 Ok(hkl) / Err(msg)。
/// 注意:ImmInstallIMEW 会把 .ime 拷到 System32 并写 Keyboard Layouts,需管理员权限。
pub fn register_imm() -> Result<HKL, String> {
    let path = IME_DEPLOY_PATH;
    if !std::path::Path::new(path).exists() {
        return Err(format!(".ime 不存在: {path}(先把 prisir_ime_tsf.dll 复制为该名部署)"));
    }
    let wpath = to_wide(path);
    let wtext = to_wide(IME_LAYOUT_TEXT);
    let hkl = unsafe {
        ImmInstallIMEW(
            PCWSTR(wpath.as_ptr()),
            PCWSTR(wtext.as_ptr()),
        )
    };
    if hkl.0.is_null() {
        let gle = unsafe { windows::Win32::Foundation::GetLastError() };
        return Err(format!(
            "ImmInstallIMEW 返 NULL GetLastError=0x{:08X}(.ime={path};常因非提权,需管理员)",
            gle.0
        ));
    }
    Ok(hkl)
}

/// 打印注册结果(HKL 形如 0x0804xxxx / 0xE0xx0804)。同时写 C:\Temp\register_imm_out.txt
/// 供 schtasks 非交互运行时回读(那里 stdout 看不到)。
pub fn run_register_imm() {
    let line = match register_imm() {
        Ok(hkl) => format!("[register-imm] OK HKL=0x{:X}", hkl.0 as usize),
        Err(e) => format!("[register-imm] FAIL: {e}"),
    };
    println!("{line}");
    let _ = std::fs::write(r"C:\Temp\register_imm_out.txt", format!("{line}\r\n"));
    if line.contains("FAIL") {
        std::process::exit(1);
    }
    println!("[register-imm] 下一步:注销重登或重启让 winlogon 重建输入法栈,然后在系统搜索框切到 Prisir 验证。");
}
