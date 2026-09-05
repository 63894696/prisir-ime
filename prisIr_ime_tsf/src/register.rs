//! register.rs — TSF CTF 注册 + COM 类注册 (HKCU only, 免管理员)
//!
//! 严格遵守宪法 §5b: 任何路径不许写系统全局。全部走 `RegOpenCurrentUser`。
//!
//! 子命令:
//!   do_register(dll_path)   — 写入 HKCU CTF TIP + COM InprocServer32
//!   do_unregister()         — 递归删 HKCU 两个 key, 幂等
//!   do_status()             — 检查两个 key 是否存在
//!   do_enable()             — Enable=1,告诉 CTF 拉起该 TIP
//!   do_disable()            — Enable=0,告诉 CTF 不要拉起(dll 还在注册表)
//!
//! 纯函数(可在 smoke test 中调用, 不写注册表):
//!   normalize_dll_path(p)   — 去掉末尾 `\` `/`
//!   build_reg_tree(dll)     — 拼出 (sub_key, value_name, value) 列表, 同一输入幂等

#![allow(dead_code)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR, BOOL};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::TextServices::{
    CLSID_TF_InputProcessorProfiles, ITfInputProcessorProfileMgr, ITfInputProcessorProfiles,
    TF_PROFILETYPE_INPUTPROCESSOR,
};

// =====================================================================
// 公共常量 — 与 `tsf_input_processor.rs:25` 的 CLSID_PRISIR_IME 保持一致
// =====================================================================

/// Prisir IME 的 CLSID(字符串形态, 注入注册表用)。
pub const CLSID_PRISIR_IME_STR: &str = "{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}";

/// 在 Windows 设置 → 输入法列表里显示的名字(2026-09-04 改为「Prisir 灵犀拼音」)。
/// UTF-16 注册表值,中文正常显示;早期「避免中文」的顾虑已被 mojibake 修复(null 结尾)覆盖。
pub const DISPLAY_NAME: &str = "Prisir 灵犀拼音";

/// CTF TIP 注册表的 Description 字段。
pub const DESCRIPTION: &str = "Prisir 灵犀拼音";

/// LANGID for zh-CN(windows hex 0804)。
pub const LANGID_ZH_CN: &str = "0804";

/// LanguageProfile 子树里的 Profile GUID — 标识 Prisir IME 在 zh-CN 这一语言下的
/// 输入法 profile。固定写死,保证注册/反注册幂等。
///
/// 参照 msctf.h 标准做法: 一个 TIP CLSID 下可以有多个 LanguageProfile\0x<LANGID>\
/// {<PROFILE_GUID>} 子项, 每个子项可独立 Enable=0/1。
///
/// 注意: 不能用 TIP_CLSID 自己当 PROFILE_GUID — 必须是另一个独立的 GUID。
///
/// **2026-09-01 换真实 GUID**: 旧占位符 `{12345678-1234-1234-1234-123456789ABC}`
/// 是一眼假的占位 GUID, `AddLanguageProfile` 注册时 Windows 校验为「待安装键盘布局」,
/// 每次重启桌面跳「正在安装新键盘」。改成真实随机 GUID 对齐 MS拼音/搜狗。
/// `LEGACY_PROFILE_GUIDS` 列出历史占位 GUID,注册时迁移清理。
pub const PROFILE_GUID: &str = "{7C3A9E21-4B5D-4F8A-9C2E-6D1B8A4F3E05}";

/// 历史占位 profile GUID — 换 GUID 时从注册表迁移清理,避免 InputMethodTips 残留旧项。
pub const LEGACY_PROFILE_GUIDS: &[&str] = &[
    "{12345678-1234-1234-1234-123456789ABC}",
];

/// CATID(TIP 类别) — 真实 msctf.h 里的 `CATID_TIP_KEYBOARD`。
///
/// 来自 Windows SDK `msctf.h`:
/// ```c
/// DEFINE_GUID(CATID_TIP_KEYBOARD,
///     0x36c679d9, 0x696d, 0x4a1b, 0x9d, 0x8b, 0x31, 0x3e, 0x62, 0xcd, 0x3c, 0x30);
/// ```
///
/// 命名沿用 `CATEGORY_TIP_KEYBOARD`(与 Windows SDK 语义对齐),
/// T4 的占位 `CATEGORY_TIP_NORMAL` 已删除,这里就是真实值。
pub const CATEGORY_TIP_KEYBOARD: &str =
    "{36C679D9-696D-4A1B-9D8B-313E62CD3C30}";

/// TIPC 走 `ICatInformation::EnumClassesOfCategories` 枚举 TIP 类,
/// CLSID 必须声明自己实现了 CATID_TIP_KEYBOARD(放在 Implemented Categories 下),
/// TIPC 才会把它放进候选列表。仅 `InprocServer32` 注册不够。
///
/// 同时再加 2 个常见的 TIP category,提升兼容性 ——
///   - `{2E4B07D4-B8F4-4D8C-8C50-C25BF6E5A4B8}` 见某些 SDK 头文件定义
///   - `{13A016DF-560B-46CD-947A-4C3AF1E0E35D}` Microsoft 自家的辅助 category
///
/// 任一 category 子键都告诉 TIPC「这个 CLSID 实现了对应的 COM category」,
/// 由 ICatInformation::EnumClassesOfCategories 按 prefix 匹配反向查找。
pub const CATEGORY_TIP_KEYBOARD2: &str =
    "{2E4B07D4-B8F4-4D8C-8C50-C25BF6E5A4B8}";
pub const CATEGORY_TIP_KEYBOARD3: &str =
    "{13A016DF-560B-46CD-947A-4C3AF1E0E35D}";

// =====================================================================
// 2026-09-05: HKCU CTF\Assemblies 运行时映射(装完即用)
// =====================================================================
/// 键盘布局类 GUID(GUID_TFCAT_TIP_KEYBOARD) — Assemblies 下 0x<LANGID> 的子键名。
const ASM_LAYOUT_GUID: &str = "{34745C63-B2F0-4784-8B67-5E12C8701A31}";
/// Assemblies 下 zh-CN 语言目录名(注册表存的是 8 位 hex 大写)。
const ASM_LANGID_DIR: &str = "0x00000804";
/// KeyboardLayout DWORD 值 = 0x08040804(LANGID + 布局 id)。
const ASM_KEYBOARD_LAYOUT: u32 = 0x0804_0804;
/// 原占用者备份的 HKLM 键路径(SYSTEM/用户同视图, 跨上下文可读)。
const ASM_BACKUP_HKLM: &str = r"SOFTWARE\LingxiIME";
/// 原占用者备份的 HKLM 位置(2026-09-05)。
///
/// **为什么不能存 HKCU TIP 树根**: 安装器(nsi)以 elevated 上下文调
/// `prisir_tsfsvc --register`, 该进程常跑在 **SYSTEM token** 下, 此时
/// `RegOpenCurrentUser` 打开的是 **SYSTEM 的 HKCU**, 不是登录用户的 HKCU。
/// 备份写进 SYSTEM\HKCU 后, 卸载(同样可能 SYSTEM)读得到、但抢占/恢复写的
/// 用户 HKCU 又是另一份 → 跨上下文读不到备份, 恢复失败(实测 beta.4 卸载后
/// Assemblies 仍 Prisir)。HKLM 对 SYSTEM/用户同一视图且跨会话持久,
/// 卸载时无论谁跑都能读到。卸载后 do_unregister 删掉。

// =====================================================================
// 纯函数 — smoke test 直接验
// =====================================================================

/// `&str` → 末尾带 NUL 的 UTF-16 `Vec<u16>`, 适配 Win32 `*W` API。
pub fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// 规范化 DLL 路径:
///   - 去掉末尾 `\` 或 `/`(Windows 路径分隔符)
///   - 其它原样返回
///
/// 这是纯函数, 同一输入永远同一输出 → smoke 可测。
pub fn normalize_dll_path(p: &str) -> String {
    let trimmed = p.trim_end_matches(['\\', '/']);
    std::path::Path::new(trimmed).to_string_lossy().into_owned()
}

/// 单条注册表项。
///
/// 设计:
///   - `path` 形如 `SOFTWARE\Microsoft\CTF\TIP\{CLSID}\DisplayName`:
///     `\` 之前的部分当作 sub_key, 之后当作 value_name。
///   - `value` 对 `Sz` 是字符串值, 对 `Dword` 是 u32 little-endian。
///
/// 故意不写系统全局: 这是宪法红线, `do_register` 也只接 `HKCU`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegEntry {
    /// (path, value)
    Sz(String, String),
    /// (path, value)
    Dword(String, u32),
}

impl RegEntry {
    /// 解构为 `(sub_key, value_name, kind, bytes)` 方便调 Win32 API。
    /// path 必须至少含一个 `\`,否则 value_name == path 整体,无 sub_key(边界情形)。
    pub fn into_parts(&self) -> (String, String, REG_VALUE_TYPE, Vec<u8>) {
        match self {
            RegEntry::Sz(path, value) => {
                let (k, n) = split_key_value(path);
                let w = to_wide(value);
                // REG_SZ 含末尾 NUL 的 UTF-16, 字节数为 w.len() * 2
                let bytes = unsafe {
                    std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2).to_vec()
                };
                (k.to_string(), n.to_string(), REG_SZ, bytes)
            }
            RegEntry::Dword(path, value) => {
                let (k, n) = split_key_value(path);
                (k.to_string(), n.to_string(), REG_DWORD, value.to_le_bytes().to_vec())
            }
        }
    }
}

/// 把 `SOFTWARE\Foo\Bar\Baz` 切成 `("SOFTWARE\Foo\Bar", "Baz")`。
/// 末尾段为 value_name, 前面整段为 sub_key。
pub fn split_key_value(s: &str) -> (&str, &str) {
    match s.rfind('\\') {
        Some(idx) => (&s[..idx], &s[idx + 1..]),
        None => ("", s),
    }
}

