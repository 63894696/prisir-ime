//! 独立悬浮候选窗 — 对齐灵犀拼音(voice_input/lingxi_ime/frontend/pinyin/app.py)。
//!
//! **为什么独立窗(2026-09-01 真根因)**:
//! 之前把候选列表(`nihao 1.你 2.好 ...`)直接 SetText 进 composition → 候选被当成
//! 文档文本写进记事本,Esc/退格删不动,卡死。主流输入法(搜狗/微软拼音/灵犀)的做法:
//!   - composition 下划线文本**只放纯拼音 buffer**(如 `nihao`);
//!   - 候选列表画在**独立悬浮窗**,定位到光标下方,不写入文档;
//!   - 数字/空格选中 → commit 候选词 → 隐藏窗。
//!
//! **窗口属性(对齐灵犀 tkinter `overrideredirect + topmost`)**:
//!   - `WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW`:不抢焦点(点了候选字仍
//!     留在目标编辑框,灵犀注释 76/203 行的教训)、置顶、不出现在任务栏/Alt-Tab。
//!   - `WS_POPUP`:无边框。
//!   - 自绘(WM_PAINT 里 DrawTextW 横向排 5 个候选,高亮 selected)。
//!
//! **线程模型**:本 IME 是进程内 COM,单 STA 线程。窗口过程是自由函数,候选数据经
//! `GWLP_USERDATA` 传 `*const CandidateWinState`(创建时 SetWindowLongPtrW 注入)。
//! 同一 STA 线程内 OnKeyDown 与 WndProc 不会真并发,RefCell 可安全用。

use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// 单个候选的显示数据(从 keystroke::Candidate 拷贝,避免窗模块依赖引擎类型)。
#[derive(Clone)]
pub(crate) struct CandItem {
    pub word: String,
}

/// 候选窗共享状态 — TsfInputProcessor 持一份,WndProc 经 GWLP_USERDATA 读。
pub(crate) struct CandidateWinState {
    pub hwnd: Option<HWND>,
    pub buffer: String,          // 纯拼音 buffer(顶部拼音行显示)
    pub candidates: Vec<CandItem>, // 当前**显示的**这一页候选(page_slice 快照,仅作后备)
    /// 全量候选列表(2026-09-03 搜狗式滚动):滚动渲染按 scroll_offset 从这里切片,
    /// 不再用 page_slice 快照(那是旧 5 个/页替换式)。与 pinyin.candidates 同步注入。
    pub all_candidates: Vec<CandItem>,
    /// 搜狗式滚动偏移(全量索引,行首):当前可视 5 行 = all_candidates[scroll_offset..scroll_offset+5]。
    /// 翻页键 page_down → scroll_offset += 5(字在框内上移一行语义:每次滚动 1 行由调用方定步长)。
    pub scroll_offset: usize,
    pub selected: usize,
    pub visible: bool,
    pub x: i32,
    pub y: i32,
    /// 尾页竖排滚动模式已移除(2026-09-03 全横排化):候选窗只做横排单行整页换。
    /// 当前显示的页码(与 candidates 这份快照对应)。
    /// 2026-09-02 修「翻页选错字」:数字键选词必须用候选窗**实际显示**的页/候选,
    /// 而不是可能被后续按键改掉的 pinyin.page —— 看到的和选的要一致。
    pub page: usize,
}

impl CandidateWinState {
    pub fn new() -> Self {
        Self {
            hwnd: None,
            buffer: String::new(),
            candidates: Vec::new(),
            all_candidates: Vec::new(),
            scroll_offset: 0,
            selected: 0,
            visible: false,
            x: 0,
            y: 0,
            page: 0,
        }
    }
}

/// 全局共享 — WndProc 是自由函数,拿不到 &self,只能从这里取 state。
/// OnceLock 因为 TsfInputProcessor::new() 时窗还没建,activate 时才注册类。
static CAND_STATE: OnceLock<Arc<Mutex<RefCell<CandidateWinState>>>> = OnceLock::new();

// SAFETY: 本 IME 是进程内 COM,单 STA 线程。CandidateWinState 含 HWND(底层 *mut c_void),
// 不天然 Send/Sync,但所有访问都在同一 STA 线程(OnKeyDown / WndProc 同线程),无不安全并发。
unsafe impl Send for CandidateWinState {}
unsafe impl Sync for CandidateWinState {}

pub(crate) fn global_state() -> Arc<Mutex<RefCell<CandidateWinState>>> {
    CAND_STATE
        .get_or_init(|| Arc::new(Mutex::new(RefCell::new(CandidateWinState::new()))))
        .clone()
}

