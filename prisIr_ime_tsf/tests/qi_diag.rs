//! 用 windows-rs 库内真实 IID 测全部 7 接口,排除手写 IID 错误。

use prisir_ime_tsf::tsf_input_processor::TsfInputProcessor;
use windows::core::{IUnknown, Interface};
use windows::Win32::UI::TextServices::*;

fn run() {
    let obj: IUnknown = TsfInputProcessor::new().into();

    macro_rules! t {
        ($ty:ty, $name:literal) => {{
            let iid = <$ty as Interface>::IID;
            let mut pv: *mut core::ffi::c_void = core::ptr::null_mut();
            let hr = unsafe { obj.query(&iid, &mut pv) };
            println!("{:<28} hr=0x{:08X} {}", $name, hr.0 as u32, if hr.is_ok() { "S_OK" } else { "FAIL" });
        }};
    }

    println!("=== library IID QueryInterface (all 7) ===");
    t!(ITfTextInputProcessor, "ITfTextInputProcessor");
    t!(ITfTextInputProcessorEx, "ITfTextInputProcessorEx");
    t!(ITfSource, "ITfSource");
    t!(ITfKeyEventSink, "ITfKeyEventSink");
    t!(ITfThreadMgrEventSink, "ITfThreadMgrEventSink");
    t!(ITfLangBarItemSink, "ITfLangBarItemSink");
    t!(ITfCompositionSink, "ITfCompositionSink");
}

#[test]
fn qi_all_library_iids() {
    run();
}
