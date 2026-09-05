//! IMM32 `.ime` 导出层 — 让 Prisir 进系统搜索框(SearchApp 只加载 IMM32 双栈 IME)。
//!
//! 背景(2026-09-03):Win10 SearchApp 不加载纯 TSF TIP,只认 IMM32 `.ime`。本模块把
//! Prisir 包成一个最小可行 IMM32 IME:导出 ImeProcessKey/ImeToAsciiEx 等,复用
//! `keystroke::PinyinBuffer` 状态机 + `ffi` 引擎桥(query/learn),通过 TRANSMSG
//! 消息机制把 组合串/候选/上屏 转发给系统。
//!
//! 与 TSF 路径的关系:同一 DLL 双导出,但**不共享可变状态** —— TSF 用 TSF 上下文,
//! IMM 用 HIMC 私有数据挂的 `ImmContext`(各自独立 PinyinBuffer + 引擎句柄)。
//!
//! 参考:rime-winime docs/WIN32_IMM_INTERNALS.md(TRANSMSG 对齐)、ReactOS imetable.h。
//! 消息流:
//!   ImeProcessKey 返 TRUE(吃键) → ImeToAsciiEx 产 TRANSMSG:
//!     输入中:WM_IME_STARTCOMPOSITION + WM_IME_COMPOSITION|GCS_COMPSTR + WM_IME_NOTIFY|IMN_CHANGECANDIDATE
//!     上屏:  WM_IME_COMPOSITION|GCS_RESULTSTR + WM_IME_ENDCOMPOSITION(+ learn)
//!   系统据此驱动组合串/候选窗/上屏。
#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::Ime::{
    IMEINFO, TRANSMSGLIST, HIMC,
    GCS_COMPSTR, GCS_RESULTSTR, IMN_CHANGECANDIDATE,
    IME_PROP_ACCEPT_WIDE_VKEY, IME_PROP_AT_CARET, IME_PROP_UNICODE,
    UI_CAP_2700, CPS_COMPLETE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_BACK, VK_ESCAPE, VK_NEXT, VK_PRIOR, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_NOTIFY, WM_IME_STARTCOMPOSITION,
};

use crate::keystroke::PinyinBuffer;

#[cfg(feature = "dllentry_log")]
fn log(msg: &str) {
    crate::com_class_factory::log_dll_entry(msg);
}
#[cfg(not(feature = "dllentry_log"))]
fn log(_msg: &str) {}

// ============================================================
// ImmContext — 每个 HIMC 一份私有输入上下文
// ============================================================

pub struct ImmContext {
    pub buffer: PinyinBuffer,
    pub engine: *mut c_void, // prisir_tsf 引擎句柄(load_engine_with_default_db)
    pub composing: bool,     // 是否已发 WM_IME_STARTCOMPOSITION
    /// 当前组合串(拼音 buffer 的镜像,供 ImmGetCompositionString(GCS_COMPSTR) 读)。
    pub comp_str: String,
    /// 待上屏结果串(供 ImmGetCompositionString(GCS_RESULTSTR) 读,上屏后置空)。
    pub result_str: String,
}
// HIMC 上下文只在单线程 IME 回调里访问,但句柄经 Mutex map 共享,声明 Send 以进 map。
unsafe impl Send for ImmContext {}

static CONTEXTS: Mutex<Option<HashMap<usize, Box<ImmContext>>>> = Mutex::new(None);

fn with_ctx<R>(himc: HIMC, f: impl FnOnce(&mut ImmContext) -> R) -> Option<R> {
    let mut guard = CONTEXTS.lock().ok()?;
    let map = guard.get_or_insert_with(HashMap::new);
    let ctx = map.get_mut(&(himc.0 as usize))?;
    Some(f(ctx))
}

fn insert_ctx(himc: HIMC, ctx: ImmContext) {
    if let Ok(mut guard) = CONTEXTS.lock() {
        guard.get_or_insert_with(HashMap::new).insert(himc.0 as usize, Box::new(ctx));
    }
}

fn remove_ctx(himc: HIMC) {
    if let Ok(mut guard) = CONTEXTS.lock() {
        if let Some(map) = guard.as_mut() {
            map.remove(&(himc.0 as usize));
        }
    }
}

// ============================================================
// 按键判定
// ============================================================

fn vk_u16(vk: VIRTUAL_KEY) -> u16 {
    vk.0
}

