//! C ABI FFI 桥 — 动态加载 `prisIr_ime.dll` + 暴露拼音查询接口给宿主(C / Python ctypes / Chromium Mojo)
//!
//! T3 阶段:本模块承担「拼音引擎进程外化」的核心桥梁。
//!   - `prisIr_ime.dll` 由 `prisIr_ime` crate 编出,在 Windows 上是真实的 Rust cdylib。
//!   - 本模块通过 `LoadLibraryA` + `GetProcAddress` 在运行时动态拉起,不在链接期静态耦合,
//!     这样 `prisIr_ime_tsf` 单独编译不需要 `rusqlite`,与 D1 决策一致。
//!
//! 暴露给宿主的 5 个 C ABI:
//!   - `prisir_tsf_load_engine(db_path) -> handle`  — 加载词库
//!   - `prisir_tsf_query(handle, pinyin) -> json`    — 候选查询(JSON 数组)
//!   - `prisir_tsf_smart_sentence(handle, pinyin) -> str` — 整句首选
//!   - `prisir_tsf_free_string(s)`                  — 释放 query / smart 返回串
//!   - `prisir_tsf_free_engine(handle)`             — 释放引擎
//!
//! T2 阶段保留的两个接口(`prisir_tsf_version` / `prisir_tsf_echo`)继续导出,
//! 以兼容 T2 的最小烟囱测试。
//!
//! DLL 路径查找顺序(每次 `load_ime_dll()` 调用只走一次,后续走 `OnceLock` 缓存):
//!   1. `PRISIR_IME_DLL` 环境变量
//!   2. `%LOCALAPPDATA%\Prisir\Browser\prisIr_ime.dll`
//!   3. 当前目录下的 `prisIr_ime.dll`(开发期)
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::OnceLock;

use windows::core::PCSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

// ============================================================
// 类型别名 — prisIr_ime.dll 的 C ABI 签名(对齐 prisIr_ime/src/ffi.rs)
// ============================================================

type FnLoad = unsafe extern "C" fn(*const c_char, i32) -> *mut c_void;
type FnQuery = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char;
type FnSmart = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char;
type FnLearn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char);
type FnFreeStr = unsafe extern "C" fn(*mut c_char);
type FnFree = unsafe extern "C" fn(*mut c_void);
// 学习词库管理(Step 2,2026-09-04):引擎侧新增 4 导出。
type FnUserList = unsafe extern "C" fn(*mut c_void) -> *mut c_char;
type FnUserAdd = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> i32;
type FnUserRemove = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> i32;
type FnUserClear = unsafe extern "C" fn(*mut c_void) -> i32;
// 主词库搜索(只读)。
type FnDictSearch = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char;
// 主词库分页搜索 + 计数(2026-09-04 灵犀式分页)。
type FnDictSearchPage = unsafe extern "C" fn(*mut c_void, *const c_char, i32, i32) -> *mut c_char;
type FnDictCount = unsafe extern "C" fn(*mut c_void, *const c_char) -> i64;
// 主库可写(2026-09-04,删/加/改权重)。
type FnMainDelete = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> i32;
type FnMainUpsert = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, i64) -> i32;
type FnMainMaxWeight = unsafe extern "C" fn(*mut c_void, *const c_char) -> i64;

// ============================================================
// ImeDll — 包装好的 DLL 句柄 + 函数指针集合
// ============================================================

pub(crate) struct ImeDll {
    /// 模块句柄(防泄漏,只在进程退出时释放,符合 COM in-proc server 生命周期)
    _handle: HMODULE,
    load: FnLoad,
    query: FnQuery,
    smart: FnSmart,
    learn: FnLearn,
    free_string: FnFreeStr,
    free: FnFree,
    // 学习词库管理(Step 2):旧引擎 DLL 可能没这 4 个导出 → Option,None 时词库管理窗口提示升级引擎。
    user_list: Option<FnUserList>,
    user_add: Option<FnUserAdd>,
    user_remove: Option<FnUserRemove>,
    user_clear: Option<FnUserClear>,
    dict_search: Option<FnDictSearch>,
    dict_search_page: Option<FnDictSearchPage>,
    dict_count: Option<FnDictCount>,
    main_delete: Option<FnMainDelete>,
    main_upsert: Option<FnMainUpsert>,
    main_max_weight: Option<FnMainMaxWeight>,
}

