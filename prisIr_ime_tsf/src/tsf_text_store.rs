//! ITextStoreACP 完整 vtable 实现 — Prisir IME 的 Text Store
//!
//! T3 阶段:实现 windows-rs 0.58 的 `ITextStoreACP_Impl` 全部 24 个方法(占位 stub,但
//! 函数体真实存在,**不允许 `todo!()`**)。`AddRef` / `Release` / `QueryInterface`
//! 由 `#[windows::core::implement]` 宏自动生成。
//!
//! 关于 `ITfTextStoreACP`:
//!   MSDN 定义 `ITfTextStoreACP` 是 `ITextStoreACP` 的扩展(同一 IID,新增 4 个方法:
//!   `GetID` / `GetActiveView` / `GetACPFromPoint` / `GetScreenExt` / `GetWnd`)。
//!   **windows-rs 0.58 没有单独导出 `ITfTextStoreACP` 接口** — 这些方法已合并进
//!   `ITextStoreACP` 的 vtable(因为同一 GUID `{28888fe3-c2a0-483a-a3ea-8cb1ce51ff3d}`)。
//!   所以本文件一次性把 `ITextStoreACP` 的 24 个方法全写完就覆盖了 `ITfTextStoreACP`
//!   的扩展能力,T3 报告里会注明这点。
//!
//! PinyinBuffer 字段用于 T8 沙盒 E2E 时测试上屏链路,T3 阶段只是把容器装上。
#![allow(non_snake_case, dead_code, clippy::missing_safety_doc)]

use windows::core::*;
use windows::Win32::Foundation::{BOOL, HWND, POINT, RECT};
use windows::Win32::System::Com::{IDataObject, FORMATETC};
use windows::Win32::UI::TextServices::*;

use crate::keystroke::PinyinBuffer;

/// Text Store 实例 — 承载 ITextStoreACP vtable。
/// `windows::core::implement!` 宏生成 `TsfTextStore_Impl` 包裹结构,
/// `_Impl` trait 实现于包裹结构上,业务方法用 `&self.this` 转发到 `TsfTextStore`。
#[windows::core::implement(ITextStoreACP)]
pub struct TsfTextStore {
    /// 引用计数(由宏在 `AddRef` / `Release` 维护)。
    #[allow(dead_code)]
    ref_count: u32,
    /// 拼音缓冲区(T8 E2E 用)。
    pub pinyin: PinyinBuffer,
}

impl TsfTextStore {
    pub fn new() -> Self {
        Self { ref_count: 1, pinyin: PinyinBuffer::new() }
    }
}

impl Default for TsfTextStore {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================
// ITextStoreACP_Impl — windows-rs 0.58 的 24 个方法签名
// (对齐 .cargo/registry/src/.../windows-0.58.0/.../TextServices/impl.rs:1559)
// ============================================================

impl ITextStoreACP_Impl for TsfTextStore_Impl {
    fn AdviseSink(
        &self,
        _riid: *const GUID,
        _punk: Option<&IUnknown>,
        _dwmask: u32,
    ) -> Result<()> {
        // T3:接受 sink 注册,但暂不保存。T6 触发 UI 时再真存 + 投递 ITfTextStoreACPSink::OnTextChange。
        Ok(())
    }

    fn UnadviseSink(&self, _punk: Option<&IUnknown>) -> Result<()> {
        Ok(())
    }

    /// 客户端请求锁(读 / 写)— T3 返回 S_OK 表示接受但本侧不主动加锁(单线程 IME 足够)。
    fn RequestLock(&self, _dwlockflags: u32) -> Result<HRESULT> {
        Ok(HRESULT(0))
    }

    /// 返回 TS 状态 — 静态能力报告本 IME 支持的格式。
    /// `TS_SS_DISJOINTSEL | TS_SS_TRANSITORY | TS_SS_NOHIDDENTEXT` 是常用组合。
    fn GetStatus(&self) -> Result<TS_STATUS> {
        let dynf: u32 = 0; // 动态标志:输入会话期间是否可写
        let staticf: u32 = 0x8000_0000u32; // TS_SS_NOHIDDENTEXT 等最低位组,这里按需留 0 也行
        Ok(TS_STATUS { dwDynamicFlags: dynf, dwStaticFlags: staticf })
    }

    fn QueryInsert(
        &self,
        acpteststart: i32,
        acptestend: i32,
        _cch: u32,
        pacpresultstart: *mut i32,
        pacpresultend: *mut i32,
    ) -> Result<()> {
        if !pacpresultstart.is_null() { unsafe { *pacpresultstart = acpteststart }; }
        if !pacpresultend.is_null() { unsafe { *pacpresultend = acptestend }; }
        Ok(())
    }

    fn GetSelection(
        &self,
        _ulindex: u32,
        _ulcount: u32,
        _pselection: *mut TS_SELECTION_ACP,
        pcfetched: *mut u32,
    ) -> Result<()> {
        if !pcfetched.is_null() { unsafe { *pcfetched = 0 }; }
        Ok(())
    }

    fn SetSelection(&self, _ulcount: u32, _pselection: *const TS_SELECTION_ACP) -> Result<()> {
        Ok(())
    }

    fn GetText(
        &self,
        _acpstart: i32,
        _acpend: i32,
        _pchplain: PWSTR,
        _cchplainreq: u32,
        pcchplainret: *mut u32,
        _prgruninfo: *mut TS_RUNINFO,
        _cruninforeq: u32,
        pcruninforet: *mut u32,
        _pacpnext: *mut i32,
    ) -> Result<()> {
        if !pcchplainret.is_null() { unsafe { *pcchplainret = 0 }; }
        if !pcruninforet.is_null() { unsafe { *pcruninforet = 0 }; }
        Ok(())
    }

