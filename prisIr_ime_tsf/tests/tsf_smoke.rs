//! Smoke test — 静态产物校验 + T3 新增 PinyinBuffer + ffi path lookup + T4 注册表纯函数 + T5 daemon 纯结构 + 真 CATID + T6 ipc parse + T7 hotkey/method/ak-guard
//!
//! 检查项:
//!   T2 的 6 项:
//!     1. DLL 二进制文件存在(证明 cdylib 真编出文件,不是光过编译)
//!     2. EXE 二进制文件存在(证明 binary 真编出文件)
//!     3. Cargo.toml 关键依赖被正确写出(windows = "0.58", implement feature)
//!     4. lib.rs 暴露六个模块(ffi / tsf_input_processor / tsf_text_store / keystroke / com_class_factory / register)
//!     5. TsfInputProcessor 用 #[implement] 宏(违反红线 = 失败)
//!     6. 全代码无 HKLM 字样(T2/T3/T4/T5/T6/T7 阶段禁止 HKLM 注册)
//!   T3 新增 3 项:
//!     7. keystroke_buffer_echo       — PinyinBuffer a-z 累加 + 大写拒绝 + 退格 + ESC
//!     8. keystroke_buffer_digit_space — 数字键 / 空格上屏候选
//!     9. ffi_path_lookup              — DLL 路径查找顺序正确(env → LOCALAPPDATA → 当前目录)
//!   T3 隐式 1 项(#[ignore]):
//!    10. ffi_sanity_load              — 真正 LoadLibrary + prisir_tsf_load_engine (需要本地有 dll+db,默认跳过)
//!   T4 新增 2 项(**纯函数**,绝不调 do_register/do_unregister,避免污染用户 HKCU):
//!    11. register_path_strips_trailing_slash  — normalize_dll_path 切尾分隔符
//!    12. register_idempotent_paths            — build_reg_tree 同输入同输出(含 trailing slash)
//!   T5 新增 3 项(**纯函数**,绝不调 do_register/do_unregister/do_enable/do_disable/daemon 轮询,避免污染):
//!    13. daemon_path_parse_dll     — DaemonConfig::default() dll_path 是 prisir_ime_tsf.dll
//!    14. register_catid_real_msctf — CATID 必须是 {36C679D9-...} 真实 msctf.h 值
//!    15. daemon_wts_event_filter   — poll_interval_secs >= 1
//!   T6 新增 1 项(**纯 parse**,绝不调 register/unregister/enable/disable, 也不调 ffi query(避免 LoadLibrary 副作用)):
//!    16. ipc_request_parse          — version / 未知 method / parse error / query 缺 pinyin 全部返回正确 JSON
//!   T7 新增 3 项(**纯函数 / 静态扫**,绝不走 HKCU / ffi LoadLibrary / 网络):
//!    17. hotkey_normalize_vk_agrees_with_python — ALLOWED_VKS / normalize_vk / 激活键常量沿用 voice_input
//!    18. method_dispatch_table                    — InputMethod 枚举 + parse_method 接受所有别名
//!    19. aliyun_ak_invalid_protection             — 源码禁止含 aliyun / AccessKey 标识符(防泄露)

use std::path::PathBuf;

/// 优先用 CARGO_TARGET_TMPDIR(cargo test 期间运行)回退到 OUT_DIR,
/// 最后兜底为 `<crate_dir>/target/release`。
fn release_dir() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_TARGET_TMPDIR") {
        return PathBuf::from(p).join("../../../release");
    }
    if let Ok(p) = std::env::var("OUT_DIR") {
        // OUT_DIR 形如 target/release/build/<crate>-<hash>/out
        if let Some(target) = PathBuf::from(p).ancestors().nth(3) {
            return target.to_path_buf();
        }
    }
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    PathBuf::from(crate_dir).join("target").join("release")
}

// ============================================================
// T2 的 6 项
// ============================================================

#[test]
fn dll_binary_exists() {
    let dll = release_dir().join("prisir_ime_tsf.dll");
    assert!(dll.exists(), "DLL 未生成: {:?}", dll);
    let meta = std::fs::metadata(&dll).expect("DLL 元数据不可读");
    assert!(meta.len() > 0, "DLL 文件大小为 0: {:?}", dll);
    eprintln!("[smoke] DLL 路径 = {:?}", dll);
    eprintln!("[smoke] DLL 大小 = {} bytes", meta.len());
}

