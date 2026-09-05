//! 输入法激活键 + 输入法切换 — 沿用 voice_input/lingxi_ime/backend 范式
//!
//! 设计目的:
//!   - 拼音 / 五笔 共享同一套 TSF 进程 + 同一套 ciku.db
//!   - 区分仅在「激活键」与「码表查询方式」(拼音=全拼/简拼,五笔=根码)
//!   - T7 阶段只把输入法切换能力落进 keystroke + main --method,
//!     真按激活键自动切换是 T8 沙盒 E2E 才接(本模块只暴露常量与解析,不接 TSF 事件流)
//!
//! 数值约定(严格沿用 voice_input/lingxi_ime/backend/hotkey_field.py + ime_config.py):
//!   - Shift:  VK_LSHIFT=0xA0  VK_RSHIFT=0xA1  → 通用 0x10
//!   - Ctrl:   VK_LCTRL =0xA2  VK_RCTRL =0xA3  → 通用 0x11
//!   - Alt:    VK_LALT  =0xA4  VK_RALT  =0xA5  → 通用 0x12
//!   - Win:    VK_LWIN  =0x5B  VK_RWIN  =0x5C  → 通用 0x5B (T7 暂未启用,留给 Win+某键 路径)
//!   - PrintScreen=0x2C Insert=0x2D Delete=0x2E Home=0x2F NumLock=0x90
//!
//! 默认激活键:
//!   - 拼音 = 右 Ctrl(0xA3)  —— 与 ime_config.PINYIN_TRIGGER_KEY 一致
//!   - 五笔 = 右 Shift(0xA1) —— 与 ime_config.WUBI_TRIGGER_KEY 一致
#![allow(dead_code)]

use std::str::FromStr;

/// 允许作为输入法激活键的 VK 集合(沿用 voice_input ALLOWED_VKS,扩展 PrintScreen/Insert/
/// Delete/Home/NumLock —— 这些是 lingxi_hotkeys 暂未列但本进程内便于调试加的)。
pub const ALLOWED_VKS: &[u16] = &[
    0x10, 0x11, 0x12, 0x14,              // Shift/Ctrl/Alt/CapsLock 通用(归并后)
    0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5,  // 左右 Shift/Ctrl/Alt/Win
    0x2C, 0x2D, 0x2E, 0x2F,              // PrintScreen/Insert/Delete/Home
    0x90,                                  // NumLock
];

/// 拼音激活键 = 右 Ctrl(沿用 ime_config.PINYIN_TRIGGER_KEY = 0xA3)
pub const PINYIN_TRIGGER_KEY: u16 = 0xA3;

/// 五笔激活键 = 右 Shift(沿用 ime_config.WUBI_TRIGGER_KEY = 0xA1)
pub const WUBI_TRIGGER_KEY: u16 = 0xA1;

/// 当前输入法枚举。T7 阶段仅两态,语音激活在 T7 后做增量(不在 T7 范围)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMethod {
    Pinyin,
    Wubi,
}

impl InputMethod {
    /// 序列化用字符串(给 --status / ipc 输出)。固定小写英文,与 parse_method 入参对齐。
    pub fn as_str(self) -> &'static str {
        match self {
            InputMethod::Pinyin => "pinyin",
            InputMethod::Wubi   => "wubi",
        }
    }
}

/// 输入法名串 → 枚举。接受拼音/中文/简写(大小写不敏感)。
/// 失败时返 `Err(msg)`,给 main.rs `--method` 用作 stderr + 退出码 1。
pub fn parse_method(s: &str) -> Result<InputMethod, String> {
    let m = InputMethod::from_str(s)?;
    Ok(m)
}

impl FromStr for InputMethod {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pinyin" | "拼音" | "py" => Ok(InputMethod::Pinyin),
            "wubi" | "五笔" | "wb"   => Ok(InputMethod::Wubi),
            other => Err(format!("unknown method: {other}")),
        }
    }
}

/// VK 归一化(还原被归并的左右修饰键),再判断是否在 ALLOWED_VKS 内。
/// - 0xA0/0xA1 → 0x10 (Shift,通用)
/// - 0xA2/0xA3 → 0x11 (Ctrl,通用)
/// - 0xA4/0xA5 → 0x12 (Alt,通用)
/// - 0x5B/0x5C → 0x5B (Win,通用)
/// - 其他:原样返回
/// 在 ALLOWED_VKS 内才返 `Some(norm)`,否则 `None`。
pub fn normalize_vk(raw: u16) -> Option<u16> {
    let normalized = match raw {
        0xA0 | 0xA1 => 0x10,
        0xA2 | 0xA3 => 0x11,
        0xA4 | 0xA5 => 0x12,
        0x5B | 0x5C => 0x5B,
        other => other,
    };
    if ALLOWED_VKS.contains(&normalized) {
        Some(normalized)
    } else {
        None
    }
}

// ============================================================
// 单元测试 — 不依赖 ffi / 不依赖 TSF,只测纯函数
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_vk_merges_modifier_pairs() {
        assert_eq!(normalize_vk(0xA0), Some(0x10), "VK_LSHIFT -> 通用 Shift");
        assert_eq!(normalize_vk(0xA1), Some(0x10), "VK_RSHIFT -> 通用 Shift");
        assert_eq!(normalize_vk(0xA2), Some(0x11), "VK_LCTRL -> 通用 Ctrl");
        assert_eq!(normalize_vk(0xA3), Some(0x11), "VK_RCTRL -> 通用 Ctrl");
        assert_eq!(normalize_vk(0xA4), Some(0x12), "VK_LALT -> 通用 Alt");
        assert_eq!(normalize_vk(0xA5), Some(0x12), "VK_RALT -> 通用 Alt");
    }

    #[test]
    fn normalize_vk_rejects_non_allowed() {
        assert_eq!(normalize_vk(0x41), None, "VK_A 不允许");
        assert_eq!(normalize_vk(0x20), None, "VK_SPACE 不允许");
        assert_eq!(normalize_vk(0x1B), None, "VK_ESC 不允许");
        assert_eq!(normalize_vk(0x00), None, "VK_NUL 不允许");
    }

    #[test]
    fn trigger_keys_match_voice_input() {
        assert_eq!(PINYIN_TRIGGER_KEY, 0xA3, "拼音激活键必须=右Ctrl(沿用 voice_input)");
        assert_eq!(WUBI_TRIGGER_KEY,   0xA1, "五笔激活键必须=右Shift(沿用 voice_input)");
    }

    #[test]
    fn parse_method_roundtrip() {
        assert_eq!(parse_method("pinyin").unwrap(), InputMethod::Pinyin);
        assert_eq!(parse_method("Pinyin").unwrap(), InputMethod::Pinyin);
        assert_eq!(parse_method("拼音").unwrap(),    InputMethod::Pinyin);
        assert_eq!(parse_method("py").unwrap(),      InputMethod::Pinyin);
        assert_eq!(parse_method("wubi").unwrap(),    InputMethod::Wubi);
        assert_eq!(parse_method("WUBI").unwrap(),    InputMethod::Wubi);
        assert_eq!(parse_method("五笔").unwrap(),    InputMethod::Wubi);
        assert_eq!(parse_method("wb").unwrap(),      InputMethod::Wubi);
        assert!(parse_method("foo").is_err(),       "未知 method 必须 Err");
        assert!(parse_method("").is_err(),          "空串必须 Err");
    }

    #[test]
    fn method_as_str_canonical() {
        assert_eq!(InputMethod::Pinyin.as_str(), "pinyin");
        assert_eq!(InputMethod::Wubi.as_str(),   "wubi");
    }
}