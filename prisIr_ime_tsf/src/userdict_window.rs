//! 词库管理独立窗口(Step 2,2026-09-04)。
//!
//! **定位**:状态条「词」按钮 / 菜单「词库管理」点开。对齐 Android UserDictActivity:
//! 查看学习词(词+拼音+学习次序)、手动加词(词+拼音→学成置顶)、删除选中、清空学习记录。
//!
//! **窗口形态(2026-09-04 v2,按用户实测反馈改版)**:
//!   - **可调大小**(WS_OVERLAPPEDWINDOW 含 WS_THICKFRAME+WS_MAXIMIZEBOX):用户可拉伸/
//!     最大化看更多词。布局走 WM_SIZE 按当前客户区实时重排(列表填满、滚动条贴右、
//!     加词区贴底),不再固定坐标(固定坐标一拉伸就错位 = 用户看到的「不能自行拉伸」)。
//!   - **原生垂直滚动条**(WS_VSCROLL 子控件):行数超可视区出滚动条,拖动/滚轮滚动
//!     (v1 只有滚轮无滚动条 = 用户「没有滚动条」)。
//!   - **行尾 [删] 按钮**:点中某行直接点该行尾部「删」删该词,不用再找底部按钮
//!     (v1 只有底部「删除选中」入口 = 用户「不知道怎么删除」)。底部仍保留删除选中/清空/刷新。
//!   - **加词区完整可见**:WM_SIZE 把它钉在客户区底部,AdjustWindowRectEx 把标题栏算进
//!     外框 → 客户区足尺,输入框不被压扁(v1「输入框看不见」根因 = 没算标题栏高度)。
//!
//! **数据通道**:不直连 rusqlite(D1:TSF 壳不静态耦合引擎)。走 ffi.rs user_dict_* 包装 →
//!   运行时 LoadLibrary 引擎 DLL 的 user_list/user_add/user_remove/user_clear 4 导出 →
//!   写独立 user.db(不碰只读主库,不影响索引指纹)。
//!
//! **自绘**:苹果风 GDI(暖白底/雅黑)。编辑框/按钮/滚动条用系统子控件(中文输入走当前 IME)。

use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::WindowsAndMessaging::*;

const WIN_CLASS: &str = "PrisirUserDict";
const WIN_TITLE: &str = "Prisir 灵犀拼音词库管理";

/// 诊断 trace(2026-09-04):用户实测「点词库→表头出来、数据行不出→notepad 崩
/// USER32+0x1f18b / 0xc000041d」。日志只到 created hwnd 就断,说明崩在窗口过程
/// 绘制/消息处理且没打点。此宏在关键路径逐行打点,定位到具体哪一步崩。
macro_rules! ulog {
    ($($arg:tt)*) => {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!($($arg)*));
    };
}