// `HMODULE` 内部是 `*mut c_void`,不天然 Send/Sync。但本 IME 是进程内 COM 单线程服务,
// DLL 句柄 + 函数指针在初始化后只读,显式声明 Send/Sync 以装进 `OnceLock<Result<_, _>>`。
// SAFETY: ImeDll 初始化后所有字段不可变,只读访问是线程安全的(进程内 COM 也只一个 STA 线程)。
unsafe impl Send for ImeDll {}
unsafe impl Sync for ImeDll {}

// 静态单次缓存 — 一进程只 LoadLibrary 一次,后续直接拿函数指针。
// 用 `Result<ImeDll, String>` 是为了把加载错误永久缓存(避免每次调用都重 LoadLibrary)。
static IME_DLL: OnceLock<Result<ImeDll, String>> = OnceLock::new();

// ============================================================
// 公开 API — 给同 crate 其他模块调
// ============================================================

/// 拿到已加载的 ImeDll 引用(失败时 `Err(msg)` 永久缓存)。
/// 返回 `'static` 因为 OnceLock 内部数据生命周期为进程。
pub(crate) fn load_ime_dll() -> Result<&'static ImeDll, String> {
    IME_DLL
        .get_or_init(|| unsafe { try_load_ime_dll() })
        .as_ref()
        .map_err(|e| e.clone())
}

// ============================================================
// 加载逻辑
// ============================================================

unsafe fn try_load_ime_dll() -> Result<ImeDll, String> {
    let path = locate_ime_dll_path();
    #[cfg(feature = "dllentry_log")]
    crate::com_class_factory::log_dll_entry(&format!("try_load_ime_dll: path={path}"));
    let path_c = CString::new(path.clone()).map_err(|e| format!("bad dll path {path:?}: {e}"))?;

    // Windows 上 `PCSTR` 期望 `*const u8`(字节指针)。
    let h = LoadLibraryA(PCSTR(path_c.as_bytes_with_nul().as_ptr()))
        .map_err(|e| format!("LoadLibraryA({path}) failed: {e}"))?;

    // 拉所有需要的符号。任一缺失直接返回 Err,触发后续 5 个 FFI 一致返 null。
    macro_rules! sym {
        ($name:literal, $ty:ty) => {{
            let bytes = concat!($name, "\0").as_bytes();
            match GetProcAddress(h, PCSTR(bytes.as_ptr())) {
                Some(p) => std::mem::transmute::<_, $ty>(p as *const ()),
                None => return Err(format!("missing symbol: {}", $name)),
            }
        }};
    }

    // 学习词库管理 4 导出单独容忍缺失(Option):旧引擎 DLL 无这些符号时,
    // 核心打字 5 导出已齐仍可用,词库管理窗口另提示「引擎过旧请升级」。
    macro_rules! sym_opt {
        ($name:literal, $ty:ty) => {{
            let bytes = concat!($name, "\0").as_bytes();
            GetProcAddress(h, PCSTR(bytes.as_ptr()))
                .map(|p| std::mem::transmute::<_, $ty>(p as *const ()))
        }};
    }

    Ok(ImeDll {
        _handle: h,
        load: sym!("prisir_ime_load", FnLoad),
        query: sym!("prisir_ime_query", FnQuery),
        smart: sym!("prisir_ime_smart_sentence", FnSmart),
        learn: sym!("prisir_ime_learn", FnLearn),
        free_string: sym!("prisir_ime_free_string", FnFreeStr),
        free: sym!("prisir_ime_free", FnFree),
        user_list: sym_opt!("prisir_ime_user_list", FnUserList),
        user_add: sym_opt!("prisir_ime_user_add", FnUserAdd),
        user_remove: sym_opt!("prisir_ime_user_remove", FnUserRemove),
        user_clear: sym_opt!("prisir_ime_user_clear", FnUserClear),
        dict_search: sym_opt!("prisir_ime_dict_search", FnDictSearch),
        dict_search_page: sym_opt!("prisir_ime_dict_search_page", FnDictSearchPage),
        dict_count: sym_opt!("prisir_ime_dict_count", FnDictCount),
        main_delete: sym_opt!("prisir_ime_main_delete", FnMainDelete),
        main_upsert: sym_opt!("prisir_ime_main_upsert", FnMainUpsert),
        main_max_weight: sym_opt!("prisir_ime_main_max_weight", FnMainMaxWeight),
    })
}