#[test]
fn exe_binary_exists() {
    let exe = release_dir().join("prisir_tsfsvc.exe");
    assert!(exe.exists(), "EXE 未生成: {:?}", exe);
    let meta = std::fs::metadata(&exe).expect("EXE 元数据不可读");
    assert!(meta.len() > 0, "EXE 文件大小为 0: {:?}", exe);
    eprintln!("[smoke] EXE 路径 = {:?}", exe);
    eprintln!("[smoke] EXE 大小 = {} bytes", meta.len());
}

#[test]
fn cargo_toml_has_real_windows_dep() {
    let cargo = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let toml_path = PathBuf::from(&cargo).join("Cargo.toml");
    let content = std::fs::read_to_string(&toml_path).expect("Cargo.toml 不可读");
    assert!(
        content.contains("windows = {"),
        "Cargo.toml 缺少 windows 依赖: {}",
        content
    );
    assert!(
        content.contains("Win32_UI_TextServices"),
        "Cargo.toml 缺少 Win32_UI_TextServices feature"
    );
    // implement feature 是 #[implement] 宏可用前提
    assert!(
        content.contains("\"implement\""),
        "Cargo.toml 缺少 implement feature(#[implement] 宏需要)"
    );
    // T3 新增 LibraryLoader + WindowsAndMessaging features + serde
    assert!(
        content.contains("Win32_System_LibraryLoader"),
        "Cargo.toml 缺少 Win32_System_LibraryLoader feature (T3 FFI LoadLibrary 需要)"
    );
    assert!(
        content.contains("serde"),
        "Cargo.toml 缺少 serde 依赖 (T3 Candidate 序列化需要)"
    );
    // T5 新增 WTS feature
    assert!(
        content.contains("Win32_System_StationsAndDesktops"),
        "Cargo.toml 缺少 Win32_System_StationsAndDesktops feature (T5 daemon WTSGetActiveConsoleSessionId 需要)"
    );
}

#[test]
fn lib_rs_declares_modules() {
    let cargo = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let lib_path = PathBuf::from(&cargo).join("src").join("lib.rs");
    let content = std::fs::read_to_string(&lib_path).expect("lib.rs 不可读");
    assert!(content.contains("pub mod ffi"));
    assert!(content.contains("pub mod tsf_input_processor"));
    assert!(content.contains("pub mod tsf_text_store"), "T3: 缺 tsf_text_store 模块");
    assert!(content.contains("pub mod keystroke"), "T3: 缺 keystroke 模块");
    assert!(content.contains("pub mod com_class_factory"));
    assert!(content.contains("pub mod register"), "T4: 缺 register 模块");
    assert!(content.contains("pub mod daemon"), "T5: 缺 daemon 模块");
    assert!(content.contains("pub mod ipc"), "T6: 缺 ipc 模块");
    assert!(content.contains("pub mod hotkey"), "T7: 缺 hotkey 模块");
}

#[test]
fn tsf_input_processor_uses_implement_macro() {
    let cargo = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let src_path = PathBuf::from(&cargo).join("src").join("tsf_input_processor.rs");
    let content = std::fs::read_to_string(&src_path).expect("tsf_input_processor.rs 不可读");
    // T25: 宏已从单行 (ITfTextInputProcessor, ITfSource) 扩展为多接口实现,
    // 包含 ITfKeyEventSink / ITfThreadMgrEventSink / ITfLangBarItemSink 等。
    // 检查宏存在 + 关键接口名都在参数列表中即可,不再要求单行写法。
    assert!(
        content.contains("#[windows::core::implement("),
        "TsfInputProcessor 未用 #[windows::core::implement] 宏(违反红线)"
    );
    for iface in &[
        "ITfTextInputProcessor",
        "ITfSource",
        "ITfKeyEventSink",
        "ITfThreadMgrEventSink",
        "ITfLangBarItemSink",
    ] {
        assert!(
            content.contains(iface),
            "TsfInputProcessor 宏缺接口 {iface}"
        );
    }
}