/// 拼出 Prisir IME 在 HKCU 下需要写入的全部注册表项。
///
/// 顺序:
///   1. `HKCU\SOFTWARE\Microsoft\CTF\TIP\{CLSID}\*`                              — CTF TIP 父 key (4 项)
///   2. `HKCU\..\TIP\{CLSID}\LanguageProfile\0x<LANGID>\{PROFILE_GUID}\*`        — Profile 子项 (3 项)
///        含 Description + DisplayDescription + Enable
///   3. `HKCU\SOFTWARE\Classes\CLSID\{CLSID}\*`                                  — COM 类注册 (3 项 InprocServer32)
///   4. `HKCU\..\Classes\CLSID\{CLSID}\Implemented Categories\{CATID}*`          — TIPC 枚举 TIP 的关键 (3 项)
///
/// 关键(T11 hotfix:补 Implemented Categories):
///   TIPC 通过 `ICatInformation::EnumClassesOfCategories` 找 TIP,CLSID 必须声明
///   自己实现了 CATID_TIP_KEYBOARD(放在 Implemented Categories 下作为子 key)才能
///   被 TIPC 看见。仅 InprocServer32 + ThreadingModel = Both 是不够的。
///
/// 关键(T9 hotfix #3):
///   LanguageProfile 子项必须含 DisplayDescription REG_SZ 字符串, Windows 设置 UI
///   才把 TIP 列入「键盘 → 添加键盘」列表。仅 Enable DWORD=1 不够。
///
/// 关键(T9 hotfix #3):
///   LanguageProfile 子项必须含 DisplayDescription REG_SZ 字符串, Windows 设置 UI
///   才把 TIP 列入「键盘 → 添加键盘」列表。仅 Enable DWORD=1 不够。
///
/// 关键(T9 hotfix #2):
///   旧版扁平写法 `Profile = "0804\{...}"` 不被 Windows 10/11 识别为合法 LanguageProfile。
///   必须按 msctf.h 标准建真子 key 三级嵌套:
///     `{TIP_CLSID}\LanguageProfile\0x00000804\{PROFILE_GUID}\*`
///
/// 同一输入(含 trailing slash 与否)产出完全一致的 Vec。
pub fn build_reg_tree(dll_path: &str) -> Vec<RegEntry> {
    let dll = normalize_dll_path(dll_path);
    let clsid = CLSID_PRISIR_IME_STR;

    vec![
        // ---- CTF TIP 父 key ----
        // T20 hotfix: 之前版本注释里写"父 key 不写 DisplayDescription/Description 也不会乱码" ——
        // 用户在 vm-sata 实测发现任务栏 LangBar 显示"Prisir IME 磊磊磊k磊嘎鸣0",即 TIPC 在没有
        // DisplayDescription 时拿 LanguageProfile Description + 某些内部状态拼接出乱码。
        // 修法: 父 TIP key 补两个 REG_SZ 值。
        RegEntry::Sz(
            format!(r"SOFTWARE\Microsoft\CTF\TIP\{clsid}\DisplayDescription"),
            DISPLAY_NAME.to_string(),
        ),
        RegEntry::Sz(
            format!(r"SOFTWARE\Microsoft\CTF\TIP\{clsid}\Description"),
            DISPLAY_NAME.to_string(),
        ),
        // 其余父 key (DisplayName/IconFile/Category) 按 Sogou 模板继续省略 — TIPC 不读它们。
        // ---- LanguageProfile 子树(三级嵌套)----
        // CreateKeyExW 会递归创建中间层, 所以这里直接写最深处的值。
        //
        // v0.8 对齐 Sogou 模板:
        //   Description       REG_SZ     "Prisir IME"
        //   IconFile          REG_SZ     <dll path>  (REG_SZ, NOT REG_EXPAND_SZ)
        //   IconIndex         REG_DWORD  0
        //   Enable            REG_DWORD  1
        //   HiddenInSettingUI REG_DWORD  0
        //   SubItemInSettingUI REG_DWORD 0
        // (Sogou 验证: 不需要 DisplayDescription / ProfileFlags,这俩字段是过度补全)
        RegEntry::Sz(
            format!(
                r"SOFTWARE\Microsoft\CTF\TIP\{clsid}\LanguageProfile\0x0000{LANGID_ZH_CN}\{PROFILE_GUID}\Description"
            ),
            DISPLAY_NAME.to_string(),
        ),
        RegEntry::Sz(
            format!(
                r"SOFTWARE\Microsoft\CTF\TIP\{clsid}\LanguageProfile\0x0000{LANGID_ZH_CN}\{PROFILE_GUID}\IconFile"
            ),
            dll.clone(),
        ),
        RegEntry::Dword(
            format!(
                r"SOFTWARE\Microsoft\CTF\TIP\{clsid}\LanguageProfile\0x0000{LANGID_ZH_CN}\{PROFILE_GUID}\IconIndex"
            ),
            0,
        ),
        RegEntry::Dword(
            format!(
                r"SOFTWARE\Microsoft\CTF\TIP\{clsid}\LanguageProfile\0x0000{LANGID_ZH_CN}\{PROFILE_GUID}\Enable"
            ),
            1,
        ),
        RegEntry::Dword(
            format!(
                r"SOFTWARE\Microsoft\CTF\TIP\{clsid}\LanguageProfile\0x0000{LANGID_ZH_CN}\{PROFILE_GUID}\HiddenInSettingUI"
            ),
            0,
        ),
        RegEntry::Dword(
            format!(
                r"SOFTWARE\Microsoft\CTF\TIP\{clsid}\LanguageProfile\0x0000{LANGID_ZH_CN}\{PROFILE_GUID}\SubItemInSettingUI"
            ),
            0,
        ),
        // T9 hotfix #2: 删掉扁平的 Profile 字符串项 (已被 LanguageProfile 子树替代)
        // T9 hotfix #1: 删掉 SubstitutedLayout = 1 (那是误导项)
        // ---- COM InprocServer32 ----
        RegEntry::Sz(
            format!(r"SOFTWARE\Classes\CLSID\{clsid}\(Default)"),
            "Prisir 灵犀拼音".to_string(),
        ),
        RegEntry::Sz(
            format!(r"SOFTWARE\Classes\CLSID\{clsid}\InprocServer32\(Default)"),
            dll.clone(),
        ),
        RegEntry::Sz(
            format!(r"SOFTWARE\Classes\CLSID\{clsid}\InprocServer32\ThreadingModel"),
            "Apartment".to_string(),
        ),
        // ---- Implemented Categories (TIPC 枚举 TIP 的关键) ----
        // TIPC 通过 `ICatInformation::EnumClassesOfCategories(implCat, reqCat, ...)` 找 TIP,
        // 每个 CATID 必须是 `Implemented Categories\{CATID}` 的**子 key**(空 key,无 value)。
        //
        // T12 hotfix: 之前版本把这写成了 `RegEntry::Sz(...Implemented Categories\{GUID}, "")`,
        // 即把 GUID 当作 value_name 在 `Implemented Categories` 下设 REG_SZ = "",
        // 但 Windows 期望 GUID 是 **sub_key**,不是 value。
        // 实际写完之后 reg query 只看见 1 个 (Default) 空串,TIPC 因此枚举不到 Prisir。
        //
        // 修法: 不写任何 value,只让 `do_register` 调 RegCreateKeyExW 创建这 3 个空 sub_key。
        // 这里在 build_reg_tree 末尾放一个特殊的 sentinel —— 一个 path 不含 `\` 的 key,只表
        // 意图,不参与 RegSetValueExW。do_register 识别后只 create_key 不 set_value。
    ]
}

/// `do_register` 末尾会单独创建的「只创 sub_key、不写任何 value」的路径列表。
/// 用于 `Implemented Categories\{CATID}` 等 Windows 只看 key 存在的项。
///
/// 返回的路径是完整 HKCU 相对路径,例如
/// `SOFTWARE\Classes\CLSID\{...}\Implemented Categories\{36C679D9-...}`。
///
/// v0.8 加 9 个标准 CATID 子键到 TIP\Category 树 ——
/// TIPC Settings UI 过滤 TIP 时按这些 Category 分类。没有这些子键,
/// Settings 根本不会列出 Prisir IME。
pub fn key_only_paths() -> Vec<String> {
    let clsid = CLSID_PRISIR_IME_STR;
    let mut paths = vec![
        // CLSID\Implemented Categories\{CATID} — TIPC ICatInformation 枚举关键
        format!(r"SOFTWARE\Classes\CLSID\{clsid}\Implemented Categories\{CATEGORY_TIP_KEYBOARD}"),
        format!(r"SOFTWARE\Classes\CLSID\{clsid}\Implemented Categories\{CATEGORY_TIP_KEYBOARD2}"),
        format!(r"SOFTWARE\Classes\CLSID\{clsid}\Implemented Categories\{CATEGORY_TIP_KEYBOARD3}"),
    ];

    // TIP\Category\Category\{CATID}\{TIP_CLSID} + TIP\Category\Item\{TIP_CLSID}\{CATID}
    // — Sogou 模板全套 9 个标准 CATID,见 hotkey/猫叔笔记 §3。
    for cat in CATID_GUIDS {
        paths.push(format!(
            r"SOFTWARE\Microsoft\CTF\TIP\{clsid}\Category\Category\{cat}\{clsid}"
        ));
        paths.push(format!(
            r"SOFTWARE\Microsoft\CTF\TIP\{clsid}\Category\Item\{clsid}\{cat}"
        ));
    }

    paths
}

/// v0.8 Sogou 对齐的标准 9 个 CATID — 必须完整,Settings UI 才把 Prisir 列在键盘分类。
///
/// 实测(2026-08-31 vm-sata):这些 GUID 必须写到
/// `HKLM\..\TIP\{CLSID}\Category\Category\{CATID}\{CLSID}`(**双层嵌套**,fix6)
/// TIPC 系统级枚举才能看见 Prisir。仅写到 `HKLM\..\Classes\CLSID\..\Implemented Categories`
/// (fix3 老路径)Settings 「添加键盘」列表里找不到 Prisir。
///
/// 以下注释的「GUID 名」来自 Sogou/微软拼音 CTF 树实测枚举结果,
/// 与 msctf.h 公开的常量名不完全对应。**GUID 值对就行**,名字标错不影响。
pub const CATID_GUIDS: &[&str] = &[
    "{046B8C80-1647-40F7-9B21-B93B81AABC1B}", // CATID_TIP_DISPLAYATTRIBUTEPROVIDER (Sogou 模板实测)
    "{13A016DF-560B-46CD-947A-4C3AF1E0E35D}", // CATID_TIPCAP_IMMERSIVESUPPORT
    "{25504FB4-7BAB-4BC1-9C69-CF81890F0EF5}", // CATID_TIPCAP_SYSTRAYSUPPORT
    "{34745C63-B2F0-4784-8B67-5E12C8701A31}", // GUID_TFCAT_TIP_KEYBOARD (键盘 TIP 关键)
    "{364215D9-75BC-11D7-A6EF-00065B84435C}", // CATID_TIPCAP_COMLESS
    "{364215DA-75BC-11D7-A6EF-00065B84435C}", // CATID_TIPCAP_WOW16
    "{49D2F9CE-1F5E-11D7-A6D3-00065B84435C}", // CATID_TIPCAP_SECUREMODE
    "{49D2F9CF-1F5E-11D7-A6D3-00065B84435C}", // CATID_TIPCAP_UIELEMENTENABLED
    "{CCF05DD7-4A87-11D7-A6E2-00065B84435C}", // CATID_TIPCAP_INPUTMODECOMPARTMENT
];

// =====================================================================
// 实际写 / 删 / 查注册表
// =====================================================================

/// `do_register` 的可观测结果。`entries_written` 计数方便 `--register` 报数。
#[derive(Debug, Clone)]
pub struct RegisterResult {
    pub entries_written: usize,
}

/// 打开 `HKEY_CURRENT_USER`。
///
/// windows-rs 0.58 的 `RegOpenCurrentUser(samdesired: u32, phkresult: *mut HKEY) -> WIN32_ERROR`。
/// 这里直接传 `u32`(`KEY_WRITE.0` / `KEY_READ.0`),不靠 `Param` 推断。
fn open_hkcu(sam: u32) -> Result<HKEY, WIN32_ERROR> {
    let mut hkcu: HKEY = HKEY::default();
    let status = unsafe { RegOpenCurrentUser(sam, &mut hkcu as *mut HKEY) };
    if status == ERROR_SUCCESS {
        Ok(hkcu)
    } else {
        Err(status)
    }
}

