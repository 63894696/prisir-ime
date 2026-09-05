//! 手写画板面板(2026-09-04,Step 4)。
//!
//! **定位**:状态条「手」按钮 / 候选窗「✎手写」点开。对齐安卓 LingxiIME 手写板 UX
//! (用户实测安卓上屏逻辑体验更好,Win 端结合 Win 特性优化):
//!   - **抬笔停顿自动识别**(同安卓):一笔落下后 600ms 没再落笔 → 自动把整组笔画
//!     发给 prisir_hw 子进程识别,候选实时刷新。不用点「识别」按钮(外挂式 app_debug
//!     要手动点识别 = 打断书写节奏,安卓版停顿即识别更顺)。
//!   - **点候选上屏 + 自动清板**(同安卓 renderHwCandidates):点中候选字 → SendInput
//!     上屏到前台输入焦点 → 自动清空画板,可连续书写下一个字。
//!   - **撤销/清空/⌫**:撤销=删最后一笔并重识别(安卓 undo 同款);⌫=向前台发
//!     一个 Backspace;清空=清掉全部笔画与候选。
//!
//! **识别通道(独立进程,与 IME DLL 解耦)**:画板捕获的笔画坐标 → stdio JSON-RPC
//!   发给 prisir_hw.exe 子进程(ochw ort 推理,28MB 模型不背进 IME DLL)→ 收 top-N
//!   候选。子进程懒加载常驻,首笔画 spawn,后续复用管道。
//!
//! **上屏通道**:SendInput(KEYEVENTF_UNICODE),同 panels.rs emoji 上屏 —— 面板跑在
//!   状态条 owner 进程(常是 explorer),拿不到前台 app 的 TSF context,SendInput
//!   直达前台输入焦点最通用,不依赖跨进程 TSF。
//!
//! **窗口形态**:WS_POPUP + NOACTIVATE + TOPMOST + TOOLWINDOW 自绘(同 panels.rs),
//!   不抢前台焦点(书写时焦点留在目标 app,上屏才发键盘)。销毁走 WM_CLOSE。

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Variant::InitVariantFromInt32Array;
use windows::Win32::UI::TabletPC::{
    IInkDisp, IInkRecognitionResult, IInkRecognizerContext, InkDisp as CLSID_InkDisp,
    InkRecognizerContext as CLSID_InkRecognizerContext, InkRecognitionStatus, IRS_NoError,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SendInput, SetCapture, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY,
};

const HW_CLASS: &str = "PrisirHwPanel";

/// 诊断 trace(走 dllentry_log 通道,与 panels/status_bar 一致)。
macro_rules! hlog {
    ($($arg:tt)*) => {{
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!($($arg)*));
    }};
}

fn wcs(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// 苹果风配色(BGR 0x00BBGGRR),与 panels/userdict_window 统一。
const COL_BG: u32 = 0x00FAF9F7;      // 暖白底
const COL_PAD: u32 = 0x00FFFFFF;     // 画板纯白
const COL_INK: u32 = 0x002B2620;     // 笔画墨色(近黑暖灰)
const COL_SEP: u32 = 0x00ECE9E4;     // 分隔浅灰
const COL_ACCENT: u32 = 0x00A9750A;  // 苹果蓝(候选悬停/按钮)
const COL_TEXT: u32 = 0x005A544D;    // 暖灰正文
const COL_HINT: u32 = 0x00B9B2A8;    // 提示淡灰(空画板提示)
const COL_GRID: u32 = 0x00F0EDE8;    // 画板米字格浅线

// 布局常量。
const MARGIN: i32 = 10;
const PAD_TOP: i32 = MARGIN;             // 画板区顶部
const PAD_H: i32 = 230;                  // 画板区高(对齐安卓 230dp)
const CAND_H: i32 = 46;                  // 候选条高
const BTN_BAR_H: i32 = 40;               // 底部按钮行高
const STROKE_W: i32 = 5;                 // 画板上笔画显示宽(比识别栅格粗,好看)
const MAX_CAND: usize = 8;               // 候选条最多显示
const RECOG_DELAY_MS: u32 = 600;         // 抬笔停顿自动识别延时(同安卓)

// 子控件/定时器 ID。
const TIMER_RECOG: usize = 1;

/// 一个笔画 = 一串客户区像素点。
type Stroke = Vec<(f32, f32)>;

struct HwPanelState {
    hwnd: Option<HWND>,
    visible: bool,
    x: i32,
    y: i32,
    strokes: Vec<Stroke>,        // 已完成的笔画
    cur: Stroke,                 // 正在画的笔画
    drawing: bool,
    candidates: Vec<String>,     // 最近一次识别的 top-N
    busy: bool,                  // 一次识别进行中(防重入)
}

impl HwPanelState {
    fn new() -> Self {
        Self {
            hwnd: None, visible: false, x: 200, y: 200,
            strokes: Vec::new(), cur: Vec::new(), drawing: false,
            candidates: Vec::new(), busy: false,
        }
    }
}

static HW_STATE: OnceLock<Arc<Mutex<RefCell<HwPanelState>>>> = OnceLock::new();
unsafe impl Send for HwPanelState {}
unsafe impl Sync for HwPanelState {}

fn global_hw_state() -> &'static Arc<Mutex<RefCell<HwPanelState>>> {
    HW_STATE.get_or_init(|| Arc::new(Mutex::new(RefCell::new(HwPanelState::new()))))
}