/// UTF-16 buffer **带 null 结尾**(2026-09-04 崩溃修复)。
/// windows-rs 0.58 的 DrawTextW 绑定对 `&mut Vec<u16>` 按 C 字符串(-1 长度)处理,
/// 要求 buffer 以 0 结尾;没结尾就越界读 → GDI 字体排版一路读到非法内存 →
/// USER32.dll 访问违例 0xc000041d(带崩宿主 notepad)。表头字少侥幸不越界,
/// 数据行(词+拼音+#seq)字多一画就越界。统一走 wcs() 补 `\0`。
fn wcs(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// 苹果风配色(BGR 0x00BBGGRR)。
const COL_BG: u32 = 0x00FAF9F7;      // 暖白底
const COL_SEP: u32 = 0x00ECE9E4;     // 分隔浅灰
const COL_ACCENT: u32 = 0x00A9750A;  // 苹果蓝
const COL_TEXT: u32 = 0x005A544D;    // 暖灰正文
const COL_SELBG: u32 = 0x00E8EEFB;   // 选中行浅蓝底
const COL_HDR: u32 = 0x00F3F1ED;     // 表头底
const COL_DEL: u32 = 0x003C3CD2;     // 「删」按钮红(BGR)

// 布局常量(单搜索页,自上而下:搜索行/编辑行/表头/列表/底部翻页行)。
const SEARCH_TOP: i32 = 10;          // 搜索框行 y
const SEARCH_H: i32 = 28;
const EDIT_TOP: i32 = SEARCH_TOP + SEARCH_H + 12; // 编辑行(拼音/词语/词频)y
const EDIT_H: i32 = 28;
const LIST_TOP: i32 = EDIT_TOP + EDIT_H + 12;     // 表头起始 y
const HDR_H: i32 = 26;               // 表头高
const ROW_H: i32 = 28;
const SCROLL_W: i32 = 14;            // 滚动条宽
const MARGIN: i32 = 12;
const BTN_BAR_H: i32 = 50;           // 底部翻页/状态行(上一页/下一页 + 页码·总数)
const DEL_BTN_W: i32 = 40;           // 行尾「删」按钮宽
const MIN_W: i32 = 640;              // 最小宽度:编辑行「删除」按钮完整可见(默认开 800)
const MIN_H: i32 = 420;

// 列宽比例(按客户区可用宽算)。
const COL_WORD_RATIO: i32 = 44;      // 「词」占 44%
const COL_PY_RATIO: i32 = 32;        // 「拼音」占 32%
// 「次序」+「删」 = 剩余。

// 控件 ID(子 EDIT / BUTTON / SCROLLBAR)。
const IDC_SCROLL: u16 = 2007;
const IDC_EDIT_SEARCH: u16 = 2008;   // 顶部统一搜索框
const IDC_BTN_SEARCH: u16 = 2009;
// 主库编辑区(2026-09-04 主库可写;单搜索页模式直接复用为灵犀式「拼音/词语/词频」编辑行)。
const IDC_M_EDIT_WORD: u16 = 2010;   // 词
const IDC_M_EDIT_PINYIN: u16 = 2011; // 拼音
const IDC_M_EDIT_WEIGHT: u16 = 2012; // 词频/权重
const IDC_M_BTN_SAVE: u16 = 2013;    // 保存(加/改)
const IDC_M_BTN_DEL: u16 = 2014;     // 删除词条
const IDC_BTN_PREV: u16 = 2015;      // 上一页
const IDC_BTN_NEXT: u16 = 2016;      // 下一页
const PAGE_SIZE: usize = 20;         // 每页条数(灵犀式分页)
// 已删(2026-09-04 用户拍板,与编辑行重复):IDC_EDIT_WORD/IDC_EDIT_PINYIN/IDC_BTN_ADD(底部加词)、
//   IDC_BTN_CLEAR(清空学习)、IDC_BTN_REFRESH(刷新)、IDC_BTN_DEL(删除选中)。

/// 标签页:学习词库(可增删)/ 全部词库(主库只读搜索)。
/// 2026-09-04 起单搜索页模式只走 Tab::All(统一搜主库),Tab/User 分支为死代码留待清理。
#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab { User, All }

#[derive(Clone)]
struct DictRow {
    key: String,
    value: String,
    #[allow(dead_code)]
    weight: i64,
    seq: i64,
}

/// 主词库搜索结果行(只读,无 seq,有 weight + source)。
#[derive(Clone)]
struct MainRow {
    key: String,
    value: String,
    weight: i64,
    source: String,
}

struct UserDictState {
    hwnd: Option<HWND>,
    rows: Vec<DictRow>,
    sel: Option<usize>,
    scroll: i32,   // 列表滚动起始行
    visible: bool,
    tab: Tab,                  // 当前标签页
    main_rows: Vec<MainRow>,   // 全部词库搜索结果
    main_scroll: i32,
    main_sel: Option<usize>,   // 全部词库选中行(编辑/删除目标)
    search_term: String,       // 当前搜索词(分页/保存后重搜共用)
    page: usize,               // 当前页(0 起)
    total: usize,              // 匹配总条数(「共 N 条」)
}

impl UserDictState {
    fn new() -> Self {
        Self {
            hwnd: None, rows: Vec::new(), sel: None, scroll: 0, visible: false,
            tab: Tab::All, main_rows: Vec::new(), main_scroll: 0, main_sel: None,
            search_term: String::new(), page: 0, total: 0,
        }
    }
}

static STATE: OnceLock<Arc<Mutex<RefCell<UserDictState>>>> = OnceLock::new();
unsafe impl Send for UserDictState {}
unsafe impl Sync for UserDictState {}

fn global_state() -> Arc<Mutex<RefCell<UserDictState>>> {
    STATE.get_or_init(|| Arc::new(Mutex::new(RefCell::new(UserDictState::new())))).clone()
}

// ── 数据:从引擎拉学习词 ─────────────────────────────────────────────────────
fn reload_rows() {
    let rows = crate::ffi::user_dict_list_json()
        .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
        .map(|arr| {
            arr.iter()
                .map(|v| DictRow {
                    key: v["key"].as_str().unwrap_or("").to_string(),
                    value: v["value"].as_str().unwrap_or("").to_string(),
                    weight: v["weight"].as_i64().unwrap_or(0),
                    seq: v["seq"].as_i64().unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default();
    let st = global_state();
    let g = st.lock().unwrap();
    let mut s = g.borrow_mut();
    s.rows = rows;
    s.sel = None;
    s.scroll = 0;
}

// ── 数据:主词库分页搜索(灵犀式:拼音/文字同搜 + 总数 + 翻页)──────────────────
/// 拉取当前 search_term + page 对应的一页 + 总条数。
fn reload_main_page() {
    let (term, page) = {
        let g = global_state();
        let g = g.lock().unwrap();
        let s = g.borrow();
        (s.search_term.clone(), s.page)
    };
    let offset = page * PAGE_SIZE;
    let total = crate::ffi::dict_count(&term).max(0) as usize;
    let rows = crate::ffi::dict_search_page_json(&term, PAGE_SIZE as i32, offset as i32)
        .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
        .map(|arr| {
            arr.iter()
                .map(|v| MainRow {
                    key: v["key"].as_str().unwrap_or("").to_string(),
                    value: v["value"].as_str().unwrap_or("").to_string(),
                    weight: v["weight"].as_i64().unwrap_or(0),
                    source: v["source"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let st = global_state();
    let g = st.lock().unwrap();
    let mut s = g.borrow_mut();
    s.main_rows = rows;
    s.total = total;
    s.main_scroll = 0;
    s.main_sel = None;
}

/// 新搜索(词变了):重置到第 0 页再拉。
fn reload_main_rows(term: &str) {
    {
        let st = global_state();
        let g = st.lock().unwrap();
        let mut s = g.borrow_mut();
        s.search_term = term.trim().to_string();
        s.page = 0;
    }
    reload_main_page();
}

// ── 窗口类注册 ──────────────────────────────────────────────────────────────
fn register_class() -> Result<()> {
    let hinst = unsafe { GetModuleHandleW(None) }?;
    let class_name: Vec<u16> = WIN_CLASS.encode_utf16().chain(std::iter::once(0)).collect();
    let mut existing = WNDCLASSEXW::default();
    if unsafe { GetClassInfoExW(hinst, PCWSTR(class_name.as_ptr()), &mut existing) }.is_ok() {
        return Ok(());
    }
    // 灵犀拼音专属图标:build.rs 用 winres 把 lingxi.ico 编成本模块资源 ID 1。
    // LoadImageW 从本 DLL 资源段按 ID 1 提取(大/小图标都给,任务栏+标题栏一致);
    // 失败(资源缺失)则留默认 NULL 句柄,窗口照样能开,只是标题栏回退系统图标。
    let hinst_h: HINSTANCE = hinst.into();
    let load_icon = |size: i32| -> HICON {
        unsafe {
            LoadImageW(
                hinst_h,
                PCWSTR(1usize as *const u16), // 资源 ID 1 = MAKEINTRESOURCEW(1)
                IMAGE_ICON,
                size,
                size,
                LR_DEFAULTCOLOR,
            )
        }
        .map(|h| HICON(h.0))
        .unwrap_or_default()
    };
    let sm_x = unsafe { GetSystemMetrics(SM_CXSMICON) };
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(userdict_wnd_proc),
        hInstance: hinst_h,
        hIcon: load_icon(0),            // 大图标(0=系统默认大图尺寸)
        hIconSm: load_icon(sm_x),       // 小图标(标题栏,SM_CXSMICON 见方)
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

// ── 子控件创建 ──────────────────────────────────────────────────────────────
fn create_children(hwnd: HWND) {
    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None) }.unwrap_or_default().into();
    let mk_edit = |id: u16| unsafe {
        let cls: Vec<u16> = "EDIT".encode_utf16().chain(std::iter::once(0)).collect();
        CreateWindowExW(
            WS_EX_CLIENTEDGE, PCWSTR(cls.as_ptr()), PCWSTR::null(),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | (ES_AUTOHSCROLL as u32)),
            0, 0, 10, 10, hwnd, HMENU(id as usize as *mut _), hinst, None,
        ).ok()
    };
    let mk_btn = |id: u16, text: &str| unsafe {
        let cls: Vec<u16> = "BUTTON".encode_utf16().chain(std::iter::once(0)).collect();
        let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        CreateWindowExW(
            WINDOW_EX_STYLE(0), PCWSTR(cls.as_ptr()), PCWSTR(t.as_ptr()),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | (BS_PUSHBUTTON as u32)),
            0, 0, 10, 10, hwnd, HMENU(id as usize as *mut _), hinst, None,
        ).ok()
    };
    // 顶部统一搜索框 + 搜索。
    mk_edit(IDC_EDIT_SEARCH);
    mk_btn(IDC_BTN_SEARCH, "搜索");
    // 编辑行(词语/拼音/词频 + 保存/删除,2026-09-04 主库可写)。
    mk_edit(IDC_M_EDIT_WORD);
    mk_edit(IDC_M_EDIT_PINYIN);
    mk_edit(IDC_M_EDIT_WEIGHT);
    mk_btn(IDC_M_BTN_SAVE, "保存");
    mk_btn(IDC_M_BTN_DEL, "删除");
    // 翻页(灵犀式)。
    mk_btn(IDC_BTN_PREV, "< 上一页");
    mk_btn(IDC_BTN_NEXT, "下一页 >");
    // 子控件统一字体(与自绘/候选窗一致,否则 EDIT/BUTTON 用系统默认字体看着不齐)。
    let ui_font = cached_font(false);
    if !ui_font.is_invalid() {
        for id in [IDC_EDIT_SEARCH, IDC_M_EDIT_WORD, IDC_M_EDIT_PINYIN, IDC_M_EDIT_WEIGHT,
                   IDC_BTN_SEARCH, IDC_M_BTN_SAVE, IDC_M_BTN_DEL, IDC_BTN_PREV, IDC_BTN_NEXT] {
            if let Ok(c) = unsafe { GetDlgItem(hwnd, id as i32) } {
                if !c.0.is_null() {
                    unsafe { SendMessageW(c, WM_SETFONT, WPARAM(ui_font.0 as usize), LPARAM(1)); }
                }
            }
        }
    }
    // 原生垂直滚动条(行数超可视区才用,范围见 layout_scrollbar)。
    unsafe {
        let cls: Vec<u16> = "SCROLLBAR".encode_utf16().chain(std::iter::once(0)).collect();
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0), PCWSTR(cls.as_ptr()), PCWSTR::null(),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | (SBS_VERT as u32)),
            0, 0, 10, 10, hwnd, HMENU(IDC_SCROLL as usize as *mut _), hinst, None,
        );
    }
    // 初始布局。
    layout_children(hwnd);
}

/// 按当前客户区重排所有子控件 + 列表几何(可调大小核心,WM_SIZE 也调它)。
fn layout_children(hwnd: HWND) {
    let mut rc = RECT::default();
    unsafe { let _ = GetClientRect(hwnd, &mut rc); }
    let cw = rc.right - rc.left;
    let ch = rc.bottom - rc.top;

    // 列表可视区(表头之下、加词区之上、滚动条之左)。
    let list_x = MARGIN;
    let list_y = LIST_TOP + HDR_H;
    let list_w = cw - MARGIN * 2 - SCROLL_W;
    let list_h = (ch - BTN_BAR_H - list_y - 8).max(ROW_H);

    // 滚动条(列表右侧,高度=列表高)。
    let sb = unsafe { GetDlgItem(hwnd, IDC_SCROLL as i32) };
    if let Ok(sb) = sb {
        if !sb.0.is_null() {
            unsafe {
                let _ = SetWindowPos(sb, HWND_TOP, list_x + list_w, list_y, SCROLL_W, list_h, SWP_SHOWWINDOW);
            }
            layout_scrollbar(hwnd, list_h);
        }
    }

    // 底部翻页/状态行(贴客户区底):上一页/下一页 在左,页码·总数文字画在右(见 paint)。
    let base = ch - BTN_BAR_H;
    let oy = base + 12;
    let set = |id: u16, x: i32, y: i32, w: i32, h: i32| unsafe {
        if let Ok(c) = GetDlgItem(hwnd, id as i32) {
            if !c.0.is_null() {
                let _ = SetWindowPos(c, HWND_TOP, x, y, w, h, SWP_SHOWWINDOW);
            }
        }
    };
    // ① 顶部搜索行:「搜索:」标签(绘制) + 输入框 + [搜索]。
    let lbl_w = 48;
    let sw2 = cw - MARGIN * 2 - lbl_w - 84;
    set(IDC_EDIT_SEARCH, MARGIN + lbl_w, SEARCH_TOP, sw2, SEARCH_H);
    set(IDC_BTN_SEARCH, MARGIN + lbl_w + sw2 + 8, SEARCH_TOP, 76, SEARCH_H);
    // ② 编辑行:「词语/拼音/词频」标签(绘制) + 三输入框 + [保存][删除]。
    let ew_word = ((cw - MARGIN * 2) * 30) / 100;
    let ew_py = ((cw - MARGIN * 2) * 24) / 100;
    let ew_wt = ((cw - MARGIN * 2) * 12) / 100;
    let mut ex = MARGIN + lbl_w;
    set(IDC_M_EDIT_WORD, ex, EDIT_TOP, ew_word, EDIT_H); ex += ew_word + lbl_w;
    set(IDC_M_EDIT_PINYIN, ex, EDIT_TOP, ew_py, EDIT_H); ex += ew_py + lbl_w;
    set(IDC_M_EDIT_WEIGHT, ex, EDIT_TOP, ew_wt, EDIT_H); ex += ew_wt + 6;
    let remain = cw - MARGIN - ex;
    let btn_w = ((remain - 6) / 2).max(56);
    set(IDC_M_BTN_SAVE, ex, EDIT_TOP, btn_w, EDIT_H); ex += btn_w + 6;
    set(IDC_M_BTN_DEL, ex, EDIT_TOP, btn_w, EDIT_H);
    // ③ 底部翻页按钮(页码·总数文字在其右,见 paint)。
    set(IDC_BTN_PREV, MARGIN, oy, 84, 26);
    set(IDC_BTN_NEXT, MARGIN + 92, oy, 84, 26);
}

/// 按当前 tab 的行数/可视行设置滚动条范围。
fn layout_scrollbar(hwnd: HWND, list_h: i32) {
    let st = global_state();
    let (n_rows, scroll) = {
        let g = st.lock().unwrap();
        let s = g.borrow();
        match s.tab {
            Tab::User => (s.rows.len() as i32, s.scroll),
            Tab::All => (s.main_rows.len() as i32, s.main_scroll),
        }
    };
    let visible = (list_h / ROW_H).max(1);
    let max_scroll = (n_rows - visible).max(0);
    let sb = unsafe { GetDlgItem(hwnd, IDC_SCROLL as i32) };
    if let Ok(sb) = sb {
        if !sb.0.is_null() {
            let mut si = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                nMin: 0,
                nMax: (n_rows - 1).max(0),
                nPage: visible as u32,
                nPos: scroll.clamp(0, max_scroll),
                ..Default::default()
            };
            unsafe { let _ = SetScrollInfo(sb, SB_CTL, &mut si, true); }
            si.fMask = SIF_POS;
        }
    }
}

fn ensure_window() -> Result<HWND> {
    let st = global_state();
    let existing = st.lock().unwrap().borrow().hwnd;
    if let Some(hwnd) = existing {
        if unsafe { IsWindow(hwnd) }.as_bool() {
            return Ok(hwnd);
        }
        st.lock().unwrap().borrow_mut().hwnd = None;
    }
    register_class()?;
    let hinst = unsafe { GetModuleHandleW(None) }?;
    let class_name: Vec<u16> = WIN_CLASS.encode_utf16().chain(std::iter::once(0)).collect();
    let title: Vec<u16> = WIN_TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    // 可调大小(WS_OVERLAPPEDWINDOW 含 THICKFRAME+MAXIMIZEBOX)。初始 560x560 客户区,
    // AdjustWindowRectEx 把标题栏算进外框(否则客户区被压、底部加词区看不见)。
    //
    // 2026-09-04 稳定性修复:**去掉 WS_EX_TOPMOST**。
    //   这是一个可获得焦点的管理窗口,不是候选浮窗。常驻顶层的可聚焦窗口压在
    //   notepad 上,配合 IME 注入进任意进程的上下文,容易让宿主焦点/绘制陷入假死
    //   (实测:点开词库窗空白、等一会儿 notepad 连带关闭)。改普通层级,由用户
    //   点击置前,不抢顶层;绘制期也不再每帧触发顶层重排。
    let style = WS_OVERLAPPEDWINDOW.0 & !(WS_DLGFRAME.0); // 保留可调边框+最大化
    // 初始客户区 800x560(2026-09-04 再加宽:编辑行输入框随宽度等比拉伸,
    // 默认 660 时「删除」按钮仍被右边距裁掉约 2 字宽 → 一次给够,免去用户手动拉大)。
    let mut wr = RECT { left: 0, top: 0, right: 800, bottom: 560 };
    unsafe { let _ = AdjustWindowRectEx(&mut wr, WINDOW_STYLE(style), false, WINDOW_EX_STYLE(0)); }
    let outer_w = wr.right - wr.left;
    let outer_h = wr.bottom - wr.top;
    let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let x = (sw - outer_w) / 2;
    let y = (sh - outer_h) / 2;
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WINDOW_STYLE(style),
            x, y, outer_w, outer_h,
            None, None, hinst, None,
        )
    }?;
    create_children(hwnd);
    st.lock().unwrap().borrow_mut().hwnd = Some(hwnd);
    #[cfg(feature = "dllentry_log")]
    crate::com_class_factory::log_dll_entry(&format!("UserDict: created hwnd={:p}", hwnd.0));
    Ok(hwnd)
}

/// 打开/切换词库管理窗口。
pub(crate) fn toggle() {
    let st = global_state();
    let visible = st.lock().unwrap().borrow().visible;
    if visible {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("UserDict: toggle OFF");
        hide_impl();
        return;
    }
    #[cfg(feature = "dllentry_log")]
    crate::com_class_factory::log_dll_entry("UserDict: toggle ON");
    reload_rows();
    // 开窗即铺当前搜索(空=全库 top 词),灵犀式开箱即用。
    reload_main_page();
    if let Ok(hwnd) = ensure_window() {
        layout_children(hwnd); // 开窗即按客户区排好
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let _ = InvalidateRect(hwnd, None, true);
        }
        st.lock().unwrap().borrow_mut().visible = true;
    }
}