fn locate_ime_dll_path() -> String {
    if let Ok(p) = std::env::var("PRISIR_IME_DLL") {
        if !p.is_empty() { return p; }
    }
    // 部署态常见位置(2026-09-01): 引擎 DLL 实际部署在 C:\PrisirIME\prisir_ime.dll。
    // ctfmon/notepad 进程上下文里 LOCALAPPDATA 指向系统账号或不存在 Browser 子目录,
    // 导致默认路径找不到 → 候选恒 0。加部署目录 fallback,存在即用。
    for cand in [r"C:\PrisirIME\prisir_ime.dll", r"C:\Program Files\PrisirIME\prisir_ime.dll"] {
        if std::path::Path::new(cand).exists() {
            return cand.to_string();
        }
    }
    if let Ok(appdata) = std::env::var("LOCALAPPDATA") {
        return format!("{}\\Prisir\\Browser\\prisIr_ime.dll", appdata);
    }
    "prisIr_ime.dll".to_string()
}

/// 找 ciku.db 词库绝对路径(2026-09-01 新增)。
/// `prisir_ime_load(db_path=null)` 让引擎用默认路径,但引擎默认路径在 ctfmon/notepad
/// 上下文里找不到词库 → load 返 null(实测 handle_null=true)→ 候选恒 0。
/// 改为显式传绝对路径:先 PRISIR_CIKU_DB 环境变量,再几个部署常见位置,取第一个存在的。
fn locate_ciku_db() -> Option<CString> {
    if let Ok(p) = std::env::var("PRISIR_CIKU_DB") {
        if !p.is_empty() && std::path::Path::new(&p).exists() {
            return CString::new(p).ok();
        }
    }
    for cand in [
        r"C:\PrisirIME\models\ciku.db",
        r"C:\PrisirIME\ciku.db",
        r"C:\Program Files\PrisirIME\models\ciku.db",
    ] {
        if std::path::Path::new(cand).exists() {
            return CString::new(cand).ok();
        }
    }
    None
}

/// 用显式词库路径加载引擎(2026-09-01 新增,替代 engine_handle 里传 null)。
/// 找不到 ciku.db 时退化为传 null(让引擎用默认路径,保持原行为)。
pub(crate) fn load_engine_with_default_db() -> *mut std::ffi::c_void {
    match locate_ciku_db() {
        Some(db) => {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!(
                "load_engine_with_default_db: db={:?}", db));
            prisir_tsf_load_engine(db.as_ptr())
        }
        None => {
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry("load_engine_with_default_db: ciku.db NOT FOUND, fallback null");
            prisir_tsf_load_engine(std::ptr::null())
        }
    }
}

// ============================================================
// C ABI 导出 — prisir_tsf_*
// ============================================================

