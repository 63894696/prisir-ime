//! IClassFactory + DllGetClassObject(DLL 入口)
//!
//! DLL 侧:`DllGetClassObject(rclsid, riid, ppv)` 是 CTF/COM 拉起输入法时的入口,
//! 本模块实现:
//!   - `PrisirImeClassFactory` — 当 CTF 拉起时返回的新对象工厂
//!   - `DllGetClassObject`     — DLL 入口点(C ABI 导出)
//!
//! T2 阶段 IClassFactory 只返回 `TsfInputProcessor`,真注册(`--register` 走 HKCU)
//! 留给 T4。
use windows::core::*;
use windows::Win32::Foundation::{REGDB_E_CLASSNOTREG, BOOL};
use windows::Win32::System::Com::*;

use crate::tsf_input_processor::{TsfInputProcessor, CLSID_PRISIR_IME};

/// IClassFactory 实现 —— CTF 通过它来创建 TsfInputProcessor 实例。
/// CreateInstance 返回 IUnknown(由 `windows::core::implement!` 宏生成)。
#[windows::core::implement(IClassFactory)]
pub struct PrisirImeClassFactory;

impl PrisirImeClassFactory {
    pub fn new() -> Self {
        Self
    }
}

/// `_Impl` trait 实现于包裹结构 `PrisirImeClassFactory_Impl`(由宏生成,`pub` 可见)。
/// 通过 `&self.this` 访问用户结构 `PrisirImeClassFactory` 的字段。
impl IClassFactory_Impl for PrisirImeClassFactory_Impl {
    /// 工厂方法 —— CTF 调用以创建新的 TsfInputProcessor。
    ///
    /// **关键 bug 修复(2026-08-31)**:旧实现无视 `riid`,直接把对象转成 `IUnknown`
    /// 再 `into_raw()` 返回。COM 规范要求 CreateInstance 必须按调用方给的 `riid`
    /// QueryInterface 出对应接口再返回 —— TSF 激活时 riid=IID_ITfTextInputProcessor,
    /// 返回 IUnknown 的 vtable 头会让系统拿不到 ITfTextInputProcessor → 实测
    /// CoCreateInstance(IID_ITfTextInputProcessor) 返 E_NOINTERFACE(0x80004002),
    /// 系统激活管线因此静默跳过 Prisir(打字出原字母)。改为 query(riid) 对齐
    /// DllGetClassObject 的正确模式。
    fn CreateInstance(
        &self,
        _punkouter: Option<&IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        if ppvobject.is_null() {
            return Err(Error::from_win32());
        }
        // COM 约定:进入时 *ppvobject 必须置 NULL,失败时调用方据此判断。
        unsafe { *ppvobject = core::ptr::null_mut() }
        if riid.is_null() {
            return Err(Error::from_win32());
        }
        let instance: IUnknown = TsfInputProcessor::new().into();
        // 按调用方要的 riid QueryInterface —— 成功时填 *ppvobject 并返 S_OK,
        // 失败(如 IID 不认识)时返回对应错误码,*ppvobject 保持 NULL。
        let hr = unsafe { instance.query(riid, ppvobject) };
        #[cfg(feature = "dllentry_log")]
        {
            let g = unsafe { &*riid };
            log_dll_entry(&format!(
                "CreateInstance: riid={:08X}-{:04X}-{:04X} query_hr=0x{:08X}",
                g.data1, g.data2, g.data3, hr.0 as u32
            ));
        }
        if hr.is_ok() {
            Ok(())
        } else {
            Err(Error::from(hr))
        }
    }