fn hide_impl() {
    let st = global_state();
    let hwnd = st.lock().unwrap().borrow_mut().hwnd.take();
    if let Some(hwnd) = hwnd {
        unsafe { let _ = DestroyWindow(hwnd); }
    }
    st.lock().unwrap().borrow_mut().visible = false;
}

pub(crate) fn destroy() {
    hide_impl();
}

// ── 操作 ────────────────────────────────────────────────────────────────────
fn get_edit_text(hwnd: HWND, id: u16) -> String {
    let ctl = unsafe { GetDlgItem(hwnd, id as i32) };
    let Ok(ctl) = ctl else { return String::new(); };
    if ctl.0.is_null() { return String::new(); }
    let len = unsafe { GetWindowTextLengthW(ctl) };
    if len <= 0 { return String::new(); }
    let mut buf = vec![0u16; (len + 1) as usize];
    let n = unsafe { GetWindowTextW(ctl, &mut buf) };
    String::from_utf16_lossy(&buf[..n as usize])
}

fn set_edit_text(hwnd: HWND, id: u16, text: &str) {
    let ctl = unsafe { GetDlgItem(hwnd, id as i32) };
    let Ok(ctl) = ctl else { return; };
    if ctl.0.is_null() { return; }
    let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { let _ = SetWindowTextW(ctl, PCWSTR(wide.as_mut_ptr())); }
}

