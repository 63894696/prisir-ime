//! ITfTextInputProcessor + ITfSource + ITfKeyEventSink + ITfThreadMgrEventSink
//! 四接口合一 COM 对象 —— v0.8 真 TSF 输入路径接通。
//!
//! 设计要点(关键约束):
//!   1. 同一 COM 对象同时实现 4 个接口 → TSF 在 Activate 时 QueryInterface(IID_ITfKeyEventSink)
//!      必须拿到 self。`#[windows::core::implement(...)]` 宏生成的多接口 vtable 天然支持。
//!   2. Activate(ITfThreadMgr, tid):
//!      - 保存 ptim 到 self.tim
//!      - 调 `ptim.GetFocus()` 取 ITfDocumentMgr → `ITfContext`
//!      - 调 `ITfSource::AdviseSink(IID_ITfKeyEventSink, self)` 拿 cookie
//!      - 调 `ITfSource::AdviseSink(IID_ITfThreadMgrEventSink, self)` 监听聚焦切换
//!      - 调 `prisIr_ime.dll` LoadLibrary + LoadEngine 准备引擎句柄
//!   3. OnKeyDown(WPARAM, LPARAM) → ITfKeyEventSink:
//!      - 解析 VK → 调 PinyinBuffer 状态机(on_char / on_digit / on_space / on_escape / on_backspace)
//!      - 中文模式:把当前 buffer 写到 ITfContext::StartComposition 的 range 上(composing 显示)
//!      - 英文模式(Shift 按下 / 用户切换):commit 字母直接 passthrough
//!      - 缓冲区有变化 → 调 ITfInsertAtSelection::InsertTextAtSelection 上屏 + EndComposition
//!   4. OnKeyUp:清 shift_held(用于 Shift 临时切英文)
//!   5. ITfContext 通过保存的 self.context 句柄,每次 OnThreadMgrEvent::OnSetFocus
//!      重新拉一次 — 否则切换窗口时 context 失效,候选上屏会失败。
//!
//! 注:InsertTextAtSelection 走 ITfInsertAtSelection 接口(ITfContext 同时实现),
//! 返回 ITfRange 表示插入位置。这里先不做候选 UI(UI 渲染放 v0.9),v0.8 只打通
//! "按键 → 状态机 → 上屏文字" 主链路。
#![allow(non_snake_case, non_upper_case_globals)]

use std::sync::{Arc, Mutex, OnceLock};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::TextServices::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};

// ──────────────────────────────────────────────────────────────────────
// 常量

/// 实时查询 Shift 是否按住(2026-09-01 修「大写卡死」):
/// 之前用 shift_held 跟踪状态(OnKeyDown 置 true / OnKeyUp 置 false),但 OnKeyUp
/// 会丢(焦点切换/事件路由到别的进程)→ shift_held 卡死 true → 中英两模式全大写+拼音不进。
/// 改成每次按键 GetKeyState(VK_SHIFT) 实时查询,根除跟踪状态卡死这一类问题。
/// 返 true = Shift 当前按下。
fn shift_is_down() -> bool {
    unsafe { (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0 }
}

/// Ctrl 当前是否按下(实时查询,同 shift_is_down 套路)。
/// 2026-09-02 修「Ctrl 组合键失效」:wants_key_state 对 A-Z 无条件吃,Ctrl+C/V/Z 被吞。
/// 用 GetKeyState(VK_CONTROL) 实时判,按下时对字母/数字/标点放行给 app(参考外挂式
/// app_debug.py 的 PASS-THRU letter 逻辑)。VK_CONTROL=0x11 是左右 Ctrl 归并后的通用键。
fn ctrl_is_down() -> bool {
    unsafe { (GetKeyState(0x11) as u16 & 0x8000) != 0 }
}

/// Alt 当前是否按下(VK_MENU=0x12)。Alt+字母 = 菜单/快捷键,必须放行。
fn alt_is_down() -> bool {
    unsafe { (GetKeyState(0x12) as u16 & 0x8000) != 0 }
}
// ──────────────────────────────────────────────────────────────────────

/// Prisir IME 的 CLSID(与 register.rs:CLSID_PRISIR_IME_STR + com_class_factory.rs 一致)。
pub const CLSID_PRISIR_IME: GUID = GUID::from_u128(0xA1B2_C3D4_E5F6_7890_ABCD_EF1234567890);

// ── TSF 接口 IID (windows-rs 0.58 不导出这些 IID 常量,直接照 SDK 写) ──
//
// 来自 msctf.h。`#[implement(...)]` 宏生成的 vtable 内部已经用了同一组 IID,
// 客户端 AdviseSink 传过来的 IID 必须匹配。

/// `IID_ITfKeyEventSink` = {aa80e7f5-2021-11d2-93e0-0060b067b86e}
pub const IID_ITF_KEY_EVENT_SINK: GUID = GUID::from_u128(0xaa80e7f5_2021_11d2_93e0_0060b067b86e);

/// `IID_ITfThreadMgrEventSink` = {aa80e80e-2021-11d2-93e0-0060b067b86e}
pub const IID_ITF_THREAD_MGR_EVENT_SINK: GUID = GUID::from_u128(0xaa80e80e_2021_11d2_93e0_0060b067b86e);

/// LangBarItem GUID(进程内任意唯一 GUID) — windows crate 已从 msctf.h 拿:
/// IID_ITfLangBarItemSink = {57dbe1a0-de25-11d2-afdd-00105a2799b5}。
/// 但我们注册自己的 LangBarItem 用的是 guidItem,不是 IID。这里随机一个唯一 GUID。
pub const GUID_PRISIR_LANGBAR_ITEM: GUID =
    GUID::from_u128(0x12345678_1234_1234_1234_123456789001);

// ──────────────────────────────────────────────────────────────────────
// 公开 TsfInputProcessor COM 类 — 5 接口合一
// ──────────────────────────────────────────────────────────────────────

/// 6 接口 COM 对象:`ITfTextInputProcessor` + `ITfTextInputProcessorEx` + `ITfSource` + `ITfKeyEventSink` +
/// `ITfThreadMgrEventSink` + `ITfLangBarItemSink` + `ITfCompositionSink`。
/// `windows::core::implement!` 宏为这 7 个接口生成 vtable,以及 `AddRef / Release / QueryInterface`。
///
/// **T25 修复**:必须显式加上 `ITfTextInputProcessorEx`。explorer 启动走
/// ImmSetActiveContext → msctf!CtfImeAssociateFocus → TF_SendLangBandMsg 时,msctf 会
/// QueryInterface 出 ITfTextInputProcessorEx 并调它的 ActivateEx。如果我们没 implement 这个接口,
/// implement 宏会给「required 但 _Impl 没写」的方法生成一个 zero-out-params 默认 stub,
/// 该 stub 盲写调用方传入的 out-ptr(本次 r8=0x38 垃圾地址)→ c0000005 把 explorer 干崩。
/// 显式实现 ActivateEx(内部转调同一 activate_inner)后,这个 slot 被真方法占据,不再踩 stub。
#[windows::core::implement(
    ITfTextInputProcessor,
    ITfTextInputProcessorEx,
    ITfSource,
    ITfKeyEventSink,
    ITfThreadMgrEventSink,
    ITfLangBarItemSink,
    ITfCompositionSink
)]
pub struct TsfInputProcessor {
    /// 引用计数(由宏自动维护)。
    #[allow(dead_code)]
    pub(crate) ref_count: u32,
    /// `Activate()` 保存的 ITfThreadMgr 句柄 — TSF 主动通过它枚举文档。
    /// `Arc<Mutex<Option<ITfThreadMgr>>>` 因为我们需要跨越 `&self` 边界访问。
    pub(crate) tim: Arc<Mutex<Option<ITfThreadMgr>>>,
    /// Activate 传入的 client id(tid)— AdviseKeyEventSink/UnadviseKeyEventSink 都要用。
    pub(crate) client_tid: Arc<Mutex<u32>>,
    /// 当前聚焦的 ITfContext — OnKeyDown / OnThreadMgrEvent 都会用到。
    pub(crate) context: Arc<Mutex<Option<ITfContext>>>,
    /// ITfContext 上 AdviseSink(IID_ITfKeyEventSink) 拿到的 cookie。Deactivate 时用。
    pub(crate) key_sink_cookie: Arc<Mutex<u32>>,
    /// ITfThreadMgr 上 AdviseSink(IID_ITfThreadMgrEventSink) 拿到的 cookie。
    pub(crate) threadmgr_sink_cookie: Arc<Mutex<u32>>,
    /// 拼音缓冲区 + FFI 引擎句柄。状态机在这里吃按键、做候选。
    pub(crate) state: Arc<Mutex<ProcessorState>>,
    // ── T20 新增字段 ──
    /// 当前模式 — true = 中文(composing), false = 英文(passthrough)。
    /// 用户决策:默认中文。Shift 按下时临时英文,松开恢复中文。
    pub(crate) is_chinese_mode: Arc<Mutex<bool>>,
    /// Shift 按下状态 — true 表示当前临时英文模式。
    pub(crate) shift_held: Arc<Mutex<bool>>,
    /// Composition 句柄 — StartComposition() 返的 ITfComposition。
    pub(crate) composition: Arc<Mutex<Option<ITfComposition>>>,
    /// Composition range — 在 ITfContext 上 SetText 用。
    pub(crate) composition_range: Arc<Mutex<Option<ITfRange>>>,
    /// LangBarItem 实例 — LangBar 按钮,用于点中/英图标切换。
    pub(crate) langbar_item: Arc<Mutex<Option<ITfLangBarItem>>>,
    /// LangBarItemMgr — 用于 AddItem / RemoveItem。
    pub(crate) langbar_mgr: Arc<Mutex<Option<ITfLangBarItemMgr>>>,
    /// LangBar AdviseItemSink cookie。
    pub(crate) langbar_cookie: Arc<Mutex<u32>>,
    /// LangBar OnClick 翻转模式后置 true,OnKeyDown 入口检查到会 EndComposition。
    /// 必须跟 langbar.rs::PrisirLangBarItem 共享同一个 Arc。
    pub(crate) pending_mode_change: Arc<Mutex<bool>>,
    /// 中/英标点模式 — true = 中文标点(,。;「」), false = 英文标点(,.;"")。
    /// 状态条「标」按钮翻转;OnKeyDown 按它决定标点映射。默认中文标点。
    pub(crate) is_chinese_punct: Arc<Mutex<bool>>,
}