/// 该虚拟键是否由本 IME 消费(中文输入态下)。
fn is_ime_key(vk: u16) -> bool {
    matches!(vk, 0x41..=0x5A) // A-Z
        || vk == vk_u16(VK_BACK)
        || vk == vk_u16(VK_SPACE)
        || vk == vk_u16(VK_ESCAPE)
        || vk == vk_u16(VK_PRIOR)  // PageUp
        || vk == vk_u16(VK_NEXT)   // PageDown
        || (0x31..=0x39).contains(&vk) // 数字 1-9(选候选)
}

// ============================================================
// 导出:ImeInquire — 声明 IME 能力
// ============================================================

#[no_mangle]
pub unsafe extern "system" fn ImeInquire(
    lpimeinfo: *mut IMEINFO,
    lpszuiclass: *mut u16,
    dwsysteminfoflags: u32,
) -> BOOL {
    let _ = dwsysteminfoflags;
    if lpimeinfo.is_null() {
        return BOOL(0);
    }
    let info = &mut *lpimeinfo;
    info.dwPrivateDataSize = 0; // 私有数据我们自己用 map 管,不走 hIMC 内嵌
    // 能力位:at-caret + unicode + 接受宽 vkey。fdwProperty 组合参考简体 IME。
    info.fdwProperty = IME_PROP_AT_CARET | IME_PROP_UNICODE | IME_PROP_ACCEPT_WIDE_VKEY;
    info.fdwConversionCaps = 0x01 | 0x08; // IME_CMODE_NATIVE | IME_CMODE_FULLSHAPE
    info.fdwSentenceCaps = 0x10;          // IME_SMODE_CONVERSATION
    info.fdwUICaps = UI_CAP_2700;
    info.fdwSCSCaps = 0x02;               // SCS_CAP_COMPSTR
    info.fdwSelectCaps = 0x00;
    if !lpszuiclass.is_null() {
        let name: &[u16] = &[
            'P' as u16, 'r' as u16, 'i' as u16, 's' as u16, 'i' as u16, 'r' as u16,
            'I' as u16, 'M' as u16, 'M' as u16, 0,
        ];
        std::ptr::copy_nonoverlapping(name.as_ptr(), lpszuiclass, name.len());
    }
    log("IMM: ImeInquire OK");
    BOOL(1)
}

// ============================================================
// 导出:ImeSelect — 选/取消选,挂上下文
// ============================================================

#[no_mangle]
pub unsafe extern "system" fn ImeSelect(himc: HIMC, fselect: BOOL) -> BOOL {
    if fselect.0 != 0 {
        let engine = crate::ffi::load_engine_with_default_db();
        insert_ctx(
            himc,
            ImmContext { buffer: PinyinBuffer::new(), engine, composing: false, comp_str: String::new(), result_str: String::new() },
        );
        log(&format!("IMM: ImeSelect SELECT himc={:?} engine_null={}", himc, engine.is_null()));
    } else {
        remove_ctx(himc);
        log(&format!("IMM: ImeSelect UNSELECT himc={:?}", himc));
    }
    BOOL(1)
}

// ============================================================
// 导出:ImeProcessKey — 过滤器
// ============================================================

#[no_mangle]
pub unsafe extern "system" fn ImeProcessKey(
    himc: HIMC,
    uvirkey: u32,
    lparam: LPARAM,
    lpbkeystate: *const u8,
) -> BOOL {
    let _ = lpbkeystate;
    if lparam.0 == 0 {
        return BOOL(0); // 无 scan code(bits 16-23),拒
    }
    if himc.0.is_null() {
        return BOOL(0);
    }
    let vk = (uvirkey & 0xFF) as u16;
    let eat = is_ime_key(vk);
    log(&format!("IMM: ImeProcessKey vk=0x{:02X} eat={}", vk, eat));
    BOOL(if eat { 1 } else { 0 })
}

// ============================================================
// 导出:ImeToAsciiEx — 核心:驱动状态机产 TRANSMSG
// ============================================================

/// 往 TRANSMSGLIST 追加一条消息,返回是否追加成功(容量够)。
unsafe fn push_msg(list: *mut TRANSMSGLIST, cap: usize, message: u32, wparam: usize, lparam: isize) -> bool {
    if list.is_null() {
        return false;
    }
    let lst = &mut *list;
    let used = lst.uMsgCount as usize;
    if used >= cap {
        return false; // 溢出(最小核:丢弃;完整实现应进 hMsgBuf)
    }
    // TransMsg 是 [TRANSMSG;1] 弹性数组头,后续元素紧随其后(24 字节对齐)。
    let base = lst.TransMsg.as_mut_ptr();
    let slot = base.add(used);
    (*slot).message = message;
    (*slot).wParam = WPARAM(wparam);
    (*slot).lParam = LPARAM(lparam);
    lst.uMsgCount = (used + 1) as u32;
    true
}