const WND_CLASS: &str = "PrisirCandidateWindow";

/// 注册窗口类(幂等)。在 activate 时调一次。
fn register_class() -> Result<()> {
    let hinst = unsafe { GetModuleHandleW(None) }?;
    let class_name: Vec<u16> = WND_CLASS.encode_utf16().chain(std::iter::once(0)).collect();
    // 已注册则 GetClassInfoExW 成功,直接返回。
    let mut existing = WNDCLASSEXW::default();
    if unsafe { GetClassInfoExW(hinst, PCWSTR(class_name.as_ptr()), &mut existing) }.is_ok() {
        return Ok(());
    }
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(candidate_wnd_proc),
        hInstance: hinst.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }?,
        hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        return Err(Error::from_win32());
    }
    Ok(())
}

/// 判断候选窗所属进程是否还活着(用于清理死进程遗留的孤儿窗)。
/// GetWindowThreadProcessId 拿 pid → OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)。
/// 打开成功=活;打开失败(进程不存在)=死。当前进程自己的窗直接视为活(不会自杀)。
fn hwnd_owner_alive(hwnd: HWND) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true; // 本进程的窗,永远视为活(复用,不销毁)
    }
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => {
            unsafe { let _ = CloseHandle(h); }
            true
        }
        Err(_) => false,
    }
}

/// 确保候选窗存在(不存在或已失效则重建),返回有效 hwnd。
/// 2026-09-01 根因:切换输入法时 Prisir 收不到 Deactivate,候选窗没被 destroy,
/// 但系统/ctfmon 已把旧 popup 销毁 → CAND_STATE.hwnd 残留死句柄。切回来后
/// 若直接用死句柄,SetWindowPos/ShowWindow 静默失败 → 候选窗再也不弹。
/// 故每次先用 IsWindow 校验,死了就清空重建。
fn ensure_window(state: &Arc<Mutex<RefCell<CandidateWinState>>>) -> Result<HWND> {
    let existing = state.lock().unwrap().borrow().hwnd;
    if let Some(hwnd) = existing {
        // IsWindow 校验句柄活性(切换 IME 后旧 hwnd 可能已死)。
        if unsafe { IsWindow(hwnd) }.as_bool() {
            return Ok(hwnd);
        }
        // 死句柄 → 清空,走下面重建。
        state.lock().unwrap().borrow_mut().hwnd = None;
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("CandWin: stale hwnd detected, recreate");
    }
    register_class()?;
    let hinst = unsafe { GetModuleHandleW(None) }?;
    let class_name: Vec<u16> = WND_CLASS.encode_utf16().chain(std::iter::once(0)).collect();
    let title: Vec<u16> = "PrisirCand".encode_utf16().chain(std::iter::once(0)).collect();
    // 2026-09-02 清孤儿残窗:候选窗是**每进程一个**(CAND_STATE 进程级),旧进程(如反复
    // 开关的记事本)退出时若没收到 Deactivate 就遗留孤儿窗挂屏显示旧页 → 「残影/自动跳变」。
    // 此处建窗前枚举所有同类候选窗,销毁两类:①所属进程已死的孤儿;②**本进程**的旧窗
    // (Esc 时跨线程 DestroyWindow 失败遗留的,同进程可能积多个,只留即将新建的这个)。
    // 保留其它活进程正在用的候选窗(不能误删)。销毁统一走跨线程安全路径。
    unsafe {
        let mut prev = HWND::default();
        loop {
            let found = FindWindowExW(None, prev, PCWSTR(class_name.as_ptr()), PCWSTR(title.as_ptr()));
            match found {
                Ok(h) if !h.is_invalid() => {
                    let mut pid: u32 = 0;
                    GetWindowThreadProcessId(h, Some(&mut pid));
                    let mine = pid == std::process::id();
                    if Some(h) != existing && (!hwnd_owner_alive(h) || mine) {
                        destroy_hwnd_cross_thread(h);
                        #[cfg(feature = "dllentry_log")]
                        crate::com_class_factory::log_dll_entry(&format!(
                            "CandWin: destroyed stale hwnd={:p} pid={} mine={}", h.0, pid, mine));
                    }
                    prev = h;
                }
                _ => break,
            }
        }
    }
    // GWLP_USERDATA 传 state 指针 — WndProc 据此读候选数据。
    let state_ptr = Arc::into_raw(state.clone()) as *const _ as *mut std::ffi::c_void;
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            0, 0, 10, 10,
            None, None,
            hinst,
            Some(state_ptr),
        )
    }?;
    state.lock().unwrap().borrow_mut().hwnd = Some(hwnd);
    #[cfg(feature = "dllentry_log")]
    crate::com_class_factory::log_dll_entry(&format!("CandWin: created hwnd={:p}", hwnd.0));
    Ok(hwnd)
}