#[test]
fn no_hklm_in_code() {
    let cargo = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let src_dir = PathBuf::from(&cargo).join("src");
    // T25 更新:HKLM 路径不再是一刀切红线。T17/T25 实测 HKLM 必须写:
    //   - HKLM\..\Classes\CLSID\{CLSID}\InprocServer32 — TIPC 系统级 COM 可见
    //   - HKLM\..\TIP\{CLSID}\Category\Category\{CATID}\{CLSID} — fix6 真因
    // 但**只有 register.rs 允许写 HKLM**;其它模块(tsf_*/daemon/keystroke/ipc)
    // 仍要保持 HKCU-only 边界。
    let allow_hklm = |p: &std::path::Path| -> bool {
        // 允许:register 模块(真写 HKLM)+ main.rs(CLI 说明文案涉及 HKLM)
        p.file_name()
            .map(|n| {
                n == "register.rs"
                    || n == "register_export.rs"
                    || n == "main.rs"
            })
            .unwrap_or(false)
    };
    for entry in std::fs::read_dir(&src_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().map(|e| e == "rs").unwrap_or(false) {
            if allow_hklm(&path) {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap();
            // 红线(T25 限定):除 register.rs 外,其它模块不许写 HKLM
            assert!(
                !content.contains("HKLM"),
                "非 register 模块出现 HKLM(违反 T25 限定红线): {}",
                path.display()
            );
        }
    }
}

// ============================================================
// T3 新增项
// ============================================================

#[test]
fn keystroke_buffer_echo() {
    let mut buf = prisir_ime_tsf::PinyinBuffer::new();
    assert_eq!(buf.buf, "");
    buf.on_char('n'); buf.on_char('i'); buf.on_char('h'); buf.on_char('a'); buf.on_char('o');
    assert_eq!(buf.buf, "nihao");
    buf.on_backspace();
    assert_eq!(buf.buf, "niha");
    buf.on_escape();
    assert_eq!(buf.buf, "");
    // 大写字母也应被拒收(只 a-z)
    buf.on_char('N');
    assert_eq!(buf.buf, "");
    // 非字母字符也拒收
    buf.on_char('1');
    buf.on_char(' ');
    assert_eq!(buf.buf, "");
}

#[test]
fn keystroke_buffer_digit_space() {
    use prisir_ime_tsf::Candidate;

    // 数字键 1-9 选候选(选完一次后会清空 buffer + candidates)
    for (n, expected) in [(1u8, "你好"), (2u8, "泥灰"), (3u8, "拟或")] {
        let mut buf = prisir_ime_tsf::PinyinBuffer::new();
        buf.set_candidates(vec![
            Candidate::new("你好", 100),
            Candidate::new("泥灰", 50),
            Candidate::new("拟或", 30),
        ]);
        assert_eq!(buf.on_digit(n).as_deref(), Some(expected), "on_digit({n})");
    }
    // 数字键越界返 None
    let mut buf = prisir_ime_tsf::PinyinBuffer::new();
    buf.set_candidates(vec![Candidate::new("你", 1)]);
    assert_eq!(buf.on_digit(0), None, "digit 0");
    assert_eq!(buf.on_digit(9), None, "digit 9 越界");

    // 空格选首位(无候选时提交 buf)
    let mut buf2 = prisir_ime_tsf::PinyinBuffer::new();
    buf2.set_candidates(vec![
        Candidate::new("你", 100),
        Candidate::new("泥", 50),
    ]);
    assert_eq!(buf2.on_space().as_deref(), Some("你"));  // selected=0 默认首位
    // 空格无候选时返回 buf
    let mut buf3 = prisir_ime_tsf::PinyinBuffer::new();
    for c in "ni".chars() { buf3.on_char(c); }
    assert_eq!(buf3.on_space().as_deref(), Some("ni"));
}

#[test]
fn ffi_path_lookup_returns_string() {
    // 不实际 LoadLibrary,只验证路径解析函数存在且返回非空。
    let p = prisir_ime_tsf::ffi::locate_dll_path_for_test();
    assert!(!p.is_empty(), "DLL 路径解析结果为空");
    eprintln!("[smoke] ffi locate path = {}", p);
}

#[test]
#[ignore]  // 需要本地有 prisIr_ime.dll + ciku.db 才能跑, 默认跳过
fn ffi_sanity_load() {
    use std::ffi::CString;

    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    // 开发期路径 + 环境变量覆盖
    let db = std::env::var("PRISIR_CIKU_DB")
        .unwrap_or_else(|_| format!("{}/../../voice_input/lingxi_ime/backend/ciku.db", crate_dir));
    let db_c = CString::new(db.clone()).expect("bad db path");
    let h = unsafe { prisir_ime_tsf::ffi::prisir_tsf_load_engine(db_c.as_ptr()) };
    if h.is_null() {
        eprintln!("ffi_sanity_load: engine load returned null (env incomplete, ignored): db={}", db);
        return;
    }
    eprintln!("ffi_sanity_load: engine handle={:p} db={}", h, db);

    // 跑一次 query
    let pin = CString::new("nihao").unwrap();
    let json_ptr = unsafe { prisir_ime_tsf::ffi::prisir_tsf_query(h, pin.as_ptr()) };
    assert!(!json_ptr.is_null(), "prisir_tsf_query returned null");
    let json = unsafe { std::ffi::CStr::from_ptr(json_ptr) }.to_string_lossy().into_owned();
    eprintln!("ffi_sanity_load: query 'nihao' → {}", json);
    unsafe { prisir_ime_tsf::ffi::prisir_tsf_free_string(json_ptr); };
    unsafe { prisir_ime_tsf::ffi::prisir_tsf_free_engine(h); };
}

// ============================================================
// T4 新增项 — 注册表纯函数
//
// **绝对不调 do_register / do_unregister**, 会污染用户 HKCU。
// 只测纯函数, 验证幂等性 + 路径规范化。
// ============================================================

#[test]
fn register_path_strips_trailing_slash() {
    use prisir_ime_tsf::register::normalize_dll_path;

    // 末尾 `\` → 去掉
    assert_eq!(normalize_dll_path("C:\\foo\\bar\\"), "C:\\foo\\bar");
    // 末尾 `/` → 也去掉(混合分隔符)
    assert_eq!(normalize_dll_path("C:/foo/bar/"), "C:/foo/bar");
    // 末尾 `\\` → 多余的反斜杠也剥光
    assert_eq!(normalize_dll_path("C:\\foo\\bar\\\\"), "C:\\foo\\bar");
    // 末尾没分隔符 → 原样返回
    assert_eq!(normalize_dll_path("C:\\foo\\bar"), "C:\\foo\\bar");
    // 整个串就是分隔符 → 切光后空
    assert_eq!(normalize_dll_path("\\"), "");
    assert_eq!(normalize_dll_path("///"), "");
}

#[test]
fn register_idempotent_paths() {
    use prisir_ime_tsf::register::{build_reg_tree, normalize_dll_path};

    // 同输入两次 → 输出 Vec 完全一致(逐元素 ==)
    let tree1 = build_reg_tree("C:\\foo\\prisir_ime_tsf.dll");
    let tree2 = build_reg_tree("C:\\foo\\prisir_ime_tsf.dll");
    assert_eq!(tree1, tree2, "同输入两次必须输出同 Vec(否则真注册会反复写不幂等)");

    // 加 trailing slash 的同一路径 → 输出 Vec 也必须完全一致
    let tree3 = build_reg_tree("C:\\foo\\prisir_ime_tsf.dll\\");
    assert_eq!(tree1, tree3, "trailing slash 必须规范化掉, 不许进入注册表值");

    // normalize_dll_path 也必须对两个输入出一致结果
    let a = normalize_dll_path("C:\\foo\\prisir_ime_tsf.dll");
    let b = normalize_dll_path("C:\\foo\\prisir_ime_tsf.dll\\");
    assert_eq!(a, b);

    // 内容清单: 应有 11 项(v0.8 实际值,T12 测试时是 10 但后续 hotfix 加了 SubItemInSettingUI,
    // 真实构建一直是 11;这里以实际值对齐):
    //   - 2 项 CTF TIP 父 key (DisplayDescription, Description)
    //   - 6 项 LanguageProfile (Description, IconFile, IconIndex, Enable, HiddenInSettingUI, SubItemInSettingUI)
    //   - 3 项 COM InprocServer32 ((Default), InprocServer32\(Default), ThreadingModel)
    //
    //   历史:T12 hotfix: Implemented Categories\{CATID} 移到 `key_only_paths()`(只创 sub_key 不写 value)
    //   因为 Windows ICatInformation 只看 key 存在与否,而不是 value。
    assert_eq!(tree1.len(), 11, "v0.8 注册表树必须是 11 项, 实得 {}", tree1.len());

    // T25 hotfix: `key_only_paths()` 必须返回 21 项(实测):
    //   - 3 项 HKCU\..\Classes\CLSID\..\Implemented Categories\(Sogou 兼容)
    //   - 9 项 HKCU\..\TIP\Category\Category\{CATID}\{CLSID}(fix6 真因双层嵌套)
    //   - 9 项 HKCU\..\TIP\Category\Item\{CLSID}\{CATID}(Sogou 模板双键备份)
    // 老断言是 3 项(T12 hotfix 当时),但 fix6/fix7 加了 TIP\Category\Category + Item
    // 两组 9 个,实测加起来 21。
    let key_only = prisir_ime_tsf::register::key_only_paths();
    assert_eq!(
        key_only.len(),
        21,
        "T25: key_only_paths 必须返回 21 项 (3 Implemented Categories + 9 Category\\Category + 9 Category\\Item), 实得 {}",
        key_only.len()
    );
    // 至少要有 3 项 `Implemented Categories\` 路径(Sogou 兼容)
    let impl_cat_count = key_only
        .iter()
        .filter(|k| k.contains(r"\Implemented Categories\"))
        .count();
    assert_eq!(
        impl_cat_count, 3,
        "T25: Implemented Categories 必须 3 项, 实得 {impl_cat_count}"
    );
    // 至少要有 9 项 TIP\Category\Category\{CATID}\{CLSID} 双层嵌套(fix6)
    let cat_cat_count = key_only
        .iter()
        .filter(|k| k.contains(r"\Category\Category\"))
        .count();
    assert_eq!(
        cat_cat_count, 9,
        "T25: Category\\Category 双层嵌套必须 9 项, 实得 {cat_cat_count}"
    );

    // 每个 sub_key 都以 SOFTWARE 开头 → 绝不能混入 HKLM
    for entry in &tree1 {
        let (sub, _name) = match entry {
            prisir_ime_tsf::register::RegEntry::Sz(p, _) => {
                let (s, n) = prisir_ime_tsf::register::split_key_value(p);
                (s, n)
            }
            prisir_ime_tsf::register::RegEntry::Dword(p, _) => {
                let (s, n) = prisir_ime_tsf::register::split_key_value(p);
                (s, n)
            }
        };
        assert!(
            sub.starts_with(r"SOFTWARE\") && !sub.contains("MACHINE"),
            "T4 红线: 注册表 sub_key 必须 HKCU (SOFTWARE), 实得 {}",
            sub
        );
    }
}

// ============================================================
// T5 新增项 — daemon 纯结构 + 真 CATID
//
// **绝对不调 do_register / do_unregister / do_enable / do_disable / daemon 主循环**,
// 任何调都污染用户 HKCU 或拉起 LoadLibrary 副作用。只验纯结构常量。
// ============================================================

#[test]
fn daemon_path_parse_dll() {
    let c = prisir_ime_tsf::daemon::DaemonConfig::default();
    assert!(
        c.dll_path.to_string_lossy().contains("prisir_ime_tsf.dll"),
        "T5 红线: DaemonConfig::default() 必须指向 prisir_ime_tsf.dll, 实得 {:?}",
        c.dll_path
    );
    eprintln!("[smoke] daemon dll_path = {:?}", c.dll_path);
    // 默认 auto_register 必须是 false,避免开发者误启 daemon 就污染 IME 列表
    assert!(
        !c.auto_register_on_start,
        "T5 红线: DaemonConfig::default().auto_register_on_start 必须 false,避免误启污染"
    );
}

#[test]
fn register_catid_real_msctf() {
    use prisir_ime_tsf::register::{build_reg_tree, CATEGORY_TIP_KEYBOARD};

    // 1. 常量必须是真实 msctf.h CATID_TIP_KEYBOARD 值,不是占位
    assert_eq!(
        CATEGORY_TIP_KEYBOARD,
        "{36C679D9-696D-4A1B-9D8B-313E62CD3C30}",
        "T5 红线: CATID 必须是 msctf.h CATID_TIP_KEYBOARD 真实值, 实得 {}",
        CATEGORY_TIP_KEYBOARD
    );
    // 不允许 T4 那个占位 GUID 残留
    assert!(
        !CATEGORY_TIP_KEYBOARD.contains("C1A8B7B0"),
        "T5 红线: 占位 CATID (C1A8B7B0) 不应再出现"
    );

    // 2. T25: CATID 在 `key_only_paths()`(只创 sub_key 不写 value),
    // 不在 build_reg_tree(后者写 REG_SZ/Dword value)。这是 fix6/T12 后的设计,
    // build_reg_tree 只写"键-值"对的注册表项,key_only_paths 写"只创 sub_key"
    // 的注册表项(Windows ICatInformation 只看 key 存在与否)。
    //
    // 因此这条测试改验:`key_only_paths()` 里必须含 CATID_TIP_KEYBOARD 子 key。
    let key_only = prisir_ime_tsf::register::key_only_paths();
    let catid = key_only
        .iter()
        .find(|k| k.contains(&CATEGORY_TIP_KEYBOARD))
        .unwrap_or_else(|| {
            panic!(
                "T25: key_only_paths 缺 CATID_TIP_KEYBOARD ({CATEGORY_TIP_KEYBOARD}), 实得 {key_only:?}"
            )
        });
    // CATID 必须作为 sub_key 路径的一部分,而非 value_name
    assert!(
        catid.ends_with(&CATEGORY_TIP_KEYBOARD),
        "T25: CATID 必须作为 sub_key 路径末尾, 实得 {catid}"
    );
}

#[test]
fn daemon_wts_event_filter() {
    let c = prisir_ime_tsf::daemon::DaemonConfig::default();
    assert!(
        c.poll_interval_secs >= 1,
        "T5: poll_interval 必须 >= 1s, 实得 {}",
        c.poll_interval_secs
    );
    // shutdown 标志位默认 false
    assert!(
        !prisir_ime_tsf::daemon::is_shutdown(),
        "T5: SHUTDOWN 标志位默认必须 false"
    );
    // request_shutdown 后必须 true
    prisir_ime_tsf::daemon::request_shutdown();
    assert!(
        prisir_ime_tsf::daemon::is_shutdown(),
        "T5: request_shutdown 后 is_shutdown 必须 true"
    );
    // 复位,避免污染同进程后续测试
    prisir_ime_tsf::daemon::reset_shutdown_for_test();
    assert!(
        !prisir_ime_tsf::daemon::is_shutdown(),
        "T5: 复位后 is_shutdown 必须 false"
    );
}

// ============================================================
// T6 新增项 — ipc 纯 parse 测试
//
// **绝对不调 do_register / do_unregister / do_enable / do_disable / ffi query**:
//   - register/unregister/enable/disable 会污染用户 HKCU
//   - ffi query 会触发 LoadLibrary 副作用(虽然这里只走 smoke, 但纯 parse 已覆盖
//     主要逻辑 — 真链路留给 --ipc-test 跑)
// 测的是:
//   - version 方法返回 crate 名 + version + tsf_version
//   - 未知 method → -32601 method not found
//   - parse error → -32700 parse error
//   - query 缺 params.pinyin → -32602 invalid params
// ============================================================

#[test]
fn ipc_request_parse() {
    use prisir_ime_tsf::ipc::handle_request;

    // 1. version 方法: 必须含 result + crate 名 + version
    let resp = handle_request(r#"{"method":"version","params":{},"id":1}"#);
    assert!(
        resp.contains("\"result\""),
        "version method 必须返 result, 实得: {resp}"
    );
    assert!(
        resp.contains("\"crate\":\"prisir_ime_tsf\""),
        "version method 必须含 crate=prisir_ime_tsf, 实得: {resp}"
    );
    assert!(
        resp.contains("\"id\":1"),
        "version method 必须 echo id=1, 实得: {resp}"
    );
    assert!(
        resp.contains("\"tsf_version\""),
        "version method 必须含 tsf_version, 实得: {resp}"
    );

    // 2. 未知 method → -32601 method not found
    let resp = handle_request(r#"{"method":"foo","params":{},"id":2}"#);
    assert!(
        resp.contains("\"error\""),
        "未知 method 必须返 error, 实得: {resp}"
    );
    assert!(
        resp.contains("method not found"),
        "未知 method 错误信息必须含 'method not found', 实得: {resp}"
    );
    assert!(
        resp.contains("-32601"),
        "未知 method 错误码必须 -32601, 实得: {resp}"
    );

    // 3. parse error → -32700
    let resp = handle_request("not valid json");
    assert!(
        resp.contains("parse error"),
        "非法 JSON 必须含 'parse error', 实得: {resp}"
    );
    assert!(
        resp.contains("-32700"),
        "parse error 错误码必须 -32700, 实得: {resp}"
    );

    // 4. query 缺 pinyin → -32602 invalid params
    let resp = handle_request(r#"{"method":"query","params":{},"id":3}"#);
    assert!(
        resp.contains("missing params.pinyin"),
        "query 缺 pinyin 必须报 missing params.pinyin, 实得: {resp}"
    );
    assert!(
        resp.contains("-32602"),
        "query 缺 pinyin 错误码必须 -32602, 实得: {resp}"
    );

    // 不调 register/unregister/enable/disable — 避免污染开发机
    // 不调 query 真链路 — ffi LoadLibrary 是副作用,留给 --ipc-test 跑
    eprintln!("[smoke] ipc_request_parse OK (4 sub-cases)");
}

// ============================================================
// T7 新增项 — hotkey 纯函数 + InputMethod 分发表 + 源码 AK 防护
//
// **绝对不调 do_register / ffi query / 网络 / spawn 任何子进程**:
//   - hotkey.rs + keystroke.rs 的 method 切换是纯内存的,只需验纯函数与常量
//   - aliyun_ak_invalid_protection 是静态扫,扫 src/ 下 *.rs, 命中即失败
// 测的是:
//   - ALLOWED_VKS / normalize_vk / PINYIN_TRIGGER_KEY / WUBI_TRIGGER_KEY 严格沿用 voice_input
//   - InputMethod::Pinyin / Wubi 的 parse / as_str 一致性
//   - 源码里不许出现云 AK 标识符(防历史代码残留)
// ============================================================

#[test]
fn hotkey_normalize_vk_agrees_with_python() {
    use prisir_ime_tsf::hotkey::{
        ALLOWED_VKS, PINYIN_TRIGGER_KEY, WUBI_TRIGGER_KEY, normalize_vk,
    };

    // 1. ALLOWED_VKS 每个值都能被 normalize_vk 接受
    for &vk in ALLOWED_VKS {
        assert!(
            normalize_vk(vk).is_some(),
            "vk {vk:#x} 必须在 ALLOWED_VKS 内并被 normalize_vk 接受"
        );
    }

    // 2. 不允许的键必须 None
    assert!(normalize_vk(0x41).is_none(), "VK_A 必须 None");
    assert!(normalize_vk(0x20).is_none(), "VK_SPACE 必须 None");
    assert!(normalize_vk(0x1B).is_none(), "VK_ESC 必须 None");

    // 3. 左右修饰键归并到通用键码
    assert_eq!(normalize_vk(0xA0), Some(0x10), "VK_LSHIFT -> 通用 Shift");
    assert_eq!(normalize_vk(0xA1), Some(0x10), "VK_RSHIFT -> 通用 Shift");
    assert_eq!(normalize_vk(0xA2), Some(0x11), "VK_LCTRL -> 通用 Ctrl");
    assert_eq!(normalize_vk(0xA3), Some(0x11), "VK_RCTRL -> 通用 Ctrl");

    // 4. 激活键必须严格沿用 voice_input/lingxi_ime/backend/ime_config.py
    assert_eq!(PINYIN_TRIGGER_KEY, 0xA3, "拼音激活键必须 = 右 Ctrl (0xA3)");
    assert_eq!(WUBI_TRIGGER_KEY,   0xA1, "五笔激活键必须 = 右 Shift (0xA1)");

    eprintln!("[smoke] hotkey constants OK: ALLOWED_VKS={} entries", ALLOWED_VKS.len());
}

#[test]
fn method_dispatch_table() {
    use prisir_ime_tsf::hotkey::{InputMethod, parse_method};

    // 1. 两个 enum 值都能被 parse_method 接受,且 as_str 往返一致
    for m in [InputMethod::Pinyin, InputMethod::Wubi] {
        assert_eq!(
            parse_method(m.as_str()).unwrap(),
            m,
            "as_str -> parse_method 必须回到原 enum 值"
        );
    }

    // 2. 接受大小写不敏感 + 中文 + 简写
    assert_eq!(parse_method("pinyin").unwrap(), InputMethod::Pinyin);
    assert_eq!(parse_method("PINYIN").unwrap(), InputMethod::Pinyin);
    assert_eq!(parse_method("拼音").unwrap(),    InputMethod::Pinyin);
    assert_eq!(parse_method("py").unwrap(),      InputMethod::Pinyin);
    assert_eq!(parse_method("wubi").unwrap(),    InputMethod::Wubi);
    assert_eq!(parse_method("WUBI").unwrap(),    InputMethod::Wubi);
    assert_eq!(parse_method("五笔").unwrap(),    InputMethod::Wubi);
    assert_eq!(parse_method("wb").unwrap(),      InputMethod::Wubi);

    // 3. 未知 method 必须 Err
    assert!(parse_method("foo").is_err(),  "未知 method 'foo' 必须 Err");
    assert!(parse_method("").is_err(),     "空串必须 Err");
    assert!(parse_method("cangjie").is_err(), "仓颉未在 T7 范围, 必须 Err");

    // 4. 进程级 active_method 默认 Pinyin
    assert_eq!(
        prisir_ime_tsf::keystroke::active_method(),
        InputMethod::Pinyin,
        "T7: 进程启动默认 active_method 必须是 Pinyin"
    );
    // 切到 Wubi 后能读到
    prisir_ime_tsf::keystroke::set_active_method(InputMethod::Wubi);
    assert_eq!(
        prisir_ime_tsf::keystroke::active_method(),
        InputMethod::Wubi,
        "T7: set_active_method(Wubi) 后 active_method 必须 Wubi"
    );
    // 复位,避免污染同进程后续测试
    prisir_ime_tsf::keystroke::set_active_method(InputMethod::Pinyin);

    eprintln!("[smoke] method dispatch OK (2 enum × 7 parse aliases)");
}

// ============================================================
// T10 新增项 — WTS 真消息循环 + hidden HWND 纯结构验证
//
// **绝对不调 daemon::run_daemon()**:
//   - 真跑会调 SetConsoleCtrlHandler + CreateWindowExW + 起后台 thread +
//     进 GetMessageW 真消息循环(开发机跑不干净)
//   - 只验纯结构常量 + 编译期可达的 WTS 常量
// 测的是:
//   - DaemonConfig::default() 关键字段(dll_path / poll_interval / auto_register)
//   - WTS 常量引用编译期可达(NOTIFY_FOR_THIS_SESSION / WTS_CONSOLE_CONNECT 等)
//     证明 daemon.rs 的 WndProc match 分支常量真存在
// ============================================================

#[test]
fn daemon_message_loop_wiring() {
    let config = prisir_ime_tsf::daemon::DaemonConfig::default();
    assert!(
        config.dll_path.to_string_lossy().contains("prisir_ime_tsf.dll"),
        "T10: DaemonConfig::default() dll_path 必须指向 prisir_ime_tsf.dll, 实得 {:?}",
        config.dll_path
    );
    assert!(
        config.poll_interval_secs >= 1,
        "T10: poll_interval_secs 必须 >= 1s, 实得 {}",
        config.poll_interval_secs
    );
    assert_eq!(
        config.auto_register_on_start, false,
        "T10: 默认 auto_register_on_start 必须是 false"
    );
    assert_eq!(
        config.auto_unregister_on_exit, false,
        "T10: 默认 auto_unregister_on_exit 必须是 false"
    );
    eprintln!("[smoke] T10 daemon config OK: {:?}", config.dll_path);
}

#[test]
fn daemon_session_event_handlers() {
    // 编译期验证常量引用可达 — 证明 daemon.rs WndProc match 分支常量真存在
    // 这些常量由 windows-rs 0.58 在 Win32_UI_WindowsAndMessaging + Win32_System_RemoteDesktop 暴露
    let _a = windows::Win32::System::RemoteDesktop::NOTIFY_FOR_THIS_SESSION;
    let _b = windows::Win32::UI::WindowsAndMessaging::WTS_CONSOLE_CONNECT;
    let _c = windows::Win32::UI::WindowsAndMessaging::WTS_CONSOLE_DISCONNECT;
    let _d = windows::Win32::UI::WindowsAndMessaging::WTS_SESSION_LOGON;
    let _e = windows::Win32::UI::WindowsAndMessaging::WTS_SESSION_LOGOFF;
    let _f = windows::Win32::UI::WindowsAndMessaging::WTS_REMOTE_CONNECT;
    let _g = windows::Win32::UI::WindowsAndMessaging::WTS_REMOTE_DISCONNECT;
    let _h = windows::Win32::UI::WindowsAndMessaging::WM_WTSSESSION_CHANGE;
    let _i = windows::Win32::UI::WindowsAndMessaging::HWND_MESSAGE;
    let _j = windows::Win32::UI::WindowsAndMessaging::WS_EX_TOOLWINDOW;
    eprintln!("[smoke] T10 WTS + window constants all reachable");
}

#[test]
fn aliyun_ak_invalid_protection() {
    // 静态扫: src/ 下 *.rs 不能含 aliyun / AccessKey 标识符
    // 理由: 历史 dev_lessons 记录(aliyun-ak-invalid-notfound.md)告诉我们
    //       云 AK 标识符不应出现在产品代码里 — 防泄露/防误用。
    // 仅扫非注释行(// 开头的不算),避免误杀文档注释。
    let cargo = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let src_dir = PathBuf::from(&cargo).join("src");
    let forbidden = ["aliyun", "accesskey", "ak_id", "ak_secret"];
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(&src_dir).expect("src/ 不可读") {
        let entry = entry.expect("dirent");
        let path = entry.path();
        if path.extension().map(|e| e == "rs").unwrap_or(false) {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let non_comment: String = content
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            let lower = non_comment.to_lowercase();
            for &bad in &forbidden {
                if lower.contains(bad) {
                    hits.push(format!("{}: '{}'", path.display(), bad));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "T7 红线: 源码禁止含云 AK 标识符(aliyun / accesskey / ak_id / ak_secret). 命中: {:#?}",
        hits
    );
    eprintln!("[smoke] aliyun_ak protection OK (扫 {} 项)", forbidden.len());
}