#[no_mangle]
pub unsafe extern "system" fn ImeToAsciiEx(
    uvirkey: u32,
    uscancode: u32,
    lpbkeystate: *const u8,
    lptransmsglist: *mut TRANSMSGLIST,
    fustate: u32,
    himc: HIMC,
) -> u32 {
    let _ = uscancode;
    let _ = lpbkeystate;
    let _ = fustate;
    if lptransmsglist.is_null() || himc.0.is_null() {
        return 0;
    }
    // TRANSMSGLIST 容量:由 IMM32 分配的缓冲区,约定至少能放十几条。最小核取保守 16。
    const CAP: usize = 16;
    let vk = (uvirkey & 0xFF) as u16;

    let r = with_ctx(himc, |ctx| -> u32 {
        // 快照输入串(on_space/on_digit 会 reset() 清空 buf,learn 需要原始拼音)
        let input_snapshot = ctx.buffer.buf.clone();
        // 根据按键驱动状态机
        let mut want_commit: Option<String> = None;
        match vk {
            v if (0x41..=0x5A).contains(&v) => {
                ctx.buffer.on_char((v as u8 as char).to_ascii_lowercase());
                let cands = ctx.buffer.query_candidates(ctx.engine);
                ctx.buffer.set_candidates(cands);
            }
            v if v == vk_u16(VK_BACK) => {
                ctx.buffer.on_backspace();
                if !ctx.buffer.buf.is_empty() {
                    let cands = ctx.buffer.query_candidates(ctx.engine);
                    ctx.buffer.set_candidates(cands);
                }
            }
            v if v == vk_u16(VK_SPACE) => {
                want_commit = ctx.buffer.on_space();
            }
            v if (0x31..=0x39).contains(&v) => {
                want_commit = ctx.buffer.on_digit((v - 0x30) as u8);
            }
            v if v == vk_u16(VK_NEXT) => {
                ctx.buffer.page_down();
            }
            v if v == vk_u16(VK_PRIOR) => {
                ctx.buffer.page_up();
            }
            v if v == vk_u16(VK_ESCAPE) => {
                ctx.buffer.on_escape();
            }
            _ => {}
        }

        // 组合串镜像:每次按键后同步(组合串 = 当前拼音 buffer)。上屏/ESC 后 buffer 已空 → 组合串清空。
        ctx.comp_str = ctx.buffer.buf.clone();

        // 组装 TRANSMSG。
        if let Some(text) = want_commit {
            if !text.is_empty() {
                // 学习 + 上屏消息(input_snapshot 是 reset 前的原始拼音)
                if !input_snapshot.is_empty() {
                    let ci = std::ffi::CString::new(input_snapshot.clone()).unwrap_or_default();
                    let cs = std::ffi::CString::new(text.clone()).unwrap_or_default();
                    crate::ffi::prisir_tsf_learn(ctx.engine, ci.as_ptr(), cs.as_ptr());
                }
                // 修「Explorer 搜索 你好nihao 叠加」(2026-09-03 现象②):
                // 上屏前先清组合串(COMPSTR 空 + CPS_COMPLETE 丢弃残留 nihao),再发结果串。
                // 否则 Explorer 把原始拼音残留 + 结果串叠加显示。
                ctx.comp_str.clear();
                ctx.result_str = text.clone();
                // 1) 清组合串:GCS_COMPSTR 空 + lParam CPS_COMPLETE 让 IMM 丢弃旧组合内容
                push_msg(lptransmsglist, CAP, WM_IME_COMPOSITION, 0, CPS_COMPLETE.0 as isize);
                // 2) 结果串:GCS_RESULTSTR(ImmGetCompositionString 读 result_str)
                push_msg(lptransmsglist, CAP, WM_IME_COMPOSITION, GCS_RESULTSTR.0 as usize, 0);
                // 3) 结束组合
                push_msg(lptransmsglist, CAP, WM_IME_ENDCOMPOSITION, 0, 0);
                ctx.composing = false;
                let n = (*lptransmsglist).uMsgCount;
                log(&format!("IMM: ImeToAsciiEx COMMIT text={} msgs={}", text, n));
                return n;
            }
        }

        // 组合中(非上屏):STARTCOMPOSITION(首次) + COMPOSITION|GCS_COMPSTR + CHANGECANDIDATE
        if !ctx.composing {
            push_msg(lptransmsglist, CAP, WM_IME_STARTCOMPOSITION, 0, 0);
            ctx.composing = true;
        }
        push_msg(lptransmsglist, CAP, WM_IME_COMPOSITION, GCS_COMPSTR.0 as usize, 0);
        push_msg(lptransmsglist, CAP, WM_IME_NOTIFY, IMN_CHANGECANDIDATE as usize, 0);
        let n = (*lptransmsglist).uMsgCount;
        log(&format!("IMM: ImeToAsciiEx vk=0x{:02X} buf={} msgs={}", vk, ctx.buffer.buf, n));
        n
    });

    r.unwrap_or(0)
}