/// 更新候选内容并重排窗口(不强制 show)。OnKeyDown 每次重查候选后调。
/// `all_candidates` = 全量候选列表,`scroll_offset` = 可视区首行的全量索引(搜狗式滚动)。
/// `page` = scroll_offset/CAND_PER_PAGE,数字键选词据此定位,保证所见即所选。
pub(crate) fn update(
    state: &Arc<Mutex<RefCell<CandidateWinState>>>,
    buffer: &str,
    all_candidates: Vec<CandItem>,
    scroll_offset: usize,
    selected: usize,
    x: i32,
    y: i32,
    page: usize,
) {
    {
        let g = state.lock().unwrap();
        let mut s = g.borrow_mut();
        s.buffer = buffer.to_string();
        // 可视区快照(当前页 slice,后备/日志用)。一页 = cand_per_page() 个。
        let cpp = crate::keystroke::cand_per_page();
        let end = (scroll_offset + cpp).min(all_candidates.len());
        s.candidates = if scroll_offset < all_candidates.len() {
            all_candidates[scroll_offset..end].to_vec()
        } else {
            Vec::new()
        };
        s.all_candidates = all_candidates;
        s.scroll_offset = scroll_offset;
        s.selected = selected;
        s.x = x;
        s.y = y;
        s.page = page;
        #[cfg(feature = "dllentry_log")]
        {
            let words: Vec<String> = s.candidates.iter().map(|c| c.word.clone()).collect();
            crate::com_class_factory::log_dll_entry(&format!(
                "CandUpdate: buf='{}' page={} off={} total={} visible={:?}",
                buffer, page, scroll_offset, s.all_candidates.len(), words));
        }
    }
    if let Ok(hwnd) = ensure_window(state) {
        reposition_and_show(state, hwnd);
    }
}

/// 隐藏候选窗(commit / Esc / 切换输入法 / 失焦时)。
/// 2026-09-02 改:SW_HIDE 不够——失焦重建时若旧 hwnd 被 IsWindow 误判死亡、state 被新
/// 窗口覆盖,旧窗口就成孤儿残窗挂在屏幕上显示旧残页(用户见「翻页残影/自动跳变」)。
/// 故 hide 直接 DestroyWindow 彻底销毁,下次需要时 ensure_window 干净新建。
/// 销毁一个候选窗句柄,**跨线程安全**(2026-09-02 残页真根因修复)。
/// 候选窗由 ctfmon/UI 线程创建,而 hide() 常被 app 线程(OnKeyDown/Esc)调用:
/// `DestroyWindow` 只能由**创建该窗的线程**调用,跨线程调用返回 FALSE——之前 `let _=`
/// 把失败吞掉,旧窗没销毁、state.hwnd 又被 take() 清空 → 下次 ensure_window 新建一个,
/// 旧窗成孤儿挂屏显示旧页 = 用户看到的「残页/自动跳变」。
/// 修法:先 ShowWindow(SW_HIDE) 立刻隐藏(消残影),再 PostMessage(WM_CLOSE) 把销毁
/// 投递到**拥有者线程**的消息队列,由 WndProc 在自己线程里 DestroyWindow。
/// 返回后句柄可能尚未销毁(异步),但已隐藏,不会留残影。
unsafe fn destroy_hwnd_cross_thread(hwnd: HWND) {
    let _ = ShowWindow(hwnd, SW_HIDE); // 立即隐藏,不管后续销毁是否异步
    if IsWindow(hwnd).as_bool() {
        // 交给拥有者线程销毁;WM_CLOSE 默认处理即 DestroyWindow(我们在 WndProc 显式处理)。
        let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!(
            "CandWin: hide posted WM_CLOSE hwnd={:p}", hwnd.0));
    }
}

pub(crate) fn hide(state: &Arc<Mutex<RefCell<CandidateWinState>>>) {
    let hwnd = state.lock().unwrap().borrow_mut().hwnd.take();
    if let Some(hwnd) = hwnd {
        unsafe { destroy_hwnd_cross_thread(hwnd); }
    }
    state.lock().unwrap().borrow_mut().visible = false;
}

/// 销毁窗口(Deactivate 时)。
pub(crate) fn destroy(state: &Arc<Mutex<RefCell<CandidateWinState>>>) {
    let hwnd = state.lock().unwrap().borrow_mut().hwnd.take();
    if let Some(hwnd) = hwnd {
        unsafe { destroy_hwnd_cross_thread(hwnd); }
    }
}