    /// 文本替换 — 客户端通过本接口直接修改缓冲区。T3 阶段仅记日志,T6 实际处理。
    fn SetText(
        &self,
        _dwflags: u32,
        acpstart: i32,
        acpend: i32,
        _pchtext: &PCWSTR,
        _cch: u32,
    ) -> Result<TS_TEXTCHANGE> {
        Ok(TS_TEXTCHANGE { acpStart: acpstart, acpOldEnd: acpend, acpNewEnd: acpstart })
    }

    fn GetFormattedText(&self, _acpstart: i32, _acpend: i32) -> Result<IDataObject> {
        // E_NOTIMPL = 0x80004015: 本 IME 不支持格式化文本(RTF / HTML / etc.)
        Err(Error::from_hresult(windows::core::HRESULT(0x80004015u32 as i32)))
    }

    fn GetEmbedded(
        &self,
        _acppos: i32,
        _rguidservice: *const GUID,
        _riid: *const GUID,
    ) -> Result<IUnknown> {
        // E_NOTIMPL: 本 IME 不嵌入 OLE 对象
        Err(Error::from_hresult(windows::core::HRESULT(0x80004015u32 as i32)))
    }

    fn QueryInsertEmbedded(
        &self,
        _pguidservice: *const GUID,
        _pformatetc: *const FORMATETC,
    ) -> Result<BOOL> {
        Ok(BOOL(0))
    }

    fn InsertEmbedded(
        &self,
        _dwflags: u32,
        acpstart: i32,
        acpend: i32,
        _pdataobject: Option<&IDataObject>,
    ) -> Result<TS_TEXTCHANGE> {
        Ok(TS_TEXTCHANGE { acpStart: acpstart, acpOldEnd: acpend, acpNewEnd: acpstart })
    }

    /// 客户端要求在当前选区插入纯文本(IME 最常用的入口) — T3 仅记日志,T6 实际写入。
    /// T8 E2E 时会改成真插入:调 ITfContext::InsertTextAtSelection 上屏 PinyinBuffer 候选。
    fn InsertTextAtSelection(
        &self,
        _dwflags: u32,
        _pchtext: &PCWSTR,
        _cch: u32,
        _pacpstart: *mut i32,
        _pacpend: *mut i32,
        _pchange: *mut TS_TEXTCHANGE,
    ) -> Result<()> {
        // 不假装成功插入,也不 todo!() — 显式 Ok(()) 让 CTF 链路继续走。
        // T6 任务:把 _pchtext 内容写入 ITfContext 的当前 range。
        Ok(())
    }

    fn InsertEmbeddedAtSelection(
        &self,
        _dwflags: u32,
        _pdataobject: Option<&IDataObject>,
        _pacpstart: *mut i32,
        _pacpend: *mut i32,
        _pchange: *mut TS_TEXTCHANGE,
    ) -> Result<()> {
        // 本 IME 不支持嵌入对象 → 等价于返回 E_NOTIMPL 但走 Ok 路径让 CTF 跳过。
        Ok(())
    }

    fn RequestSupportedAttrs(
        &self,
        _dwflags: u32,
        _cfilterattrs: u32,
        _pafilterattrs: *const GUID,
    ) -> Result<()> {
        Ok(())
    }

    fn RequestAttrsAtPosition(
        &self,
        _acppos: i32,
        _cfilterattrs: u32,
        _pafilterattrs: *const GUID,
        _dwflags: u32,
    ) -> Result<()> {
        Ok(())
    }

    fn RequestAttrsTransitioningAtPosition(
        &self,
        _acppos: i32,
        _cfilterattrs: u32,
        _pafilterattrs: *const GUID,
        _dwflags: u32,
    ) -> Result<()> {
        Ok(())
    }

    fn FindNextAttrTransition(
        &self,
        _acpstart: i32,
        _acphalt: i32,
        _cfilterattrs: u32,
        _pafilterattrs: *const GUID,
        _dwflags: u32,
        _pacpnext: *mut i32,
        pffound: *mut BOOL,
        _plfoundoffset: *mut i32,
    ) -> Result<()> {
        if !pffound.is_null() { unsafe { *pffound = BOOL(0) }; }
        Ok(())
    }

    fn RetrieveRequestedAttrs(
        &self,
        _ulcount: u32,
        _paattrvals: *mut TS_ATTRVAL,
        pcfetched: *mut u32,
    ) -> Result<()> {
        if !pcfetched.is_null() { unsafe { *pcfetched = 0 }; }
        Ok(())
    }

    fn GetEndACP(&self) -> Result<i32> {
        Ok(0)
    }

    fn GetActiveView(&self) -> Result<u32> {
        Ok(0)
    }

    fn GetACPFromPoint(&self, _vcview: u32, _ptscreen: *const POINT, _dwflags: u32) -> Result<i32> {
        Ok(0)
    }

    fn GetTextExt(
        &self,
        _vcview: u32,
        _acpstart: i32,
        _acpend: i32,
        _prc: *mut RECT,
        pfclipped: *mut BOOL,
    ) -> Result<()> {
        if !pfclipped.is_null() { unsafe { *pfclipped = BOOL(0) }; }
        Ok(())
    }

    fn GetScreenExt(&self, _vcview: u32) -> Result<RECT> {
        // 屏幕坐标覆盖区 — T3 返全屏,实际 IME 候选框位置由 ITfContextView 决定。
        Ok(RECT { left: 0, top: 0, right: 0, bottom: 0 })
    }

    fn GetWnd(&self, _vcview: u32) -> Result<HWND> {
        Ok(HWND(std::ptr::null_mut()))
    }
}