// ── Windows Ink 识别(2026-09-04 路线A,替代 ochw 模型)─────────────────────────
// ochw MobileNetV2 对「一」「个」等简单字识别弱(安卓真机实测同样差,非移植 bug)。
// 用户拍板换 Windows 自带 Ink 识别(msinkaut,与外挂灵犀 app_debug.py 同源)。
// 优势:系统级识别引擎,简体中文字库全,无需 28MB 模型 / prisir_hw 子进程 / 黑窗。
// 通道:CoCreateInstance(InkDisp) → CreateStroke(每笔画,SAFEARRAY<i32> x,y 交错)
//   → InkRecognizerContext.putref_Strokes → Recognize → AlternatesFromSelection 取 top-N。

/// COM 初始化(一次性,STA)。InkRecognizerContext 是 STA COM 组件,需先 CoInitializeEx。
/// 面板跑在 UI 线程(可能已是 STA/未初始化),重复初始化无害(返 S_FALSE)。
fn ensure_com_init() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    });
}

/// 用 Windows Ink 识别一组笔画,返 top-N 候选字符串。失败/无识别器返空。
/// 笔画坐标 = 画板客户区像素(f32),这里转 i32 直接喂给 Ink(Windows 内部按
/// 设备坐标识别,像素量级足够,无需 HIMETRIC 转换 — 与外挂灵犀直接传像素一致)。
fn recognize_ink(strokes: &[Stroke], limit: usize) -> Vec<String> {
    ensure_com_init();
    unsafe {
        // 1) InkDisp 收集笔画。
        let ink: IInkDisp = match CoCreateInstance(&CLSID_InkDisp, None, CLSCTX_ALL) {
            Ok(i) => i,
            Err(e) => { hlog!("HwInk: CoCreate InkDisp fail: {e}"); return Vec::new(); }
        };
        let ink_strokes = match ink.Strokes() {
            Ok(s) => s,
            Err(e) => { hlog!("HwInk: ink.Strokes fail: {e}"); return Vec::new(); }
        };
        // 2) 每笔画 → CreateStroke。packetdata = SAFEARRAY<i32> [x0,y0,x1,y1,...]。
        //    packetdescription 传空 VARIANT(VT_EMPTY)→ 默认按 X/Y 坐标识别。
        let empty_desc = VARIANT::default();
        for st in strokes {
            if st.is_empty() { continue; }
            // 单点笔画补成两点(起=终),Ink 拒绝零长度笔画。
            let mut pts: Vec<i32> = Vec::with_capacity(st.len().max(2) * 2);
            for p in st {
                pts.push(p.0.round() as i32);
                pts.push(p.1.round() as i32);
            }
            if st.len() == 1 {
                pts.push(st[0].0.round() as i32);
                pts.push(st[0].1.round() as i32);
            }
            let pd = match InitVariantFromInt32Array(&pts) {
                Ok(v) => v,
                Err(e) => { hlog!("HwInk: InitVariantFromInt32Array fail: {e}"); continue; }
            };
            match ink.CreateStroke(&pd, &empty_desc) {
                Ok(stroke) => {
                    if let Err(e) = ink_strokes.Add(&stroke) {
                        hlog!("HwInk: strokes.Add fail: {e}");
                    }
                }
                Err(e) => hlog!("HwInk: CreateStroke fail: {e}"),
            }
        }
        // 3) RecognizerContext 识别。
        let ctx: IInkRecognizerContext = match CoCreateInstance(&CLSID_InkRecognizerContext, None, CLSCTX_ALL) {
            Ok(c) => c,
            Err(e) => { hlog!("HwInk: CoCreate RecognizerContext fail: {e}"); return Vec::new(); }
        };
        if let Err(e) = ctx.putref_Strokes(&ink_strokes) {
            hlog!("HwInk: putref_Strokes fail: {e}"); return Vec::new();
        }
        let mut status = InkRecognitionStatus(0);
        let mut result: Option<IInkRecognitionResult> = None;
        if let Err(e) = ctx.Recognize(&mut status, &mut result) {
            hlog!("HwInk: Recognize fail: {e}"); return Vec::new();
        }
        if status != IRS_NoError {
            hlog!("HwInk: Recognize status={:?}(非 NoError)", status);
        }
        let Some(res) = result else { hlog!("HwInk: Recognize 无结果"); return Vec::new(); };
        // 4) 取 top-N。TopString 长度决定 selection 范围(对齐外挂灵犀)。
        let top = res.TopString().unwrap_or_default().to_string();
        let sel_len = top.chars().count() as i32;
        if sel_len == 0 { return Vec::new(); }
        let alts = match res.AlternatesFromSelection(0, sel_len, limit.max(1) as i32) {
            Ok(a) => a,
            Err(e) => { hlog!("HwInk: AlternatesFromSelection fail: {e}"); return Vec::new(); }
        };
        let count = alts.Count().unwrap_or(0);
        let mut out = Vec::new();
        for i in 0..count.min(limit.max(1) as i32) {
            if let Ok(alt) = alts.Item(i) {
                if let Ok(s) = alt.String() {
                    let s = s.to_string();
                    if !s.is_empty() { out.push(s); }
                }
            }
        }
        hlog!("HwInk: recognized top='{}' alts={} -> {:?}", top, count, out);
        out
    }
}