/// 候选窗布局常量(2026-09-03 全横排 + 苹果风)。
/// 横排单行:拼音行 + 一行候选(整页换)。窗宽固定,翻页只改 scroll_offset,无残页窗口期。
const PYINYIN_ROW_H: i32 = 34;   // 顶部拼音行(字稍大,留呼吸感)
const CAND_ROW_H: i32 = 34;      // 候选行高(苹果式宽松行距)
const WIN_W: i32 = 460;          // 固定宽度(单行候选 + 手写入口)
// 苹果风配色(BGR 0x00BBGGRR)
const COL_BG: u32 = 0x00FAF9F7;      // 暖白底
const COL_PINYIN: u32 = 0x008A8480;  // 拼音灰
const COL_NUM: u32 = 0x00AEA8A0;     // 序号浅灰
const COL_TEXT: u32 = 0x002A2723;    // 候选深灰
const COL_SEL_TEXT: u32 = 0x00FFFFFF;// 选中白字
const COL_SEL_BG: u32 = 0x00A9750A;  // 选中底:苹果蓝 #0A84FF(BGR)
const COL_SEP: u32 = 0x00ECE9E4;     // 分隔线浅灰
const COL_HW: u32 = 0x009C968E;      // 手写入口灰

fn reposition_and_show(state: &Arc<Mutex<RefCell<CandidateWinState>>>, hwnd: HWND) {
    let (w, h, x, y, has_cand) = {
        let g = state.lock().unwrap();
        let s = g.borrow();
        if s.all_candidates.is_empty() {
            (0, 0, s.x, s.y, false)
        } else {
            // 横排单行:拼音行 + 一行候选。固定尺寸,翻页只改 scroll_offset,无残页窗口期。
            let h = PYINYIN_ROW_H + CAND_ROW_H + 6;
            (WIN_W, h, s.x, s.y, true)
        }
    };
    if !has_cand {
        unsafe { let _ = ShowWindow(hwnd, SW_HIDE); }
        state.lock().unwrap().borrow_mut().visible = false;
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            hwnd, HWND_TOPMOST,
            x, y + 4, w, h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        // 苹果式圆角:裁剪窗口为圆角矩形区域。
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, 16, 16);
        let _ = SetWindowRgn(hwnd, rgn, true);
        let _ = InvalidateRect(hwnd, None, true);
        let _ = RedrawWindow(hwnd, None, None, RDW_ERASE | RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN);
    }
    state.lock().unwrap().borrow_mut().visible = true;
}