impl TsfInputProcessor {
    pub fn new() -> Self {
        Self {
            ref_count: 1,
            tim: Arc::new(Mutex::new(None)),
            client_tid: Arc::new(Mutex::new(0)),
            context: Arc::new(Mutex::new(None)),
            key_sink_cookie: Arc::new(Mutex::new(0)),
            threadmgr_sink_cookie: Arc::new(Mutex::new(0)),
            state: Arc::new(Mutex::new(ProcessorState::new())),
            // T20 初始化:默认中文模式,Shift 未按,无 composition,无 langbar。
            is_chinese_mode: Arc::new(Mutex::new(true)),
            shift_held: Arc::new(Mutex::new(false)),
            composition: Arc::new(Mutex::new(None)),
            composition_range: Arc::new(Mutex::new(None)),
            langbar_item: Arc::new(Mutex::new(None)),
            langbar_mgr: Arc::new(Mutex::new(None)),
            langbar_cookie: Arc::new(Mutex::new(0)),
            pending_mode_change: Arc::new(Mutex::new(false)),
            is_chinese_punct: Arc::new(Mutex::new(true)),
        }
    }
}

impl Default for TsfInputProcessor {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────
// 状态机包装 — 把 keystroke::PinyinBuffer + engine handle 放一起
// ──────────────────────────────────────────────────────────────────────

/// 特殊键处理结果(handle_special 返回)。
pub(crate) enum SpecialResult {
    /// 有候选上屏(空格/数字选中)。
    Commit(String),
    /// buffer 变了(退格删字母 / Esc 清空),需重刷 composition 或结束。
    Handled,
    /// 翻页(PgDn/PgUp/+-/,.):候选页变了,buffer 不变,只重刷候选窗。
    Repage,
    /// buffer 为空 — 此键与拼音无关,**放行**给 app(退格删文档 / 空格上屏空格 / Esc 给 app)。
    /// 2026-09-01 根因:buffer 空时仍吃退格 → 文档文字永远删不到(实测按键完全无效)。
    Passthrough,
}

/// 进程内单实例的 IME 状态。
pub(crate) struct ProcessorState {
    /// 拼音缓冲区(来自 keystroke.rs,纯 Rust 状态机)。
    pub pinyin: crate::keystroke::PinyinBuffer,
}

// ──────────────────────────────────────────────────────────────────────
// 引擎句柄进程级全局 — 2026-09-02 修「首次切换卡记事本 2 秒 + 候选窗跳变」。
// 原设计:句柄挂 ProcessorState(每个 COM 对象一份),首次按键 get_or_init 同步
// `prisir_ime_load` → 反序列化 322MB .idx + finalize 排序,实测 2.03 秒,阻塞
// OnKeyDown 线程 → 记事本卡死;加载期 query 返空,候选窗显示空/半残快照,加载完
// 才跳对(用户见「只显示数字4没字,等会跳成另外3个字」)。
// 改:进程级 OnceLock + 后台线程预建。engine_handle() 非阻塞(get 拿到就用、拿不到
// 返 null 不卡 UI),后台线程建好后 set。全局 static 无 COM 对象悬垂风险。
// **绝不能 LoadLibrary 在 loader lock 下**(DllGetClassObject/CreateInstance),
// 只在 OnKeyDown / Activate / 后台线程调用。
// ──────────────────────────────────────────────────────────────────────
/// 引擎句柄包装:裸指针不 Send/Sync,显式声明以便装进 static OnceLock + 跨线程 set。
/// SAFETY: 句柄建成后只读共享;prisir_ime 引擎 query 走 &self 只读(内存索引 + SQLite
/// 连接),进程内多 STA 线程只读访问,与激活后逐键调用场景一致。建/写只发生一次(后台线程)。
#[derive(Clone, Copy)]
struct EnginePtr(*mut std::ffi::c_void);
unsafe impl Send for EnginePtr {}
unsafe impl Sync for EnginePtr {}

static ENGINE_HANDLE: OnceLock<EnginePtr> = OnceLock::new();
static ENGINE_BUILDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 非阻塞拿引擎句柄:已建好返句柄,没建好触发后台预建并返 null(候选暂空,不卡 UI)。
fn engine_handle_async() -> *mut std::ffi::c_void {
    if let Some(h) = ENGINE_HANDLE.get() {
        return h.0;
    }
    // CAS 保证只 spawn 一次后台建引擎。
    if !ENGINE_BUILDING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("engine_build: spawn background thread");
        std::thread::spawn(|| {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry("engine_build: thread START (loading engine...)");
            let h = crate::ffi::load_engine_with_default_db();
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!(
                "engine_build: background load DONE, handle_null={}", h.is_null()));
            // static OnceLock 进程级,无悬垂;set 只生效一次。
            let _ = ENGINE_HANDLE.set(EnginePtr(h));
            ENGINE_BUILDING.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
    // 注:每日活跃信号挂在 activate_inner(切输入法即触发),不在此(引擎预建要打字才到)。
    std::ptr::null_mut()
}

impl ProcessorState {
    fn new() -> Self {
        Self {
            pinyin: crate::keystroke::PinyinBuffer::new(),
        }
    }

    /// 拿引擎句柄(非阻塞)。转发到进程级全局 `engine_handle_async`。
    /// 没建好返 null(query 返空候选,不卡 UI),后台线程建好后后续调用即得句柄。
    pub(crate) fn engine_handle(&self) -> *mut std::ffi::c_void {
        engine_handle_async()
    }
}

// 安全:Process 内部各字段在 com-apartment 单线程上下文中使用,但因为 Arc<Mutex<>> 包裹,
unsafe impl Send for TsfInputProcessor {}
unsafe impl Sync for TsfInputProcessor {}

// ──────────────────────────────────────────────────────────────────────
// ITfTextInputProcessor 接口实现 — Activate / Deactivate
// ──────────────────────────────────────────────────────────────────────

impl ITfTextInputProcessor_Impl for TsfInputProcessor_Impl {
    /// TSF 调用以激活本 IME — 此处装 ITfKeyEventSink + 装 ITfThreadMgrEventSink + 拿当前 context + 注册 LangBarItem。
    /// **TIPC 验证期也会调 Activate**,此时可能没有真实 focus 上下文 → GetFocus() 返 null,
    /// 任何 `?` 都会让 Activate 返 Err → TIPC 把本 TIP 从 Assemblies 剔除。必须对每个步骤容错。
    fn Activate(&self, ptim: Option<&ITfThreadMgr>, tid: u32) -> Result<()> {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("TsfInputProcessor::Activate:ENTER");
        let r = self.activate_inner(ptim, tid);
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(if r.is_ok() {
            "TsfInputProcessor::Activate:OK"
        } else {
            "TsfInputProcessor::Activate:FAIL"
        });
        r
    }

    /// TSF 调用以停用本 IME — 撤销所有 sink,释放 context 句柄,Remove LangBarItem。
    fn Deactivate(&self) -> Result<()> {
        let inner = &self.this;
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("Deactivate: ENTER (destroy status bar + release owner)");

        // 0a. 销毁候选悬浮窗(切换输入法/卸载时)。先销毁,避免残留悬浮窗。
        crate::candidate_window::destroy(&crate::candidate_window::global_state());
        // 0a2. 销毁悬浮中/英状态条。
        crate::status_bar::destroy(&crate::status_bar::global_bar_state());
        // 0a3. 销毁符号/emoji 面板(切换输入法/卸载时,避免残留)。
        crate::panels::destroy(&crate::panels::global_panel_state());

        // 0. T20: 撤销 LangBarItem + AdviseItemSink
        //     拿 mgr 的 clone(防止 Option take 后 lifetime 冲突),RemoveItem + UnadviseItemSink。
        let mgr_opt = inner.langbar_mgr.lock().unwrap().clone();
        if let Some(mgr) = mgr_opt {
            let cookie = *inner.langbar_cookie.lock().unwrap();
            if cookie != 0 {
                if let Err(e) = unsafe { mgr.UnadviseItemSink(cookie) } {
                    eprintln!("[prisir_tsf] UnadviseItemSink failed: {e}");
                }
                *inner.langbar_cookie.lock().unwrap() = 0;
            }
            let item_opt = inner.langbar_item.lock().unwrap().clone();
            if let Some(item) = item_opt {
                if let Err(e) = unsafe { mgr.RemoveItem(&item) } {
                    eprintln!("[prisir_tsf] RemoveItem failed: {e}");
                }
            }
        }
        inner.langbar_item.lock().unwrap().take();
        inner.langbar_mgr.lock().unwrap().take();

        // 1. 撤销 ITfKeyEventSink — 走 ITfKeystrokeMgr::UnadviseKeyEventSink(tid)(与 Advise 对齐)
        if *inner.key_sink_cookie.lock().unwrap() != 0 {
            if let Some(tim) = inner.tim.lock().unwrap().as_ref() {
                let tid = *inner.client_tid.lock().unwrap();
                if let Ok(kmgr) = tim.cast::<ITfKeystrokeMgr>() {
                    let _ = unsafe { kmgr.UnadviseKeyEventSink(tid) };
                }
            }
            *inner.key_sink_cookie.lock().unwrap() = 0;
        }

        // 2. 撤销 ITfThreadMgr 上的 ITfThreadMgrEventSink
        if let Some(tim) = inner.tim.lock().unwrap().as_ref() {
            let cookie = *inner.threadmgr_sink_cookie.lock().unwrap();
            if cookie != 0 {
                let tim_source: ITfSource = tim.cast()?;
                let _ = unsafe { tim_source.UnadviseSink(cookie) };
                *inner.threadmgr_sink_cookie.lock().unwrap() = 0;
            }
        }

        // 3. 清空句柄
        inner.context.lock().unwrap().take();
        inner.tim.lock().unwrap().take();

        // 4. 清空拼音缓冲(下一轮激活是干净的)
        inner.state.lock().unwrap().pinyin.reset();

        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────
// 共享激活逻辑 — Activate 与 ActivateEx 都走这里(T25:避免两接口行为分叉)
// ──────────────────────────────────────────────────────────────────────

impl TsfInputProcessor_Impl {
    /// 真正的激活实现。`Activate` 与 `ITfTextInputProcessorEx::ActivateEx` 都转调到这里。
    /// 每个步骤容错:TIPC 验证期可能无 focus 上下文,任何一步失败都不让整体返 Err。
    fn activate_inner(&self, ptim: Option<&ITfThreadMgr>, tid: u32) -> Result<()> {
        // T25+ 诊断宏:把激活链路每一步写进 dll log(ctfmon/系统进程里 eprintln 不可见)。
        #[cfg(feature = "dllentry_log")]
        macro_rules! alog {
            ($($arg:tt)*) => { crate::com_class_factory::log_dll_entry(&format!($($arg)*)) };
        }
        #[cfg(not(feature = "dllentry_log"))]
        macro_rules! alog { ($($arg:tt)*) => {{}} }

        let ptim = match ptim {
            Some(t) => t,
            None => {
                eprintln!("[prisir_tsf] Activate: ptim is null (validation?), succeed as no-op");
                alog!("activate_inner: ptim=NULL -> noop");
                return Ok(());
            }
        };
        let inner = &self.this;

        // 1. 保存 ITfThreadMgr 句柄(便于后续 GetFocus / SetFocus 调用)
        inner.tim.lock().unwrap().replace(ptim.clone());

        // 1a0. 每日活跃信号(2026-09-04):切到 Prisir 输入法即算「活跃」,不管打不打字。
        //    挂 Activate 而非引擎预建(后者要真打字才触发;只翻菜单的用户统计不到)。
        //    tick 内部按本地日去重 + 异步后台 GET,不卡激活线程,失败静默。
        crate::active_signal::tick_daily_active();

        // 1a. 注册 ITfKeyEventSink — **不依赖 GetFocus,无条件最先武装**。
        //    2026-09-01 根因:Outlook/Win设置 等应用激活时 GetFocus 可能失败 → 旧代码走
        //    early-return,key sink 永不武装 → 打字出原字母(切搜狗再切回触发重新 Activate 才恢复)。
        //    AdviseKeyEventSink 只需 ptim+tid,与 focus context 无关,放最前保证总能武装。
        //    幂等:重复 Activate 先 Unadvise 再 Advise,避免重复订阅。
        {
            let tid_existing = *inner.client_tid.lock().unwrap();
            if *inner.key_sink_cookie.lock().unwrap() != 0 && tid_existing != 0 {
                if let Ok(kmgr) = ptim.cast::<ITfKeystrokeMgr>() {
                    let _ = unsafe { kmgr.UnadviseKeyEventSink(tid_existing) };
                }
                *inner.key_sink_cookie.lock().unwrap() = 0;
            }
            let ks_res: Result<()> = (|| {
                let kmgr: ITfKeystrokeMgr = ptim.cast()?;
                let me_sink: ITfKeyEventSink = unsafe { self.cast()? };
                unsafe { kmgr.AdviseKeyEventSink(tid, &me_sink, true) }
            })();
            match ks_res {
                Ok(()) => {
                    *inner.key_sink_cookie.lock().unwrap() = 1;
                    *inner.client_tid.lock().unwrap() = tid;
                    alog!("activate_inner: KeyEventSink armed via ITfKeystrokeMgr tid={tid}");
                }
                Err(e) => {
                    eprintln!("[prisir_tsf] AdviseKeyEventSink failed: {e}");
                    alog!("activate_inner: KeyEventSink AdviseKeyEventSink FAIL {e}");
                }
            }
        }

        // 1b. T20: 注册 LangBarItem (中/英切换按钮)。失败只打 warn,不影响 Activate 返 Ok。
        let langbar_item: Option<ITfLangBarItem> = (|| -> Option<ITfLangBarItem> {
            let mgr_res: Result<ITfLangBarItemMgr> = ptim.cast();
            let mgr = match mgr_res {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[prisir_tsf] LangBarItemMgr cast failed: {e}");
                    alog!("activate_inner: LangBarItemMgr cast FAIL {e}");
                    return None;
                }
            };
            let mode_ref = inner.is_chinese_mode.clone();
            let pending_ref = inner.pending_mode_change.clone();
            let item: ITfLangBarItem =
                crate::langbar::PrisirLangBarItem::new(mode_ref, pending_ref).into();
            // 诊断(2026-09-01): mgr AddItem 需 QI 出 ITfLangBarItemButton 才能拿
            // OnClick/GetIcon/GetText。若我们的 implement 宏 QI 不到 Button → mgr 拿不到
            // 这些 → E_FAIL(可解释 GetIcon/GetText 从不被调)。先自测 QI。
            #[cfg(feature = "dllentry_log")]
            {
                let qi_btn: Result<ITfLangBarItemButton> = item.cast();
                crate::com_class_factory::log_dll_entry(&format!(
                    "activate_inner: item QI ITfLangBarItemButton = {}",
                    if qi_btn.is_ok() { "OK" } else { "FAIL" }
                ));
                let qi_src: Result<ITfSource> = item.cast();
                crate::com_class_factory::log_dll_entry(&format!(
                    "activate_inner: item QI ITfSource = {}",
                    if qi_src.is_ok() { "OK" } else { "FAIL" }
                ));
            }
            if let Err(e) = unsafe { mgr.AddItem(&item) } {
                eprintln!("[prisir_tsf] LangBarItemMgr::AddItem failed: {e}");
                alog!("activate_inner: LangBar AddItem FAIL {e}");
                return None;
            }
            let mut cookie: u32 = 0;
            let me_as_sink: ITfLangBarItemSink = unsafe { self.cast() }.ok()?;
            let guid_item = GUID_PRISIR_LANGBAR_ITEM;
            let adv_res = unsafe { mgr.AdviseItemSink(&me_as_sink, &mut cookie, &guid_item) };
            if let Err(e) = adv_res {
                eprintln!("[prisir_tsf] LangBarItemMgr::AdviseItemSink failed: {e}");
                alog!("activate_inner: LangBar AdviseItemSink FAIL {e}");
            }
            *inner.langbar_mgr.lock().unwrap() = Some(mgr);
            *inner.langbar_cookie.lock().unwrap() = cookie;
            *inner.langbar_item.lock().unwrap() = Some(item.clone());
            eprintln!("[prisir_tsf] LangBarItem registered (cookie={cookie})");
            alog!("activate_inner: LangBar registered cookie={cookie}");
            Some(item)
        })();
        let _ = langbar_item;

        // 2. 拿当前聚焦的 ITfDocumentMgr / ITfContext — 失败走 no-op,下次 OnSetFocus 重装
        let dm = match unsafe { ptim.GetFocus() } {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[prisir_tsf] Activate: GetFocus() failed ({e}) — no focus ctx, skipping");
                alog!("activate_inner: GetFocus FAIL {e} -> EARLY RETURN (key sink NOT installed)");
                // 即便无 focus 也要先装 ThreadMgrEventSink,否则 OnSetFocus 永远不触发、无法补装 key sink。
                if let Ok(ts) = ptim.cast::<ITfSource>() {
                    if let Ok(mu) = unsafe { self.cast::<IUnknown>() } {
                        if let Ok(c) = unsafe { ts.AdviseSink(&IID_ITF_THREAD_MGR_EVENT_SINK, &mu) } {
                            *inner.threadmgr_sink_cookie.lock().unwrap() = c;
                            alog!("activate_inner: ThreadMgrEventSink armed early cookie={c} (will retry key sink on SetFocus)");
                        }
                    }
                }
                return Ok(());
            }
        };
        let ctx = match unsafe { dm.GetTop() } {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[prisir_tsf] Activate: GetTop() failed ({e}) — no top ctx, skipping");
                alog!("activate_inner: GetTop FAIL {e} -> no ctx (key sink NOT installed)");
                return Ok(());
            }
        };
        inner.context.lock().unwrap().replace(ctx.clone());

        // 3. KeyEventSink 已在 activate 开头(步骤 1a)无条件武装,此处不再重复。

        // 4. 在 ITfThreadMgr 上装 ITfThreadMgrEventSink — 监听窗口切换导致 context 变化
        let cookie_res2 = unsafe {
            let tim_source: ITfSource = ptim.cast()?;
            let me_as_unk2: IUnknown = self.cast()?;
            tim_source.AdviseSink(&IID_ITF_THREAD_MGR_EVENT_SINK, &me_as_unk2)
        };
        match cookie_res2 {
            Ok(c) => {
                *inner.threadmgr_sink_cookie.lock().unwrap() = c;
                alog!("activate_inner: ThreadMgrEventSink armed cookie={c}");
            }
            Err(e) => {
                eprintln!("[prisir_tsf] AdviseSink(ITfThreadMgrEventSink) failed: {e}");
                alog!("activate_inner: ThreadMgrEventSink Advise FAIL {e}");
            }
        }

        // 5. 同步 conversion compartment(框架模式语义,非可视化)。
        //    19045 沉浸式指示器不据此为第三方 TIP 显示中/英(见 conversion_mode.rs 头);
        //    此处写入只为让应用/系统查 conversion mode 时读到对的值。
        {
            let tid = *inner.client_tid.lock().unwrap();
            let chinese = *inner.is_chinese_mode.lock().unwrap();
            crate::conversion_mode::set_mode(ptim, tid, chinese);
        }

        // 6. 悬浮工具条(自绘,对齐搜狗/灵犀)— 19045 系统指示器不渲染第三方 TIP 中/英。
        //    按钮: 中/英 + 中/英标点 + 手写/符号/词库入口。仅 Prisir 激活时显示。
        //    bind_toggles 幂等(OnceLock);show 按当前 mode+punct 显示并置顶。
        {
            crate::status_bar::bind_toggles(
                inner.is_chinese_mode.clone(),
                inner.pending_mode_change.clone(),
                inner.is_chinese_punct.clone(),
            );
            let chinese = *inner.is_chinese_mode.lock().unwrap();
            let punct = *inner.is_chinese_punct.lock().unwrap();
            crate::status_bar::show(&crate::status_bar::global_bar_state(), chinese, punct);
        }

        // 7. 符号/emoji 面板上屏通道(点选符号经此走 edit_session 上屏)。
        //    bind_commit 幂等(OnceLock),存 context/tid/composition 全局句柄。
        {
            crate::panels::bind_commit(
                inner.context.clone(),
                inner.client_tid.clone(),
                inner.composition.clone(),
                inner.composition_range.clone(),
            );
        }

        // 8. 切换输入法即后台预建引擎(2026-09-02):不等首次按键才建,用户切到 Prisir
        //    打字时引擎多半已就绪,首次按键就有候选,避免「首次切换卡 + 候选窗跳变」。
        //    engine_handle_async 非阻塞、幂等(CAS 只 spawn 一次),此处调用只触发预建。
        let _ = engine_handle_async();

        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────
// ITfTextInputProcessorEx 接口实现 — ActivateEx(T25:显式实现,堵住 macro zero-out stub)
// ──────────────────────────────────────────────────────────────────────

impl ITfTextInputProcessorEx_Impl for TsfInputProcessor_Impl {
    /// TSF 在 Vista+ 优先调 ActivateEx(带 dwFlags)。内部转调同一 activate_inner,
    /// 否则 implement 宏会生成 zero-out stub 盲写调用方 out-ptr → explorer c0000005。
    fn ActivateEx(&self, ptim: Option<&ITfThreadMgr>, tid: u32, _dwflags: u32) -> Result<()> {
        eprintln!("[prisir_tsf] ActivateEx called (tid={tid}), delegating to activate_inner");
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("TsfInputProcessor::ActivateEx:ENTER");
        let r = self.activate_inner(ptim, tid);
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(if r.is_ok() {
            "TsfInputProcessor::ActivateEx:OK"
        } else {
            "TsfInputProcessor::ActivateEx:FAIL"
        });
        r
    }
}

// ──────────────────────────────────────────────────────────────────────
// ITfSource 接口实现 — AdviseSink / UnadviseSink
// ──────────────────────────────────────────────────────────────────────

impl ITfSource_Impl for TsfInputProcessor_Impl {
    /// 客户端(ITfThreadMgr / ITfContext)注册事件 sink。
    /// TSF 标准做法: 客户端传 IID + IUnknown,sink 自己 cast。我们直接接收并保存即可。
    fn AdviseSink(&self, _riid: *const GUID, _punk: Option<&IUnknown>) -> Result<u32> {
        // v0.8 简化: 不区分 riid, 统一返非零 cookie 表示订阅成功。
        // 真正的区分在 windows-rs 0.58 内部 QueryInterface 阶段已经处理(它会从我们
        // 多接口 implement 中挑出匹配的 QueryInterface)。
        Ok(1)
    }

    fn UnadviseSink(&self, _dwcookie: u32) -> Result<()> {
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────
// ITfKeyEventSink 接口实现 — OnSetFocus / OnTestKeyDown / OnTestKeyUp /
//                          OnKeyDown / OnKeyUp / OnPreservedKey
// ──────────────────────────────────────────────────────────────────────

impl ITfKeyEventSink_Impl for TsfInputProcessor_Impl {
    /// TSF 通知 IME focus 状态变化。前台 = true,后台 = false。
    fn OnSetFocus(&self, fforeground: BOOL) -> Result<()> {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!("KeyEventSink::OnSetFocus foreground={}", fforeground.as_bool()));
        // 后台(切走 IME / 失焦)→ 结束 composition + 清 buffer + 隐藏候选窗。
        // 2026-09-01 两根因:
        //  (a) 切 IME 时 Deactivate 不触发,候选窗靠这里兜底隐藏;
        //  (b) 切走时未上屏的拼音字母残留 buffer,切回来后 buffer 非空 → 退格误删拼音
        //      而不删文档。切走时清 buffer + EndComposition,残留拼音随 composition 丢弃。
        if !fforeground.as_bool() {
            let inner = &self.this;
            crate::candidate_window::hide(&crate::candidate_window::global_state());
            // 切走/失焦 → 隐藏悬浮中/英状态条(仅 Prisir 激活时显示)。
            crate::status_bar::hide(&crate::status_bar::global_bar_state());
            // 切走/失焦 → 隐藏符号/emoji 面板。
            crate::panels::hide(&crate::panels::global_panel_state());
            let had_composition = inner.composition.lock().unwrap().is_some();
            if had_composition {
                let tid = *inner.client_tid.lock().unwrap();
                let ctx_opt = inner.context.lock().unwrap().clone();
                if let Some(ctx) = ctx_opt {
                    let st = crate::edit_session::EditSessionState {
                        context: inner.context.clone(),
                        composition: inner.composition.clone(),
                        composition_range: inner.composition_range.clone(),
                    };
                    // 2026-09-02 改 EndComposition→CancelComposition:失焦丢弃未上屏拼音,
                    // 不把 zi 提交进文档(用户:切走后 zi 留在记事本是 bug,原本该没东西)。
                    let _ = crate::edit_session::run_edit_session(
                        tid, &ctx, st,
                        crate::edit_session::DocOp::CancelComposition,
                    );
                }
            }
            inner.state.lock().unwrap().pinyin.reset();
        }
        Ok(())
    }

    /// TSF 询问"这个键你吃吗?"——比 OnKeyDown 先调。返 true 表示吃。
    /// **必须与 OnKeyDown 一致**:退格/空格/Esc/数字仅在打字(buffer/候选非空)才吃,
    /// 否则 OnTest 报吃 + OnKeyDown 放行 → TSF 矛盾丢键(2026-09-01 实测按键全无效)。
    fn OnTestKeyDown(
        &self,
        _pic: Option<&ITfContext>,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<BOOL> {
        let vk = wparam.0 as u16;
        // Ctrl/Alt 按下时:字母/数字/标点一律放行(组合键 Ctrl+C/V/Z、Alt+F4 等归 app),
        // 不进入拼音。这是「Ctrl 组合键失效」的修复(2026-09-02)。必须 OnTest/OnKeyDown 一致。
        if ctrl_is_down() || alt_is_down() {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!("OnTestKeyDown: vk=0x{:02X} PASS(ctrl/alt down)", vk));
            return Ok(BOOL(0));
        }
        let (buf_empty, cands_empty, has_next, has_prev, typing) = {
            let state = self.this.state.lock().unwrap();
            (
                state.pinyin.buf.is_empty(),
                state.pinyin.candidates.is_empty(),
                state.pinyin.has_next_page(),
                state.pinyin.has_prev_page(),
                !state.pinyin.candidates.is_empty(),
            )
        };
        let want = match vk {
            // 翻页键:打字(有候选)时恒吃 —— OnKeyDown 里翻得动就翻页、翻不动就放行符号,
            // 两条路都「吃」所以 OnTest 必须报吃,否则 TSF 矛盾丢键(2026-09-02 边界 bug)。
            0x22 | 0xBB | 0x21 | 0xBD | 0xBC | 0xBE if typing => true,
            _ => TsfInputProcessor::wants_key_state_full(vk, buf_empty, cands_empty, has_next, has_prev),
        };
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!(
            "OnTestKeyDown: vk=0x{:02X} want={} buf_empty={} cands_empty={}", vk, want, buf_empty, cands_empty
        ));
        Ok(BOOL::from(want))
    }

    fn OnTestKeyUp(
        &self,
        _pic: Option<&ITfContext>,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<BOOL> {
        Ok(BOOL(0))
    }

    /// 真按键入口 — TSF 把按键给我们。返 true = 吃, false = 让 TSF 默认处理。
    ///
    /// T20 设计:
    ///   1. VK_SHIFT 走特殊路径:OnKeyDown 设 shift_held=true,OnKeyUp 清掉。
    ///      (吃 Shift 意味着用户不能用 Shift+Tab 之类系统快捷键,v0.8 暂时接受这限制)
    ///   2. 检查 pending_mode_change:LangBar OnClick 翻了模式 → 把旧 composition End 掉 + 清 buffer
    ///   3. 中文模式(默认 + Shift 松开):a-z 累积到 PinyinBuffer,显示在 composition range;
    ///      数字/空格选候选 → commit + EndComposition
    ///   4. 临时英文模式(Shift 按下 或 模式被翻成英文):字母直接 commit(InsertTextAtSelection)
    ///   5. 其它(wants_key false 的键):放行
    fn OnKeyDown(
        &self,
        pic: Option<&ITfContext>,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<BOOL> {
        let vk = wparam.0 as u16;
        let inner = &self.this;

        // 2026-09-02 回删卡顿端到端定位:OnKeyDown 入口时间戳。
        #[cfg(feature = "dllentry_log")]
        let _kd_start = std::time::Instant::now();
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!("OnKeyDown: vk=0x{:02X}", vk));

        // ───── 0. Shift 按下:只吃掉,状态由 shift_is_down() 实时查询 ─────
        if vk == 0x10 {
            eprintln!("[prisir_tsf] Shift DOWN");
            return Ok(BOOL::from(true)); // 吃
        }

        // ───── 0a. CapsLock 按下:翻转 中/英 模式,吃 ─────
        // 用户诉求(2026-09-01):按 Caps 切英文大写。翻转 is_chinese_mode,
        // 后续字母走 treat_as_english 分支(commit 大写)。不吃的话会触发系统 CapsLock 灯
        // 且字母仍走拼音 → 混乱。吃掉由我们独占模式切换。
        if vk == 0x14 {
            let new_mode = {
                let mut mode = inner.is_chinese_mode.lock().unwrap();
                *mode = !*mode;
                *mode
            };
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!("CapsLock: is_chinese_mode={}", new_mode));
            // 同步任务栏指示器「中/英」compartment。
            let ptim = inner.tim.lock().unwrap().clone();
            if let Some(ptim) = ptim {
                let tid = *inner.client_tid.lock().unwrap();
                crate::conversion_mode::set_mode(&ptim, tid, new_mode);
            }
            // 搜狗联动:切中→中文标点,切英→英文标点(用户仍可按 。/. 按钮单独覆盖)。
            { *inner.is_chinese_punct.lock().unwrap() = new_mode; }
            // 同步悬浮状态条显示(mode + 联动后的 punct)。
            crate::status_bar::refresh(&crate::status_bar::global_bar_state(), new_mode, new_mode);
            return Ok(BOOL::from(true)); // 吃
        }

        // ───── 0b. LangBar 翻了模式 → EndComposition + 清 buffer ─────
        // 注(2026-09-01): 此处的 EndComposition 理论上也该走 edit session,但 LangBar
        // AddItem 当前失败(activate_inner 日志 0x80004005),pending_mode_change 永不会被
        // 置 true,此路径实际不可达。待 LangBar 修好后需改为 run_edit_session。
        if *inner.pending_mode_change.lock().unwrap() {
            *inner.pending_mode_change.lock().unwrap() = false;
            if let Some(comp) = inner.composition.lock().unwrap().as_ref() {
                let _ = unsafe { comp.EndComposition(TF_DEFAULT_SELECTION) };
            }
            inner.composition.lock().unwrap().take();
            inner.composition_range.lock().unwrap().take();
            inner.state.lock().unwrap().pinyin.reset();
            eprintln!("[prisir_tsf] pending_mode_change applied: composition ended, buffer cleared");
            // 不 return — 继续处理这个键
        }

        // ───── 0a'. Ctrl/Alt 按下:组合键放行给 app(与 OnTestKeyDown 一致),不进拼音 ─────
        // 必须放在字母/标点/特殊键处理之前,否则 Ctrl+C 的 C 会被当拼音累积。
        if ctrl_is_down() || alt_is_down() {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!("OnKeyDown: vk=0x{:02X} PASS(ctrl/alt down)", vk));
            return Ok(BOOL(0));
        }

        // ───── 1. 决定当前是英文还是中文模式 ─────
        let mode = *inner.is_chinese_mode.lock().unwrap();
        let shift_held = shift_is_down(); // 实时查询,不用跟踪状态(防卡死)
        let treat_as_english = !mode || shift_held;

        // ───── 2. 字母 a-z 处理 ─────
        let ch_lower_opt: Option<char> = {
            let n = (vk as u32) | 0x20;
            char::from_u32(n).filter(|c| c.is_ascii_lowercase())
        };
        if let Some(ch) = ch_lower_opt {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!("OnKeyDown letter '{}' mode={} shift_held={} treat_en={}", ch, mode, shift_held, treat_as_english));
            if treat_as_english {
                // 英文模式:默认小写,按住 Shift 才大写(对齐常规输入法/搜狗)。
                // 之前 CapsLock 切英文时无条件大写,违背「默认小写」直觉(2026-09-01 用户报)。
                let s = if shift_held {
                    ch.to_ascii_uppercase().to_string()
                } else {
                    ch.to_string()
                };
                TsfInputProcessor::commit_text(inner, &s);
                return Ok(BOOL::from(true)); // 吃,不让 TSF 再 forward
            }
            // 中文模式:累积到 PinyinBuffer + 重查候选(纯状态机,不碰 document)
            // composition 只放纯拼音 buffer;候选列表走独立悬浮窗(不写进文档)。
            let display: String;
            let cand_words: Vec<String>;
            {
                let mut state = inner.state.lock().unwrap();
                // 引擎冷启动保护(2026-09-02):引擎没建好(query 必返空)时,中文模式下
                // 字母键直接吃掉,不进 buffer、不写 composition。否则字母经 UpdateComposition
                // 写进文档、候选却空 → 用户见「切换后打字上字母,等引擎建好才正常」。
                // 引擎建好(后台线程,VM 上 322MB idx 反序列化要几十秒)前打字会被吞,
                // 好过把字母写进文档后再也撤不回。
                let h = state.engine_handle();
                if h.is_null() {
                    #[cfg(feature = "dllentry_log")]
                    crate::com_class_factory::log_dll_entry(&format!(
                        "OnKeyDown letter '{}' eaten: engine not ready", ch));
                    return Ok(BOOL::from(true));
                }
                state.pinyin.on_char(ch);
                #[cfg(feature = "dllentry_log")]
                let t0 = std::time::Instant::now();
                let mut cands = state.pinyin.query_candidates(h);
                #[cfg(feature = "dllentry_log")]
                let t_query = t0.elapsed();
                // smart_sentence(2026-09-01 接入): 多音节拼音先跑整句,命中则把整句
                // 提为候选#1(空格/数字1 直接上屏整句),逐词候选顺移为其后。整句未命中
                // 或权重不高于逐词首候选时保持原列表,避免把词级首选顶掉。
                #[cfg(feature = "dllentry_log")]
                let t1 = std::time::Instant::now();
                let sent = state.pinyin.smart_sentence(h);
                #[cfg(feature = "dllentry_log")]
                {
                    let t_smart = t1.elapsed();
                    crate::com_class_factory::log_dll_entry(&format!(
                        "OnKeyDown timing: query={:?} smart={:?} buf='{}'", t_query, t_smart, state.pinyin.buf));
                }
                if let Some(sent) = sent {
                    let top_w = cands.first().map(|c| c.weight).unwrap_or(0);
                    let sent_w = top_w.saturating_add(1);
                    if !cands.iter().any(|c| c.word == sent) {
                        cands.insert(0, crate::keystroke::Candidate::new(sent, sent_w));
                    }
                }
                state.pinyin.set_candidates(cands);
                display = state.pinyin.buf.clone();
                cand_words = state.pinyin.page_slice().iter().map(|c| c.word.clone()).collect();
                #[cfg(feature = "dllentry_log")]
                crate::com_class_factory::log_dll_entry(&format!("OnKeyDown: buf='{}' cands={}", state.pinyin.buf, state.pinyin.candidates.len()));
            }

            // 用 edit session 建/刷 composition(TSF 规定 document 修改必须在 DoEditSession 里)。
            let ctx_opt = TsfInputProcessor::get_active_ctx(pic, inner);
            if let Some(ctx) = ctx_opt {
                let tid = *inner.client_tid.lock().unwrap();
                let st = crate::edit_session::EditSessionState {
                    context: inner.context.clone(),
                    composition: inner.composition.clone(),
                    composition_range: inner.composition_range.clone(),
                };
                let r = crate::edit_session::run_edit_session(
                    tid, &ctx, st,
                    crate::edit_session::DocOp::UpdateComposition { display },
                );
                #[cfg(feature = "dllentry_log")]
                crate::com_class_factory::log_dll_entry(&format!("OnKeyDown: edit_session UpdateComposition ok={}", r.is_ok()));
                // 候选悬浮窗定位到光标下方并刷新内容。
                TsfInputProcessor::refresh_candidate_window(tid, &ctx, inner, &cand_words);
            } else {
                #[cfg(feature = "dllentry_log")]
                crate::com_class_factory::log_dll_entry("OnKeyDown: no active ctx for composition");
            }
            return Ok(BOOL::from(true)); // 吃
        }

        // ───── 2b. OEM 标点键:中文模式下做中/英标点映射,英文模式放行 ─────
        // wants_key_state 已对 OEM 键返 true(中文吃);此处按 mode 决定 commit 还是放行。
        if matches!(vk, 0xBA | 0xBB | 0xBC | 0xBD | 0xBE | 0xBF | 0xC0 | 0xDB | 0xDC | 0xDD | 0xDE) {
            if treat_as_english {
                return Ok(BOOL(0)); // 英文模式:放行,系统出原生 ASCII 标点
            }
            let shift = shift_is_down(); // 实时查询
            // 翻页键边界修复(2026-09-02):+-=,. 在「正在打字(有候选)」时是翻页键,
            // 当前页翻不动时**必须放行**(让符号直接上屏),不能落到下面的标点映射 —
            // 否则首页往前翻按出 -、末页往后翻按出 =。仅当 buffer 空(没在打字)才走标点。
            let is_paging_key = matches!(vk, 0xBB | 0xBD | 0xBC | 0xBE);
            if !shift {
                let paged = {
                    let mut state = inner.state.lock().unwrap();
                    match vk {
                        0xBB | 0xBE => state.pinyin.page_down(),
                        0xBD | 0xBC => state.pinyin.page_up(),
                        _ => false,
                    }
                };
                if paged {
                    let (cand_words, cur_page, total_pages) = {
                        let state = inner.state.lock().unwrap();
                        (
                            state.pinyin.page_slice().iter().map(|c| c.word.clone()).collect::<Vec<_>>(),
                            state.pinyin.page,
                            (state.pinyin.candidates.len() + crate::keystroke::CAND_PER_PAGE - 1) / crate::keystroke::CAND_PER_PAGE,
                        )
                    };
                    #[cfg(feature = "dllentry_log")]
                    crate::com_class_factory::log_dll_entry(&format!(
                        "PageTurn: vk=0x{:02X} -> page {}/{} cands={:?}", vk, cur_page, total_pages, cand_words));
                    let tid = *inner.client_tid.lock().unwrap();
                    if let Some(ctx) = TsfInputProcessor::get_active_ctx(pic, inner) {
                        TsfInputProcessor::refresh_candidate_window(tid, &ctx, inner, &cand_words);
                    }
                    return Ok(BOOL::from(true)); // 吃
                }
                // 没翻动:正在打字(有候选)的翻页键 → **吃掉不动作**(对齐搜狗/微软拼音:
                // 末页再按 + 无效但不上屏符号、不丢键)。
                // 2026-09-02 根因:此处原 return BOOL(0) 放行,但 OnTest(662 行)对打字中的
                // 翻页键恒报「吃」→ TSF 认为键被 IME 接管,OnKeyDown 放行 = 键被丢弃。用户
                // 末页狂按 + 23 次全无反应(以为没翻到底)。改吃掉:OnTest 吃 + OnKeyDown 吃,
                // 前后一致,键不丢、符号也不上屏(打字中 +- 本就只为翻页)。
                let has_cands = !inner.state.lock().unwrap().pinyin.candidates.is_empty();
                if is_paging_key && has_cands {
                    return Ok(BOOL::from(true)); // 吃掉不动作
                }
            }
            let chinese_punct = *inner.is_chinese_punct.lock().unwrap();
            if let Some(text) = TsfInputProcessor::map_punct(vk, shift, chinese_punct) {
                TsfInputProcessor::commit_text(inner, text);
                #[cfg(feature = "dllentry_log")]
                crate::com_class_factory::log_dll_entry(&format!("OnKeyDown: punct vk=0x{:02X} -> '{}' (cn_punct={})", vk, text, chinese_punct));
                return Ok(BOOL::from(true)); // 吃
            }
            return Ok(BOOL(0)); // 无映射 → 放行
        }

        // ───── 3. 数字 / 退格 / 空格 / Esc / 回车(中文模式才吃) ─────
        // wants_key 包含 0x08 / 0x20 / 0x1B / 0x0D / 0x30..=0x39
        if TsfInputProcessor::wants_key(vk) {
            if treat_as_english {
                // 英文模式空格/退格/Esc 不做特殊处理 — 透传给 PinyinBuffer 也无意义。
                // 直接放行让 TSF forward 给 app。
                return Ok(BOOL(0));
            }
            let result = TsfInputProcessor::handle_special(vk, inner);
            let ctx_opt = TsfInputProcessor::get_active_ctx(pic, inner);
            let tid = *inner.client_tid.lock().unwrap();
            match result {
                SpecialResult::Passthrough => {
                    // buffer 空 — 与拼音无关,放行给 app(退格删文档 / 空格 / Esc / 数字)。
                    #[cfg(feature = "dllentry_log")]
                    crate::com_class_factory::log_dll_entry(&format!("OnKeyDown: vk=0x{:02X} passthrough (empty buf)", vk));
                    return Ok(BOOL(0));
                }
                SpecialResult::Commit(text) => {
                    // 上屏:edit session 里 InsertTextAtSelection + EndComposition
                    if let Some(ctx) = ctx_opt {
                        let st = crate::edit_session::EditSessionState {
                            context: inner.context.clone(),
                            composition: inner.composition.clone(),
                            composition_range: inner.composition_range.clone(),
                        };
                        let r = crate::edit_session::run_edit_session(
                            tid, &ctx, st,
                            crate::edit_session::DocOp::Commit { text: text.clone() },
                        );
                        #[cfg(feature = "dllentry_log")]
                        crate::com_class_factory::log_dll_entry(&format!("OnKeyDown: edit_session Commit '{}' ok={}", text, r.is_ok()));
                    }
                    // 提交后清空拼音缓冲 + 隐藏候选窗
                    inner.state.lock().unwrap().pinyin.reset();
                    crate::candidate_window::hide(&crate::candidate_window::global_state());
                }
                SpecialResult::Repage => {
                    // 翻页:候选页变了,buffer/composition 不动,只重刷候选窗。
                    let cand_words = {
                        let state = inner.state.lock().unwrap();
                        state.pinyin.page_slice().iter().map(|c| c.word.clone()).collect::<Vec<_>>()
                    };
                    if let Some(ctx) = ctx_opt {
                        TsfInputProcessor::refresh_candidate_window(tid, &ctx, inner, &cand_words);
                    }
                }
                SpecialResult::Handled => {
                    // 退格删字母 / Esc 清空:buffer 变了。空 buffer → EndComposition + 隐藏候选窗;
                    // 非空 → 重刷 composition(只放纯 buffer)+ 刷新候选窗。
                    let (display, cand_words, buf_empty) = {
                        let state = inner.state.lock().unwrap();
                        (
                            state.pinyin.buf.clone(),
                            state.pinyin.page_slice().iter().map(|c| c.word.clone()).collect::<Vec<_>>(),
                            state.pinyin.buf.is_empty(),
                        )
                    };
                    if let Some(ctx) = ctx_opt {
                        let st = crate::edit_session::EditSessionState {
                            context: inner.context.clone(),
                            composition: inner.composition.clone(),
                            composition_range: inner.composition_range.clone(),
                        };
                        if vk == 0x1B || buf_empty {
                            // Esc 取消 / 退格删空:都需**清空 composition 文本再结束**。
                            // 2026-09-01 bug:退格删空走 EndComposition(不清文本),最后一个字母
                            // 残留在 range 里被 finalize 进文档,需多按一次退格。CancelComposition 先清文本。
                            // 2026-09-02 回删删空失败根因:失败不再静默吞。RequestEditSession 拿不到
                            // 写锁时 session 不执行,composition 残留 → 用户以为 z 删不掉。记日志暴露。
                            let r = crate::edit_session::run_edit_session(
                                tid, &ctx, st,
                                crate::edit_session::DocOp::CancelComposition,
                            );
                            #[cfg(feature = "dllentry_log")]
                            if r.is_err() {
                                crate::com_class_factory::log_dll_entry(
                                    "OnKeyDown: CancelComposition FAILED (composition NOT cleared!)");
                            }
                            crate::candidate_window::hide(&crate::candidate_window::global_state());
                        } else {
                            let _ = crate::edit_session::run_edit_session(
                                tid, &ctx, st,
                                crate::edit_session::DocOp::UpdateComposition { display },
                            );
                            TsfInputProcessor::refresh_candidate_window(tid, &ctx, inner, &cand_words);
                        }
                    }
                }
            }
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!(
                "OnKeyDown: vk=0x{:02X} TOTAL took={:.1}ms", vk, _kd_start.elapsed().as_secs_f64() * 1000.0));
            return Ok(BOOL::from(true)); // 吃(Handled/Commit 才到这)
        }

        // ───── 4. 其它键(系统键 / F1-F12 / 方向键 ...)放行 ─────
        Ok(BOOL(0))
    }