/// 换 profile GUID 时清理旧占位 GUID 的 LanguageProfile 子键。
///
/// 历史占位 GUID(见 `LEGACY_PROFILE_GUIDS`)残留在
/// `HKCU/HKLM\..\TIP\{CLSID}\LanguageProfile\0x00000804\{OLD_GUID}` 会让
/// InputMethodTips 留旧项,Windows 校验旧 GUID 无效 → 每次重启跳「正在安装新键盘」。
/// best-effort 删除(失败仅 eprintln,不阻断注册)。
fn migrate_legacy_profile_guids() {
    let hkcu = match open_hkcu(KEY_WRITE.0) {
        Ok(h) => h,
        Err(_) => return,
    };
    unsafe {
        for old in LEGACY_PROFILE_GUIDS {
            if *old == PROFILE_GUID {
                continue; // 没换 GUID 就跳过
            }
            for hive in [hkcu, HKEY_LOCAL_MACHINE] {
                let path = format!(
                    r"SOFTWARE\Microsoft\CTF\TIP\{}\LanguageProfile\0x0000{}\{}",
                    CLSID_PRISIR_IME_STR, LANGID_ZH_CN, old
                );
                let w = to_wide(&path);
                let status = RegDeleteTreeW(hive, PCWSTR(w.as_ptr()));
                if status == ERROR_SUCCESS {
                    eprintln!("[migrate] 删除旧 profile GUID 子键: {path}");
                }
                // ERROR_FILE_NOT_FOUND / ACCESS_DENIED 都静默(HKLM 可能没 admin)。

                // 同时清 InputMethodTips 里残留的旧 GUID 值名
                // (HKCU\Control Panel\International\User Profile\zh-Hans-CN 下
                //  值名 = "0804:{CLSID}{OLD_GUID}",不删会一直触发「安装新键盘」)。
                let profile_key = format!(
                    r"Control Panel\International\User Profile\zh-Hans-CN"
                );
                let pk_w = to_wide(&profile_key);
                let mut pk: HKEY = HKEY::default();
                if RegOpenKeyExW(hkcu, PCWSTR(pk_w.as_ptr()), 0, KEY_WRITE, &mut pk) == ERROR_SUCCESS {
                    let val_name = format!("0804:{}{}", CLSID_PRISIR_IME_STR, old);
                    let vn_w = to_wide(&val_name);
                    if RegDeleteValueW(pk, PCWSTR(vn_w.as_ptr())) == ERROR_SUCCESS {
                        eprintln!("[migrate] 删除 InputMethodTips 旧项: {val_name}");
                    }
                    let _ = RegCloseKey(pk);
                }
            }
        }
        let _ = RegCloseKey(hkcu);
    }
}

/// 捕获 `HKCU\CTF\Assemblies\0x0804\{layout}` 当前的占用者(Default/Profile),
/// 供卸载时恢复。返回 `(default_clsid, profile_guid)`;没有占用者(全新机器)返 None。
/// 只读,不改任何值。失败/无值不致命。
fn capture_asm_incumbent() -> Option<(String, String)> {
    let path = format!(
        r"SOFTWARE\Microsoft\CTF\Assemblies\{ASM_LANGID_DIR}\{ASM_LAYOUT_GUID}"
    );
    let read_sz = |name: &str| -> Option<String> {
        let hkcu = open_hkcu(KEY_READ.0).ok()?;
        let mut key: HKEY = HKEY::default();
        let path_w = to_wide(&path);
        let st = unsafe { RegOpenKeyExW(hkcu, PCWSTR(path_w.as_ptr()), 0, KEY_READ, &mut key) };
        if st != ERROR_SUCCESS {
            let _ = unsafe { RegCloseKey(hkcu) };
            return None;
        }
        let name_w = to_wide(name);
        // 先查类型+长度, 再读。
        let mut kind = REG_VALUE_TYPE(0);
        let mut len: u32 = 0;
        let q = unsafe {
            RegQueryValueExW(key, PCWSTR(name_w.as_ptr()), None, Some(&mut kind), None, Some(&mut len))
        };
        if q != ERROR_SUCCESS || kind != REG_SZ || len == 0 {
            let _ = unsafe { RegCloseKey(key) };
            let _ = unsafe { RegCloseKey(hkcu) };
            return None;
        }
        let mut buf: Vec<u16> = vec![0u16; (len as usize / 2) + 1];
        let q2 = unsafe {
            RegQueryValueExW(
                key,
                PCWSTR(name_w.as_ptr()),
                None,
                None,
                Some(buf.as_mut_ptr() as *mut u8),
                Some(&mut len),
            )
        };
        let _ = unsafe { RegCloseKey(key) };
        let _ = unsafe { RegCloseKey(hkcu) };
        if q2 != ERROR_SUCCESS {
            return None;
        }
        let s = String::from_utf16_lossy(&buf)
            .trim_end_matches('\0')
            .to_string();
        if s.is_empty() { None } else { Some(s) }
    };
    let default = read_sz("Default")?;
    let profile = read_sz("Profile").unwrap_or_default();
    Some((default, profile))
}

/// 把 `HKCU\CTF\Assemblies\0x0804\{layout}` 的 Default/Profile 改成指定 TIP。
/// `prev` 为 None 时写 Prisir 自己; Some((clsid, profile)) 时恢复该占用者。
/// 这是「装完即用」的核心: ctfmon 运行时读这条映射, 写完任务栏输入法即变,
/// 不需要重启 explorer。0x0804 布局键同一时刻只绑一个 TIP(与搜狗共存互斥)。
/// 失败不致命(WARN) — 注册表主体已写, 注销重登后 TIPC 仍会枚举生效。
fn write_asm_map(prev: Option<(&str, &str)>) -> Result<(), String> {
    let (clsid, profile) = match prev {
        Some((c, p)) => (c.to_string(), p.to_string()),
        None => (
            CLSID_PRISIR_IME_STR.to_string(),
            PROFILE_GUID.to_string(),
        ),
    };
    let path = format!(
        r"SOFTWARE\Microsoft\CTF\Assemblies\{ASM_LANGID_DIR}\{ASM_LAYOUT_GUID}"
    );
    let hkcu = open_hkcu(KEY_WRITE.0)
        .map_err(|e| format!("RegOpenCurrentUser 失败: {:?}", WIN32_ERROR(e.0)))?;
    let path_w = to_wide(&path);
    let mut key: HKEY = HKEY::default();
    let st = unsafe {
        RegCreateKeyExW(
            hkcu,
            PCWSTR(path_w.as_ptr()),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    };
    if st != ERROR_SUCCESS {
        let _ = unsafe { RegCloseKey(hkcu) };
        return Err(format!("RegCreateKeyExW({path}) 失败: {:?}", WIN32_ERROR(st.0)));
    }
    let r = (|| -> Result<(), String> {
        unsafe {
            write_sz(key, "Default", &clsid)?;
            if !profile.is_empty() {
                write_sz(key, "Profile", &profile)?;
            }
            write_dword(key, "KeyboardLayout", ASM_KEYBOARD_LAYOUT)?;
        }
        Ok(())
    })();
    let _ = unsafe { RegCloseKey(key) };
    let _ = unsafe { RegCloseKey(hkcu) };
    r
}

/// 卸载时恢复 Assemblies 布局键给安装前的占用者(搜狗等)。
/// 在 do_unregister 里, 删 Prisir 注册表之前调用(此时还能读到当前 Default=Prisir)。
/// 策略:
///  - 当前 Default 不是 Prisir → 已被别的 IME 抢走, 不动(避免误伤)。
///  - 当前是 Prisir, 且我们存过原占用者 → 写回原占用者。
///  - 当前是 Prisir 但没存过(全新机器) → 清空为占位 0-GUID(与 host 干净态一致)。
/// 失败不致命(WARN)。
fn restore_asm_incumbent() {
    let path = format!(
        r"SOFTWARE\Microsoft\CTF\Assemblies\{ASM_LANGID_DIR}\{ASM_LAYOUT_GUID}"
    );
    let cur = capture_asm_incumbent();
    let cur_default = cur.as_ref().map(|(d, _)| d.clone()).unwrap_or_default();
    let prisir = CLSID_PRISIR_IME_STR.trim_matches('{').trim_matches('}').to_uppercase();
    if !cur_default.to_uppercase().contains(&prisir) {
        eprintln!("[unregister] Assemblies 布局键当前占用者不是 Prisir({cur_default}), 不动。");
        return;
    }
    // 读我们存的备份(HKLM, 跨 SYSTEM/用户可读, 见 ASM_BACKUP_HKLM 注释)。
    let read_backup = |name: &str| -> Option<String> {
        let mut key: HKEY = HKEY::default();
        let pw = to_wide(ASM_BACKUP_HKLM);
        if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(pw.as_ptr()), 0, KEY_READ, &mut key) } != ERROR_SUCCESS {
            return None;
        }
        let nw = to_wide(name);
        let mut kind = REG_VALUE_TYPE(0);
        let mut len: u32 = 0;
        if unsafe { RegQueryValueExW(key, PCWSTR(nw.as_ptr()), None, Some(&mut kind), None, Some(&mut len)) } != ERROR_SUCCESS || kind != REG_SZ || len == 0 {
            let _ = unsafe { RegCloseKey(key) };
            return None;
        }
        let mut buf: Vec<u16> = vec![0u16; (len as usize / 2) + 1];
        let ok = unsafe {
            RegQueryValueExW(key, PCWSTR(nw.as_ptr()), None, None, Some(buf.as_mut_ptr() as *mut u8), Some(&mut len))
        };
        let _ = unsafe { RegCloseKey(key) };
        if ok != ERROR_SUCCESS { return None; }
        let s = String::from_utf16_lossy(&buf).trim_end_matches('\0').to_string();
        if s.is_empty() { None } else { Some(s) }
    };
    let prev_default = read_backup("AsmPrevDefault");
    let prev_profile = read_backup("AsmPrevProfile").unwrap_or_default();
    let target = match prev_default {
        Some(d) if !d.is_empty() => Some((d, prev_profile)),
        _ => None,
    };
    match target {
        Some((d, p)) => {
            if let Err(e) = write_asm_map(Some((&d, &p))) {
                eprintln!("[unregister] WARN: 恢复 Assemblies 占用者失败(不致命): {e}");
            } else {
                eprintln!("[unregister] 已恢复 Assemblies 布局键给原输入法: {d}");
            }
        }
        None => {
            // 全新机器无原占用者 — 清空为占位 0-GUID(host 干净态就是这样)。
            if let Err(e) = write_asm_map(Some((
                "{00000000-0000-0000-0000-000000000000}",
                "{00000000-0000-0000-0000-000000000000}",
            ))) {
                eprintln!("[unregister] WARN: 清空 Assemblies 布局键失败(不致命): {e}");
            } else {
                eprintln!("[unregister] Assemblies 布局键已清空(无原占用者)。");
            }
        }
    }
}