/// 窗口过程 — 自绘候选列表。
unsafe extern "system" fn candidate_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // CREATESTRUCT.lpCreateParams = state 指针,存进 GWLP_USERDATA。
            let cs = lparam.0 as *const CREATESTRUCTW;
            if !cs.is_null() {
                let ptr = (*cs).lpCreateParams;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_PAINT => {
            paint_candidates(hwnd);
            LRESULT(0)
        }
        // 拥有者线程收到 WM_CLOSE(hide/清孤儿投来的)→ 在自己线程里真正销毁。
        // 跨线程 DestroyWindow 会失败,必须在这里(创建线程)执行。
        WM_CLOSE => {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!("CandWin: WM_CLOSE destroy hwnd={:p}", hwnd.0));
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        // 不抢焦点:点击/激活全部吃掉,不转发。
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => {
            // 「✎手写」入口命中(右上角拼音行右侧)。命中 → 弹手写画板。
            // 标签绘制在 win_w - hw_sz.cx - 12 .. win_w - 12,py_y..py_y+行高。
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let win_w = rc.right - rc.left;
            // 手写标签约 3 字宽(-17 字体 ≈ 51px) + 右侧留白 12;给足热区。
            let hw_left = win_w - 70;
            if x >= hw_left && y >= 0 && y <= PYINYIN_ROW_H {
                crate::handwriting_panel::toggle_panel(None);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn paint_candidates(hwnd: HWND) {
    // 候选数据走 global_state()(同一 STA 线程,与 OnKeyDown 写入端同源)。
    let state = global_state();
    let (buffer, all, scroll_offset, selected) = {
        let g = state.lock().unwrap();
        let s = g.borrow();
        (s.buffer.clone(), s.all_candidates.clone(), s.scroll_offset, s.selected)
    };

    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let win_w = rc.right - rc.left;
        // 苹果风:暖白底铺满(圆角由 SetWindowRgn 裁剪,这里直角填满即可)。
        let bg = CreateSolidBrush(COLORREF(COL_BG));
        FillRect(hdc, &rc, bg);
        let _ = DeleteObject(bg);

        SetBkMode(hdc, TRANSPARENT);
        // 苹果式现代字体:稍大(-17)、ClearType 抗锯齿。
        let face: Vec<u16> = "Microsoft YaHei UI".encode_utf16().chain(std::iter::once(0)).collect();
        let ui_font = CreateFontW(
            -17, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
            DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32, DEFAULT_PITCH.0 as u32, PCWSTR(face.as_ptr()),
        );
        let old_font = if !ui_font.is_invalid() { Some(SelectObject(hdc, ui_font)) } else { None };

        // ── 拼音行(顶部):左侧纯拼音,右侧手写入口 ──
        let py_y = 7i32;
        SetTextColor(hdc, COLORREF(COL_PINYIN));
        if !buffer.is_empty() {
            let wide: Vec<u16> = buffer.encode_utf16().collect();
            let _ = TextOutW(hdc, 12, py_y, &wide);
        }
        let hw_label: Vec<u16> = "✎手写".encode_utf16().collect();
        let mut hw_sz = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &hw_label, &mut hw_sz);
        SetTextColor(hdc, COLORREF(COL_HW));
        let _ = TextOutW(hdc, win_w - hw_sz.cx - 12, py_y, &hw_label);
        // 拼音行与候选行分隔线(浅灰,内缩)
        let sep = CreatePen(PS_SOLID, 1, COLORREF(COL_SEP));
        let old_pen = SelectObject(hdc, sep);
        let _ = MoveToEx(hdc, 10, PYINYIN_ROW_H, None);
        let _ = LineTo(hdc, win_w - 10, PYINYIN_ROW_H);
        SelectObject(hdc, old_pen);
        let _ = DeleteObject(sep);

        // ── 候选区:横排单行,可视区 = all[scroll_offset..scroll_offset+cpp] 整页换 ──
        let cpp = crate::keystroke::cand_per_page();
        let end = (scroll_offset + cpp).min(all.len());
        let vis: &[CandItem] = if scroll_offset < all.len() { &all[scroll_offset..end] } else { &[] };
        #[cfg(feature = "dllentry_log")]
        {
            let words: Vec<String> = vis.iter().map(|c| c.word.clone()).collect();
            crate::com_class_factory::log_dll_entry(&format!(
                "CandPaint: buf='{}' off={} total={} visible={:?}",
                buffer, scroll_offset, all.len(), words));
        }
        let row_y = PYINYIN_ROW_H + 9;
        let mut x = 12i32;
        for (i, c) in vis.iter().enumerate() {
            let num = format!("{}", i + 1);
            let numw: Vec<u16> = num.encode_utf16().collect();
            let mut nsz = SIZE::default();
            let _ = GetTextExtentPoint32W(hdc, &numw, &mut nsz);
            let wordw: Vec<u16> = c.word.encode_utf16().collect();
            let mut wsz = SIZE::default();
            let _ = GetTextExtentPoint32W(hdc, &wordw, &mut wsz);
            // 选中候选:圆角蓝色胶囊高亮(苹果蓝),序号+词整体反白。
            if i == selected {
                let pad = 6;
                let pill = RECT { left: x - pad, top: row_y - 4, right: x + nsz.cx + 4 + wsz.cx + pad, bottom: row_y + wsz.cy + 4 };
                let hl = CreateSolidBrush(COLORREF(COL_SEL_BG));
                // 用圆角路径填充:RoundRect 画胶囊。
                let old_br = SelectObject(hdc, hl);
                let nopen = GetStockObject(NULL_PEN);
                let old_pn = SelectObject(hdc, nopen);
                let _ = RoundRect(hdc, pill.left, pill.top, pill.right, pill.bottom, 12, 12);
                SelectObject(hdc, old_pn);
                SelectObject(hdc, old_br);
                let _ = DeleteObject(hl);
            }
            let num_color = if i == selected { COL_SEL_TEXT } else { COL_NUM };
            SetTextColor(hdc, COLORREF(num_color));
            let _ = TextOutW(hdc, x, row_y, &numw);
            x += nsz.cx + 4;
            let text_color = if i == selected { COL_SEL_TEXT } else { COL_TEXT };
            SetTextColor(hdc, COLORREF(text_color));
            let _ = TextOutW(hdc, x, row_y, &wordw);
            x += wsz.cx + 22; // 候选间距(宽松)
            if x >= win_w - 50 { break; } // 防爆出右边界
        }
        if let Some(old) = old_font {
            SelectObject(hdc, old);
            let _ = DeleteObject(ui_font);
        }
        let _ = EndPaint(hwnd, &ps);
    }
}