    fn OnKeyUp(
        &self,
        _pic: Option<&ITfContext>,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<BOOL> {
        let vk = wparam.0 as u16;
        if vk == 0x10 {
            // Shift 状态改实时查询(shift_is_down),这里无需清跟踪状态。
            eprintln!("[prisir_tsf] Shift UP");
            return Ok(BOOL::from(true)); // 吃,跟 OnKeyDown 对称
        }
        Ok(BOOL(0))
    }

    /// 保留键(用户定义的 IME 快捷键) — v0.8 暂不实现。
    fn OnPreservedKey(
        &self,
        _pic: Option<&ITfContext>,
        _rguid: *const GUID,
    ) -> Result<BOOL> {
        Ok(BOOL(0))
    }
}

// ──────────────────────────────────────────────────────────────────────
// ITfThreadMgrEventSink — 监听 context 切换
// ──────────────────────────────────────────────────────────────────────

impl ITfThreadMgrEventSink_Impl for TsfInputProcessor_Impl {
    fn OnInitDocumentMgr(&self, _pdim: Option<&ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }
    fn OnUninitDocumentMgr(&self, _pdim: Option<&ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }

    /// 焦点切换 — 必须更新 self.context,否则 OnKeyDown 用的句柄失效。
    fn OnSetFocus(
        &self,
        pdimfocus: Option<&ITfDocumentMgr>,
        _pdimprevfocus: Option<&ITfDocumentMgr>,
    ) -> Result<()> {
        let inner = &self.this;
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!("ThreadMgrEventSink::OnSetFocus has_dm={}", pdimfocus.is_some()));
        // 2026-09-02 修「失焦候选窗残留」:同 IME 内切应用焦点(不切输入法)走本回调,
        // 不走 KeyEventSink::OnSetFocus。pdimfocus=None(焦点离开本进程文档)时必须
        // 隐藏候选窗/状态条/面板 + 结束 composition + 清 buffer,否则候选窗残留显示
        // 旧页(真机:记事本打 zi 翻到第9页,切到 cmd/别的应用,候选窗仍挂「甾茈觜」)。
        if pdimfocus.is_none() {
            crate::candidate_window::hide(&crate::candidate_window::global_state());
            crate::status_bar::hide(&crate::status_bar::global_bar_state());
            crate::panels::hide(&crate::panels::global_panel_state());
            // 结束未上屏的 composition(zi 等),残留拼音随 composition 丢弃。
            // 2026-09-02 用 CancelComposition(先清空 range 再 End)而非 EndComposition(提交),
            // 否则 zi 会被 finalize 进记事本(用户:切走后 zi 应消失,不该留下)。
            if inner.composition.lock().unwrap().is_some() {
                let tid = *inner.client_tid.lock().unwrap();
                let ctx_opt = inner.context.lock().unwrap().clone();
                if let Some(ctx) = ctx_opt {
                    let st = crate::edit_session::EditSessionState {
                        context: inner.context.clone(),
                        composition: inner.composition.clone(),
                        composition_range: inner.composition_range.clone(),
                    };
                    let _ = crate::edit_session::run_edit_session(
                        tid, &ctx, st,
                        crate::edit_session::DocOp::CancelComposition,
                    );
                }
            }
            inner.state.lock().unwrap().pinyin.reset();
            return Ok(());
        }
        let Some(dm) = pdimfocus else {
            return Ok(());
        };
        if let Ok(ctx) = unsafe { dm.GetTop() } {
            // 只更新 context 句柄(OnKeyDown 拿 composition 用)。
            // **不在此重装 KeyEventSink**: key sink 是 threadmgr 级,activate_inner 已装在
            // ptim 上,threadmgr 不变无需重订。此处再装到 ctx 上会返 0x80040202(见 activate_inner 注释)。
            inner.context.lock().unwrap().replace(ctx);
        }
        // 2026-09-03 修「点桌面悬浮栏消失后回不来」:pdimfocus=Some(焦点回到本进程文档)时
        // 重建悬浮状态条。失焦分支(none)里 status_bar::hide 是销毁窗口(2026-09-01 起),
        // 若此处不重建,点回记事本悬浮栏永远消失(旧版 hide 只 SW_HIDE 不销毁所以没事)。
        // bind_toggles 幂等(OnceLock);show 内部拿不到 bar 所有权会跳过,安全。
        {
            crate::status_bar::bind_toggles(
                inner.is_chinese_mode.clone(),
                inner.pending_mode_change.clone(),
                inner.is_chinese_punct.clone(),
            );
            let chinese = *inner.is_chinese_mode.lock().unwrap();
            let punct = *inner.is_chinese_punct.lock().unwrap();
            crate::status_bar::show(&crate::status_bar::global_bar_state(), chinese, punct);
        }
        // 切焦点时清空拼音缓冲(否则可能把旧窗口的拼音串写到新窗口)
        inner.state.lock().unwrap().pinyin.reset();
        Ok(())
    }