/// 把 `build_reg_tree(dll_path)` 全部写入 HKCU。
///
/// 错误: 任一 key 创不出来 / value 设不上就立刻返回 Err, 已经写入的不会回滚
/// (best-effort — Windows 注册表不是事务型, 留 `--unregister` 兜底)。
///
/// **不写系统全局**, **不链 winreg**, **不调 `HKEY_LOCAL_MACHINE`**。
pub fn do_register(dll_path: &str) -> Result<RegisterResult, String> {
    let dll = normalize_dll_path(dll_path);
    let tree = build_reg_tree(&dll);

    // ---- 旧 profile GUID 迁移清理(2026-09-01) ----
    // 换 GUID 时,旧占位 GUID 的 LanguageProfile 子键还残留在注册表,
    // InputMethodTips 会留着旧项。注册前先把旧 GUID 子键删掉,只留新 GUID。
    migrate_legacy_profile_guids();

    let hkcu = open_hkcu(KEY_WRITE.0)
        .map_err(|e| format!("RegOpenCurrentUser(KEY_WRITE) 失败: {:?}", WIN32_ERROR(e.0)))?;

    let mut written = 0usize;
    for entry in &tree {
        let (sub_key, value_name, kind, bytes) = entry.into_parts();

        let sub_key_w = to_wide(&sub_key);
        let mut new_key: HKEY = HKEY::default();
        let lstatus = unsafe {
            RegCreateKeyExW(
                hkcu,
                PCWSTR(sub_key_w.as_ptr()),
                0,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut new_key,
                None,
            )
        };
        if lstatus != ERROR_SUCCESS {
            let _ = unsafe { RegCloseKey(hkcu) };
            return Err(format!(
                "RegCreateKeyExW({sub_key}) 失败: {:?}",
                WIN32_ERROR(lstatus.0)
            ));
        }

        // value_name: 空串 = "(Default)" → 传 PCWSTR(null); 否则传名字的 PCWSTR。
        // 注意 windows-rs 0.58 的 `Param<PCWSTR>` 接受 `PCWSTR`(按值)和 `&PCWSTR`,不接受 `Option<PCWSTR>`。
        let value_name_pcwstr = if value_name.is_empty() {
            PCWSTR(std::ptr::null())
        } else {
            let w = to_wide(&value_name);
            PCWSTR(w.as_ptr())
        };

        let sstatus = unsafe {
            RegSetValueExW(
                new_key,
                value_name_pcwstr,
                0,
                kind,
                Some(&bytes),
            )
        };
        let _ = unsafe { RegCloseKey(new_key) };

        if sstatus != ERROR_SUCCESS {
            let _ = unsafe { RegCloseKey(hkcu) };
            return Err(format!(
                "RegSetValueExW({sub_key}\\{value_name}) 失败: {:?}",
                WIN32_ERROR(sstatus.0)
            ));
        }
        written += 1;
    }

    // ---- T12 hotfix: 单独创建 `Implemented Categories\{CATID}` 三个 sub_key ----
    // 不写任何 value —— Windows ICatInformation 只看 key 存在与否。
    for kp in key_only_paths() {
        let w = to_wide(&kp);
        let mut new_key: HKEY = HKEY::default();
        let lstatus = unsafe {
            RegCreateKeyExW(
                hkcu,
                PCWSTR(w.as_ptr()),
                0,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut new_key,
                None,
            )
        };
        let _ = unsafe { RegCloseKey(new_key) };
        if lstatus != ERROR_SUCCESS {
            let _ = unsafe { RegCloseKey(hkcu) };
            return Err(format!(
                "RegCreateKeyExW(key-only: {kp}) 失败: {:?}",
                WIN32_ERROR(lstatus.0)
            ));
        }
        written += 1;
    }

    let _ = unsafe { RegCloseKey(hkcu) };

    // ---- 2026-09-05: 装完即用 — 占 0x0804 布局键, 写 HKCU Assemblies 映射 ----
    //
    // 用户决策(2026-09-05): 学搜狗, 装完就让 Prisir 上屏, 之后切不切是用户的事。
    // 机制: ctfmon 运行时读 HKCU\CTF\Assemblies\0x0804\{layout} 的 Default/Profile,
    // 写完任务栏输入法即变, 不需要重启 explorer。0x0804 布局键同一时刻只绑一个 TIP,
    // 我们占它 = 把当前默认输入法临时换成 Prisir(卸载时恢复原占用者, 见 do_unregister)。
    //
    // 步骤: 1) 捕获现有占用者存到 HKLM(跨 SYSTEM/用户可读, 见 ASM_BACKUP_HKLM 注释);
    //       2) 写 Prisir 为占用者(写当前进程看到的 HKCU)。
    // 失败不致命(WARN) — 注册表主体已写, 注销重登后 TIPC 仍会枚举生效。
    let incumbent = capture_asm_incumbent();
    if let Some((d, p)) = &incumbent {
        let pw = to_wide(ASM_BACKUP_HKLM);
        let mut bkey: HKEY = HKEY::default();
        let st = unsafe {
            RegCreateKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(pw.as_ptr()), 0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut bkey, None)
        };
        if st == ERROR_SUCCESS {
            let _ = unsafe { write_sz(bkey, "AsmPrevDefault", d) };
            let _ = unsafe { write_sz(bkey, "AsmPrevProfile", p) };
            let _ = unsafe { RegCloseKey(bkey) };
            eprintln!("[register] 已记录 Assemblies 原占用者到 HKLM(卸载时恢复): {d}");
        } else {
            eprintln!("[register] WARN: 备份原占用者到 HKLM 失败(不致命, 卸载将无法自动恢复): {:?}", WIN32_ERROR(st.0));
        }
    }
    if let Err(e) = write_asm_map(None) {
        eprintln!("[register] WARN: 写 Assemblies 装完即用映射失败(不致命): {e}");
        eprintln!("[register]   注销重登后 TIPC 仍会枚举生效; 装完即用失效。");
    } else {
        eprintln!("[register] 已占 0x0804 布局键, 写入装完即用 Assemblies 映射(任务栏即时生效)");
    }

    // ---- T17 → T24: 同步写 HKLM\Classes\CLSID (TIPC 枚举关键) ----
    //
    // 实测:vm-sata 上 HKCU\Classes\CLSID\{Prisir}\InprocServer32 写全了,
    // PowerShell 同会话 CoCreateInstance 也成功(走 user token 看得到 HKCU),
    // 但 TIPC/ctfmon 走的是另一条 token,**看不到 HKCU\Classes**,所以枚举时
    // CoCreateInstance 返 REGDB_E_CLASSNOTREG,TIPC 静默把 Prisir 从 Assemblies
    // 里 drop。Sogou/搜狗 / 微软自带 IME 都是 HKLM\Classes\CLSID 装系统级,所以
    // CoCreateInstance 永远成功。
    //
    // --register 末尾再写一遍 HKLM\Classes\CLSID 路径(只写 COM CLSID 那 3 项,
    // 不写 CTF TIP 树 — CTF TIP 树还是 HKCU only,per-user)。需 admin 权限,
    // **T24 hotfix**:admin 缺失时**不再静默成功**(宪法 §5b 关键路径不许假 PASS)。
    // HKLM\..\InprocServer32 写失败 → TIPC 永远激活不了 Prisir → 必须返 Err,
    // 让用户去用 --register-elevated 或 admin PowerShell 手动跑。
    if let Err(e) = write_hklm_com_class(&dll) {
        return Err(format!(
            "写 HKLM\\Classes\\CLSID\\{}\\InprocServer32 失败: {}\n\
             修复路径:\n\
             1. `prisir_tsfsvc.exe --register-elevated` (自动 UAC 提权)\n\
             2. 或管理员 PowerShell: `Start-Process prisir_tsfsvc.exe -ArgumentList '--register' -Verb RunAs`\n\
             3. 或 regsvr32: `regsvr32 \"C:\\Program Files\\PrisirIME\\prisir_ime_tsf.dll\"` (待 T24 P3 实现 DllRegisterServer)\n\
             没 HKLM 这层时 TIPC/ctfmon 调 CoCreateInstance(CLSID_PRISIR_IME) 会返 REGDB_E_CLASSNOTREG,\n\
             TIPC 静默 drop Prisir,Assemblies 永远没 Prisir — --register 必须返 Err。",
            CLSID_PRISIR_IME_STR, e
        ));
    }
    eprintln!("[register] HKLM\\Classes\\CLSID 已写入(系统级 COM 可见)");

    // ---- T25 fix6: 同步写 HKLM\..\TIP\{CLSID} 整树 ----
    //
    // 实测(2026-08-31 vm-sata):TIPC 系统级枚举真实路径是
    //   HKLM\SOFTWARE\Microsoft\CTF\TIP\{CLSID}\Category\Category\{CATID}\{CLSID}
    // (**双层嵌套,9 个 CATID**)。fix3 老路径 `HKLM\Classes\CLSID\..\Implemented Categories`
    // Settings 「添加键盘」列表里**看不到** Prisir — fix6 走了正确路径才修好。
    //
    // 此外 `HKLM\..\TIP\{CLSID}\LanguageProfile\0x00000804\{PROFILE_GUID}\Enable = 1`
    // 是 TIPC 决定「拉起该 TIP」的关键字段 — 仅 HKCU 这层 ctfmon 走 SYSTEM token
    // 看不到,Settings 选中后 ctfmon 不会 LoadLibrary DLL。
    //
    // **T25 hotfix**:admin 缺失时**不再静默成功**(宪法 §5b 关键路径不许假 PASS)。
    // 任何一项写失败 → 必须返 Err,让用户走 --register-elevated。
    if let Err(e) = write_hklm_tip_tree(&dll) {
        return Err(format!(
            "写 HKLM\\..\\TIP\\{} 树失败: {}\n\
             修复路径:\n\
             1. `prisir_tsfsvc.exe --register-elevated` (自动 UAC 提权)\n\
             2. 或管理员 PowerShell: `Start-Process prisir_tsfsvc.exe -ArgumentList '--register' -Verb RunAs`\n\
             没 HKLM TIP 树时 Settings → 添加键盘 列表里看不到 Prisir(已实测)。",
            CLSID_PRISIR_IME_STR, e
        ));
    }
    eprintln!("[register] HKLM\\..\\TIP 树已写入(系统级 TIPC 枚举可见)");

    // ---- 2026-09-05: 清历史误写的 HKLM\..\CTF\Assemblies\{Prisir CLSID} 裸键 ----
    // 见 cleanup_malformed_hklm_assemblies 注释 — 该裸键非本代码写入,是历史残留,
    // 与 Sogou 状态不一致。幂等 + best-effort,不致命。
    cleanup_malformed_hklm_assemblies();

    // ---- T15: 调 ITfInputProcessorProfiles::Register 写 HKCU\CTF\Assemblies ----
    //
    // TIPC 的 `HKCU\SOFTWARE\Microsoft\CTF\Assemblies\0x<LANGID>\<CLSID>` 是当 IME
    // 主动调 `ITfInputProcessorProfiles::Register(CLSID)` COM API 时才写入的,
    // 不是 boot-time 从 HKCU\CTF\TIP\* 枚举的。仅写注册表时 TIPC 永远不会把它列
    // 进候选 IME 列表,所以用户启动 VM 后切不到 Prisir。
    //
    // 这里在 --register 末尾走 COM 调一遍 Register + AddLanguageProfile +
    // EnableLanguageProfile,触发 TIPC 把 Prisir 写入 Assemblies(并 enable 该
    // LanguageProfile)。失败不致命(注册表已写入,下次用户重启 explorer 或
    // reboot 时 TIPC 仍会枚举;但 enable 状态没改,默认 Disabled)。
    if let Err(e) = call_itf_register_api(&dll) {
        eprintln!("[register] WARN: ITfInputProcessorProfiles COM API 失败: {}", e);
        eprintln!("[register]   注册表已写入。下次 reboot / 重启 explorer 后 TIPC 会枚举 TIP,");
        eprintln!("[register]   但 Assemblies 与 EnableLanguageProfile 仍要靠 Settings UI 加输入法才能写入。");
    } else {
        eprintln!("[register] ITfInputProcessorProfiles::Register OK");
    }

    Ok(RegisterResult { entries_written: written })
}

// =====================================================================
// T15: ITfInputProcessorProfiles COM API 调用 — 写 HKCU\CTF\Assemblies
//
// TIPC 的 HKCU\CTF\Assemblies 不是 boot-time 从 HKCU\CTF\TIP\* 枚举生成的,
// 而是当 IME 主动调 ITfInputProcessorProfiles::Register COM API 时才写入。
// 仅写注册表时 TIPC 永远不会把 Prisir 列进候选 IME 列表,所以这里在
// do_register 末尾走 COM 调 Register/AddLanguageProfile/EnableLanguageProfile。
//
// 失败不致命 — 注册表已写入。TIPC 重启 explorer 后会再次枚举,
// 但用户需要走 Settings UI "添加键盘"才能激活 — 这个调用就是为了省掉那一步。
// =====================================================================

/// 把 CLSID / PROFILE 字符串转成 windows::core::GUID。
/// "08 04 00 00" 这种带 dash 的纯 GUID 字符串都接受,失败返 None。
fn parse_guid(s: &str) -> Result<windows::core::GUID, String> {
    let stripped = s.trim().trim_matches('{').trim_matches('}');
    // 标准 8-4-4-4-12 形式
    let hex: String = stripped.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return Err(format!("GUID 长度错: {s:?} → {hex:?}({} chars)", hex.len()));
    }
    let u128 = u128::from_str_radix(&hex, 16)
        .map_err(|e| format!("GUID 解析失败: {s:?} → {e:?}"))?;
    Ok(windows::core::GUID::from_u128(u128))
}

