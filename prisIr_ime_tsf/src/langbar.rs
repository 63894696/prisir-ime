//! LangBar 中/英切换按钮 — 实现 `ITfLangBarItem` + `ITfLangBarItemButton`。
//!
//! 用户决策(2026-08-30):
//! - 默认中文模式(切到 Prisir 直接进拼音 composing)
//! - Shift 临时切英文,松开回中文
//!
//! 这个 LangBarItem 在 TsfInputProcessor::Activate 调 LangBarItemMgr::AddItem 时装上,
//! 装上后用户点这个按钮 → OnClick → 翻转 mode_ref。
//!
//! 注意:OnClick 翻转 mode 后,如果当前有 composition,需要 EndComposition。
//! 这一步通过 `pending_mode_change: Arc<Mutex<bool>>` 通知给 TsfInputProcessor,
//! OnKeyDown 入口检查到 → EndComposition + 清 buffer。

use std::sync::{Arc, Mutex};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::UI::TextServices::*;
use windows::Win32::UI::WindowsAndMessaging::{LoadIconW, HICON, IDI_APPLICATION};

use crate::tsf_input_processor::{CLSID_PRISIR_IME, GUID_PRISIR_LANGBAR_ITEM};

/// 本 DLL 内嵌主图标的资源 ID — winres `set_icon` 把第一个图标编成 IDI_ICON=1。
/// 与 build.rs 嵌入的 pinyin.ico 对应;register.rs 的 IconIndex=0x0 提取资源段第一个图标。
const IDI_PRISIR_ICON: u16 = 1;

/// 取本 DLL 的 HMODULE(不是宿主进程 notepad/ctfmon 的 exe HMODULE)。
/// 用 GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS + 本模块内一个函数的地址反查。
/// UNCHANGED_REFCOUNT:不增引用计数(DLL 已由 COM 持有,不会因我们而卸载)。
fn dll_hmodule() -> Option<HMODULE> {
    let mut hmod = HMODULE::default();
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(dll_hmodule as *const u16),
            &mut hmod,
        )
    };
    if ok.is_ok() && !hmod.0.is_null() {
        Some(hmod)
    } else {
        None
    }
}

/// 从本 DLL 资源段加载主图标(pinyin.ico)。
/// 失败(资源没编进去/句柄拿不到)→ fallback 系统 IDI_APPLICATION,仍比 null 强,
/// 保证 AddItem 期间 mgr 提取图标不 E_FAIL。
fn load_dll_icon() -> HICON {
    if let Some(hmod) = dll_hmodule() {
        let h = unsafe { LoadIconW(hmod, PCWSTR(IDI_PRISIR_ICON as usize as *const u16)) };
        if let Ok(h) = h {
            if !h.0.is_null() {
                return h;
            }
        }
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("load_dll_icon: IDI_PRISIR_ICON not found, fallback IDI_APPLICATION");
    }
    // fallback:系统默认应用图标(总比 null 好,且一定存在)。
    unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or(HICON(std::ptr::null_mut()))
}

// ──────────────────────────────────────────────────────────────────────
// PrisirLangBarItem — 双接口 COM 对象
// ──────────────────────────────────────────────────────────────────────

/// LangBar 按钮 — 显示当前中/英模式,点击切换。
///
/// `mode_ref` 跟 TsfInputProcessor 共享同一个 `Arc<Mutex<bool>>`,OnClick 翻它。
///
/// `pending_mode_change` 在 OnClick 后置 true,TsfInputProcessor::OnKeyDown 入口检查到
/// 会主动 EndComposition(避免翻模式后旧 composition 还显示在屏幕上)。
///
/// **必须实现 ITfSource(2026-09-01 根因)**:`ITfLangBarItemMgr::AddItem` 内部会
/// QI item 的 `ITfSource`,QI 不到直接 E_FAIL 0x80004005(实测日志确认 QI FAIL)。
/// mgr 通过 `ITfSource::AdviseSink(IID_ITfLangBarItemSink)` 反向订阅我们的状态变更,
/// 这样模式翻转时我们调 `sink.OnUpdate(0)`,LangBar 才刷新按钮显示。没它 mgr 不给挂。
#[windows::core::implement(ITfLangBarItem, ITfLangBarItemButton, ITfSource)]
pub struct PrisirLangBarItem {
    /// 共享 is_chinese_mode 状态(跟 TsfInputProcessor 是同一 Arc)。
    pub(crate) mode_ref: Arc<Mutex<bool>>,
    /// 模式翻转后通知 processor:下次按键前先把旧 composition End 掉。
    pub(crate) pending_mode_change: Arc<Mutex<bool>>,
    /// ITfSource: mgr 通过 AdviseSink 登记的 LangBarItemSink(用于模式翻转时推 OnUpdate)。
    sink: Mutex<Option<ITfLangBarItemSink>>,
    /// ITfSource: AdviseSink 发的 cookie,UnadviseSink 据此匹配。
    sink_cookie: Mutex<u32>,
}

