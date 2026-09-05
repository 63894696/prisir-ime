//! about.rs — 灵犀拼音输入法 Win 端 关于页内容
//!
//! 与 Android 端 UserDictActivity.openLink 的 4 段正文保持一致(2026-08-29 决策对齐),
//! 给 `prisir_tsfsvc --about <about|privacy|terms|contact>` 子命令用, 也给
//! `prisir_tsfsvc --version` 输出末尾的"更多"行用。
//!
//! 设计目标: Win 端无 GUI, 用户靠命令行/记事本阅读; 4 段都打成单行 println!,
//!         对中文终端友好(GBK / UTF-8 都行, 避开 emoji / 框线字符)。

/// 联系人邮件 — 与 Android 端 `mailto:lsjdlijie@outlook.com` 一致。
pub const CONTACT_EMAIL: &str = "lsjdlijie@outlook.com";

/// 关于页 4 段正文 — 跟 Android 端 UserDictActivity.java:425-436 一字不差。
pub const ABOUT_BODY: &str = "灵犀拼音输入法 for Windows 是 Prisir(湃睿思) AI 出品的用户隐私保护软件,纯本地运行,不强制联网,没有账号体系,支持用户百分百管理自己词库,根据使用词频自动跳第一页显示,方便快捷录入。";

pub const PRIVACY_BODY: &str = "灵犀拼音输入法 for Windows 是 Prisir(湃睿思) AI 出品的用户隐私保护软件,纯本地运行,词库存本机不外发,支持用户百分百管理自己词库,不做行为分析,不设账号。唯一的联网行为:每日一次匿名更新检查(向我们的网站 GET 一个 updates.json,不含任何身份/内容/使用数据,仅用于统计大致活跃数,与 PrisirAI 同口径)。";

pub const TERMS_BODY: &str = "灵犀拼音输入法 for Windows 是 Prisir(湃睿思) AI 出品的用户隐私保护软件,本软件的轻量条款核心是「本地工具,自负其责」,使用本输入法即视为同意:词库存本机,不外发。";

pub const CONTACT_BODY: &str = "反馈邮件: lsjdlijie@outlook.com (主题请加 [灵犀输入法] 前缀, 我们会在 1-3 个工作日内回信)。反馈请附带: Win 版本号 (--version 输出)、触发场景的复现步骤、必要的截图/记事本。";

/// 4 个段落的标题列表 — 给 `--about` 命令枚举用。
pub const ABOUT_TITLES: &[&str] = &["关于", "隐私说明", "使用条款", "反馈联系"];

/// 给 `--about <key>` 查表: 返回 (title, body)。
pub fn lookup(key: &str) -> Option<(&'static str, &'static str)> {
    match key {
        "about"    => Some(("关于",     ABOUT_BODY)),
        "privacy"  => Some(("隐私说明", PRIVACY_BODY)),
        "terms"    => Some(("使用条款", TERMS_BODY)),
        "contact"  => Some(("反馈联系", CONTACT_BODY)),
        _ => None,
    }
}

/// 给 `--version` 命令末尾打印"更多"链接用。
pub fn about_links_inline() -> String {
    format!(
        "更多: 关于/隐私/使用条款/反馈联系 — 跑 `prisir_tsfsvc --about <about|privacy|terms|contact>` 查看正文; 反馈邮件 {}",
        CONTACT_EMAIL
    )
}