/// 加载词库。失败返 null(可能是 dll 缺、db 不存在、bad path)。
#[no_mangle]
pub extern "C" fn prisir_tsf_load_engine(db_path: *const c_char) -> *mut c_void {
    match load_ime_dll() {
        Ok(dll) => {
            // build_index=1(2026-09-02 修「翻页卡顿」): 启用引擎的全内存 Trie 索引。
            // 引擎 prisir_ime 自带 MemoryIndex(trie.rs)+ load_or_build_index(带 .idx 持久化
            // 缓存,指纹对上秒开,否则首次构建落盘)。之前传 0 → 每次按键走 SQLite,长拼音 11ms+
            // 卡顿;启用后前缀查询走内存,O(前缀长),对齐外挂式全内存 trie 体验。
            let h = unsafe { (dll.load)(db_path, 1) };
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!(
                "load_engine: dll_ok handle_null={} (build_index=1, mem-trie ON)", h.is_null()));
            h
        }
        Err(e) => {
            eprintln!("[prisir_tsf] load_ime_dll failed: {e}");
            #[cfg(feature = "dllentry_log")]
            crate::com_class_factory::log_dll_entry(&format!("load_engine: load_ime_dll FAIL {e}"));
            std::ptr::null_mut()
        }
    }
}

/// 候选查询。返 JSON 数组字符串,调用方须 `prisir_tsf_free_string` 释放。
/// 输入 / 输出都按 UTF-8 处理(与 `prisIr_ime.dll` 一致)。
#[no_mangle]
pub extern "C" fn prisir_tsf_query(handle: *mut c_void, input: *const c_char) -> *mut c_char {
    match load_ime_dll() {
        Ok(dll) => unsafe { (dll.query)(handle, input) },
        Err(_) => std::ptr::null_mut(),
    }
}

/// 整句首选。返拼接整句字符串,调用方须 `prisir_tsf_free_string` 释放。
#[no_mangle]
pub extern "C" fn prisir_tsf_smart_sentence(handle: *mut c_void, input: *const c_char) -> *mut c_char {
    match load_ime_dll() {
        Ok(dll) => unsafe { (dll.smart)(handle, input) },
        Err(_) => std::ptr::null_mut(),
    }
}

/// 学习用户选择 — T3 阶段暂未在本侧用,T8 沙盒 E2E 时再走。
#[no_mangle]
pub extern "C" fn prisir_tsf_learn(handle: *mut c_void, input: *const c_char, selected: *const c_char) {
    if let Ok(dll) = load_ime_dll() {
        unsafe { (dll.learn)(handle, input, selected) };
    }
}

/// 释放 `prisir_tsf_query` / `_smart_sentence` 返回的字符串。
/// 优先走 dll 的 `prisir_ime_free_string`(因为它返回的是 dll 自己 CString::into_raw 的指针),
/// 仅当 dll 加载失败时退化为本地 CString 释放(兼容 T2 阶段 echo 串)。
#[no_mangle]
pub extern "C" fn prisir_tsf_free_string(s: *mut c_char) {
    if s.is_null() { return; }
    match load_ime_dll() {
        Ok(dll) => unsafe { (dll.free_string)(s) },
        Err(_) => unsafe { drop(CString::from_raw(s)); },
    }
}

/// 释放引擎句柄。失败时无操作。
#[no_mangle]
pub extern "C" fn prisir_tsf_free_engine(handle: *mut c_void) {
    if handle.is_null() { return; }
    if let Ok(dll) = load_ime_dll() {
        unsafe { (dll.free)(handle) };
    }
}

// ============================================================
// T2 兼容 — 这两个函数 T2 报告里已被外部测试程序引用,继续导出
// ============================================================

/// 返回版本字符串(调用方无须 free,生命周期为进程)。
#[no_mangle]
pub extern "C" fn prisir_tsf_version() -> *const c_char {
    // 静态 C 字符串,带 \0 终止符,作为进程内常量返回。
    concat!("prisir_ime_tsf 0.2.0 (T3 + ffi bridge to prisIr_ime.dll)\0").as_ptr() as *const c_char
}

