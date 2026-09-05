//! plugin.rs — 进程外插件框架(2026-09-05)
//!
//! **目标**:AI/皮肤/桌面宠物等扩展功能做成**进程外插件**(独立 exe),
//! 用户从网站下载解压到 `%LOCALAPPDATA%\Prisir\plugins\` 即用,
//! 不重装整个输入法;菜单/悬浮栏按钮按已安装插件**动态生成**。
//!
//! **为什么进程外(不 LoadLibrary dll 插件)**:
//!   - 崩溃隔离:插件崩只崩自己,不带垮 ctfmon/explorer/notepad(T25 已吃够进程内崩)。
//!   - 安全:dll 会注入每个宿主进程,杀软易报毒;独立 exe 行为干净。
//!   - 语言自由:AI 用 Python、皮肤用 WebView、宠物用任意框架,不需 Rust/ABI 兼容。
//!   - 动态增删:删插件目录即可,下次菜单触发检测「未安装」自动隐藏。
//!
//! **运行模型(对齐语音 trigger_voice,泛化而来)**:
//!   1. 输入法启动/菜单弹出前调 `available_plugins()` → 读 plugins.json,
//!      对每个 enabled 插件查 exe 是否存在,存在才进可用列表。
//!   2. 点按钮/菜单 → `trigger_plugin(id)`:
//!        - OpenEvent(event) 成功 → SetEvent(通知已运行插件 toggle,对齐语音激活模型);
//!        - 失败(ERROR_FILE_NOT_FOUND= 插件没在跑) → ShellExecute(exe) 拉起。
//!   3. AI 按钮 toggle(用户定):点 AI 按钮发 PrisirLingXi_AiToggle_Event,
//!      AI 窗激活后打字转发进 AI 窗;再点关闭。状态机在 AI 插件进程,输入法零侵入击键热路径。
//!
//! **plugins.json 位置**:`%LOCALAPPDATA%\Prisir\plugins.json`(用户可写,免管理员)。
//! 插件 exe 相对路径基于 `%LOCALAPPDATA%\Prisir\`。

use serde::Deserialize;
use std::path::PathBuf;
use windows::core::PCWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

fn logp(msg: &str) {
    crate::com_class_factory::log_dll_entry(&format!("[plugin] {}", msg));
}

/// 单个插件声明(plugins.json 数组元素)。
#[derive(Debug, Deserialize, Clone)]
pub struct PluginSpec {
    /// 稳定 id,如 "voice" / "ai" / "pet"。用于 trigger_plugin 定位。
    pub id: String,
    /// 显示名(菜单/提示),如 "语音听写" / "AI 助手"。
    pub name: String,
    /// 插件 exe 相对路径(基于 %LOCALAPPDATA%\Prisir\),如 "plugins\\voice\\lingxi_voice.exe"。
    pub exe: String,
    /// toggle 命名事件名。插件进程内应 CreateEvent 同名并监听。
    pub event: String,
    /// 悬浮栏按钮文字(1~2 字),如 "语" / "AI"。None= 不进悬浮栏只进菜单。
    #[serde(default)]
    pub button: Option<String>,
    /// 是否启用。false= 即使 exe 存在也不显示。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct PluginsFile {
    #[serde(default)]
    plugins: Vec<PluginSpec>,
}

/// 插件根目录:`%LOCALAPPDATA%\Prisir`。无 LOCALAPPDATA 时退 C:\Temp。
pub fn plugin_root() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("Prisir"))
        .unwrap_or_else(|_| PathBuf::from(r"C:\Temp\Prisir"))
}

fn plugins_json_path() -> PathBuf {
    plugin_root().join("plugins.json")
}

fn file_exists(path: &PathBuf) -> bool {
    // windows 0.58 的 GetFileAttributesW 返回 (),不能判 u32::MAX;用 std Path::exists 更直观。
    path.exists()
}

/// 读 plugins.json 并过滤出「已安装(enabled + exe 存在)」的可用插件。
/// 任何一步失败(json 缺失/解析错/exe 不在)都返回缩小后的列表,不报错 ——
/// 插件机制对核心输入是**纯增量**,绝不能让插件问题影响打字。
pub fn available_plugins() -> Vec<PluginSpec> {
    let json_path = plugins_json_path();
    let raw = match std::fs::read_to_string(&json_path) {
        Ok(s) => s,
        Err(_) => {
            // 无 plugins.json = 没装任何插件,正常态,不刷屏日志。
            return Vec::new();
        }
    };
    let parsed: PluginsFile = match serde_json::from_str(&raw) {
        Ok(p) => p,
        Err(e) => {
            // 容错:用户手写 Windows 路径常把 \ 当目录分隔符(非法 JSON 转义),
            // 自动把 \ 替换为 \\ 再试一次。仍失败则日志留因,返回空(绝不影响打字)。
            let fixed = raw.replace('\\', "\\\\");
            match serde_json::from_str(&fixed) {
                Ok(p) => {
                    logp("plugins.json auto-fixed backslashes (hint: use / or \\\\ in exe path)");
                    p
                }
                Err(e2) => {
                    logp(&format!("plugins.json parse FAIL {:?} -> {}", e2, json_path.display()));
                    return Vec::new();
                }
            }
        }
    };
    let root = plugin_root();
    let mut out = Vec::new();
    for spec in parsed.plugins {
        if !spec.enabled {
            continue;
        }
        let exe_abs = root.join(&spec.exe);
        if file_exists(&exe_abs) {
            out.push(spec);
        } else {
            logp(&format!(
                "plugin '{}' declared but exe missing: {}",
                spec.id,
                exe_abs.display()
            ));
        }
    }
    out
}

/// 按 id 查一个可用插件(供 trigger)。
fn find_plugin(id: &str) -> Option<PluginSpec> {
    available_plugins().into_iter().find(|p| p.id == id)
}

/// 通用插件 toggle:发命名事件;插件未运行则拉起 exe。
/// 与语音 trigger_voice 同构 —— 插件进程监听同名 event 做「按下激活/再按关闭」。
pub fn trigger_plugin(id: &str) {
    let spec = match find_plugin(id) {
        Some(s) => s,
        None => {
            logp(&format!("trigger '{}' but not available", id));
            return;
        }
    };
    unsafe {
        let name: Vec<u16> = spec.event.encode_utf16().chain(std::iter::once(0)).collect();
        match OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr())) {
            Ok(h) if !h.is_invalid() => {
                let _ = SetEvent(h);
                let _ = CloseHandle(h);
                logp(&format!("signaled running plugin '{}'", id));
            }
            _ => {
                // 插件未运行 → 拉起 exe。
                let exe_abs = plugin_root().join(&spec.exe);
                let exe_w: Vec<u16> = exe_abs
                    .as_os_str()
                    .to_string_lossy()
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                let open: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
                let r = ShellExecuteW(
                    None,
                    PCWSTR(open.as_ptr()),
                    PCWSTR(exe_w.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_HIDE,
                );
                logp(&format!("launched plugin '{}' ret={:?}", id, r));
            }
        }
    }
}