/// 调 ITfInputProcessorProfiles 把 Prisir 写入 HKCU\CTF\Assemblies 并 enable profile。
///
/// 这里用 raw vtable,因为 windows crate 给的 Register/AddLanguageProfile 等
/// 是 unsafe raw 方法,直接当 HRESULT 处理(`Result::err().code()` 可读 hr 值)。
fn call_itf_register_api(dll_path: &str) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let profile_cls = parse_guid(CLSID_PRISIR_IME_STR)?;
    let profile_id  = parse_guid(PROFILE_GUID)?;

    // DLL 路径做 wide 串(必须带末尾 NUL)。
    //
    // T25 真因: windows-rs 0.58 AddLanguageProfile 签名 `pchdesc: &[u16]`,
    // 内部把 as_ptr() 当 PCWSTR 传给 msctf.dll COM,TIPC 在 cchdesc 计数之外
    // **还会读 null terminator** 才停。如果 dll_w/desc_w 没 null,TIPC 会一直
    // 读到栈上残留字节 → 写入注册表的 Description/IconFile 变成 mojibake
    // (实测:`Prisir IME` + 后面残留 `ettirisir_????highentropyaslr`)。
    let dll_w: Vec<u16> = OsStr::new(dll_path)
        .encode_wide()
        .chain(std::iter::once(0u16))
        .collect();
    let desc_w: Vec<u16> = OsStr::new(DISPLAY_NAME)
        .encode_wide()
        .chain(std::iter::once(0u16))
        .collect();

    unsafe {
        // CoInitializeEx — 任何线程调 CoCreateInstance 前都要先 init COM。
        // hr=0x800401F0(CO_E_INIT)= 当前线程未 init。
        // 取 COINIT_APARTMENTTHREADED 与 ITfInputProcessorProfiles 默认值一致。
        // RPC_E_CHANGED_MODE(0x80010106) 不算错 — 线程已被另一个 init 模式占,直接忽略。
        let com_init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let com_initialized_here = com_init.is_ok();
        if !com_init.is_ok() && com_init.0 != 0x80010106u32 as i32 {
            return Err(format!("CoInitializeEx 失败: hr=0x{:08X}", com_init.0));
        }

        let result = (|| -> Result<(), String> {
            // CoCreateInstance(CLSID_TF_InputProcessorProfiles, ..., CLSCTX_INPROC_SERVER, IID_ITfInputProcessorProfiles)
            // CoCreateInstance 的第二个 outer 参数是 Option<&IUnknown>,这里传 None
            let profiles: ITfInputProcessorProfiles = CoCreateInstance(
                &CLSID_TF_InputProcessorProfiles,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(|e| format!("CoCreateInstance(CLSID_TF_InputProcessorProfiles) 失败: hr=0x{:08X}", e.code().0))?;

            // 1) Register(CLSID) — 触发 TIPC 写 HKCU\CTF\Assemblies\0x<LANGID>\<CLSID>
            profiles
                .Register(&profile_cls)
                .map_err(|e| format!("Register hr=0x{:08X}", e.code().0))?;

            // 2) AddLanguageProfile — 把 0x0804 zh-CN profile 注册到 TIP
            profiles
                .AddLanguageProfile(
                    &profile_cls,
                    u16::from_str_radix(LANGID_ZH_CN, 16).unwrap_or(0x0804),
                    &profile_id,
                    &desc_w,
                    &dll_w,
                    0,
                )
                .map_err(|e| format!("AddLanguageProfile hr=0x{:08X}", e.code().0))?;

            // 3) EnableLanguageProfile — enable 这个 profile,否则 TIPC 不会激活
            profiles
                .EnableLanguageProfile(
                    &profile_cls,
                    u16::from_str_radix(LANGID_ZH_CN, 16).unwrap_or(0x0804),
                    &profile_id,
                    BOOL(1),
                )
                .map_err(|e| format!("EnableLanguageProfile hr=0x{:08X}", e.code().0))?;

            Ok(())
        })();

        if com_initialized_here {
            CoUninitialize();
        }
        result
    }
}

/// 把 HKCU 写过的 COM CLSID 3 项也写到 HKLM\Classes\CLSID(系统级)。
///
/// TIPC/ctfmon 枚举 TIP 时,即使在同一 user session,实际进程 token 看到的
/// HKCU 与 user PowerShell 看到的不一样 — 它走的是 LocalService/SYSTEM 那套
/// filtered token,看不到 HKCU\Classes。HKLM\Classes 它看得到,所以 Sogou /
/// 微软自带 IME 都装系统级。
///
/// 这里**仅写 COM CLSID 那 3 项**(Default + InprocServer32\Default +
/// ThreadingModel)。**CTF TIP 树由 `write_hklm_tip_tree` 单独写**(fix6 真因)。
/// `HKLM\..\Classes\CLSID\..\Implemented Categories` 这条老路径对 TIPC
/// Settings 列表**无效**,我们继续按 Sogou 兼容风格写在 HKCU 那侧就好。
///
/// 失败不致命,只打 WARN。需 admin 时返 Err(非 admin 调 RegCreateKeyExW
/// 返 ERROR_ACCESS_DENIED)。
fn write_hklm_com_class(dll_path: &str) -> Result<(), String> {
    let dll = normalize_dll_path(dll_path);
    let clsid = CLSID_PRISIR_IME_STR;

    unsafe {
        // Open HKLM\SOFTWARE\Classes\CLSID
        let mut classes: HKEY = HKEY::default();
        let status = RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(to_wide("SOFTWARE\\Classes\\CLSID").as_ptr()),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut classes,
            None,
        );
        if status != ERROR_SUCCESS {
            return Err(format!(
                "RegCreateKeyExW(HKLM\\SOFTWARE\\Classes\\CLSID) 失败: {:?}",
                WIN32_ERROR(status.0)
            ));
        }

        // 1) HKLM\Classes\CLSID\{clsid} (Default) = "Prisir IME"
        let mut clsid_key: HKEY = HKEY::default();
        let s1 = RegCreateKeyExW(
            classes,
            PCWSTR(to_wide(&format!("{{{}}}", clsid.trim_matches('{').trim_matches('}'))).as_ptr()),
            0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None,
            &mut clsid_key, None,
        );
        if s1 != ERROR_SUCCESS {
            let _ = RegCloseKey(classes);
            return Err(format!("RegCreateKeyExW(HKLM CLSID\\{}) 失败: {:?}", clsid, WIN32_ERROR(s1.0)));
        }
        let default_str = to_wide("Prisir 灵犀拼音");
        let default_bytes: Vec<u8> = default_str.iter().flat_map(|w| w.to_le_bytes()).collect();
        let s1b = RegSetValueExW(clsid_key, PCWSTR(std::ptr::null()), 0, REG_SZ, Some(&default_bytes));
        if s1b != ERROR_SUCCESS {
            let _ = RegCloseKey(clsid_key);
            let _ = RegCloseKey(classes);
            return Err(format!("RegSetValueExW(HKLM CLSID\\{}\\(Default)) 失败: {:?}", clsid, WIN32_ERROR(s1b.0)));
        }

        // 2) HKLM\Classes\CLSID\{clsid}\InprocServer32 (Default) = dll path
        let mut inproc_key: HKEY = HKEY::default();
        let s2 = RegCreateKeyExW(
            clsid_key,
            PCWSTR(to_wide("InprocServer32").as_ptr()),
            0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None,
            &mut inproc_key, None,
        );
        let _ = RegCloseKey(clsid_key);
        if s2 != ERROR_SUCCESS {
            let _ = RegCloseKey(classes);
            return Err(format!("RegCreateKeyExW(HKLM CLSID\\{}\\InprocServer32) 失败: {:?}", clsid, WIN32_ERROR(s2.0)));
        }
        let dll_w = to_wide(&dll);
        let dll_bytes: Vec<u8> = dll_w.iter().flat_map(|w| w.to_le_bytes()).collect();
        let s2b = RegSetValueExW(inproc_key, PCWSTR(std::ptr::null()), 0, REG_SZ, Some(&dll_bytes));
        if s2b != ERROR_SUCCESS {
            let _ = RegCloseKey(inproc_key);
            let _ = RegCloseKey(classes);
            return Err(format!("RegSetValueExW(InprocServer32\\(Default)) 失败: {:?}", WIN32_ERROR(s2b.0)));
        }

        // 3) HKLM\Classes\CLSID\{clsid}\InprocServer32\ThreadingModel = "Apartment"
        let tm_str = to_wide("Apartment");
        let tm_bytes: Vec<u8> = tm_str.iter().flat_map(|w| w.to_le_bytes()).collect();
        let s3 = RegSetValueExW(inproc_key, PCWSTR(to_wide("ThreadingModel").as_ptr()), 0, REG_SZ, Some(&tm_bytes));
        let _ = RegCloseKey(inproc_key);
        let _ = RegCloseKey(classes);

        if s3 != ERROR_SUCCESS {
            return Err(format!("RegSetValueExW(ThreadingModel) 失败: {:?}", WIN32_ERROR(s3.0)));
        }

        Ok(())
    }
}