fn refresh_ui(hwnd: HWND) {
    reload_rows();
    layout_children(hwnd); // 行数变了 → 滚动条范围重算
    unsafe { let _ = InvalidateRect(hwnd, None, true); }
}

/// 同步滚动条滑块位置(WM_VSCROLL/WM_MOUSEWHEEL 共用)。
fn sync_scrollbar_pos(hwnd: HWND, pos: i32) {
    if let Ok(sb) = unsafe { GetDlgItem(hwnd, IDC_SCROLL as i32) } {
        if !sb.0.is_null() {
            let mut si = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_POS, nPos: pos, ..Default::default()
            };
            unsafe { let _ = SetScrollInfo(sb, SB_CTL, &mut si, true); }
        }
    }
}

/// 全部词库搜索(搜索按钮/回车)。
fn on_search(hwnd: HWND) {
    let term = get_edit_text(hwnd, IDC_EDIT_SEARCH).trim().to_string();
    reload_main_rows(&term); // 允许空:空=全表 top 词
    layout_children(hwnd); // 结果数变了 → 滚动条范围重算
    unsafe { let _ = InvalidateRect(hwnd, None, true); }
}

/// 翻页(delta=+1 下一页 / -1 上一页),带边界钳制。
fn on_page(hwnd: HWND, delta: i32) {
    {
        let st = global_state();
        let g = st.lock().unwrap();
        let mut s = g.borrow_mut();
        let max_page = s.total.saturating_sub(1) / PAGE_SIZE;
        let np = (s.page as i32 + delta).clamp(0, max_page as i32) as usize;
        if np == s.page { return; }
        s.page = np;
    }
    reload_main_page();
    layout_children(hwnd);
    unsafe { let _ = InvalidateRect(hwnd, None, true); }
}

