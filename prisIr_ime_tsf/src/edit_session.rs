//! ITfEditSession 实现 — TSF 规定的 document 修改唯一合法通道(T25+ 架构修复)。
//!
//! **为什么需要这个模块(2026-09-01 真根因)**:
//! TSF 架构硬性要求:凡是读写 document 内容 / selection 的操作
//! (`ITfRange::SetText`、`ITfContextComposition::StartComposition`、
//! `ITfInsertAtSelection::InsertTextAtSelection`、`ITfComposition::EndComposition`、
//! 甚至 `ITfContext::GetStart`)都**必须**在 `ITfEditSession::DoEditSession(ec)` 回调里执行,
//! 由 `ITfContext::RequestEditSession(tid, session, TF_ES_SYNC|TF_ES_READWRITE)` 触发。
//! 直接在 OnKeyDown 同步上下文里调 → ec(edit cookie)无效 → GetStart/SetText 返错
//! (实测 `GetStart ok=false`)→ composition 建不起来 → 按键被吃但字不上屏。
//!
//! **设计**:
//!   - `DocOp` 枚举:一次 edit session 要干的 document 操作。
//!     display/commit 文本在 OnKeyDown 主线程(读 pinyin 状态机)算好塞进来,
//!     edit session 只负责"怎么写 document",不管拼音逻辑 — 职责单一,不易崩。
//!   - `PrisirEditSession` 实现 `ITfEditSession`,持有若干 `Arc<Mutex<>>` 字段的克隆
//!     (composition / composition_range / context),不持有整个 COM 对象 → 无循环引用。
//!   - `run_edit_session(...)` 是给 OnKeyDown 调的一站式入口:构造 session + RequestEditSession。

use std::sync::{Arc, Mutex};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::TextServices::*;

/// 一次 edit session 要执行的 document 操作。
#[derive(Clone)]
pub(crate) enum DocOp {
    /// 确保 composition 存在(没有就 StartComposition),并把 display 写到 composition range。
    /// 用于:字母键累积拼音、退格改 buffer 后刷新 composing 显示。
    /// display 只放纯拼音 buffer(候选走独立悬浮窗,不写进文档)。
    UpdateComposition { display: String },
    /// 上屏:在当前 selection 插入 text,并 EndComposition + 清 composition 句柄。
    /// 用于:空格/数字选中候选后提交。
    Commit { text: String },
    /// 仅结束 composition(不上屏)。用于:buffer 删空时收尾(composition 已是空文本)。
    /// 不修就上屏卡死(2026-09-01 实测:Esc 后 composition 挂着,后续按键无法再起 composition)。
    EndComposition,
    /// Esc 取消 composition — 丢弃 composition 文本,不留进文档。
    /// 与 EndComposition 的区别:先清空 range 再 End,避免拼音被 finalize 进文档。
    CancelComposition,
}

/// edit session 需要的共享字段(全部 Arc 克隆,来自 TsfInputProcessor)。
/// 独立打包是为了不持有整个 COM 对象,避免 AddRef 循环。
pub(crate) struct EditSessionState {
    pub context: Arc<Mutex<Option<ITfContext>>>,
    pub composition: Arc<Mutex<Option<ITfComposition>>>,
    pub composition_range: Arc<Mutex<Option<ITfRange>>>,
}

/// ITfEditSession COM 对象 — RequestEditSession 的回调。
/// `windows::core::implement!` 生成 vtable + AddRef/Release/QI。
#[windows::core::implement(ITfEditSession)]
pub(crate) struct PrisirEditSession {
    op: DocOp,
    st: EditSessionState,
}

impl ITfEditSession_Impl for PrisirEditSession_Impl {
    /// TSF 在拿到 document 写锁后回调这里,ec 是本次合法的 edit cookie。
    /// 所有 GetStart/StartComposition/SetText/InsertText/EndComposition 都必须用 ec。
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        match &self.op {
            DocOp::UpdateComposition { display } => self.update_composition(ec, display),
            DocOp::Commit { text } => self.commit(ec, text),
            DocOp::EndComposition => self.end_composition(ec),
            DocOp::CancelComposition => self.cancel_composition(ec),
        }
    }
}