// ── prisir_hw 子进程管道(懒加载常驻,留作 ochw 回退通道) ─────────────────────
struct HwProc {
    child: Child,
    stdin: ChildStdin,
    // 子进程 stdout 读端:BufReader 按行读 JSON 响应。
    reader: BufReader<std::process::ChildStdout>,
}
static HW_PROC: OnceLock<Mutex<Option<HwProc>>> = OnceLock::new();
// SAFETY: 单 STA 线程访问,Mutex 兜底。
unsafe impl Send for HwProc {}

fn hw_proc() -> &'static Mutex<Option<HwProc>> {
    HW_PROC.get_or_init(|| Mutex::new(None))
}

/// 确保子进程起来。返 false = 启动失败(模型/exe 缺失)。
fn ensure_hw_proc() -> bool {
    let mut guard = hw_proc().lock().unwrap();
    if let Some(p) = guard.as_mut() {
        // 子进程还活着?(try_wait Ok(None)=运行中)。
        match p.child.try_wait() {
            Ok(None) => return true,
            _ => { *guard = None; } // 死了,重启
        }
    }
    // 定位 prisir_hw.exe:exe 同目录 → C:\PrisirIME\。
    let exe = std::env::current_exe().ok()
        .and_then(|e| e.parent().map(|d| d.join("prisir_hw.exe")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\PrisirIME\prisir_hw.exe"));
    if !exe.exists() {
        hlog!("HwPanel: prisir_hw.exe not found at {}", exe.display());
        return false;
    }
    let mut cmd = Command::new(&exe);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    // 不弹 cmd 黑窗:GUI 进程(explorer)spawn console 子进程默认会带一个可见
    // 控制台窗口(用户实测「打开手写跳 prisir_hw.exe 黑窗」)。CREATE_NO_WINDOW
    // (0x08000000) 让子进程无窗口运行,stdio 仍走管道。
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    // 让子进程在 exe 同目录找 models\(asset_dir 第一优先 exe 同目录\models)。
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    match cmd.spawn() {
        Ok(mut child) => {
            let stdin = match child.stdin.take() {
                Some(s) => s,
                None => { hlog!("HwPanel: no stdin pipe"); return false; }
            };
            let stdout = match child.stdout.take() {
                Some(s) => s,
                None => { hlog!("HwPanel: no stdout pipe"); return false; }
            };
            hlog!("HwPanel: spawned prisir_hw pid={}", child.id());
            *guard = Some(HwProc { child, stdin, reader: BufReader::new(stdout) });
            true
        }
        Err(e) => {
            hlog!("HwPanel: spawn prisir_hw fail: {e}");
            false
        }
    }
}

/// ochw 子进程识别通道(回退用,2026-09-04 路线A 后默认不再走这里)。
/// 组 JSON 发给 prisir_hw.exe stdio,同步读回 top-N。
fn recognize_ochw(strokes: &[Stroke]) -> Vec<String> {
    // 组 JSON: {"strokes":[[[x,y],...],...],"limit":N}。
    let mut js = String::from("{\"strokes\":[");
    for (i, st) in strokes.iter().enumerate() {
        if i > 0 { js.push(','); }
        js.push('[');
        for (j, p) in st.iter().enumerate() {
            if j > 0 { js.push(','); }
            js.push_str(&format!("[{},{}]", p.0 as i32, p.1 as i32));
        }
        js.push(']');
    }
    js.push_str(&format!("],\"limit\":{}}}", MAX_CAND));

    // 同步 stdio 往返。
    let mut result: Vec<String> = Vec::new();
    if ensure_hw_proc() {
        let mut guard = hw_proc().lock().unwrap();
        if let Some(p) = guard.as_mut() {
            let write_ok = writeln!(p.stdin, "{}", js).and_then(|_| p.stdin.flush()).is_ok();
            if write_ok {
                let mut line = String::new();
                match p.reader.read_line(&mut line) {
                    Ok(_) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                            if let Some(arr) = v.get("candidates").and_then(|c| c.as_array()) {
                                for c in arr {
                                    if let Some(s) = c.as_str() {
                                        result.push(s.to_string());
                                    }
                                }
                            } else if let Some(e) = v.get("error") {
                                hlog!("HwPanel: hw error: {}", e);
                            }
                        }
                    }
                    Err(e) => hlog!("HwPanel: read resp fail: {e}"),
                }
            } else {
                hlog!("HwPanel: write req fail");
                *guard = None; // 管道断了,下次重 spawn
            }
        }
    }
    result
}