/// 点中全部词库某行:记下选中 + 回填编辑框(词/拼音/权重)供改。
fn on_main_row_click(hwnd: HWND, idx: usize) {
    let (key, value, weight) = {
        let st = global_state();
        let g = st.lock().unwrap();
        let s = g.borrow();
        match s.main_rows.get(idx) {
            Some(r) => (r.key.clone(), r.value.clone(), r.weight),
            None => return,
        }
    };
    global_state().lock().unwrap().borrow_mut().main_sel = Some(idx);
    set_edit_text(hwnd, IDC_M_EDIT_WORD, &value);
    set_edit_text(hwnd, IDC_M_EDIT_PINYIN, &key);
    set_edit_text(hwnd, IDC_M_EDIT_WEIGHT, &weight.to_string());
    unsafe { let _ = InvalidateRect(hwnd, None, true); }
}

/// 主库保存(加词/改权重):词+拼音+权重 → upsert。保存后重搜刷新。
fn on_main_save(hwnd: HWND) {
    let word = get_edit_text(hwnd, IDC_M_EDIT_WORD).trim().to_string();
    let pinyin = get_edit_text(hwnd, IDC_M_EDIT_PINYIN).trim().to_lowercase();
    let wt_s = get_edit_text(hwnd, IDC_M_EDIT_WEIGHT).trim().to_string();
    if word.is_empty() || pinyin.is_empty() { return; }
    let weight: i64 = wt_s.parse().unwrap_or(-1);
    if weight < 0 {
        // 权重非法:提示用同 key 最高词频(外挂「新词提频排第一」语义)。
        let cap: Vec<u16> = "权重".encode_utf16().chain(std::iter::once(0)).collect();
        let hint = format!("权重须为非负整数。当前「{}」主库最高词频 = {}", pinyin, crate::ffi::main_dict_max_weight(&pinyin));
        let mut tw: Vec<u16> = hint.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { MessageBoxW(hwnd, PCWSTR(tw.as_mut_ptr()), PCWSTR(cap.as_ptr()), MB_OK | MB_ICONINFORMATION); }
        return;
    }
    if !crate::ffi::main_dict_writable() {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry("UserDict: main save FAIL (engine lacks main_* exports)");
        return;
    }
    if crate::ffi::main_dict_upsert(&pinyin, &word, weight) {
        // 保存成功:重拉当前页(新权重/新词反映到列表)。
        reload_main_page();
        layout_children(hwnd);
        unsafe { let _ = InvalidateRect(hwnd, None, true); }
    }
}