impl PrisirEditSession_Impl {
    /// 确保 composition 存在 + 把 display 写进 range(composing 下划线显示)。
    fn update_composition(&self, ec: u32, display: &str) -> Result<()> {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!("EditSession: UpdateComposition display='{}'", display));

        // 取 context(供后面 SetSelection 把光标设到拼音串末尾)。
        let ctx_for_sel = {
            let g = self.this.st.context.lock().unwrap();
            g.clone()
        };

        // 1. composition 不存在 → StartComposition
        if self.this.st.composition.lock().unwrap().is_none() {
            let ctx = {
                let g = self.this.st.context.lock().unwrap();
                g.clone()
            };
            let Some(ctx) = ctx else {
                #[cfg(feature = "dllentry_log")]
                crate::com_class_factory::log_dll_entry("EditSession: no context, skip StartComposition");
                return Ok(());
            };
            // 修「按 s 光标跳回你好前面」(2026-09-03 真根因):旧代码用 ctx.GetStart() 拿**文档
            // 起点**建 composition → 每次新 composition 都落在文档开头(「你好」前面),新拼音
            // 插到已上屏文字之前。必须用**当前 selection(光标)**位置建 composition。
            // GetSelection(TF_DEFAULT_SELECTION) 拿光标 range → 折叠到末尾(光标处)→ 在此建 composition。
            let range = {
                let mut fetched: u32 = 0;
                let mut selbuf: [TF_SELECTION; 1] = [TF_SELECTION::default()];
                let got = unsafe { ctx.GetSelection(ec, TF_DEFAULT_SELECTION, &mut selbuf, &mut fetched) };
                if got.is_ok() && fetched > 0 {
                    // 取 selection 的 range(ManuallyDrop 包裹,克隆出独立 range),折叠到末尾 = 光标插入点。
                    let r: Option<ITfRange> = unsafe { core::mem::ManuallyDrop::take(&mut selbuf[0].range) };
                    if let Some(r) = r {
                        let _ = unsafe { r.Collapse(ec, TF_ANCHOR_END) };
                        r
                    } else {
                        // 拿不到 selection range → 退回文档起点(保底,不该发生)。
                        unsafe { ctx.GetStart(ec)? }
                    }
                } else {
                    // GetSelection 失败 → 退回文档起点(保底)。
                    unsafe { ctx.GetStart(ec)? }
                }
            };
            let ctx_comp: ITfContextComposition = ctx.cast()?;
            // composition 终止 sink:用 TsfInputProcessor 本体不必要,这里传一个简单的 sink 也行,
            // 但 StartComposition 要求非 null sink。复用主对象的 ITfCompositionSink 会引入循环,
            // 故用一个独立的 no-op sink。
            let sink: ITfCompositionSink = NoopCompositionSink.into();
            let comp = unsafe { ctx_comp.StartComposition(ec, &range, &sink) }?;
            *self.this.st.composition.lock().unwrap() = Some(comp);
            *self.this.st.composition_range.lock().unwrap() = Some(range);
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry("EditSession: StartComposition(at selection) OK");
        }