/// T25 fix6: 把 HKCU TIP 树镜像写到 HKLM(系统级 TIPC 枚举关键)。
///
/// 写的内容(对照 `build_reg_tree` 与 `key_only_paths`):
///   1. HKLM\..\TIP\{CLSID}\DisplayDescription = "Prisir IME"
///   2. HKLM\..\TIP\{CLSID}\Description         = "Prisir IME"
///   3. HKLM\..\TIP\{CLSID}\IconFile            = <dll path>
///   4. HKLM\..\TIP\{CLSID}\LanguageProfile\0x00000804\{PROFILE_GUID}\*
///      (Description / IconFile / IconIndex / Enable=1 / HiddenInSettingUI=0 / SubItemInSettingUI=0)
///   5. HKLM\..\TIP\{CLSID}\Category\Category\{9 CATID}\{CLSID}  ← 双层嵌套,fix6 真因
///
/// **HKLM\..\Assemblies** 不在这里写 — 它由 `call_itf_register_api()` 调
/// `ITfInputProcessorProfiles::Register` COM 触发 TIPC 写入(per-user,
/// 跟随 user token,不在 HKLM)。
///
/// **HKLM\..\Classes\CLSID\..\Implemented Categories** 也不在这里写 ——
/// fix6 验证它对 TIPC Settings 列表**无效**,真路径在 TIP\Category\Category。
/// 我们仍由 `key_only_paths()` 在 HKCU 侧写 Sogou 兼容备份。
///
/// 需 admin。失败 → Err(宪法 §5b 关键路径不许假 PASS)。
fn write_hklm_tip_tree(dll_path: &str) -> Result<(), String> {
    let dll = normalize_dll_path(dll_path);
    let clsid = CLSID_PRISIR_IME_STR;
    let clsid_braced = format!("{{{}}}", clsid.trim_matches('{').trim_matches('}'));
    let clsid_tip = to_wide(&clsid_braced);
    let pg = PROFILE_GUID;
    let lang_hex = format!("0x0000{LANGID_ZH_CN}");

    unsafe {
        // Open HKLM\SOFTWARE\Microsoft\CTF\TIP
        let mut tip_root: HKEY = HKEY::default();
        let s = RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(to_wide(r"SOFTWARE\Microsoft\CTF\TIP").as_ptr()),
            0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None,
            &mut tip_root, None,
        );
        if s != ERROR_SUCCESS {
            return Err(format!(
                "RegCreateKeyExW(HKLM\\SOFTWARE\\Microsoft\\CTF\\TIP) 失败: {:?}",
                WIN32_ERROR(s.0)
            ));
        }

        // Open HKLM\..\TIP\{CLSID}
        let mut tip_cls: HKEY = HKEY::default();
        let s2 = RegCreateKeyExW(
            tip_root, PCWSTR(clsid_tip.as_ptr()),
            0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None,
            &mut tip_cls, None,
        );
        let _ = RegCloseKey(tip_root);
        if s2 != ERROR_SUCCESS {
            return Err(format!(
                "RegCreateKeyExW(HKLM\\..\\TIP\\{clsid}) 失败: {:?}",
                WIN32_ERROR(s2.0)
            ));
        }

        // ---- 顶层值 ----
        write_sz(tip_cls, "DisplayDescription", DISPLAY_NAME)?;
        write_sz(tip_cls, "Description", DISPLAY_NAME)?;
        write_sz(tip_cls, "IconFile", &dll)?;
        write_dword(tip_cls, "IconIndex", 0)?;
        // 2026-09-01 实锤「已禁用/新键盘接入」叉根因之一:
        // 父 TIP key 残留 legacy REG_SZ "Enable"="1",MS拼音/搜狗父 key 都没有。
        // 现代 ctfmon 只读 LanguageProfile\...\Enable(DWORD);父 key 这个旧串值会被
        // 早期兼容路径误读,配合 ActivateEx 抛错触发框架把 TIP 标记为「已禁用键盘」。
        // 显式删掉,对齐 Sogou/MS拼音父 key(无 Enable)。
        let _ = delete_value(tip_cls, "Enable");

        // ---- LanguageProfile\0x00000804\{PROFILE_GUID} ----
        let lp_sub = format!(r"LanguageProfile\{lang_hex}\{pg}");
        let lp_sub_w = to_wide(&lp_sub);
        let mut lp_key: HKEY = HKEY::default();
        let s3 = RegCreateKeyExW(
            tip_cls, PCWSTR(lp_sub_w.as_ptr()),
            0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None,
            &mut lp_key, None,
        );
        if s3 != ERROR_SUCCESS {
            let _ = RegCloseKey(tip_cls);
            return Err(format!(
                "RegCreateKeyExW(HKLM TIP\\LanguageProfile\\...) 失败: {:?}",
                WIN32_ERROR(s3.0)
            ));
        }
        write_sz(lp_key, "Description", DISPLAY_NAME)?;
        write_sz(lp_key, "DisplayDescription", DISPLAY_NAME)?;
        write_sz(lp_key, "IconFile", &dll)?;
        write_dword(lp_key, "IconIndex", 0)?;
        // T25: LanguageProfile\Enable=1 是 TIPC 拉起 TIP 的关键字段 —
        // ctfmon 启动时枚举到 Enable=0 的 profile 直接跳过不 LoadLibrary。
        write_dword(lp_key, "Enable", 1)?;
        write_dword(lp_key, "HiddenInSettingUI", 0)?;
        write_dword(lp_key, "SubItemInSettingUI", 0)?;
        // 清掉 fix6 留下的旧字段(同名 typo plural),免得下个 ctfmon 误读
        // legacy path 走 fix6 旧路径触发 +0x1525。
        let _ = delete_value(lp_key, "Enabled");
        let _ = RegCloseKey(lp_key);

        // ---- Category\Category\{9 CATID}\{CLSID} (双层嵌套,fix6 真因) ----
        let cat_root_w = to_wide(r"Category\Category");
        let mut cat_root: HKEY = HKEY::default();
        let s4 = RegCreateKeyExW(
            tip_cls, PCWSTR(cat_root_w.as_ptr()),
            0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None,
            &mut cat_root, None,
        );
        // 注意: tip_cls 不在这里 close — 后面 Category\Item 段还要用它当 parent。
        // 统一在 Category\Item 写完后 close(见下)。
        if s4 != ERROR_SUCCESS {
            let _ = RegCloseKey(tip_cls);
            return Err(format!(
                "RegCreateKeyExW(HKLM TIP\\Category\\Category) 失败: {:?}",
                WIN32_ERROR(s4.0)
            ));
        }
        for cat in CATID_GUIDS {
            // Category\Category\{CATID}
            let cat_key_w = to_wide(cat);
            let mut cat_key: HKEY = HKEY::default();
            let s5 = RegCreateKeyExW(
                cat_root, PCWSTR(cat_key_w.as_ptr()),
                0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None,
                &mut cat_key, None,
            );
            if s5 != ERROR_SUCCESS {
                let _ = RegCloseKey(cat_root);
                return Err(format!(
                    "RegCreateKeyExW(HKLM TIP\\Category\\Category\\{cat}) 失败: {:?}",
                    WIN32_ERROR(s5.0)
                ));
            }
            // Category\Category\{CATID}\{CLSID}
            let s6 = RegCreateKeyExW(
                cat_key, PCWSTR(clsid_tip.as_ptr()),
                0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None,
                &mut HKEY::default(),  // 不需要 handle
                None,
            );
            let _ = RegCloseKey(cat_key);
            if s6 != ERROR_SUCCESS {
                let _ = RegCloseKey(cat_root);
                return Err(format!(
                    "RegCreateKeyExW(HKLM TIP\\Category\\Category\\{cat}\\{clsid}) 失败: {:?}",
                    WIN32_ERROR(s6.0)
                ));
            }
        }
        let _ = RegCloseKey(cat_root);

        // ---- Category\Item\{CLSID}\{9 CATID} (Sogou/MS拼音实测同款,补齐「它们有的值」) ----
        //
        // 2026-09-01 三方 COM/TIP 全量值 diff (prisirtip-ime-tree-diff):
        //   Sogou HKLM TIP 树在 `Category\Item\{CLSID}\{CATID}` 下挂了完整 9 个 CATID 子键,
        //   MS拼音 8 个,Prisir 此前**只有 HKCU 侧**(key_only_paths),HKLM 侧缺这条。
        //   用户要求「系统中它们有的值我们都对照写上」→ HKLM 也补 Category\Item 镜像。
        // Windows ICatInformation / TIPC 枚举时双路(Category\Category + Category\Item)都读。
        let item_root_w = to_wide(&format!(r"Category\Item\{}", clsid_braced));
        let mut item_root: HKEY = HKEY::default();
        // 注意: 这里必须用 tip_cls(还开着),不能用 tip_root(line 850 已 close → ERROR_INVALID_HANDLE 6)。
        let si = RegCreateKeyExW(
            tip_cls, PCWSTR(item_root_w.as_ptr()),
            0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None,
            &mut item_root, None,
        );
        if si != ERROR_SUCCESS {
            return Err(format!(
                "RegCreateKeyExW(HKLM TIP\\Category\\Item\\{clsid}) 失败: {:?}",
                WIN32_ERROR(si.0)
            ));
        }
        for cat in CATID_GUIDS {
            // Category\Item\{CLSID}\{CATID}
            let _ = RegCreateKeyExW(
                item_root, PCWSTR(to_wide(cat).as_ptr()),
                0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None,
                &mut HKEY::default(),  // 不需要 handle,只创建 key
                None,
            );
        }
        let _ = RegCloseKey(item_root);
        // Category\Item 写完,现在才 close tip_cls(Category\Category 段不 close 它)。
        let _ = RegCloseKey(tip_cls);

        Ok(())
    }
}

/// HKLM 删除 value helper — 失败不致命(本来就不存在时返
/// ERROR_FILE_NOT_FOUND,我们当成功处理;其它错才返 Err)。
unsafe fn delete_value(key: HKEY, name: &str) -> Result<(), String> {
    let name_w = to_wide(name);
    let name_pcwstr = if name.is_empty() {
        PCWSTR(std::ptr::null())
    } else {
        PCWSTR(name_w.as_ptr())
    };
    let status = RegDeleteValueW(key, name_pcwstr);
    // ERROR_FILE_NOT_FOUND = value 本来就不存在,不算错。
    if status != ERROR_SUCCESS && status.0 != 2 {
        return Err(format!(
            "RegDeleteValueW({name}) 失败: {:?}",
            WIN32_ERROR(status.0)
        ));
    }
    Ok(())
}

/// HKLM REG_SZ 写 value helper — 失败返 Err(具体 OS 错误码)。
unsafe fn write_sz(key: HKEY, name: &str, value: &str) -> Result<(), String> {
    let name_w = to_wide(name);
    let val_w = to_wide(value);
    let bytes: Vec<u8> = val_w.iter().flat_map(|w| w.to_le_bytes()).collect();
    let name_pcwstr = if name.is_empty() {
        PCWSTR(std::ptr::null())
    } else {
        PCWSTR(name_w.as_ptr())
    };
    let status = RegSetValueExW(
        key, name_pcwstr, 0, REG_SZ, Some(&bytes),
    );
    if status != ERROR_SUCCESS {
        return Err(format!(
            "RegSetValueExW({name}={value:?}) 失败: {:?}",
            WIN32_ERROR(status.0)
        ));
    }
    Ok(())
}

/// HKLM REG_DWORD 写 value helper — 失败返 Err。
unsafe fn write_dword(key: HKEY, name: &str, value: u32) -> Result<(), String> {
    let name_w = to_wide(name);
    let bytes = value.to_le_bytes();
    let name_pcwstr = if name.is_empty() {
        PCWSTR(std::ptr::null())
    } else {
        PCWSTR(name_w.as_ptr())
    };
    let status = RegSetValueExW(
        key, name_pcwstr, 0, REG_DWORD, Some(&bytes),
    );
    if status != ERROR_SUCCESS {
        return Err(format!(
            "RegSetValueExW({name}={value}) 失败: {:?}",
            WIN32_ERROR(status.0)
        ));
    }
    Ok(())
}

/// 删除 Prisir IME 的全部注册表项 — HKCU per-user 部分 + HKLM 系统级部分。
/// 幂等: 子 key 不存在也返回 Ok。
///
/// **T25 hotfix**:之前只删 HKCU 两个根 key(TIP + COM CLSID),HKLM 那三处
/// (`HKLM\..\TIP`、`HKLM\..\Classes\CLSID`、`HKLM\..\Assemblies`)留着导致
/// 下次 `--register` 前 TIPC 还能枚举到 Prisir,Settings 仍能「看到」但注册表
/// 状态不一致;fix6/fix7 暴露 fix2 那次误删 HKCU TIP 后 unregister 残留
/// HKLM 半截状态。今天起两边一起删。
///
/// HKLM 删除需 admin(失败时 WARN 但不 Err — 用户可能本就没 admin)。
/// 2026-09-05 防御: 清掉历史误写的 HKLM\..\CTF\Assemblies\{Prisir CLSID} 裸键。
///
/// 真因: Sogou/MS 拼音的 HKLM\..\CTF\Assemblies 下**没有任何 TIP 的裸 CLSID 键**,
/// 真正的运行时映射在 **HKCU**\..\CTF\Assemblies\0x<LANGID>\{layout-GUID}(见
/// memory prisirtip-assemblies-runtime-activation-2026-09-05)。历史上某次
/// (旧版注册器或手动 reg add)错在 HKLM 下写了
/// `HKLM\..\CTF\Assemblies\{A1B2C3D4-..}` 裸键 + 默认值=dll路径,结构全错,
/// 与 Sogou 状态不一致,可能干扰 TIPC 枚举。当前 register.rs 已无此写入(全仓 grep
/// 无 HKLM Assemblies 写入点,已验证 --register 后不再复活),这里只是**清理旧残留**。
///
/// 幂等 + 不致命: key 不存在(ERROR_FILE_NOT_FOUND=2)视为成功; 删失败(如无 admin)
/// 仅 WARN 不 Err — 清理是 best-effort,不影响主注册路径。
fn cleanup_malformed_hklm_assemblies() {
    let key_path = format!(
        r"SOFTWARE\Microsoft\CTF\Assemblies\{CLSID_PRISIR_IME_STR}"
    );
    let w = to_wide(&key_path);
    unsafe {
        let status = RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(w.as_ptr()));
        match status.0 {
            0 => eprintln!("[register] 已清理历史误写 HKLM\\..\\CTF\\Assemblies\\{CLSID_PRISIR_IME_STR} 裸键"),
            2 => {} // 不存在 = 干净, 静默
            code => eprintln!(
                "[register] WARN: 清理 HKLM Assemblies 裸键失败 {:?}(不致命, 仅残留历史误写)",
                WIN32_ERROR(code)
            ),
        }
    }
}