/// 主库删除选中词条(确认后删,再重搜刷新)。
fn on_main_delete(hwnd: HWND) {
    let sel = global_state().lock().unwrap().borrow().main_sel;
    let Some(idx) = sel else { return; };
    let (key, value) = {
        let st = global_state();
        let g = st.lock().unwrap();
        let s = g.borrow();
        match s.main_rows.get(idx) {
            Some(r) => (r.key.clone(), r.value.clone()),
            None => return,
        }
    };
    let cap: Vec<u16> = "删除主库词条".encode_utf16().chain(std::iter::once(0)).collect();
    let msg = format!("确定从主库删除「{}」({})?此操作直接改主库文件。", value, key);
    let mut mw: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
    let r = unsafe { MessageBoxW(hwnd, PCWSTR(mw.as_mut_ptr()), PCWSTR(cap.as_ptr()), MB_OKCANCEL | MB_ICONWARNING) };
    if r != IDOK { return; }
    if crate::ffi::main_dict_delete(&key, &value) > 0 {
        // 删除后:若当前页被删空且不是第 0 页,回退一页;否则重拉当前页。
        {
            let st = global_state();
            let g = st.lock().unwrap();
            let mut s = g.borrow_mut();
            if s.page > 0 && s.main_rows.len() <= 1 { s.page -= 1; }
        }
        reload_main_page();
        layout_children(hwnd);
        unsafe { let _ = InvalidateRect(hwnd, None, true); }
    }
}