        // 2. 把 display 写进 composition range。
        //    2026-09-03 真根因(字母累积 + 光标卡最前 + ShiftStart=0 移不动):
        //    之前缓存 StartComposition 时的 range 反复复用,但 SetText 会把它塌成末尾空 range,
        //    下次 SetText 变纯插入 → 拼音叠加;缓存 range 的锚点又被 composition 锁定,ShiftStart
        //    拉不回(实测 moved=0)。正确做法(对齐 rime/weasel):**每次从 composition.GetRange()
        //    现拿当前真实覆盖的活 range**,它始终指着 composition 实际区间,SetText 是真替换,
        //    光标随 composition 末尾走。不再复用缓存 range。
        let comp = {
            let g = self.this.st.composition.lock().unwrap();
            g.clone()
        };
        if let Some(comp) = comp {
            match unsafe { comp.GetRange() } {
                Ok(range) => {
                    let wide: Vec<u16> = display.encode_utf16().collect();
                    unsafe { range.SetText(ec, 0, &wide) }?;
                    #[cfg(feature = "dllentry_log")]
                    crate::com_class_factory::log_dll_entry("EditSession: SetText(GetRange) OK");

                    // 修「光标钉在拼音最前」(2026-09-03):composition 内容对了,但 selection 光标
                    // 一直没主动设,宿主默认把它放在 composition 起点 → 字母在光标后方长。
                    // 显式把 selection 设为 composition range 末尾(clone → Collapse(END) → SetSelection,
                    // ase=TF_AE_END 表示活动端在末尾),光标即跟到拼音串尾。
                    if let Some(ctx) = &ctx_for_sel {
                        if let Ok(sel_range) = unsafe { range.Clone() } {
                            let _ = unsafe { sel_range.Collapse(ec, TF_ANCHOR_END) };
                            let sel = TF_SELECTION {
                                range: core::mem::ManuallyDrop::new(Some(sel_range)),
                                style: TF_SELECTIONSTYLE { ase: TF_AE_END, fInterimChar: BOOL(0) },
                            };
                            let r = unsafe { ctx.SetSelection(ec, &[sel]) };
                            #[cfg(feature = "dllentry_log")]
                            crate::com_class_factory::log_dll_entry(&format!("EditSession: SetSelection(end) ok={}", r.is_ok()));
                        }
                    }
                }
                Err(e) => {
                    #[cfg(feature = "dllentry_log")]
                    crate::com_class_factory::log_dll_entry(&format!("EditSession: GetRange FAIL {e}"));
                }
            }
        }
        Ok(())
    }

    /// 上屏:把结果写进 composition range(替换拼音)→ EndComposition finalize 结果 → 清句柄。
    /// 2026-09-03 修「Explorer 搜索 你好nihao 叠加」:旧流程是 EndComposition(把拼音 finalize
    /// 留在文档) + InsertTextAtSelection(再插结果) → 宿主把拼音残留 + 结果叠加成「你好nihao」。
    /// 正确做法:composition range 还指着拼音时,直接 SetText 替换成结果串,再 EndComposition
    /// finalize —— 文档里只有结果,无拼音残留。range 不存在(极少)才退回 InsertTextAtSelection。
    fn commit(&self, ec: u32, text: &str) -> Result<()> {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!("EditSession: Commit text='{}'", text));

        let ctx = {
            let g = self.this.st.context.lock().unwrap();
            g.clone()
        };
        let Some(ctx) = ctx else {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry("EditSession: no context, skip commit");
            return Ok(());
        };

        let wide: Vec<u16> = text.encode_utf16().collect();

        // 优先:composition 的活 range 上直接写结果串(替换拼音),再 EndComposition finalize。
        let comp = {
            let mut g = self.this.st.composition.lock().unwrap();
            g.take()
        };
        // 缓存的 composition_range 一并清掉(update 已改用 GetRange,不再维护它)。
        self.this.st.composition_range.lock().unwrap().take();
        if let Some(comp) = comp {
            // 用 composition.GetRange() 现拿当前真实覆盖的活 range(同 update_composition 的修法),
            // SetText 真替换拼音为结果串;EndComposition finalize 时光标随 composition 末尾走,
            // 落在结果串之后(修「世界你好」倒置 + 缓存 range 塌掉导致 ShiftStart=0 移不动)。
            match unsafe { comp.GetRange() } {
                Ok(range) => {
                    if let Err(e) = unsafe { range.SetText(ec, 0, &wide) } {
                        #[cfg(feature = "dllentry_log")]
                        crate::com_class_factory::log_dll_entry(&format!("EditSession: Commit SetText FAIL {e}"));
                    }
                    // EndComposition 前把 selection 设到结果串末尾,finalize 后光标落在结果串后,
                    // 下次输入接在后面(修「世界你好」倒置)。
                    if let Ok(sel_range) = unsafe { range.Clone() } {
                        let _ = unsafe { sel_range.Collapse(ec, TF_ANCHOR_END) };
                        let sel = TF_SELECTION {
                            range: core::mem::ManuallyDrop::new(Some(sel_range)),
                            style: TF_SELECTIONSTYLE { ase: TF_AE_END, fInterimChar: BOOL(0) },
                        };
                        let r = unsafe { ctx.SetSelection(ec, &[sel]) };
                        #[cfg(feature = "dllentry_log")]
                        crate::com_class_factory::log_dll_entry(&format!("EditSession: Commit SetSelection(end) ok={}", r.is_ok()));
                    }
                }
                Err(e) => {
                    #[cfg(feature = "dllentry_log")]
                    crate::com_class_factory::log_dll_entry(&format!("EditSession: Commit GetRange FAIL {e}"));
                }
            }
            let _ = unsafe { comp.EndComposition(ec) };
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry("EditSession: Commit EndComposition OK");
            return Ok(());
        }

        // 兜底:无 composition(英文模式逐字母上屏) → 在 selection 插入。
        // 2026-09-03 修「英文连续输入后字母在前」:InsertTextAtSelection 返回插入串的 range,
        // 但不主动设 selection 时宿主光标停在插入串起点 → 下一个字母插到前一个前面(ab→ba)。
        // 用返回的 range 把光标设到刚插入串末尾。
        let insert_at: ITfInsertAtSelection = ctx.cast()?;
        let inserted = unsafe {
            insert_at.InsertTextAtSelection(ec, INSERT_TEXT_AT_SELECTION_FLAGS(0), &wide)
        }?;
        if let Ok(sel_range) = unsafe { inserted.Clone() } {
            let _ = unsafe { sel_range.Collapse(ec, TF_ANCHOR_END) };
            let sel = TF_SELECTION {
                range: core::mem::ManuallyDrop::new(Some(sel_range)),
                style: TF_SELECTIONSTYLE { ase: TF_AE_END, fInterimChar: BOOL(0) },
            };
            let _ = unsafe { ctx.SetSelection(ec, &[sel]) };
        }
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("EditSession: Commit InsertTextAtSelection(fallback) OK");
        Ok(())
    }

    /// 仅结束 composition(buffer 删空收尾)。composition 里已是空文本,直接 End 即可。
    /// 2026-09-01 卡死根因:退格清空 buffer 后 composition 仍挂,后续按键无法恢复。
    fn end_composition(&self, ec: u32) -> Result<()> {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("EditSession: EndComposition");
        let comp = {
            let mut g = self.this.st.composition.lock().unwrap();
            g.take()
        };
        if let Some(comp) = comp {
            let _ = unsafe { comp.EndComposition(ec) };
            self.this.st.composition_range.lock().unwrap().take();
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry("EditSession: EndComposition OK");
        }
        Ok(())
    }

    /// Esc 取消 composition — **丢弃 composition 文本,不留进文档,且前台立即刷新**。
    /// 2026-09-02 两全修复:
    ///   - 只 SetText 空串 → 前台 z 残留不刷新(notepad 对「组合变空」不重绘);
    ///   - 只 EndComposition → z 被 finalize 上屏(2026-09-01 的 bug 复现);
    ///   正确做法:SetText 清空时传 **TF_ST_CORRECTION** flag(告诉宿主这是「更正/删除」
    ///   而非普通编辑),notepad 据此正确重绘删除区域;清空后再 End,无文本可 finalize。
    fn cancel_composition(&self, ec: u32) -> Result<()> {
        use windows::Win32::UI::TextServices::TF_ST_CORRECTION;
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("EditSession: CancelComposition (SetText CORRECTION + End)");
        // 1. 清空组合 range 文本(丢弃拼音),TF_ST_CORRECTION 触发宿主重绘删除区
        let range = {
            let mut g = self.this.st.composition_range.lock().unwrap();
            g.take()
        };
        if let Some(range) = range {
            let r = unsafe { range.SetText(ec, TF_ST_CORRECTION, &[]) };
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!(
                "EditSession: CancelComposition SetText(CORRECTION,empty) hr={:?}", r.as_ref().map(|_| ()).map_err(|e| e.code().0 as u32)));
        }
        // 2. 结束组合(range 已空,无文本可 finalize)
        let comp = {
            let mut g = self.this.st.composition.lock().unwrap();
            g.take()
        };
        if let Some(comp) = comp {
            let r = unsafe { comp.EndComposition(ec) };
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!(
                "EditSession: CancelComposition EndComposition hr={:?}", r.as_ref().map(|_| ()).map_err(|e| e.code().0 as u32)));
        }
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("EditSession: CancelComposition OK");
        // 3. 2026-09-02 「光标不动就不刷新」根因修复:取消组合后 notepad 宿主不重绘删除区,
        //    靠光标 timer/下个输入事件才刷。主动对 notepad 编辑窗 InvalidateRect 强制立即重绘。
        //    ITfContextView::GetWnd 拿宿主窗口句柄(可能失败,如 UWP/无窗宿主,失败不影响功能)。
        self.repaint_host();
        Ok(())
    }

    /// 主动强制宿主窗口重绘 — 取消组合/清空文本后,notepad 这类宿主不会立刻刷新删除区,
    /// 要等光标 timer 或下一个输入事件(实测:光标停闪,任意动作一发生就"秒删")。
    /// 对 active view 的窗口句柄 InvalidateRect(整窗、擦背景)立即触发重绘,不等光标。
    /// GetWnd 对无窗宿主(UWP/某些控件)会失败,此时静默跳过(不影响正确性,只是不加速刷新)。
    fn repaint_host(&self) {
        use windows::Win32::Graphics::Gdi::InvalidateRect;
        let ctx = { let g = self.this.st.context.lock().unwrap(); g.clone() };
        let Some(ctx) = ctx else { return };
        let view: std::result::Result<ITfContextView, _> = unsafe { ctx.GetActiveView() };
        let Ok(view) = view else { return };
        // windows-rs 0.58: GetWnd 是返回 Result<HWND> 的方法(无 out 参数)。
        let hwnd: std::result::Result<HWND, _> = unsafe { view.GetWnd() };
        let Ok(hwnd) = hwnd else { return };
        if !hwnd.0.is_null() {
            // 整窗重绘 + 擦背景:true=擦除背景,删除的 z 区域立即清掉。
            let _ = unsafe { InvalidateRect(hwnd, None, true) };
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry("EditSession: repaint_host InvalidateRect OK");
        }
    }
}