    fn OnPushContext(&self, _pic: Option<&ITfContext>) -> Result<()> {
        Ok(())
    }
    fn OnPopContext(&self, _pic: Option<&ITfContext>) -> Result<()> {
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────
// ITfLangBarItemSink — TSF 叫我们通知 LangBar item 状态变了
// ──────────────────────────────────────────────────────────────────────

impl ITfLangBarItemSink_Impl for TsfInputProcessor_Impl {
    /// TSF 通过 LangBarItemMgr 通知我们:LangBar 状态变了,要刷新显示。
    /// v0.8 简化:不主动推更新,反正 LangBar 自己会定期轮询 GetText/GetStatus。
    fn OnUpdate(&self, _dwflags: u32) -> Result<()> {
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────
// ITfCompositionSink — composition 被外部终止(比如 focus 切换)的回调
// ──────────────────────────────────────────────────────────────────────

impl ITfCompositionSink_Impl for TsfInputProcessor_Impl {
    /// composition 被终结时回调(v0.8 仅清本地的 composition 句柄,不做更多)。
    fn OnCompositionTerminated(
        &self,
        _ecwrite: u32,
        _pcomposition: Option<&ITfComposition>,
    ) -> Result<()> {
        let inner = &self.this;
        inner.composition.lock().unwrap().take();
        inner.composition_range.lock().unwrap().take();
        inner.state.lock().unwrap().pinyin.reset();
        eprintln!("[prisir_tsf] OnCompositionTerminated: cleared local composition handles");
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────
// TsfInputProcessor 业务方法 — 不属于 COM 接口,只供 _Impl 内部调用
// ──────────────────────────────────────────────────────────────────────

impl TsfInputProcessor {
    /// 哪些键是我们要拦截的(VK 集合)。
    /// 字母 / 数字 / 退格 / 空格 / Esc 都吃,其它键(Win/Ctrl/Alt/方向键/F1-F12...)放行。
    /// **注: Shift (0x10) 不在 wants_key 里 — 我们单独在 OnKeyDown 入口捕获它**,
    ///     走"吃"路径(让 OnKeyUp 也被调)而不是"透传"。
    /// 决定是否吃某个键。**必须与 OnKeyDown 实际处理完全一致**——
    /// 否则 OnTestKeyDown 报 want=true 但 OnKeyDown 放行,TSF 前后矛盾把键丢弃
    /// (2026-09-01 实测:退格 buffer 空时 OnTest 报吃、OnKeyDown 放行 → 按键全无效)。
    ///
    /// 规则:
    ///   - 字母 a-z / Shift / CapsLock:总是吃(打字 / 临时英文 / 模式切换)。
    ///   - 退格 / 空格 / Esc / 数字:仅当**在打字**(buffer 非空 或 有候选)才吃;
    ///     buffer 空时放行,让退格删文档、空格/数字/Esc 归 app。
    pub(crate) fn wants_key_state(vk: u16, buf_empty: bool, cands_empty: bool) -> bool {
        Self::wants_key_state_full(vk, buf_empty, cands_empty, false, false)
    }

    /// 完整版:额外传 has_next/has_prev 决定翻页键吃不吃(翻页仅在有目标页时吃,
    /// 与 handle_special 的 page_down/page_up 判定严格一致,避免 OnTest/OnKeyDown 矛盾)。
    pub(crate) fn wants_key_state_full(
        vk: u16,
        buf_empty: bool,
        cands_empty: bool,
        has_next: bool,
        has_prev: bool,
    ) -> bool {
        match vk {
            0x41..=0x5A => true,           // A-Z 总是吃
            0x10 => true,                  // Shift
            0x14 => true,                  // CapsLock
            0x08 | 0x20 | 0x1B | 0x0D | 0x30..=0x39 => !(buf_empty && cands_empty), // 仅在打字才吃
            // 翻页键:PgDn(0x22)/+(0xBB) 仅当有下一页,PgUp(0x21)/-(0xBD) 仅当有上一页。
            0x22 | 0xBB => has_next,
            0x21 | 0xBD => has_prev,
            // OEM 标点键:中文模式下要吃(做中/英标点映射)。OnKeyDown 内部再按 mode 放行。
            // 注意:0xBC(,)/0xBE(.)在有候选时也作翻页(外挂式习惯),此处在 wants 层恒吃,
            // OnKeyDown 的标点分支里再根据有无候选决定翻页还是上屏标点。
            0xBA | 0xBB | 0xBC | 0xBD | 0xBE | 0xBF | 0xC0 | 0xDB | 0xDC | 0xDD | 0xDE => true,
            _ => false,
        }
    }

    /// OEM 标点键 → (中文标点, 英文标点)。shift 影响部分键(如 9→(, /→?)。
    /// 返回 None = 该键不做标点映射(放行)。
    pub(crate) fn map_punct(vk: u16, shift: bool, chinese_punct: bool) -> Option<&'static str> {
        // (vk, shift) → 中文 / 英文 符号
        let zh: &str = match (vk, shift) {
            (0xBC, false) => ",",   // ,  → ,
            (0xBC, true) => "<",    // <  → 《(简化用单书名号一半,常见输入法用《)
            (0xBE, false) => "。",  // .  → 。
            (0xBE, true) => ">",    // >  → 》
            (0xBF, false) => "/",   // /  → /
            (0xBF, true) => "?",    // ?  → ?
            (0xBA, false) => ";",   // ;  → ;
            (0xBA, true) => ":",    // :  → :
            (0xDE, false) => "'",   // '  → '(直引号,避免成对复杂)
            (0xDE, true) => "\"",   // "  → "
            (0xDB, false) => "[",   // [  → 【(简化用 [)
            (0xDD, false) => "]",   // ]  → 】
            (0xDC, false) => "、",  // \  → 、
            (0xC0, false) => "`",   // `  → ·
            (0xC0, true) => "~",    // ~  → ~
            (0xBD, false) => "-",   // -  → -
            (0xBD, true) => "—",    // _  → ——(破折号,取单支)
            (0xBB, false) => "=",   // =  → =
            (0xBB, true) => "+",    // +  → +
            _ => return None,
        };
        let en: &str = match (vk, shift) {
            (0xBC, false) => ",", (0xBC, true) => "<",
            (0xBE, false) => ".", (0xBE, true) => ">",
            (0xBF, false) => "/", (0xBF, true) => "?",
            (0xBA, false) => ";", (0xBA, true) => ":",
            (0xDE, false) => "'", (0xDE, true) => "\"",
            (0xDB, false) => "[", (0xDD, false) => "]",
            (0xDC, false) => "\\",
            (0xC0, false) => "`", (0xC0, true) => "~",
            (0xBD, false) => "-", (0xBD, true) => "_",
            (0xBB, false) => "=", (0xBB, true) => "+",
            _ => return None,
        };
        Some(if chinese_punct { zh } else { en })
    }

    /// 旧的无状态版本(仅按 VK 类型判断),保留给无需状态的调用点。
    /// 0x0D(回车,微软拼音式上屏字母)必须在此:OnTestKeyDown 走 wants_key_state_full
    /// 已含 0x0D,OnKeyDown 走本函数 — 两边不同步会导致「测试吃、按键丢」。
    pub(crate) fn wants_key(vk: u16) -> bool {
        matches!(
            vk,
            0x08 | 0x20 | 0x1B | 0x0D | 0x14 | 0x30..=0x39 | 0x41..=0x5A
        )
    }

    /// OnKeyDown 拿 ITfContext 的优先级:OnKeyDown 的 `pic` 参数(TSF 给的当前 ctx)
    /// → fallback 到 self.context。
    pub(crate) fn get_active_ctx(
        pic: Option<&ITfContext>,
        inner: &TsfInputProcessor,
    ) -> Option<ITfContext> {
        pic.cloned().or_else(|| inner.context.lock().unwrap().clone())
    }

    /// 处理非字母的"特殊键"——数字键选候选 / 空格选首位候选 / 退格删字符 / Esc 清空。
    /// 返回 Commit 上屏 / Handled 刷新了 buffer / Passthrough 放行给 app。
    pub(crate) fn handle_special(vk: u16, inner: &TsfInputProcessor) -> SpecialResult {
        let mut state = inner.state.lock().unwrap();
        match vk {
            0x0D => {
                // 回车(2026-09-04 微软拼音式):有拼音缓冲 → 把已输入字母原样上屏(不选候选),
                // 方便偶尔输入几个字母;无缓冲 → 放行给 app(正常回车换行)。
                if state.pinyin.buf.is_empty() {
                    SpecialResult::Passthrough
                } else {
                    let raw = state.pinyin.buf.clone();
                    state.pinyin.on_escape(); // 清缓冲+候选,不学习
                    SpecialResult::Commit(raw)
                }
            }
            0x08 => {
                // 退格:buffer 非空 → 删字母吃键;buffer 空 → 放行给记事本删文档。
                if state.pinyin.on_backspace() {
                    let h = state.engine_handle();
                    #[cfg(feature = "dllentry_log")]
                    let t0 = std::time::Instant::now();
                    let mut cands = state.pinyin.query_candidates(h);
                    // 退格重查同样融合 smart_sentence 整句首选(与字母键路径一致)。
                    let sent = state.pinyin.smart_sentence(h);
                    #[cfg(feature = "dllentry_log")]
                    crate::com_class_factory::log_dll_entry(&format!(
                        "Backspace: buf='{}' query={:?} cands={}", state.pinyin.buf, t0.elapsed(), cands.len()));
                    if let Some(sent) = sent {
                        let top_w = cands.first().map(|c| c.weight).unwrap_or(0);
                        if !cands.iter().any(|c| c.word == sent) {
                            cands.insert(0, crate::keystroke::Candidate::new(sent, top_w.saturating_add(1)));
                        }
                    }
                    state.pinyin.set_candidates(cands);
                    SpecialResult::Handled
                } else {
                    SpecialResult::Passthrough
                }
            }
            0x20 => {
                // 空格:buffer 空(没在打字)→ 放行上屏空格;有 buffer → 选候选。
                if state.pinyin.buf.is_empty() {
                    SpecialResult::Passthrough
                } else {
                    let h = state.engine_handle();
                    let buf_snap = state.pinyin.buf.clone(); // on_space 会 reset,先快照 buf 供 learn
                    match state.pinyin.on_space() {
                        Some(t) => {
                            // 学习:把选中词与本次拼音关联提频(2026-09-02 修「频率没效果」)。
                            if let (Ok(inp), Ok(sel)) = (
                                std::ffi::CString::new(buf_snap),
                                std::ffi::CString::new(t.clone()),
                            ) {
                                if !h.is_null() {
                                    crate::ffi::prisir_tsf_learn(h, inp.as_ptr(), sel.as_ptr());
                                }
                            }
                            SpecialResult::Commit(t)
                        }
                        None => SpecialResult::Passthrough,
                    }
                }
            }
            0x1B => {
                // Esc:有 buffer → 清空取消;无 buffer → 放行给 app。
                if state.pinyin.buf.is_empty() {
                    SpecialResult::Passthrough
                } else {
                    state.pinyin.on_escape();
                    SpecialResult::Handled
                }
            }
            0x30..=0x39 => {
                // 数字 1-9:有候选 → 选;无 → 放行(让数字上屏)。
                if state.pinyin.buf.is_empty() {
                    SpecialResult::Passthrough
                } else {
                    let h = state.engine_handle();
                    let buf_snap = state.pinyin.buf.clone(); // on_digit 会 reset,先快照 buf 供 learn
                    // 修「翻页选错字」(2026-09-02):用候选窗**实际显示**的页选词,而非 self.page。
                    // 候选窗显示哪页,数字键就选哪页 —— 所见即所选。候选窗还没刷新过(无快照)
                    // 时退化为 self.page(当前页)。
                    let cand_page = {
                        let cs = crate::candidate_window::global_state();
                        let g = cs.lock().unwrap();
                        let s = g.borrow();
                        if !s.candidates.is_empty() { s.page } else { state.pinyin.page }
                    };
                    match state.pinyin.on_digit_at_page((vk - 0x30) as u8, cand_page) {
                        Some(t) => {
                            if let (Ok(inp), Ok(sel)) = (
                                std::ffi::CString::new(buf_snap),
                                std::ffi::CString::new(t.clone()),
                            ) {
                                if !h.is_null() {
                                    crate::ffi::prisir_tsf_learn(h, inp.as_ptr(), sel.as_ptr());
                                }
                            }
                            SpecialResult::Commit(t)
                        }
                        None => SpecialResult::Passthrough,
                    }
                }
            }
            // 翻页键(2026-09-02 迁移自外挂式):PageDown(0x22)/+ 下一页,PageUp(0x21)/- 上一页。
            // 仅在真有目标页时吃,否则放行(让 +/- 上屏符号、PgUp/PgDn 归 app 滚动)。
            0x22 | 0xBB => {
                if state.pinyin.page_down() { SpecialResult::Repage } else { SpecialResult::Passthrough }
            }
            0x21 | 0xBD => {
                if state.pinyin.page_up() { SpecialResult::Repage } else { SpecialResult::Passthrough }
            }
            _ => SpecialResult::Passthrough,
        }
    }

    /// 刷新候选悬浮窗:查询光标屏幕坐标 → 定位到光标下方 → 更新候选内容。
    /// 坐标查询失败(无 composition / view 拿不到)→ 退化为当前鼠标位置。
    pub(crate) fn refresh_candidate_window(
        tid: u32,
        ctx: &ITfContext,
        inner: &TsfInputProcessor,
        _page_words: &[String], // 旧签名保留兼容,实际用全量候选
    ) {
        let state = crate::candidate_window::global_state();
        // 横排单行(2026-09-03):传全量候选 + row_offset(可视区首的全量索引)。
        let (all_words, page, row_offset) = {
            let st = inner.state.lock().unwrap();
            (
                st.pinyin.candidates.iter().map(|c| c.word.clone()).collect::<Vec<_>>(),
                st.pinyin.page,
                st.pinyin.row_offset,
            )
        };
        if all_words.is_empty() {
            crate::candidate_window::hide(&state);
            return;
        }
        let pos = crate::edit_session::query_caret_screen_pos(tid, ctx, inner.composition_range.clone())
            .or_else(|| {
                let mut p = windows::Win32::Foundation::POINT::default();
                unsafe { let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p); }
                Some((p.x, p.y + 20))
            });
        let Some((x, y)) = pos else { return };
        let items: Vec<crate::candidate_window::CandItem> = all_words
            .iter()
            .map(|w| crate::candidate_window::CandItem { word: w.clone() })
            .collect();
        let buf = { inner.state.lock().unwrap().pinyin.buf.clone() };
        crate::candidate_window::update(&state, &buf, items, row_offset, 0, x, y, page);
    }

    /// 调 ITfInsertAtSelection::InsertTextAtSelection 把 text 写到当前 context。
    /// UTF-16 编码由 `&[u16]` 承载(windows-rs 0.58 签名)。
    pub(crate) fn commit_text(inner: &TsfInputProcessor, text: &str) {
        let ctx = {
            let g = inner.context.lock().unwrap();
            g.clone()
        };
        let Some(ctx) = ctx else {
            eprintln!("[prisir_tsf] commit_text: no context");
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry("commit_text: no context");
            return;
        };
        let tid = *inner.client_tid.lock().unwrap();
        let st = crate::edit_session::EditSessionState {
            context: inner.context.clone(),
            composition: inner.composition.clone(),
            composition_range: inner.composition_range.clone(),
        };
        let r = crate::edit_session::run_edit_session(
            tid, &ctx, st,
            crate::edit_session::DocOp::Commit { text: text.to_string() },
        );
        match r {
            Ok(()) => {
                eprintln!("[prisir_tsf] committed: {text}");
                #[cfg(feature = "dllentry_log")]
                crate::com_class_factory::log_dll_entry(&format!("commit_text: '{}' OK", text));
                inner.state.lock().unwrap().pinyin.reset();
            }
            Err(e) => {
                eprintln!("[prisir_tsf] commit_text edit session failed: {e}");
                #[cfg(feature = "dllentry_log")]
                crate::com_class_factory::log_dll_entry(&format!("commit_text: '{}' FAIL {e}", text));
            }
        }
    }
}