//! 任务栏输入法指示器「中/英」状态 — 走 TSF conversion-mode compartment。
//!
//! **结论(2026-09-01 实测修正)**: 设了 compartment 也**不会**让 Win10 19045 的
//! 沉浸式任务栏指示器为第三方 TIP 显示「中/英」——这是系统限制,不是值没配对
//! (三方注册表树 diff 已证无 both-have-we-lack 的「中/英」值,见 memory
//! prisirtip-ime-tree-diff)。写入本 compartment 只保证**框架内模式语义正确**
//! (应用/系统查 conversion mode 时读到对的值),中/英**可视化**改走自绘悬浮条
//! status_bar.rs(对齐灵犀 _create_bar,蓝本见该文件头)。
//!
//! **数据源**: thread 级 compartment `GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION`
//!   - `TF_CONVERSIONMODE_NATIVE | TF_CONVERSIONMODE_FULLSHAPE` = 中文
//!   - `TF_CONVERSIONMODE_ALPHANUMERIC`                          = 英文
//! 配合 `GUID_COMPARTMENT_KEYBOARD_OPENCLOSE`(1=中文激活, 0=英文直通)。
//!
//! 我们的 is_chinese_mode 翻转时(CapsLock/Shift/LangBar OnClick/状态条点击)调
//! set_mode() 把两个 compartment 同步到当前 thread,保持框架语义一致。

use windows::core::*;
use windows::Win32::UI::TextServices::*;

/// 把「中/英」模式写进当前 thread 的 conversion compartment。
///
/// - `ptim`: ITfThreadMgr(Activate 时保存的),QI 出 ITfCompartmentMgr。
/// - `tid`: 本 IME 的 client id。
/// - `is_chinese`: true=中文(NATIVE|FULLSHAPE + OPENCLOSE=1), false=英文(ALPHANUMERIC + OPENCLOSE=0)。
///
/// 失败只打日志,不返 Err — 指示器不显示不致命,打字主链路不受影响。
pub(crate) fn set_mode(ptim: &ITfThreadMgr, tid: u32, is_chinese: bool) {
    if let Err(e) = set_mode_inner(ptim, tid, is_chinese) {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!(
            "conversion set_mode FAIL chinese={} err={:?}", is_chinese, e
        ));
    }
}

fn set_mode_inner(ptim: &ITfThreadMgr, tid: u32, is_chinese: bool) -> Result<()> {
    let cmgr: ITfCompartmentMgr = ptim.cast()?;

    // 1. INPUTMODE_CONVERSION: NATIVE(中) vs ALPHANUMERIC(英)
    let conv: ITfCompartment =
        unsafe { cmgr.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION) }?;
    let conv_bits: i32 = if is_chinese {
        (TF_CONVERSIONMODE_NATIVE | TF_CONVERSIONMODE_FULLSHAPE) as i32
    } else {
        TF_CONVERSIONMODE_ALPHANUMERIC as i32
    };
    let v = variant_i4(conv_bits);
    unsafe { conv.SetValue(tid, &v) }?;

    // 2. OPENCLOSE: 1=中文输入激活, 0=英文直通
    let oc: ITfCompartment =
        unsafe { cmgr.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_OPENCLOSE) }?;
    let oc_bits: i32 = if is_chinese { 1 } else { 0 };
    let v2 = variant_i4(oc_bits);
    unsafe { oc.SetValue(tid, &v2) }?;

    #[cfg(feature = "dllentry_log")]
    crate::com_class_factory::log_dll_entry(&format!(
        "conversion set_mode OK chinese={} conv=0x{:X} oc={}", is_chinese, conv_bits, oc_bits
    ));
    Ok(())
}

/// 构造一个 VT_I4 的 VARIANT。windows-core 0.58 提供 From<i32>。
fn variant_i4(val: i32) -> VARIANT {
    VARIANT::from(val)
}