/// 独立的 no-op ITfCompositionSink — StartComposition 需要非 null sink,
/// 但 composition 终止时的清理我们在 commit/Deactivate 里自己做,这里不需逻辑。
/// 独立对象避免与 TsfInputProcessor 形成 AddRef 循环。
#[windows::core::implement(ITfCompositionSink)]
pub(crate) struct NoopCompositionSink;

impl ITfCompositionSink_Impl for NoopCompositionSink_Impl {
    fn OnCompositionTerminated(&self, _ecwrite: u32, _pcomposition: Option<&ITfComposition>) -> Result<()> {
        Ok(())
    }
}

/// 一站式入口:在 ctx 上同步跑一个 edit session 执行 op。
/// OnKeyDown 调这个,而不是直接碰 document。
/// 返回 TSF 的 RequestEditSession HRESULT(Ok=执行成功)。
pub(crate) fn run_edit_session(
    tid: u32,
    ctx: &ITfContext,
    st: EditSessionState,
    op: DocOp,
) -> Result<()> {
    let session: ITfEditSession = PrisirEditSession { op, st }.into();
    // TF_ES_SYNC:同步等 session 跑完再返回(OnKeyDown 需要立刻知道结果)。
    // TF_ES_READWRITE:要改 document,需读写锁。
    // windows-rs 把 out-HRESULT 作为返回值:外层 Result 是 RequestEditSession 本身的调用错误,
    // 返回的 HRESULT 是 DoEditSession 的执行结果。
    let flags = TF_ES_SYNC | TF_ES_READWRITE;
    // 2026-09-02 回删卡顿定位:记录 RequestEditSession 拿写锁的耗时。notepad TSF 栈下
    // 同步 session 可能排队 1-2s,这是回删卡的疑似元凶,先量化。
    let _t = std::time::Instant::now();
    let call = unsafe { ctx.RequestEditSession(tid, &session, flags) };
    #[cfg(feature = "dllentry_log")]
    crate::com_class_factory::log_dll_entry(&format!(
        "EditSession: RequestEditSession(RW) took={:.1}ms", _t.elapsed().as_secs_f64() * 1000.0));
    // 2026-09-02 回删删空失败根因:不再静默吞错。RequestEditSession 调用错(外层 Err)
    // 或 session 执行失败(HRESULT 非 S_OK,session 可能根本没跑,composition 没清)→ 记日志暴露。
    match call {
        Err(e) => {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!(
                "EditSession: RequestEditSession CALL FAILED hr=0x{:08X}", e.code().0 as u32));
            return Err(e);
        }
        Ok(session_hr) => {
            if session_hr.is_err() {
                #[cfg(feature = "dllentry_log")]
                crate::com_class_factory::log_dll_entry(&format!(
                    "EditSession: DoEditSession SESSION FAILED hr=0x{:08X}", session_hr.0 as u32));
            }
            session_hr.ok()
        }
    }
}

