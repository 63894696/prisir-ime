//! 走真实 COM 激活路径 CoCreateInstance(类工厂 → CreateInstance),
//! 而不是直接 TsfInputProcessor::new()。复现 VM coc2 的环境。
//! 用库内真实 IID,排除 IID 字节序干扰。

use windows::core::{IUnknown, Interface, GUID};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::TextServices::*;

const CLSID_PRISIR: GUID = GUID::from_u128(0xA1B2C3D4_E5F6_7890_ABCD_EF1234567890);

fn run() {
    unsafe { let _ = CoInitializeEx(None, COINIT_MULTITHREADED); };

    // 1) IUnknown
    let r: windows::core::Result<IUnknown> = unsafe { CoCreateInstance(&CLSID_PRISIR, None, CLSCTX_INPROC_SERVER) };
    match &r {
        Ok(_) => println!("CoCreate(IUnknown) S_OK"),
        Err(e) => println!("CoCreate(IUnknown) hr=0x{:08X}", e.code().0 as u32),
    }

    // 2) ITfTextInputProcessor
    let r2: windows::core::Result<ITfTextInputProcessor> = unsafe { CoCreateInstance(&CLSID_PRISIR, None, CLSCTX_INPROC_SERVER) };
    match &r2 {
        Ok(_) => println!("CoCreate(ITfTextInputProcessor) S_OK"),
        Err(e) => println!("CoCreate(ITfTextInputProcessor) hr=0x{:08X}", e.code().0 as u32),
    }

    // 3) ITfTextInputProcessorEx
    let r3: windows::core::Result<ITfTextInputProcessorEx> = unsafe { CoCreateInstance(&CLSID_PRISIR, None, CLSCTX_INPROC_SERVER) };
    match &r3 {
        Ok(_) => println!("CoCreate(ITfTextInputProcessorEx) S_OK"),
        Err(e) => println!("CoCreate(ITfTextInputProcessorEx) hr=0x{:08X}", e.code().0 as u32),
    }
}

#[test]
fn coc_class_factory_path() {
    run();
}