// ============================================================
// 导出:NotifyIME / ImeConfigure / ImeEscape / ImeDestroy / ImeSetActiveContext
// ============================================================

#[no_mangle]
pub unsafe extern "system" fn NotifyIME(himc: HIMC, dwaction: u32, dwindex: u32, dwvalue: u32) -> BOOL {
    let _ = (himc, dwaction, dwindex, dwvalue);
    BOOL(1)
}

#[no_mangle]
pub unsafe extern "system" fn ImeConfigure(hkl: *mut c_void, hwnd: HWND, dwmode: u32, lpdata: *mut c_void) -> BOOL {
    let _ = (hkl, hwnd, dwmode, lpdata);
    BOOL(1)
}

#[no_mangle]
pub unsafe extern "system" fn ImeEscape(himc: HIMC, uescape: u32, lpdata: *mut c_void) -> isize {
    let _ = (himc, uescape, lpdata);
    0
}

#[no_mangle]
pub unsafe extern "system" fn ImeDestroy(_reserved: u32) -> BOOL {
    BOOL(1)
}

#[no_mangle]
pub unsafe extern "system" fn ImeSetActiveContext(himc: HIMC, fflag: BOOL) -> BOOL {
    let _ = (himc, fflag);
    BOOL(1)
}

// ============================================================
// 导出:占位(IMM32 拒载防护 — .DEF 全列出,缺符号的系统调用安全返默认)
// ============================================================

#[no_mangle]
pub unsafe extern "system" fn ImeConversionList(
    himc: HIMC, src: *const u16, candlist: *mut c_void, buflen: u32, flag: u32,
) -> u32 {
    let _ = (himc, src, candlist, buflen, flag);
    0
}

#[no_mangle]
pub unsafe extern "system" fn ImeSetCompositionString(
    himc: HIMC, dwindex: u32, comp: *mut c_void, complen: u32, read: *mut c_void, readlen: u32,
) -> BOOL {
    let _ = (himc, dwindex, comp, complen, read, readlen);
    BOOL(0)
}

#[no_mangle]
pub unsafe extern "system" fn ImeRegisterWord(lpszreading: *const u16, style: u32, lpszstring: *const u16) -> BOOL {
    let _ = (lpszreading, style, lpszstring);
    BOOL(0)
}

#[no_mangle]
pub unsafe extern "system" fn ImeUnregisterWord(lpszreading: *const u16, style: u32, lpszstring: *const u16) -> BOOL {
    let _ = (lpszreading, style, lpszstring);
    BOOL(0)
}

#[no_mangle]
pub unsafe extern "system" fn ImeGetRegisterWordStyle(nitem: u32, stylebuf: *mut c_void) -> u32 {
    let _ = (nitem, stylebuf);
    0
}

#[no_mangle]
pub unsafe extern "system" fn ImeEnumRegisterWord(
    cb: *mut c_void, lpszreading: *const u16, style: u32, lpszstring: *const u16, data: *mut c_void,
) -> u32 {
    let _ = (cb, lpszreading, style, lpszstring, data);
    0
}

/// ImeGetImeMenuItems — ImeStudy 列为必需导出,缺它 ImmInstallIME 校验导出表时返 0x715。
/// 最小核不提供 IME 菜单,返 0(0 个菜单项)。
#[no_mangle]
pub unsafe extern "system" fn ImeGetImeMenuItems(
    himc: HIMC, dwflags: u32, dwtype: u32, lpimemenuitems: *mut c_void,
    lprightmenuitems: *mut c_void, dwdatasize: u32,
) -> u32 {
    let _ = (himc, dwflags, dwtype, lpimemenuitems, lprightmenuitems, dwdatasize);
    0
}