/// 把当前笔画组识别,回填 candidates。同步识别(600ms 停顿后触发,用户感知不到阻塞;
/// 且单 STA 线程,异步反而复杂)。
///
/// 识别通道(2026-09-04 路线A):默认 Windows Ink(recognize_ink,系统自带简体识别,
/// 对「一」「个」等简单字远好于 ochw)。环境变量 `PRISIR_HW_ENGINE=ochw` 切回旧
/// ochw 子进程通道(prisir_hw.exe)作回退 —— 失败可回滚,不删旧通道。
fn recognize_now(hwnd: HWND) {
    // 取笔画快照 + 标 busy(防 600ms 内重入)。
    let strokes: Vec<Stroke> = {
        let g = global_hw_state();
        let guard = g.lock().unwrap();
        let mut s = guard.borrow_mut();
        if s.busy {
            return;
        }
        if s.strokes.is_empty() {
            s.candidates.clear();
            drop(s);
            drop(guard);
            unsafe { let _ = InvalidateRect(hwnd, None, true); }
            return;
        }
        s.busy = true;
        s.strokes.clone()
    };
    hlog!("HwPanel: recognize strokes={}", strokes.len());

    let use_ochw = std::env::var("PRISIR_HW_ENGINE").map(|v| v == "ochw").unwrap_or(false);
    let result: Vec<String> = if !use_ochw {
        // 默认:Windows Ink(系统识别引擎)。
        recognize_ink(&strokes, MAX_CAND)
    } else {
        // 回退:ochw 子进程(prisir_hw.exe stdio JSON-RPC)。
        recognize_ochw(&strokes)
    };

    // 回填候选 + 清 busy + 重绘候选条。
    {
        let g = global_hw_state();
        let guard = g.lock().unwrap();
        let mut s = guard.borrow_mut();
        s.candidates = result;
        s.busy = false;
    }
    unsafe { let _ = InvalidateRect(hwnd, None, true); }
}

// ── 上屏 / 回退(SendInput,同 panels.rs) ─────────────────────────────────────
fn send_text(text: &str) {
    let mut inputs: Vec<INPUT> = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0), wScan: unit, dwFlags: flags,
                        time: 0, dwExtraInfo: 0,
                    },
                },
            });
        }
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    hlog!("HwPanel: commit '{}' SendInput {}/{}", text, sent, inputs.len());
}

fn send_backspace() {
    let inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0x08), wScan: 0, dwFlags: KEYBD_EVENT_FLAGS(0),
                    time: 0, dwExtraInfo: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0x08), wScan: 0, dwFlags: KEYEVENTF_KEYUP,
                    time: 0, dwExtraInfo: 0,
                },
            },
        },
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    hlog!("HwPanel: backspace SendInput {}/2", sent);
}

