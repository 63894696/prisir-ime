//! elevate.rs — T24 P2: ShellExecuteExW "runas" 自动 UAC 提升
//!
//! 用户决策: 当 `--register` 因 admin 缺失返 Err, 提供 `--register-elevated`
//! 自动通过 UAC 弹框让用户在 admin context 跑 do_register。
//!
//! 设计:
//!   - 当前进程已 admin → 直接调 do_register,不需要 UAC
//!   - 当前进程未 admin → ShellExecuteExW("runas") 触发 UAC,新进程 admin 跑 do_register
//!   - 用户点"否" → 返回 Error("UAC denied"),exit 5
//!
//! Win32 API 行为:
//!   - ShellExecuteExW(lpVerb="runas") 会弹 UAC 框,用户点"是"则启动新进程
//!   - 新进程从同一 exe 启动,参数透传,但**新进程必须自己再解析 argv**
//!     (这跟 fork 不同 — Windows 是新进程从头跑 main())。
//!     所以我们传 `--register` 给新进程,而不是 `--register-elevated`(会无限递归)。
//!
//! 不在 T24 范围:
//!   - TokenElevation / CheckTokenMembership 等"检测"路径(我们已经只关心 do_register 失败)
//!   - IsUserAnAdmin 备选(ShellExecuteExW runas 自己会处理)

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::core::PCWSTR;
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};

/// 提升状态 — 跟 main.rs 的退出码契约对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevateResult {
    /// 已是 admin,直接执行完成
    AlreadyAdmin,
    /// ShellExecuteExW runas 触发成功,新进程已接管
    UacLaunched,
    /// 用户点"否" / ShellExecuteExW 失败
    Failed,
}

/// 当前进程是否以 admin token 运行。
///
/// 实现: 拿当前进程的 token,GetTokenInformation(TokenElevation),看 TokenIsElevated flag。
/// 不在 T24 范围 — 简化: 让 ShellExecuteExW runas 自己处理(传 runas 给已 admin 进程
/// 是 no-op,UAC 不弹)。但保留此函数给未来需要明确判定 admin 的路径用。
#[allow(dead_code)]
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcessToken,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };

    unsafe {
        let mut token_handle = HANDLE(std::ptr::null_mut());
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret_len: u32 = 0;
        let ok = GetTokenInformation(
            token_handle,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(token_handle);
        ok.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// 把参数数组拼成单行命令行(Windows CreateProcess 风格)。
///
/// 简单实现 — 不处理引号转义边界情形,因为我们传的参数是 `--register`(无空格)。
/// 未来如要传更复杂参数,这里要改成 CommandLineToArgvW 兼容编码。
fn build_cmd_line(args: &[&str]) -> Vec<u16> {
    let mut s = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        // 简化: 不引号,因为我们只传简单的 sub-command
        s.push_str(a);
    }
    OsStr::new(&s).encode_wide().chain(std::iter::once(0)).collect()
}

/// 在 admin context 跑命令。
///
/// `target_exe`: 要运行的 exe 路径(通常 = 当前 exe)。
/// `args`:      透传的命令行参数。
///
/// 行为:
///   - is_elevated() == true → 返回 AlreadyAdmin,**不**自动 launch(让调用方自己跑 do_register)。
///     这避免双跑。
///   - 否则 → ShellExecuteExW(lpVerb="runas", lpFile=target_exe, lpParameters=args)
///     返回 UacLaunched(成功启动)或 Failed(UAC 拒绝 / API 失败)。
pub fn run_elevated(target_exe: &str, args: &[&str]) -> ElevateResult {
    if is_elevated() {
        return ElevateResult::AlreadyAdmin;
    }

    let exe_w: Vec<u16> = OsStr::new(target_exe).encode_wide().chain(std::iter::once(0)).collect();
    let args_w = build_cmd_line(args);
    let verb_w: Vec<u16> = OsStr::new("runas").encode_wide().chain(std::iter::once(0)).collect();

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS,
        hwnd: windows::Win32::Foundation::HWND(std::ptr::null_mut()),
        lpVerb: PCWSTR(verb_w.as_ptr()),
        lpFile: PCWSTR(exe_w.as_ptr()),
        lpParameters: PCWSTR(args_w.as_ptr()),
        lpDirectory: PCWSTR(std::ptr::null()),
        nShow: 0, // SW_HIDE — 新进程不该弹窗
        ..Default::default()
    };

    unsafe {
        match ShellExecuteExW(&mut info) {
            Ok(()) => {
                // ShellExecuteExW 返 Ok + hInstApp <= 32 才算真失败
                // 但 windows-rs 0.58 的 Result<()> 已经把 SE_ERR_ACCESS_DENIED 等
                // 映射成 Err 了。这里 Ok 表示 launch 成功,新进程已 fork 出。
                // 注: UAC 用户点"否" 时 ShellExecuteExW 返 Err(ERROR_CANCELLED=1223)
                eprintln!("[elevate] UAC launched, new admin process running {target_exe}");
                ElevateResult::UacLaunched
            }
            Err(e) => {
                eprintln!("[elevate] ShellExecuteExW runas 失败: hr=0x{:08X}", e.code().0);
                eprintln!("[elevate] 用户可能点 UAC 框的\"否\",或 ShellExecuteExW 本身不可用");
                eprintln!("[elevate] 兜底: 手动 admin PowerShell 跑:");
                eprintln!("[elevate]   Start-Process {} -ArgumentList '{}' -Verb RunAs", target_exe, args.join(" "));
                ElevateResult::Failed
            }
        }
    }
}