pub fn do_unregister() -> Result<(), String> {
    // 2026-09-05: 先恢复 Assemblies 布局键给原占用者 — 必须在删 HKCU TIP 树之前调,
    // 因为恢复的备份(AsmPrevDefault/AsmPrevProfile)存在 TIP 树根下,删树就没了。
    restore_asm_incumbent();

    let hkcu = open_hkcu(KEY_WRITE.0)
        .map_err(|e| format!("RegOpenCurrentUser(KEY_WRITE) 失败: {:?}", WIN32_ERROR(e.0)))?;

    let hkcu_keys = [
        format!(r"SOFTWARE\Microsoft\CTF\TIP\{CLSID_PRISIR_IME_STR}"),
        format!(r"SOFTWARE\Classes\CLSID\{CLSID_PRISIR_IME_STR}"),
        // HKCU\..\Assemblies\{0x<LANGID>}\{CLSID} 路径跟 TIP CLSID 不在同一根,
        // 由 ITfInputProcessorProfiles::Unregister 删 — 我们不强删,留
        // TIPC 自己管理。否则下次重装时会残留。
    ];

    let mut last_err: Option<String> = None;
    unsafe {
        for k in &hkcu_keys {
            let w = to_wide(k);
            let status = RegDeleteTreeW(hkcu, PCWSTR(w.as_ptr()));
            // ERROR_FILE_NOT_FOUND = 2 → key 本来就不存在, 幂等视为成功
            if status != ERROR_SUCCESS && status.0 != 2 {
                last_err = Some(format!(
                    "RegDeleteTreeW(HKCU\\{k}) 失败: {:?}",
                    WIN32_ERROR(status.0)
                ));
            }
        }
        let _ = RegCloseKey(hkcu);

        // ---- HKLM 部分 — 需 admin,失败仅 WARN 不 Err ----
        let hklm_keys = [
            format!(r"SOFTWARE\Microsoft\CTF\TIP\{CLSID_PRISIR_IME_STR}"),
            format!(r"SOFTWARE\Classes\CLSID\{CLSID_PRISIR_IME_STR}"),
            // 2026-09-05: Assemblies 原占用者备份(ASM_BACKUP_HKLM), 卸载后清掉。
            ASM_BACKUP_HKLM.to_string(),
        ];
        for k in &hklm_keys {
            let w = to_wide(k);
            let status = RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(w.as_ptr()));
            if status != ERROR_SUCCESS && status.0 != 2 {
                // 仅 WARN — 用户可能没 admin(非 admin 时 ERROR_ACCESS_DENIED=5)。
                // 半残留状态下次 --register 走 admin 时会覆盖,不影响功能。
                eprintln!(
                    "[unregister] WARN: RegDeleteTreeW(HKLM\\{k}) 失败: {:?}",
                    WIN32_ERROR(status.0)
                );
            }
        }

        // 2026-09-05: 顺带清历史误写的 HKLM Assemblies 裸键(幂等,见该 fn 注释)。
        cleanup_malformed_hklm_assemblies();
    }

    match last_err {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

/// `--status` 的返回。两 key 都在 → REGISTERED; 都不在 → NOT REGISTERED; 其它 → PARTIAL。
#[derive(Debug, Clone)]
pub struct RegStatus {
    pub clsid: String,
    pub tip_key_exists: bool,
    pub clsid_key_exists: bool,
}

pub fn do_status() -> Result<RegStatus, String> {
    let hkcu = open_hkcu(KEY_READ.0)
        .map_err(|e| format!("RegOpenCurrentUser(KEY_READ) 失败: {:?}", WIN32_ERROR(e.0)))?;

    let tip = format!(r"SOFTWARE\Microsoft\CTF\TIP\{CLSID_PRISIR_IME_STR}");
    let clsid = format!(r"SOFTWARE\Classes\CLSID\{CLSID_PRISIR_IME_STR}");

    let tip_exists = key_exists(hkcu, &tip);
    let clsid_exists = key_exists(hkcu, &clsid);

    let _ = unsafe { RegCloseKey(hkcu) };

    Ok(RegStatus {
        clsid: CLSID_PRISIR_IME_STR.to_string(),
        tip_key_exists: tip_exists,
        clsid_key_exists: clsid_exists,
    })
}

fn key_exists(hkey: HKEY, sub_key: &str) -> bool {
    unsafe {
        let w = to_wide(sub_key);
        let mut opened: HKEY = HKEY::default();
        let status = RegOpenKeyExW(
            hkey,
            PCWSTR(w.as_ptr()),
            0,
            KEY_READ,
            &mut opened,
        );
        if status == ERROR_SUCCESS {
            let _ = RegCloseKey(opened);
            true
        } else {
            false
        }
    }
}

// =====================================================================
// --enable / --disable — 改 `HKCU\...\LanguageProfile\...\{PROFILE_GUID}\Enable` DWORD
//
// 与 `--unregister` 不同: 这里只翻 `Enable` 开关, dll 还在注册表里。
// 想要"消失"就用 `--unregister`。
// =====================================================================

/// `--enable` — `HKCU\...\LanguageProfile\0x00000804\{PROFILE_GUID}\Enable = 1`
pub fn do_enable() -> Result<(), String> {
    set_enable_dword(1)
}

/// `--disable` — `HKCU\...\LanguageProfile\0x00000804\{PROFILE_GUID}\Enable = 0`
pub fn do_disable() -> Result<(), String> {
    set_enable_dword(0)
}

/// `--activate` — 调 ITfInputProcessorProfiles::ActivateLanguageProfile,
/// 强制 ctfmon 把 Prisir DLL LoadLibrary 并 Activate。
///
/// T25+0x1525 真因调查用: EnableLanguageProfile 写注册表但 ctfmon 不一定立刻
/// 走 ITfInputProcessor::Activate 路径(只在用户切换到该 IME 时才会),所以光
/// --enable 看不到 crash。要复现 +0x1525,需要 ITfInputProcessorProfiles::
/// ActivateLanguageProfile。这正是 explorer.exe 在 Settings 选中 Prisir 时调的。
pub fn do_activate() -> Result<(), String> {
    let profile_cls = parse_guid(CLSID_PRISIR_IME_STR)?;
    let profile_id = parse_guid(PROFILE_GUID)?;

    unsafe {
        let com_init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let com_initialized_here = com_init.is_ok();
        if !com_init.is_ok() && com_init.0 != 0x80010106u32 as i32 {
            return Err(format!("CoInitializeEx 失败: hr=0x{:08X}", com_init.0));
        }

        let result = (|| -> Result<(), String> {
            let profiles: ITfInputProcessorProfiles = CoCreateInstance(
                &CLSID_TF_InputProcessorProfiles,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(|e| format!("CoCreateInstance 失败: hr=0x{:08X}", e.code().0))?;

            profiles
                .ActivateLanguageProfile(
                    &profile_cls,
                    u16::from_str_radix(LANGID_ZH_CN, 16).unwrap_or(0x0804),
                    &profile_id,
                )
                .map_err(|e| format!("ActivateLanguageProfile hr=0x{:08X}", e.code().0))?;

            Ok(())
        })();

        if com_initialized_here {
            CoUninitialize();
        }
        result
    }
}

/// `--activate-mgr` — 用**新版** `ITfInputProcessorProfileMgr::ActivateProfile` 激活。
///
/// T25「不可选」真因调查:旧版 `ITfInputProcessorProfiles::ActivateLanguageProfile`
/// (`do_activate`) 在 Win10 对现代 TSF TIP 稳定返 E_FAIL(0x80004005)。Win10 实际
/// 走的是 `ITfInputProcessorProfileMgr`(IID 71C6E74C-0F28-11D8-A82A-00065B84435C,
/// 与旧 CLSID_TF_InputProcessorProfiles 同一 COM 类,只是 QI 出更新接口)的
/// `ActivateProfile(dwProfileType=TF_PROFILETYPE_INPUTPROCESSOR, langid, clsid, profile, hkl, flags)`。
/// 这正是 Settings「添加键盘」选中某 IME 时系统调用的 API。
///
/// 风险同 do_activate: 触发 ctfmon LoadLibrary(prisir_ime_tsf.dll) + Activate,
/// 可能崩。LocalDumps 已配。
pub fn do_activate_mgr() -> Result<(), String> {
    let profile_cls = parse_guid(CLSID_PRISIR_IME_STR)?;
    let profile_id = parse_guid(PROFILE_GUID)?;

    unsafe {
        let com_init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let com_initialized_here = com_init.is_ok();
        if !com_init.is_ok() && com_init.0 != 0x80010106u32 as i32 {
            return Err(format!("CoInitializeEx 失败: hr=0x{:08X}", com_init.0));
        }

        let result = (|| -> Result<(), String> {
            let mgr: ITfInputProcessorProfileMgr = CoCreateInstance(
                &CLSID_TF_InputProcessorProfiles,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(|e| format!("CoCreateInstance(ProfileMgr) 失败: hr=0x{:08X}", e.code().0))?;

            mgr.ActivateProfile(
                TF_PROFILETYPE_INPUTPROCESSOR,
                u16::from_str_radix(LANGID_ZH_CN, 16).unwrap_or(0x0804),
                &profile_cls,
                &profile_id,
                windows::Win32::UI::Input::KeyboardAndMouse::HKL::default(), // hkl: TSF TIP 传 NULL
                0,    // dwFlags
            )
            .map_err(|e| format!("ActivateProfile hr=0x{:08X}", e.code().0))?;

            Ok(())
        })();

        if com_initialized_here {
            CoUninitialize();
        }
        result
    }
}

/// `--enum-profiles` — 诊断用:枚举 TIPC 对 zh-CN(0x0804) 看到的所有 input-processor
/// profile,打印 clsid/guidProfile/catid/dwFlags,确认 Prisir 是否被 TIPC 承认为
/// 可激活 profile。T25「不可选」E_FAIL 根因定位用。
pub fn do_enum_profiles() -> Result<(), String> {
    use windows::Win32::UI::TextServices::TF_INPUTPROCESSORPROFILE;
    unsafe {
        let com_init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let com_initialized_here = com_init.is_ok();
        if !com_init.is_ok() && com_init.0 != 0x80010106u32 as i32 {
            return Err(format!("CoInitializeEx 失败: hr=0x{:08X}", com_init.0));
        }
        let result = (|| -> Result<(), String> {
            let mgr: ITfInputProcessorProfileMgr = CoCreateInstance(
                &CLSID_TF_InputProcessorProfiles,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(|e| format!("CoCreateInstance(ProfileMgr) 失败: hr=0x{:08X}", e.code().0))?;
            let enum_p = mgr
                .EnumProfiles(0x0804)
                .map_err(|e| format!("EnumProfiles hr=0x{:08X}", e.code().0))?;
            let mut buf: Vec<TF_INPUTPROCESSORPROFILE> = Vec::with_capacity(16);
            buf.resize_with(16, || core::mem::zeroed());
            let mut fetched: u32 = 0;
            enum_p
                .Next(&mut buf[..], &mut fetched as *mut u32)
                .map_err(|e| format!("Next hr=0x{:08X}", e.code().0))?;
            println!("[enum-profiles] fetched={}", fetched);
            for i in 0..fetched as usize {
                let p = &buf[i];
                println!(
                    "  [{}] type={} lang=0x{:04x} clsid={:?} profile={:?} catid={:?} flags=0x{:x}",
                    i, p.dwProfileType, p.langid, p.clsid, p.guidProfile, p.catid, p.dwFlags
                );
            }
            Ok(())
        })();
        if com_initialized_here {
            CoUninitialize();
        }
        result
    }
}

/// `--register-profile` — 用**新版** `ITfInputProcessorProfileMgr::RegisterProfile`
/// 把 Prisir 登记成可激活的 input-processor profile。
///
/// T25「不可选」根因:`do_register` 里旧版 `AddLanguageProfile` 三步全返 S_OK,但
/// `--enum-profiles` 显示 TIPC 对 zh-CN 只枚举到 type=2(MS Pinyin 的 HKL 布局),
/// 根本没有 type=1 的 Prisir → 旧 API 是假 PASS。Win10 创建 input-processor profile
/// 真正走 `ITfInputProcessorProfileMgr::RegisterProfile`(同 COM 类 QI 出的新接口)。
pub fn do_register_profile(dll_path: &str) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let profile_cls = parse_guid(CLSID_PRISIR_IME_STR)?;
    let profile_id = parse_guid(PROFILE_GUID)?;

    let dll_w: Vec<u16> = OsStr::new(dll_path)
        .encode_wide()
        .chain(std::iter::once(0u16))
        .collect();
    let desc_w: Vec<u16> = OsStr::new(DISPLAY_NAME)
        .encode_wide()
        .chain(std::iter::once(0u16))
        .collect();

    unsafe {
        let com_init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let com_initialized_here = com_init.is_ok();
        if !com_init.is_ok() && com_init.0 != 0x80010106u32 as i32 {
            return Err(format!("CoInitializeEx 失败: hr=0x{:08X}", com_init.0));
        }
        let result = (|| -> Result<(), String> {
            let mgr: ITfInputProcessorProfileMgr = CoCreateInstance(
                &CLSID_TF_InputProcessorProfiles,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(|e| format!("CoCreateInstance(ProfileMgr) 失败: hr=0x{:08X}", e.code().0))?;

            // RegisterProfile(dwProfileType, langid, clsid, profile, desc, iconFile, iconIndex,
            //                 hklSubstitute, dwPreferredLayout, bEnabledByDefault, dwFlags)
            //
            // T25「不可选」修复曾假设 hklSubstitute 必须传 0x08040804(微软拼音布局)。
            // 2026-08-31 实测(任务 #88)两种取值都不对:
            //   - NULL:         Prisir 能进切换菜单并被选中,但选中后系统实际激活搜狗
            //                   (notepad 加载 SogouTSF.ime 而非 prisir),打字出原字母,DLL 不加载。
            //   - 0x08040804:   Prisir 直接从切换菜单消失(该布局与系统已装微软拼音冲突,
            //                   系统不显示它),比之前更糟。
            // 结论: hklSubstitute 不是「选中但激活搜狗」的病根。恢复 NULL(至少能选中),
            // 真正的激活回退根因需另查(疑 ctfmon 切换时未为 Prisir 建立激活上下文)。
            let hkl_subst = windows::Win32::UI::Input::KeyboardAndMouse::HKL(0 as *mut core::ffi::c_void);
            mgr.RegisterProfile(
                &profile_cls,
                u16::from_str_radix(LANGID_ZH_CN, 16).unwrap_or(0x0804),
                &profile_id,
                &desc_w,
                &dll_w,
                0,                                             // iconIndex
                hkl_subst,                                     // hklSubstitute = NULL (对齐 weasel)
                0,                                             // dwPreferredLayout
                true,                                          // bEnabledByDefault
                0,                                             // dwFlags
            )
            .map_err(|e| format!("RegisterProfile hr=0x{:08X}", e.code().0))?;

            Ok(())
        })();
        if com_initialized_here {
            CoUninitialize();
        }
        result
    }
}

fn set_enable_dword(v: u32) -> Result<(), String> {
    let hkcu = open_hkcu(KEY_WRITE.0)
        .map_err(|e| format!("RegOpenCurrentUser(KEY_WRITE) 失败: {:?}", WIN32_ERROR(e.0)))?;

    // T9 hotfix #2: Enable DWORD 不再写在父 key, 而是在 LanguageProfile 子 key:
    //   {TIP_CLSID}\LanguageProfile\0x00000804\{PROFILE_GUID}\Enable
    // 父 key 的 Enable 字段是 TSF 旧版兼容字段, 现代 Windows 10/11 不读它。
    let sub_key = format!(
        r"SOFTWARE\Microsoft\CTF\TIP\{CLSID_PRISIR_IME_STR}\LanguageProfile\0x0000{LANGID_ZH_CN}\{PROFILE_GUID}"
    );
    let sub_key_w = to_wide(&sub_key);

    let result: Result<(), String> = unsafe {
        let mut opened: HKEY = HKEY::default();
        let status = RegOpenKeyExW(
            hkcu,
            PCWSTR(sub_key_w.as_ptr()),
            0,
            KEY_WRITE,
            &mut opened,
        );
        if status != ERROR_SUCCESS {
            let _ = RegCloseKey(hkcu);
            return Err(format!(
                "RegOpenKeyExW({sub_key}) 失败: {:?}",
                WIN32_ERROR(status.0)
            ));
        }

        // value name = "Enable"
        let value_name_w = to_wide("Enable");
        let bytes = v.to_le_bytes();

        let sstatus = RegSetValueExW(
            opened,
            PCWSTR(value_name_w.as_ptr()),
            0,
            REG_DWORD,
            Some(&bytes),
        );
        let _ = RegCloseKey(opened);
        let _ = RegCloseKey(hkcu);

        if sstatus != ERROR_SUCCESS {
            return Err(format!(
                "RegSetValueExW(Enable={v}) 失败: {:?}",
                WIN32_ERROR(sstatus.0)
            ));
        }
        Ok(())
    };
    result
}

// =====================================================================
// T24 P4: `--register-status` 干跑验证器 — 关键路径不许假 PASS
//
// 只读 HKLM\..\InprocServer32 + 真跑 CoCreateInstance,输出 verdict。
// agent 通道可凭此判定 "TIPC 激活链路前置条件" 是否就绪。
// =====================================================================

/// 验证结果 — 给 `--register-status` JSON 输出用。
#[derive(Debug, Clone)]
pub struct HklmStatusReport {
    /// HKLM\..\InprocServer32 (默认) 的实际值(去掉末尾 NUL)。
    pub inprocserver32_default: Option<String>,
    /// HKLM\..\InprocServer32\ThreadingModel 的实际值。
    pub threading_model: Option<String>,
    /// HKLM\..\CLSID\{...} 父 key 是否存在。
    pub key_exists: bool,
    /// 真跑 CoCreateInstance(CLSCTX_INPROC_SERVER) 返的 hr。
    /// 0x00000000 = S_OK, 0x80040154 = REGDB_E_CLASSNOTREG。
    pub cocreate_hr: u32,
    /// 综合判定:`OK` 或 `BROKEN: <原因>`。
    pub verdict: String,
}

/// 读 HKLM 下 InprocServer32 + 真跑 CoCreateInstance 验证。
///
/// 这里不写注册表,只读 + COM call;失败也不会改动系统状态。
pub fn read_hklm_inprocserver32_status() -> Result<HklmStatusReport, String> {
    use windows::core::IUnknown;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER};

    let sub_key = format!(
        r"SOFTWARE\Classes\CLSID\{}\InprocServer32",
        CLSID_PRISIR_IME_STR
    );

    // 1. 读 HKLM\..\InprocServer32 (默认) + ThreadingModel
    let mut opened: HKEY = HKEY::default();
    let sub_key_w = to_wide(&sub_key);
    let open_status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(sub_key_w.as_ptr()),
            0,
            KEY_READ,
            &mut opened,
        )
    };
    let key_exists = open_status == ERROR_SUCCESS;

    let (default_val, threading_model): (Option<String>, Option<String>) = if key_exists {
        // (a) 读 (默认) — value name 是空串
        let default_val = unsafe { read_reg_sz_value(opened, PCWSTR(std::ptr::null())) };
        // (b) 读 ThreadingModel
        let tm_w = to_wide("ThreadingModel");
        let threading_model = unsafe { read_reg_sz_value(opened, PCWSTR(tm_w.as_ptr())) };
        let _ = unsafe { RegCloseKey(opened) };
        (default_val, threading_model)
    } else {
        (None, None)
    };

    // 2. 真跑 CoCreateInstance(CLSCTX_INPROC_SERVER) — 验证 COM 实际能否加载
    //
    // windows-rs 0.58 的 CoCreateInstance 是 3 参数版 (rclsid, outer, clsctx) + Param<T> 推断
    // 这里让 T = IUnknown (windows-core 定义的根接口),即拿 COM object 即可,不 QI 任何特定接口。
    let cocreate_hr: u32 = unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let init_here = init.is_ok();
        if !init.is_ok() && init.0 != 0x80010106u32 as i32 {
            0x80010106u32 // RPC_E_CHANGED_MODE 视为未初始化错误
        } else {
            let clsid_res = parse_guid(CLSID_PRISIR_IME_STR);
            let hr: u32 = match clsid_res {
                Ok(clsid) => {
                    let res: windows_core::Result<IUnknown> =
                        CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER);
                    match res {
                        Ok(_obj) => 0u32, // S_OK
                        Err(e) => e.code().0 as u32,
                    }
                }
                Err(_) => 0x80070057u32, // E_INVALIDARG
            };
            if init_here {
                CoUninitialize();
            }
            hr
        }
    };

    // 3. 综合 verdict
    let verdict = match (default_val.as_ref(), key_exists, cocreate_hr) {
        (Some(s), _, 0) if !s.is_empty() => "OK".to_string(),
        (Some(s), _, _) if s.is_empty() => "BROKEN: InprocServer32 default value is empty".to_string(),
        (None, false, _) => "BROKEN: HKLM CLSID InprocServer32 key missing".to_string(),
        (None, true, _) => "BROKEN: HKLM CLSID InprocServer32 default value unreadable".to_string(),
        (_, _, hr) => format!("BROKEN: CoCreateInstance(CLSCTX_INPROC_SERVER) hr=0x{:08X}", hr),
    };

    Ok(HklmStatusReport {
        inprocserver32_default: default_val,
        threading_model,
        key_exists,
        cocreate_hr,
        verdict,
    })
}