// ── 几何:面板分区 ────────────────────────────────────────────────────────────
fn panel_width() -> i32 { MARGIN * 2 + 260 }  // 画板宽 260(近方形,贴合手写字)
fn panel_height() -> i32 { PAD_TOP + PAD_H + 4 + CAND_H + 4 + BTN_BAR_H + MARGIN }

fn pad_rect() -> RECT {
    RECT { left: MARGIN, top: PAD_TOP, right: MARGIN + 260, bottom: PAD_TOP + PAD_H }
}
fn cand_rect() -> RECT {
    let top = PAD_TOP + PAD_H + 4;
    RECT { left: MARGIN, top, right: MARGIN + 260, bottom: top + CAND_H }
}
fn btn_rect(i: i32) -> RECT {
    // 底部 4 按钮:撤销/清空/⌫/关闭,均分。
    let top = PAD_TOP + PAD_H + 4 + CAND_H + 4;
    let bw = 260 / 4;
    RECT { left: MARGIN + i * bw, top, right: MARGIN + (i + 1) * bw, bottom: top + BTN_BAR_H }
}

// ── 窗口类注册 / 创建 ───────────────────────────────────────────────────────
fn register_class() -> Result<()> {
    let hinst = unsafe { GetModuleHandleW(None) }?;
    let class_name = wcs(HW_CLASS);
    let mut existing = WNDCLASSEXW::default();
    if unsafe { GetClassInfoExW(hinst, PCWSTR(class_name.as_ptr()), &mut existing) }.is_ok() {
        return Ok(());
    }
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(hw_wnd_proc),
        hInstance: hinst.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }?,
        hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return Err(Error::from_win32());
    }
    Ok(())
}

fn ensure_window(state: &Arc<Mutex<RefCell<HwPanelState>>>) -> Result<HWND> {
    let existing = state.lock().unwrap().borrow().hwnd;
    if let Some(hwnd) = existing {
        if unsafe { IsWindow(hwnd) }.as_bool() {
            return Ok(hwnd);
        }
        state.lock().unwrap().borrow_mut().hwnd = None;
    }
    register_class()?;
    let hinst = unsafe { GetModuleHandleW(None) }?;
    let class_name = wcs(HW_CLASS);
    let title = wcs("PrisirHw");
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()), PCWSTR(title.as_ptr()),
            WS_POPUP, 0, 0, 10, 10, None, None, hinst, None,
        )
    }?;
    state.lock().unwrap().borrow_mut().hwnd = Some(hwnd);
    Ok(hwnd)
}

/// 状态条「手」/候选窗「✎手写」入口:已显示=关掉,否则锚在状态条下方弹开。
pub(crate) fn toggle_panel(anchor_hwnd: Option<HWND>) {
    let state = global_hw_state();
    let cur_visible = { state.lock().unwrap().borrow().visible };
    if cur_visible {
        hlog!("HwPanel: toggle OFF");
        hide(&state);
        return;
    }
    hlog!("HwPanel: toggle ON");
    let (ax, ay) = if let Some(bar) = anchor_hwnd {
        let mut rc = RECT::default();
        unsafe { let _ = GetWindowRect(bar, &mut rc); }
        (rc.left, rc.bottom + 4)
    } else {
        let g = state.lock().unwrap();
        let s = g.borrow();
        (s.x, s.y)
    };
    let w = panel_width();
    let h = panel_height();
    let (x, y) = (ax.max(0), ay.max(0));
    {
        let g = state.lock().unwrap();
        let mut s = g.borrow_mut();
        s.x = x;
        s.y = y;
    }
    if let Ok(hwnd) = ensure_window(&state) {
        unsafe {
            let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let _ = InvalidateRect(hwnd, None, true);
        }
        state.lock().unwrap().borrow_mut().visible = true;
        hlog!("HwPanel: show at {},{}", x, y);
    }
}

