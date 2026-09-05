// T3/#88 LangBar + 词库管理窗口: 把灵犀拼音专属图标(lingxi.ico)编进 DLL 的资源段。
//
// 为什么需要(2026-09-01 诊断):
//   ITfLangBarItemMgr::AddItem 据我们 item 的 GetInfo.clsidService 反查 TIP profile,
//   读 profile 的 IconFile(=C:\PrisirIME\prisir_ime_tsf.dll) + IconIndex(=0) 调
//   ExtractIcon 提取图标。我们 DLL 之前没编任何图标资源 → ExtractIcon 拿不到 →
//   AddItem 直接 E_FAIL 0x80004005(日志证实 mgr 调了 GetInfo/GetStatus 就失败,
//   永远走不到 GetIcon)。
//   嵌入图标后,IconIndex 0x0 对应资源里第一个图标,mgr 提取成功。
//   词库管理窗口(userdict_window.rs)也按同一资源 ID 1 用 LoadImageW 取标题栏图标。
//
// 资源 ID 用 1 (IDI_ICON1),winres 默认 set_icon 会生成 IDI_ICON 1。
// register.rs 里 IconIndex 写 0x0 → 提取资源段第一个图标,正好命中。
fn main() {
    // 只在 Windows 目标嵌资源(cross 编译到非 Windows 时 winres 无用)。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        // 灵犀拼音专属图标在仓库根(lingxi.ico)。winres 把第一个 set_icon 编成 IDI_ICON=1 主图标。
        res.set_icon("lingxi.ico");

        res.set("FileDescription", "Prisir 灵犀拼音 (TSF)");
        res.set("ProductName", "Prisir 灵犀拼音");
        res.set_language(0x0804); // 简体中文

        if let Err(e) = res.compile() {
            // 编译失败不 panic — 打印 warning,让 DLL 仍能 build(只是没图标)。
            // 真机部署时若缺图标,AddItem 仍会 E_FAIL,日志能看出来。
            println!("cargo:warning=winres compile failed (icon not embedded): {e}");
        }

        // 只产 TSF COM 产物(2026-09-04 IMM32 分支已停用,见 register_imm.rs/imm32.rs)。
        // MSVC 链接器吃 /DEF:<path>。用 manifest 目录绝对路径,避免相对路径在增量/不同 cwd 下失效。
        let def = format!(
            "cargo:rustc-cdylib-link-arg=/DEF:{}\\prisir_ime_tsf.def",
            std::env::var("CARGO_MANIFEST_DIR").unwrap(),
        );
        println!("{def}");
    }
    // ico 变化时触发重编。
    println!("cargo:rerun-if-changed=lingxi.ico");
    println!("cargo:rerun-if-changed=prisir_ime_tsf.def");
}