/// 读 REG_SZ value — 返回去掉末尾 NUL 的 String。
/// value_name_pcwstr: PCWSTR(null()) = "(默认)"。
unsafe fn read_reg_sz_value(key: HKEY, value_name_pcwstr: PCWSTR) -> Option<String> {
    // 先查类型 + 大小
    let mut kind: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    let mut byte_len: u32 = 0;
    let qstatus = RegQueryValueExW(
        key,
        value_name_pcwstr,
        None,
        Some(&mut kind),
        None,
        Some(&mut byte_len),
    );
    if qstatus != ERROR_SUCCESS || byte_len == 0 {
        return None;
    }
    let mut buf: Vec<u8> = vec![0u8; byte_len as usize];
    let qstatus2 = RegQueryValueExW(
        key,
        value_name_pcwstr,
        None,
        Some(&mut kind),
        Some(buf.as_mut_ptr()),
        Some(&mut byte_len),
    );
    if qstatus2 != ERROR_SUCCESS {
        return None;
    }
    // REG_SZ 是 UTF-16LE,以 NUL(0x0000)结尾。byte_len 含末尾 NUL 时去掉。
    let actual_len = if byte_len >= 2 { byte_len as usize - 2 } else { 0 };
    if actual_len == 0 {
        return Some(String::new());
    }
    // 重新对齐到 u16 边界
    let u16_len = actual_len / 2;
    let mut wide: Vec<u16> = vec![0u16; u16_len];
    let src = std::slice::from_raw_parts(buf.as_ptr() as *const u16, u16_len);
    wide.copy_from_slice(src);
    Some(String::from_utf16_lossy(&wide))
}