// ── WndProc ─────────────────────────────────────────────────────────────────
/// 外层防护:TSF 注入进任意进程,本窗口任何 panic 若透出 = 宿主(notepad/explorer)一起崩。
/// 用 catch_unwind 兜住,异常只丢本窗口一次绘制/交互,绝不传染宿主。
/// (2026-09-04 稳定性兜底:用户实测「点词库空白、notepad 连带关闭」)
unsafe extern "system" fn userdict_wnd_proc(
    hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM,
) -> LRESULT {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        userdict_wnd_proc_inner(hwnd, msg, wparam, lparam)
    }));
    match r {
        Ok(v) => v,
        Err(_) => {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!("UserDict: PANIC caught msg={msg}"));
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

unsafe fn userdict_wnd_proc_inner(
    hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM,
) -> LRESULT {
    ulog!("UserDict: wndproc msg={:#x}", msg);
    match msg {
        WM_PAINT => { ulog!("UserDict: WM_PAINT enter"); paint_window(hwnd); ulog!("UserDict: WM_PAINT done"); LRESULT(0) }
        WM_CLOSE => { hide_impl(); LRESULT(0) }
        WM_CREATE => { ulog!("UserDict: WM_CREATE"); LRESULT(0) }
        WM_GETMINMAXINFO => {
            // 最小尺寸:防止拉太小布局挤没。
            let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
            mmi.ptMinTrackSize.x = MIN_W;
            mmi.ptMinTrackSize.y = MIN_H;
            LRESULT(0)
        }
        WM_SIZE => {
            ulog!("UserDict: WM_SIZE enter");
            layout_children(hwnd);
            let _ = InvalidateRect(hwnd, None, true);
            ulog!("UserDict: WM_SIZE done");
            LRESULT(0)
        }
        WM_VSCROLL => {
            // 原生滚动条 → 更新当前 tab 的 scroll。
            let code = (wparam.0 & 0xFFFF) as i32;
            let pos = ((wparam.0 >> 16) & 0xFFFF) as i32;
            let st = global_state();
            let mut g = st.lock().unwrap();
            let mut s = g.borrow_mut();
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let list_h = (rc.bottom - BTN_BAR_H - (LIST_TOP + HDR_H) - 8).max(ROW_H);
            let visible = (list_h / ROW_H).max(1);
            let (n_rows, cur) = match s.tab {
                Tab::User => (s.rows.len() as i32, s.scroll),
                Tab::All => (s.main_rows.len() as i32, s.main_scroll),
            };
            let max_scroll = (n_rows - visible).max(0);
            let new_scroll = (match code as i32 {
                x if x == SB_LINEUP.0 => cur - 1,
                x if x == SB_LINEDOWN.0 => cur + 1,
                x if x == SB_PAGEUP.0 => cur - visible,
                x if x == SB_PAGEDOWN.0 => cur + visible,
                x if x == SB_TOP.0 => 0,
                x if x == SB_BOTTOM.0 => max_scroll,
                x if x == SB_THUMBPOSITION.0 || x == SB_THUMBTRACK.0 => pos,
                _ => cur,
            })
            .clamp(0, max_scroll);
            match s.tab {
                Tab::User => s.scroll = new_scroll,
                Tab::All => s.main_scroll = new_scroll,
            }
            drop(s);
            drop(g);
            sync_scrollbar_pos(hwnd, new_scroll);
            let _ = InvalidateRect(hwnd, None, true);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as i16) as i32;
            let st = global_state();
            let mut g = st.lock().unwrap();
            let mut s = g.borrow_mut();
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let list_h = (rc.bottom - BTN_BAR_H - (LIST_TOP + HDR_H) - 8).max(ROW_H);
            let visible = (list_h / ROW_H).max(1);
            let (n_rows, cur) = match s.tab {
                Tab::User => (s.rows.len() as i32, s.scroll),
                Tab::All => (s.main_rows.len() as i32, s.main_scroll),
            };
            let max_scroll = (n_rows - visible).max(0);
            let new_scroll = (cur - delta / 120).clamp(0, max_scroll);
            match s.tab {
                Tab::User => s.scroll = new_scroll,
                Tab::All => s.main_scroll = new_scroll,
            }
            drop(s);
            drop(g);
            sync_scrollbar_pos(hwnd, new_scroll);
            let _ = InvalidateRect(hwnd, None, true);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let cx = (lparam.0 & 0xFFFF) as i16 as i32;
            let cy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let cw = rc.right - rc.left;
            // 行选中:置 main_sel 并回填编辑框(词/拼音/词频)供改/删。
            let list_y = LIST_TOP + HDR_H;
            if cy >= list_y {
                let row = (cy - list_y) / ROW_H;
                if row >= 0 {
                    let idx = {
                        let st = global_state();
                        let g = st.lock().unwrap();
                        let s = g.borrow();
                        let i = (s.main_scroll + row) as usize;
                        if i < s.main_rows.len() { Some(i) } else { None }
                    };
                    if let Some(i) = idx {
                        on_main_row_click(hwnd, i);
                    }
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16;
            let notif = ((wparam.0 >> 16) & 0xFFFF) as u16;
            if notif == BN_CLICKED as u16 {
                match id {
                    IDC_BTN_SEARCH => on_search(hwnd),
                    IDC_M_BTN_SAVE => on_main_save(hwnd),
                    IDC_M_BTN_DEL => on_main_delete(hwnd),
                    IDC_BTN_PREV => on_page(hwnd, -1),
                    IDC_BTN_NEXT => on_page(hwnd, 1),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ── 自绘 ────────────────────────────────────────────────────────────────────
/// 全局缓存字体(进程存活期复用,见 paint_window 注释)。bold=false→正文 -16, true→表头 -15 semibold。
fn cached_font(bold: bool) -> HFONT {
    static F_NORMAL: OnceLock<usize> = OnceLock::new();
    static F_BOLD: OnceLock<usize> = OnceLock::new();
    let cell = if bold { &F_BOLD } else { &F_NORMAL };
    let raw = *cell.get_or_init(|| unsafe {
        let face: Vec<u16> = "Microsoft YaHei UI".encode_utf16().chain(std::iter::once(0)).collect();
        // 与候选窗统一 Microsoft YaHei UI -17(候选窗/状态栏同字号,2026-09-04 对齐)。
        let (h, wt) = if bold { (-17, FW_SEMIBOLD.0 as i32) } else { (-17, FW_NORMAL.0 as i32) };
        CreateFontW(
            h, 0, 0, 0, wt, 0, 0, 0,
            DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32, DEFAULT_PITCH.0 as u32, PCWSTR(face.as_ptr()),
        ).0 as usize
    });
    HFONT(raw as *mut _)
}

fn paint_window(hwnd: HWND) {
    ulog!("UserDict: paint enter");
    let st = global_state();
    let (main_rows, main_scroll, main_sel, writable, search_term, page, total) = {
        let g = st.lock().unwrap();
        let s = g.borrow();
        (s.main_rows.clone(), s.main_scroll, s.main_sel, crate::ffi::main_dict_writable(),
         s.search_term.clone(), s.page, s.total)
    };
    ulog!("UserDict: paint state main_rows={} total={} page={} writable={}", main_rows.len(), total, page, writable);

    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        ulog!("UserDict: paint rect cw={} ch={}", cw, ch);

        let bg = CreateSolidBrush(COLORREF(COL_BG));
        FillRect(hdc, &rc, bg);
        let _ = DeleteObject(bg);
        SetBkMode(hdc, TRANSPARENT);

        // 2026-09-04 稳定性修复:字体全局缓存,只建一次。
        //   原实现每次 WM_PAINT 都 CreateFontW + DeleteObject。TSF 会注入任意进程,
        //   部分进程无字体缓存,高频 CreateFont 同一 CJK 字体反复触发 GDI 字体链接/
        //   回退链,极端情况首绘挂起。改为 OnceLock 缓存两个字体,进程存活期复用,
        //   不在每帧 DeleteObject(进程退出由系统回收,避免 use-after-free)。
        let font = cached_font(false);
        let hdr_font = cached_font(true);
        ulog!("UserDict: paint fonts ok");
        let old_font = if !font.is_invalid() { Some(SelectObject(hdc, font)) } else { None };

        // 顶部「输入:」行内标签(输入框左侧,与 layout 的 lbl_w=48 对齐)。
        let lbl_w = 48;
        SetTextColor(hdc, COLORREF(COL_TEXT));
        let mut sw: Vec<u16> = wcs("输入:");
        let mut src = RECT { left: MARGIN, top: SEARCH_TOP, right: MARGIN + lbl_w, bottom: SEARCH_TOP + SEARCH_H };
        DrawTextW(hdc, &mut sw, &mut src, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        // 编辑行行内标签(词语:/拼音:/词频:,与 layout 的 ex 步进对齐)。
        let ew_word = ((cw - MARGIN * 2) * 30) / 100;
        let ew_py = ((cw - MARGIN * 2) * 24) / 100;
        let ew_wt = ((cw - MARGIN * 2) * 12) / 100;
        let lbls = [
            ("词语:", MARGIN),
            ("拼音:", MARGIN + lbl_w + ew_word),
            ("词频:", MARGIN + lbl_w + ew_word + lbl_w + ew_py),
        ];
        for (t, x) in lbls {
            let mut tw: Vec<u16> = wcs(t);
            let mut trc = RECT { left: x, top: EDIT_TOP, right: x + lbl_w, bottom: EDIT_TOP + EDIT_H };
            DrawTextW(hdc, &mut tw, &mut trc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        }
        let _ = ew_wt;

        // 列表几何(同 layout_children)。
        let list_x = MARGIN;
        let list_y = LIST_TOP + HDR_H;
        let list_w = cw - MARGIN * 2 - SCROLL_W;
        let list_h = (ch - BTN_BAR_H - list_y - 8).max(ROW_H);

        // 表头(灵犀列:拼音 / 词语 / 词频)。
        let hdr_rc = RECT { left: list_x, top: LIST_TOP, right: list_x + list_w + SCROLL_W, bottom: LIST_TOP + HDR_H };
        let hb = CreateSolidBrush(COLORREF(COL_HDR));
        FillRect(hdc, &hdr_rc, hb);
        let _ = DeleteObject(hb);
        if !hdr_font.is_invalid() { SelectObject(hdc, hdr_font); }
        SetTextColor(hdc, COLORREF(COL_TEXT));
        let c_py = (list_w * 30) / 100;
        let c_word = (list_w * 46) / 100;
        let c_wt = (list_x + list_w - (list_x + c_py + c_word)).max(0);
        let headers = [("拼音", list_x, c_py), ("词语", list_x + c_py, c_word), ("词频", list_x + c_py + c_word, c_wt)];
        for (t, x, w) in headers {
            if w <= 0 { continue; }
            let mut tw: Vec<u16> = wcs(t);
            let mut trc = RECT { left: x + 6, top: LIST_TOP, right: x + w, bottom: LIST_TOP + HDR_H };
            DrawTextW(hdc, &mut tw, &mut trc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        }
        if !font.is_invalid() { SelectObject(hdc, font); }
        let sep = CreatePen(PS_SOLID, 1, COLORREF(COL_SEP));
        let old_pen = SelectObject(hdc, sep);
        let _ = MoveToEx(hdc, list_x, list_y, None);
        let _ = LineTo(hdc, list_x + list_w + SCROLL_W, list_y);
        SelectObject(hdc, old_pen);
        let _ = DeleteObject(sep);
        // 数据行。
        let visible = (list_h / ROW_H).max(1);
        for i in 0..visible {
            let idx = (main_scroll + i) as usize;
            if idx >= main_rows.len() { break; }
            let row = &main_rows[idx];
            let y = list_y + i * ROW_H;
            if main_sel == Some(idx) {
                let sel_rc = RECT { left: list_x, top: y, right: list_x + list_w, bottom: y + ROW_H };
                let sb = CreateSolidBrush(COLORREF(COL_SELBG));
                FillRect(hdc, &sel_rc, sb);
                let _ = DeleteObject(sb);
            }
            SetTextColor(hdc, COLORREF(if main_sel == Some(idx) { COL_ACCENT } else { COL_TEXT }));
            let cells = [
                (row.key.clone(), list_x, c_py),
                (row.value.clone(), list_x + c_py, c_word),
                (format!("{}", row.weight), list_x + c_py + c_word, c_wt),
            ];
            for (t, x, w) in cells {
                if w <= 0 { continue; }
                let mut tw: Vec<u16> = wcs(&t);
                let mut trc = RECT { left: x + 6, top: y, right: x + w, bottom: y + ROW_H };
                DrawTextW(hdc, &mut tw, &mut trc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
            }
            let lp = CreatePen(PS_SOLID, 1, COLORREF(COL_SEP));
            let op = SelectObject(hdc, lp);
            let _ = MoveToEx(hdc, list_x, y + ROW_H, None);
            let _ = LineTo(hdc, list_x + list_w, y + ROW_H);
            SelectObject(hdc, op);
            let _ = DeleteObject(lp);
        }
        // 空态。
        if main_rows.is_empty() {
            SetTextColor(hdc, COLORREF(COL_TEXT));
            let msg = if search_term.is_empty() { "词库为空" } else { "无匹配结果 — 换个拼音/词再搜" };
            let mut ew: Vec<u16> = wcs(msg);
            let mut erc = RECT { left: list_x, top: list_y + 30, right: list_x + list_w, bottom: list_y + 70 };
            DrawTextW(hdc, &mut ew, &mut erc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        }

        // 底部:页码·总数(画在 上一页/下一页 右侧,与 layout 的 oy 对齐)。
        SetTextColor(hdc, COLORREF(COL_TEXT));
        let base = ch - BTN_BAR_H;
        let oy = base + 12;
        let max_page = if total == 0 { 0 } else { (total - 1) / PAGE_SIZE };
        let info = if search_term.is_empty() {
            format!("全库共 {} 条 · 第 {}/{} 页", total, page + 1, max_page + 1)
        } else {
            format!("「{}」 共 {} 条 · 第 {}/{} 页", search_term, total, page + 1, max_page + 1)
        };
        let mut iw: Vec<u16> = wcs(&info);
        let info_x = MARGIN + 92 + 84 + 16;
        let mut irc = RECT { left: info_x, top: oy, right: cw - MARGIN, bottom: oy + 26 };
        DrawTextW(hdc, &mut iw, &mut irc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);

        if let Some(of) = old_font {
            SelectObject(hdc, of);
            // 字体走 cached_font 全局缓存,不 DeleteObject(进程退出系统回收)。
        }
        let _ = EndPaint(hwnd, &ps);
        ulog!("UserDict: paint endpaint done");
    }
}