// ============================================================
// 学习词库管理 crate 内 API(Step 2 词库管理窗口用,2026-09-04)
// 惰性复用 load_engine_with_default_db() 拿到的引擎句柄(缓存于 OnceLock),
// 词库管理窗口无需自己再 LoadLibrary/加载词库。引擎过旧缺导出 → 返回 None。
// ============================================================

/// 惰性拿词库管理用引擎句柄(进程级缓存)。
/// 存 usize 而非 *mut c_void(原始指针非 Send/Sync,进不了 OnceLock)。
/// SAFETY: 句柄来自 Box::into_raw,引擎进程级常驻,读出后转回指针只读使用。
fn mgmt_engine() -> Option<*mut c_void> {
    static H: OnceLock<usize> = OnceLock::new();
    let h = *H.get_or_init(|| load_engine_with_default_db() as usize);
    if h == 0 { None } else { Some(h as *mut c_void) }
}

/// 学习词库管理是否可用(引擎 DLL 已加载且带 user_* 导出)。
pub(crate) fn user_dict_available() -> bool {
    match (load_ime_dll(), mgmt_engine()) {
        (Ok(dll), Some(_)) => dll.user_list.is_some(),
        _ => false,
    }
}

/// 列出全部学习词(JSON 数组字符串)。不可用/失败 → None。
pub(crate) fn user_dict_list_json() -> Option<String> {
    let dll = load_ime_dll().ok()?;
    let h = mgmt_engine()?;
    let f = dll.user_list?;
    let ptr = unsafe { f(h) };
    if ptr.is_null() {
        return Some("[]".to_string());
    }
    let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    unsafe { (dll.free_string)(ptr) };
    Some(s)
}

/// 手动加词。可用且成功 → true。
pub(crate) fn user_dict_add(pinyin: &str, word: &str) -> bool {
    let (Ok(dll), Some(h)) = (load_ime_dll(), mgmt_engine()) else { return false; };
    let Some(f) = dll.user_add else { return false; };
    let (Ok(p), Ok(w)) = (CString::new(pinyin), CString::new(word)) else { return false; };
    unsafe { f(h, p.as_ptr(), w.as_ptr()) == 1 }
}

/// 删除某条学习词,返回删除行数。不可用 → 0。
pub(crate) fn user_dict_remove(pinyin: &str, word: &str) -> i32 {
    let (Ok(dll), Some(h)) = (load_ime_dll(), mgmt_engine()) else { return 0; };
    let Some(f) = dll.user_remove else { return 0; };
    let (Ok(p), Ok(w)) = (CString::new(pinyin), CString::new(word)) else { return 0; };
    unsafe { f(h, p.as_ptr(), w.as_ptr()) }
}

/// 清空全部学习记录。可用且成功 → true。
pub(crate) fn user_dict_clear() -> bool {
    let (Ok(dll), Some(h)) = (load_ime_dll(), mgmt_engine()) else { return false; };
    let Some(f) = dll.user_clear else { return false; };
    unsafe { f(h) == 1 }
}

/// 搜索主词库(JSON 数组字符串)。不可用/失败 → None。
pub(crate) fn dict_search_json(term: &str) -> Option<String> {
    let dll = load_ime_dll().ok()?;
    let h = mgmt_engine()?;
    let f = dll.dict_search?;
    let t = CString::new(term).ok()?;
    let ptr = unsafe { f(h, t.as_ptr()) };
    if ptr.is_null() {
        return Some("[]".to_string());
    }
    let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    unsafe { (dll.free_string)(ptr) };
    Some(s)
}

/// 分页搜索主词库(JSON 数组)。不可用 → None。
pub(crate) fn dict_search_page_json(term: &str, limit: i32, offset: i32) -> Option<String> {
    let dll = load_ime_dll().ok()?;
    let h = mgmt_engine()?;
    let f = dll.dict_search_page?;
    let t = CString::new(term).ok()?;
    let ptr = unsafe { f(h, t.as_ptr(), limit, offset) };
    if ptr.is_null() {
        return Some("[]".to_string());
    }
    let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    unsafe { (dll.free_string)(ptr) };
    Some(s)
}