// ──────────────────────────────────────────────────────────────────────
// caret 屏幕坐标查询 — 给候选悬浮窗定位用(2026-09-01 新增)
// ──────────────────────────────────────────────────────────────────────

thread_local! {
    static CARET_RESULT: std::cell::Cell<Option<(i32, i32)>> = const { std::cell::Cell::new(None) };
}

/// 读 composition range(打字时光标处)的屏幕坐标,用于把候选窗定位到光标下方。
/// ITfContextView::GetTextExt 必须在 edit session 里调,故走只读 session。
/// 无 composition(没在打字)→ None,调用方退化为鼠标位置。
pub(crate) fn query_caret_screen_pos(
    tid: u32,
    ctx: &ITfContext,
    range: Arc<Mutex<Option<ITfRange>>>,
) -> Option<(i32, i32)> {
    let session: ITfEditSession = CaretQuerySession { ctx: ctx.clone(), range }.into();
    let flags = TF_ES_SYNC | TF_ES_READ;
    // 2026-09-02 回删卡顿定位:光标坐标查询也走同步 session,量化其耗时。
    let _t = std::time::Instant::now();
    let hr = unsafe { ctx.RequestEditSession(tid, &session, flags) }.ok()?;
    #[cfg(feature = "dllentry_log")]
    crate::com_class_factory::log_dll_entry(&format!(
        "EditSession: RequestEditSession(caret RO) took={:.1}ms", _t.elapsed().as_secs_f64() * 1000.0));
    hr.ok().ok()?;
    CARET_RESULT.with(|c| c.get())
}

#[windows::core::implement(ITfEditSession)]
struct CaretQuerySession {
    ctx: ITfContext,
    range: Arc<Mutex<Option<ITfRange>>>,
}

impl ITfEditSession_Impl for CaretQuerySession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        CARET_RESULT.with(|c| c.set(None));
        let Some(range) = self.this.range.lock().unwrap().clone() else {
            return Ok(()); // 无 composition,不在打字 → None
        };
        // 拿 active view → GetTextExt 求 range 的屏幕包围盒。
        let view: ITfContextView = unsafe { self.this.ctx.GetActiveView() }?;
        let mut rc = RECT::default();
        let mut clipped = BOOL::default();
        unsafe { view.GetTextExt(ec, &range, &mut rc, &mut clipped) }?;
        // 候选窗放 range 下缘(屏幕坐标,top-left 原点是屏幕)。
        CARET_RESULT.with(|c| c.set(Some((rc.left, rc.bottom))));
        Ok(())
    }
}