impl PrisirLangBarItem {
    pub fn new(mode_ref: Arc<Mutex<bool>>, pending_mode_change: Arc<Mutex<bool>>) -> Self {
        Self {
            mode_ref,
            pending_mode_change,
            sink: Mutex::new(None),
            sink_cookie: Mutex::new(0),
        }
    }
}

impl ITfLangBarItem_Impl for PrisirLangBarItem_Impl {
    /// 告诉 LangBar 这个 item 是什么(用于 AdviseItemSink 匹配)。
    /// clsidService = 我们 TIP 的 CLSID,guidItem = 任意唯一 GUID(在 tsf_input_processor.rs 里)。
    /// dwStyle = TF_LBI_STYLE_BTN_TOGGLE(可切换按钮) + TF_LBI_STYLE_SHOWNINTRAY(任务栏右下角显示)。
    fn GetInfo(&self, pinfo: *mut TF_LANGBARITEMINFO) -> Result<()> {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("LangBarItem::GetInfo:ENTER");
        if pinfo.is_null() {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry("LangBarItem::GetInfo: pinfo NULL -> FAIL");
            return Err(Error::from_win32());
        }
        // 中文描述"中英切换",以 null 结尾,u16 数组最长 32。
        let desc: Vec<u16> = "中英切换".encode_utf16().chain(std::iter::once(0)).collect();
        let mut sz = [0u16; 32];
        let copy_len = desc.len().min(32);
        sz[..copy_len].copy_from_slice(&desc[..copy_len]);

        unsafe {
            (*pinfo).clsidService = CLSID_PRISIR_IME;
            (*pinfo).guidItem = GUID_PRISIR_LANGBAR_ITEM;
            (*pinfo).dwStyle = TF_LBI_STYLE_BTN_TOGGLE | TF_LBI_STYLE_SHOWNINTRAY;
            (*pinfo).ulSort = 0;
            (*pinfo).szDescription = sz;
        }
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!(
            "LangBarItem::GetInfo:OK clsid={:?} style=0x{:X}", CLSID_PRISIR_IME,
            TF_LBI_STYLE_BTN_TOGGLE | TF_LBI_STYLE_SHOWNINTRAY
        ));
        Ok(())
    }

    /// 按钮当前状态 — 中文模式返 TF_LBI_STATUS_BTN_TOGGLED(显示为按下/选中),
    /// 英文模式返 0(显示为正常)。
    fn GetStatus(&self) -> Result<u32> {
        let mode = *self.mode_ref.lock().unwrap();
        let st = if mode { TF_LBI_STATUS_BTN_TOGGLED } else { 0 };
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!("LangBarItem::GetStatus:OK mode={} st=0x{:X}", mode, st));
        Ok(st)
    }

    /// LangBar 询问我们"显示还是隐藏",这里永远显示。
    fn Show(&self, _fshow: BOOL) -> Result<()> {
        Ok(())
    }

    /// tooltip 字符串 — 鼠标 hover 时浮出的文字。
    fn GetTooltipString(&self) -> Result<BSTR> {
        let mode = *self.mode_ref.lock().unwrap();
        let s = if mode { "Prisir IME - 中文模式" } else { "Prisir IME - 英文模式" };
        Ok(BSTR::from(s))
    }
}

impl ITfLangBarItemButton_Impl for PrisirLangBarItem_Impl {
    /// 用户点了这个按钮。
    /// - 左键(TF_LBI_CLK_LEFT = 2):翻转模式
    /// - 右键(TF_LBI_CLK_RIGHT = 1):v0.8 不实现菜单,no-op
    fn OnClick(
        &self,
        click: TfLBIClick,
        _pt: &POINT,
        _prcarea: *const RECT,
    ) -> Result<()> {
        if click == TF_LBI_CLK_LEFT {
            let prev = {
                let mut mode = self.mode_ref.lock().unwrap();
                let p = *mode;
                *mode = !p;
                p
            };
            // 通知 processor:模式翻了,下次按键前把旧 composition End 掉。
            // 仅在"从中文 → 英文"或反之有活跃 composition 时才有意义,反正置 true 也无害。
            let _ = prev; // suppress unused warning when we don't branch on prev
            *self.pending_mode_change.lock().unwrap() = true;
            eprintln!("[langbar] OnClick LEFT: mode toggled (was_chinese={})", prev);
            // 通知 LangBar 刷新按钮显示(重新调 GetStatus/GetText 拿新模式)。
            let sink = self.sink.lock().unwrap().clone();
            if let Some(sink) = sink {
                let _ = unsafe { sink.OnUpdate(0) };
            }
        }
        Ok(())
    }