/// 主库匹配总条数。不可用 → 0。
pub(crate) fn dict_count(term: &str) -> i64 {
    let (Ok(dll), Some(h)) = (load_ime_dll(), mgmt_engine()) else { return 0; };
    let Some(f) = dll.dict_count else { return 0; };
    let Ok(t) = CString::new(term) else { return 0; };
    unsafe { f(h, t.as_ptr()) }
}

// ============================================================
// 主库可写 crate 内 API(2026-09-04,词库管理「全部词库」tab 删/加/改用)
// ============================================================

/// 主库删除词条。返回删除行数(0=不可用/不存在/失败)。
pub(crate) fn main_dict_delete(pinyin: &str, word: &str) -> i32 {
    let (Ok(dll), Some(h)) = (load_ime_dll(), mgmt_engine()) else { return 0; };
    let Some(f) = dll.main_delete else { return 0; };
    let (Ok(p), Ok(w)) = (CString::new(pinyin), CString::new(word)) else { return 0; };
    unsafe { f(h, p.as_ptr(), w.as_ptr()) }
}

/// 主库 upsert 词条(加词/改权重)。成功 → true。
pub(crate) fn main_dict_upsert(pinyin: &str, word: &str, weight: i64) -> bool {
    let (Ok(dll), Some(h)) = (load_ime_dll(), mgmt_engine()) else { return false; };
    let Some(f) = dll.main_upsert else { return false; };
    let (Ok(p), Ok(w)) = (CString::new(pinyin), CString::new(word)) else { return false; };
    unsafe { f(h, p.as_ptr(), w.as_ptr(), weight) == 1 }
}

/// 同 key 主库 phrase 最高词频(改权重参考)。不可用 → 0。
pub(crate) fn main_dict_max_weight(pinyin: &str) -> i64 {
    let (Ok(dll), Some(h)) = (load_ime_dll(), mgmt_engine()) else { return 0; };
    let Some(f) = dll.main_max_weight else { return 0; };
    let Ok(p) = CString::new(pinyin) else { return 0; };
    unsafe { f(h, p.as_ptr()) }
}

/// 主库可写是否可用(引擎带 main_* 导出)。
pub(crate) fn main_dict_writable() -> bool {
    match (load_ime_dll(), mgmt_engine()) {
        (Ok(dll), Some(_)) => dll.main_delete.is_some() && dll.main_upsert.is_some(),
        _ => false,
    }
}

/// T2 阶段用作最小烟囱的 echo — T3 阶段保留作 ABI smoke。
/// 注:本函数返回的指针由本 crate 的 `prisir_tsf_free_string` 释放。
#[no_mangle]
pub extern "C" fn prisir_tsf_echo(input: *const c_char) -> *mut c_char {
    if input.is_null() { return std::ptr::null_mut(); }
    let cstr = unsafe { CStr::from_ptr(input) };
    let s = cstr.to_string_lossy().into_owned();
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

// ============================================================
// Rust-side helper — ipc.rs + 未来 chrome UI 调
// ============================================================

/// 返回 `prisir_tsf_version()` 的 `String`(自动 *copy* 进 std 字符串)。
///
/// ipc.rs 的 `version` method 调它给 chrome settings UI 显示;不走 dll,不需要 LoadLibrary。
pub fn prisir_tsf_version_string() -> String {
    let cstr = unsafe { CStr::from_ptr(prisir_tsf_version()) };
    cstr.to_string_lossy().into_owned()
}

// ============================================================
// 测试辅助 — 给 tests/tsf_smoke.rs 用 #[ignore] smoke
// ============================================================

/// 返回 DLL 路径查找结果(只跑路径解析,不实际 LoadLibrary)。
/// 给 smoke test 用来断言路径优先级正确,不需要真实 dll 存在。
pub fn locate_dll_path_for_test() -> String {
    locate_ime_dll_path()
}