    /// 锁定/解锁服务器(进程内服务器不需实现)。
    fn LockServer(&self, _flock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// 诊断日志:把一行追加到 C:\Temp\prisir_dll_log.txt。
/// 用于定位 ctfmon 是否真的调用 DllGetClassObject(TIPC 是否尝试加载 Prisir),
/// 以及击键/候选/上屏全链路时序。反馈问题(feedback.rs)打包此文件供我们诊断。
///
/// 2026-09-05: 改为**恒开**(原 dllentry_log feature 门控删除)。
/// 原因:正式 release 若用默认 cargo build --release(不带 feature)出包,
/// dll 会零日志 → 用户点「反馈问题」打的 zip 没有日志,我们无法诊断。
/// 此函数每次击键仅一次 打开-写-关 文本追加,无常驻句柄/无网络/无敏感内容,
/// 开销可忽略,诊断价值极高,故不再用 feature 关闭。
/// 不依赖任何初始化,只用 Win32 文件 API,ctfmon/LocalService 上下文也能写 C:\Temp。
pub(crate) fn log_dll_entry(msg: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_WRITE,
        OPEN_ALWAYS,
    };
    let path: Vec<u16> = std::ffi::OsStr::new(r"C:\Temp\prisir_dll_log.txt")
        .encode_wide()
        .chain(core::iter::once(0))
        .collect();
    unsafe {
        if let Ok(h) = CreateFileW(
            windows::core::PCWSTR(path.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_WRITE,
            None,
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ) {
            // 移到文件尾
            let _ = windows::Win32::Storage::FileSystem::SetFilePointerEx(
                h,
                0,
                None,
                windows::Win32::Storage::FileSystem::FILE_END,
            );
            // 2026-09-01 加 PID:多进程(explorer/msedge)同时写同一日志,
            // 无 PID 无法分辨 GetKeyState 返 true 是哪个进程。
            let pid = windows::Win32::System::Threading::GetCurrentProcessId();
            // 2026-09-03 加毫秒时间戳:定位「切换后等几分钟才能打字」这类时序问题,
            // 需要看 ActivateEx → engine_build → OnTestKeyDown 的相对耗时。
            // 用 GetTickCount(开机毫秒)不依赖系统时钟,不受时区/夏令时影响。
            let ms = unsafe { windows::Win32::System::SystemInformation::GetTickCount() };
            let line = format!("[{} t={}] {}\r\n", pid, ms, msg);
            let mut written: u32 = 0;
            let _ = WriteFile(h, Some(line.as_bytes()), Some(&mut written as *mut u32), None);
            let _ = windows::Win32::Foundation::CloseHandle(h);
        }
    }
}

/// DLL 入口点 —— COM 调用以获取类工厂。
/// rclsid 期望是 `CLSID_PRISIR_IME`,其它 GUID 返回 CLASSNOTREG。
#[no_mangle]
pub extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    #[cfg(feature = "dllentry_log")]
    log_dll_entry("DllGetClassObject:ENTER");
    unsafe {
        if rclsid.is_null() || ppv.is_null() {
            #[cfg(feature = "dllentry_log")]
            log_dll_entry("DllGetClassObject:NULL_ARG");
            return HRESULT(0x80004003u32 as i32); // E_POINTER
        }
        if *rclsid != CLSID_PRISIR_IME {
            #[cfg(feature = "dllentry_log")]
            log_dll_entry("DllGetClassObject:WRONG_CLSID");
            return REGDB_E_CLASSNOTREG;
        }
        #[cfg(feature = "dllentry_log")]
        log_dll_entry("DllGetClassObject:CLSID_MATCH");
        // 创建类工厂并 QueryInterface 出 riid。
        // `query` 直接返回 HRESULT(成功时填 ppv,失败时返回错误码),
        // 所以这里直接当 HRESULT 返回即可。
        let factory: IClassFactory = PrisirImeClassFactory.into();
        let hr = factory.query(riid, ppv);
        #[cfg(feature = "dllentry_log")]
        log_dll_entry(if hr.0 == 0 {
            "DllGetClassObject:OK"
        } else {
            "DllGetClassObject:QUERY_FAIL"
        });
        hr
    }
}

/// DLL 入口点 —— COM 用以决定能否卸载。
/// T2 阶段:返回 S_OK 表示进程内服务器可以随时卸载。
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    HRESULT(0) // S_OK
}