    /// 弹右键菜单时调用 — v0.8 不实现菜单。
    fn InitMenu(&self, _pmenu: Option<&ITfMenu>) -> Result<()> {
        Ok(())
    }

    /// 菜单项被选中 — v0.8 不实现。
    fn OnMenuSelect(&self, _wid: u32) -> Result<()> {
        Ok(())
    }

    /// 按钮图标 — 从本 DLL 的资源段加载主图标(pinyin.ico,build.rs 嵌入 IDI_ICON=1)。
    ///
    /// 之前返 null HICON 是 Bug(2026-09-01 根因):AddItem 期间 mgr 会反查 profile 的
    /// IconFile 提取图标,DLL 无图标资源直接 E_FAIL。修好资源后,这里也返真图标,
    /// 让 LangBar 按钮 + 任务栏右下角都用我们的大图标(顺带解决"图标偏小"反馈)。
    /// 加载失败仍 fallback 到 null(LangBar 再退到 GetText),不影响 Activate 返 Ok。
    fn GetIcon(&self) -> Result<HICON> {
        let hicon = load_dll_icon();
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!(
            "LangBarItem::GetIcon: hicon_null={}", hicon.0.is_null()
        ));
        Ok(hicon)
    }

    /// 按钮文字 — 中文模式显示"中",英文模式显示"英"。
    fn GetText(&self) -> Result<BSTR> {
        let mode = *self.mode_ref.lock().unwrap();
        let s = if mode { "中" } else { "英" };
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!("LangBarItem::GetText:OK '{}'", s));
        Ok(BSTR::from(s))
    }
}

// ──────────────────────────────────────────────────────────────────────
// ITfSource — AddItem 的硬性要求(2026-09-01 根因)
// ──────────────────────────────────────────────────────────────────────
//
// mgr AddItem 内部 QI ITfSource,QI 不到 → E_FAIL。mgr 通过这里 AdviseSink 登记一个
// ITfLangBarItemSink;之后我们模式翻转(OnClick / CapsLock / Shift)时调 sink.OnUpdate(0),
// LangBar 收到通知重新调 GetStatus/GetText 刷新按钮显示。

impl ITfSource_Impl for PrisirLangBarItem_Impl {
    /// mgr 调这里登记它的 LangBarItemSink。我们只接受 IID_ITfLangBarItemSink,
    /// 其它 riid 返 E_INVALIDARG(对齐 TSF 惯例)。发一个固定 cookie=1。
    fn AdviseSink(&self, riid: *const GUID, punk: Option<&IUnknown>) -> Result<u32> {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("LangBarItem::ITfSource AdviseSink:ENTER");
        if riid.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }
        let want = unsafe { *riid };
        if want != ITfLangBarItemSink::IID {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry("LangBarItem::AdviseSink: wrong riid -> E_INVALIDARG");
            return Err(Error::from(E_INVALIDARG));
        }
        let Some(punk) = punk else {
            return Err(Error::from(E_INVALIDARG));
        };
        // QI 出 ITfLangBarItemSink 存起来,模式翻转时回调 OnUpdate。
        let sink: ITfLangBarItemSink = punk.cast()?;
        *self.sink.lock().unwrap() = Some(sink);
        *self.sink_cookie.lock().unwrap() = 1;
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("LangBarItem::AdviseSink:OK cookie=1");
        Ok(1)
    }

    /// mgr 注销 sink。cookie 匹配才清。
    fn UnadviseSink(&self, dwcookie: u32) -> Result<()> {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!("LangBarItem::UnadviseSink: cookie={}", dwcookie));
        let cur = *self.sink_cookie.lock().unwrap();
        if cur == 0 || cur != dwcookie {
            return Err(Error::from(E_INVALIDARG));
        }
        *self.sink.lock().unwrap() = None;
        *self.sink_cookie.lock().unwrap() = 0;
        Ok(())
    }
}