pub(crate) fn hide(state: &Arc<Mutex<RefCell<HwPanelState>>>) {
    let hwnd = state.lock().unwrap().borrow_mut().hwnd.take();
    if let Some(hwnd) = hwnd {
        unsafe {
            let _ = KillTimer(hwnd, TIMER_RECOG);
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = SendMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    // 重置书写态(笔画/候选),下次开是干净画板。
    {
        let g = state.lock().unwrap();
        let mut s = g.borrow_mut();
        s.strokes.clear();
        s.cur.clear();
        s.candidates.clear();
        s.drawing = false;
        s.busy = false;
    }
    state.lock().unwrap().borrow_mut().visible = false;
}

// ── 绘制 ────────────────────────────────────────────────────────────────────
unsafe fn make_font(hdc: HDC, size: i32, face: &str, bold: bool) -> Option<(HFONT, HGDIOBJ)> {
    let face_w = wcs(face);
    let f = CreateFontW(
        -size, 0, 0, 0,
        if bold { FW_BOLD.0 as i32 } else { FW_NORMAL.0 as i32 },
        0, 0, 0, DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32,
        DEFAULT_PITCH.0 as u32, PCWSTR(face_w.as_ptr()),
    );
    if !f.is_invalid() {
        let old = SelectObject(hdc, f);
        Some((f, old))
    } else { None }
}

fn paint_panel(hwnd: HWND) {
    let (strokes, cur, candidates) = {
        let g = global_hw_state();
        let guard = g.lock().unwrap();
        let s = guard.borrow();
        (s.strokes.clone(), s.cur.clone(), s.candidates.clone())
    };
    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let bg = CreateSolidBrush(COLORREF(COL_BG));
        FillRect(hdc, &rc, bg);
        let _ = DeleteObject(bg);
        SetBkMode(hdc, TRANSPARENT);

        // ── 画板区:白底 + 米字格 + 笔画 ──
        let pr = pad_rect();
        let pad_bg = CreateSolidBrush(COLORREF(COL_PAD));
        let mut prc = pr;
        FillRect(hdc, &mut prc, pad_bg);
        let _ = DeleteObject(pad_bg);
        // 边框。
        let border = CreatePen(PS_SOLID, 1, COLORREF(COL_SEP));
        let ob = SelectObject(hdc, border);
        let _ = Rectangle(hdc, pr.left, pr.top, pr.right, pr.bottom);
        SelectObject(hdc, ob);
        let _ = DeleteObject(border);
        // 米字格(虚线十字 + 对角),辅助居中。
        let grid = CreatePen(PS_DOT, 1, COLORREF(COL_GRID));
        let og = SelectObject(hdc, grid);
        let cx = (pr.left + pr.right) / 2;
        let cy = (pr.top + pr.bottom) / 2;
        let _ = MoveToEx(hdc, cx, pr.top, None);
        let _ = LineTo(hdc, cx, pr.bottom);
        let _ = MoveToEx(hdc, pr.left, cy, None);
        let _ = LineTo(hdc, pr.right, cy);
        let _ = MoveToEx(hdc, pr.left, pr.top, None);
        let _ = LineTo(hdc, pr.right, pr.bottom);
        let _ = MoveToEx(hdc, pr.right, pr.top, None);
        let _ = LineTo(hdc, pr.left, pr.bottom);
        SelectObject(hdc, og);
        let _ = DeleteObject(grid);

        // 空画板提示。
        if strokes.is_empty() && cur.is_empty() {
            let mut hrc = pr;
            hrc.top += PAD_H / 2 - 14;
            SetTextColor(hdc, COLORREF(COL_HINT));
            if let Some((f, old)) = make_font(hdc, 16, "Microsoft YaHei", false) {
                let mut t = wcs("在此手写");
                DrawTextW(hdc, &mut t, &mut hrc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                SelectObject(hdc, old);
                let _ = DeleteObject(f);
            }
        }

        // 笔画(已完成 + 正在画),墨色实线。
        let ink = CreatePen(PS_SOLID, STROKE_W, COLORREF(COL_INK));
        let oi = SelectObject(hdc, ink);
        let mut draw_stroke = |st: &Stroke| {
            if st.len() >= 2 {
                let _ = MoveToEx(hdc, st[0].0 as i32 + pr.left, st[0].1 as i32 + pr.top, None);
                for p in st.iter().skip(1) {
                    let _ = LineTo(hdc, p.0 as i32 + pr.left, p.1 as i32 + pr.top);
                }
            } else if st.len() == 1 {
                // 单点画小圆点。
                let x = st[0].0 as i32 + pr.left;
                let y = st[0].1 as i32 + pr.top;
                let _ = MoveToEx(hdc, x, y, None);
                let _ = LineTo(hdc, x + 1, y + 1);
            }
        };
        for st in &strokes {
            draw_stroke(st);
        }
        draw_stroke(&cur);
        SelectObject(hdc, oi);
        let _ = DeleteObject(ink);

        // ── 候选条:横排 top-N,分隔线,点中上屏 ──
        let cr = cand_rect();
        let cand_bg = CreateSolidBrush(COLORREF(COL_PAD));
        let mut crc = cr;
        FillRect(hdc, &mut crc, cand_bg);
        let _ = DeleteObject(cand_bg);
        let cborder = CreatePen(PS_SOLID, 1, COLORREF(COL_SEP));
        let oc = SelectObject(hdc, cborder);
        let _ = Rectangle(hdc, cr.left, cr.top, cr.right, cr.bottom);
        SelectObject(hdc, oc);
        let _ = DeleteObject(cborder);
        if !candidates.is_empty() {
            let n = candidates.len().min(MAX_CAND) as i32;
            let cw = (cr.right - cr.left) / n;
            if let Some((f, old)) = make_font(hdc, 24, "Microsoft YaHei", false) {
                for (i, c) in candidates.iter().take(MAX_CAND).enumerate() {
                    let mut cell = RECT {
                        left: cr.left + i as i32 * cw, top: cr.top,
                        right: cr.left + (i as i32 + 1) * cw, bottom: cr.bottom,
                    };
                    SetTextColor(hdc, COLORREF(if i == 0 { COL_ACCENT } else { COL_TEXT }));
                    let mut t = wcs(c);
                    DrawTextW(hdc, &mut t, &mut cell, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                    // 候选间分隔线。
                    if i > 0 {
                        let sep = CreatePen(PS_SOLID, 1, COLORREF(COL_SEP));
                        let os = SelectObject(hdc, sep);
                        let _ = MoveToEx(hdc, cell.left, cr.top + 8, None);
                        let _ = LineTo(hdc, cell.left, cr.bottom - 8);
                        SelectObject(hdc, os);
                        let _ = DeleteObject(sep);
                    }
                }
                SelectObject(hdc, old);
                let _ = DeleteObject(f);
            }
        } else {
            // 无候选提示。
            SetTextColor(hdc, COLORREF(COL_HINT));
            if let Some((f, old)) = make_font(hdc, 14, "Microsoft YaHei", false) {
                let mut t = wcs("候选区");
                let mut crc2 = cr;
                DrawTextW(hdc, &mut t, &mut crc2, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                SelectObject(hdc, old);
                let _ = DeleteObject(f);
            }
        }

        // ── 底部按钮:撤销 / 清空 / ⌫ / 关闭 ──
        let labels = ["撤销", "清空", "⌫", "关闭"];
        for (i, lb) in labels.iter().enumerate() {
            let br = btn_rect(i as i32);
            let bbg = CreateSolidBrush(COLORREF(COL_SEP));
            let mut brc = br;
            FillRect(hdc, &mut brc, bbg);
            let _ = DeleteObject(bbg);
            SetTextColor(hdc, COLORREF(COL_TEXT));
            if let Some((f, old)) = make_font(hdc, 14, "Microsoft YaHei", false) {
                let mut t = wcs(lb);
                let mut brc2 = br;
                DrawTextW(hdc, &mut t, &mut brc2, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                SelectObject(hdc, old);
                let _ = DeleteObject(f);
            }
        }

        let _ = EndPaint(hwnd, &ps);
    }
}

// ── 命中测试 ────────────────────────────────────────────────────────────────
fn in_rect(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}
/// 候选命中 → 返索引。
fn hit_candidate(x: i32, y: i32, n: usize) -> Option<usize> {
    let cr = cand_rect();
    if !in_rect(&cr, x, y) || n == 0 {
        return None;
    }
    let cw = (cr.right - cr.left) / n as i32;
    let idx = ((x - cr.left) / cw) as usize;
    if idx < n { Some(idx) } else { None }
}
/// 按钮命中 → 返 0=撤销 1=清空 2=⌫ 3=关闭。
fn hit_button(x: i32, y: i32) -> Option<i32> {
    for i in 0..4 {
        if in_rect(&btn_rect(i), x, y) {
            return Some(i);
        }
    }
    None
}

// ── 窗口过程 ────────────────────────────────────────────────────────────────
unsafe extern "system" fn hw_wnd_proc(
    hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => { paint_panel(hwnd); LRESULT(0) }
        WM_CLOSE => { let _ = DestroyWindow(hwnd); LRESULT(0) }
        WM_NCHITTEST => LRESULT(HTCLIENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER => {
            if wparam.0 == TIMER_RECOG {
                let _ = KillTimer(hwnd, TIMER_RECOG);
                recognize_now(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            // 按钮优先(在画板外底部)。
            if let Some(b) = hit_button(x, y) {
                match b {
                    0 => {
                        // 撤销:删最后一笔并重识别。
                        let has = {
                            let g = global_hw_state();
                            let __g = g.lock().unwrap(); let mut s = __g.borrow_mut();
                            s.strokes.pop().is_some()
                        };
                        if has {
                            let _ = InvalidateRect(hwnd, None, true);
                            // 立即重识别剩余笔画(无则清候选)。
                            let empty = {
                                let g = global_hw_state();
                                g.lock().unwrap().borrow().strokes.is_empty()
                            };
                            if empty {
                                let g = global_hw_state();
                                g.lock().unwrap().borrow_mut().candidates.clear();
                                let _ = InvalidateRect(hwnd, None, true);
                            } else {
                                recognize_now(hwnd);
                            }
                        }
                    }
                    1 => {
                        // 清空。
                        let g = global_hw_state();
                        let __g = g.lock().unwrap(); let mut s = __g.borrow_mut();
                        s.strokes.clear();
                        s.cur.clear();
                        s.candidates.clear();
                        let _ = KillTimer(hwnd, TIMER_RECOG);
                        let _ = InvalidateRect(hwnd, None, true);
                    }
                    2 => send_backspace(),
                    3 => {
                        let g = global_hw_state();
                        hide(&g);
                    }
                    _ => {}
                }
                return LRESULT(0);
            }
            // 候选命中 → 上屏 + 自动清板。
            let n = {
                let g = global_hw_state();
                g.lock().unwrap().borrow().candidates.len()
            };
            if let Some(idx) = hit_candidate(x, y, n) {
                let text = {
                    let g = global_hw_state();
                    g.lock().unwrap().borrow().candidates.get(idx).cloned()
                };
                if let Some(t) = text {
                    send_text(&t);
                    // 自动清板(同安卓,连续书写)。
                    let g = global_hw_state();
                    let __g = g.lock().unwrap(); let mut s = __g.borrow_mut();
                    s.strokes.clear();
                    s.cur.clear();
                    s.candidates.clear();
                    let _ = InvalidateRect(hwnd, None, true);
                }
                return LRESULT(0);
            }
            // 画板区 → 起一笔。
            let pr = pad_rect();
            if in_rect(&pr, x, y) {
                let _ = SetCapture(hwnd);
                let _ = KillTimer(hwnd, TIMER_RECOG); // 落笔即取消待识别计时
                {
                    let g = global_hw_state();
                    let __g = g.lock().unwrap(); let mut s = __g.borrow_mut();
                    s.drawing = true;
                    s.cur.clear();
                    s.cur.push(((x - pr.left) as f32, (y - pr.top) as f32));
                }
                let _ = InvalidateRect(hwnd, None, true);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let drawing = {
                let g = global_hw_state();
                g.lock().unwrap().borrow().drawing
            };
            if drawing {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let pr = pad_rect();
                let px = (x - pr.left).clamp(0, 260) as f32;
                let py = (y - pr.top).clamp(0, PAD_H) as f32;
                {
                    let g = global_hw_state();
                    let __g = g.lock().unwrap(); let mut s = __g.borrow_mut();
                    // 距上一点太近就跳过(减少点密度,识别更稳)。
                    if let Some(last) = s.cur.last() {
                        let dx = px - last.0;
                        let dy = py - last.1;
                        if dx * dx + dy * dy < 4.0 {
                            return LRESULT(0);
                        }
                    }
                    s.cur.push((px, py));
                }
                // 只重绘画板区(避免闪烁)。
                let mut prc = pr;
                let _ = InvalidateRect(hwnd, Some(&mut prc), false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let drawing = {
                let g = global_hw_state();
                g.lock().unwrap().borrow().drawing
            };
            if drawing {
                let _ = ReleaseCapture();
                {
                    let g = global_hw_state();
                    let __g = g.lock().unwrap(); let mut s = __g.borrow_mut();
                    s.drawing = false;
                    if !s.cur.is_empty() {
                        let done = std::mem::take(&mut s.cur);
                        s.strokes.push(done);
                    }
                }
                let _ = InvalidateRect(hwnd, None, true);
                // 抬笔停顿自动识别(同安卓):600ms 内没落新笔 → 识别。
                let _ = SetTimer(hwnd, TIMER_RECOG, RECOG_DELAY_MS, None);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
