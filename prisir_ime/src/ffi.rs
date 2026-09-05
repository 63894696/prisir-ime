//! C ABI 导出(为 Chromium FFI 集成 / Python ctypes 验证用,对齐 prisir_asr FFI 形态)
//!
//! 用法(C 侧):
//!   void* h = prisir_ime_load(".../ciku.db");
//!   char* json = prisir_ime_query(h, "nihao");            // [{"word":"你好","weight":123},...]
//!   char* sent = prisir_ime_smart_sentence(h, "nihaoshijie");
//!   prisir_ime_free_string(json); prisir_ime_free_string(sent);
//!   prisir_ime_free(h);

use crate::engine::ImeEngine;
use std::ffi::{c_char, c_void, CStr, CString};

fn cstr(ptr: *const c_char) -> Option<&'static str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

fn to_c_string(s: String) -> *mut c_char {
    CString::new(s).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
}

fn get<'a>(handle: *mut c_void) -> Option<&'a ImeEngine> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const ImeEngine) })
}

/// 加载词库,返回 opaque 句柄(失败返回 null)。build_index!=0 时加载内存 Trie 索引:
/// 优先读 `<db>.idx` 持久化缓存(指纹对上则秒开),否则首次构建并落盘,之后启动直接加载。
#[no_mangle]
pub extern "C" fn prisir_ime_load(db_path: *const c_char, build_index: i32) -> *mut c_void {
    let Some(path) = cstr(db_path) else {
        return std::ptr::null_mut();
    };
    let mut engine = match ImeEngine::new(path) {
        Ok(e) => e,
        Err(_) => return std::ptr::null_mut(),
    };
    // 默认开模糊音(2026-09-02,对齐外挂式 ime_config.FUZZY_RULES): 平翘舌 z/zh c/ch s/sh。
    // 仅单音节单字查询时扩展(engine.query 内 fuzzy_expand),不影响多音节整句。
    engine.set_fuzzy_rules(vec!["z_zh", "c_ch", "s_sh"]);
    if build_index != 0 {
        let (_ok, src) = engine.load_or_build_index(path);
        eprintln!("[prisir_ime] index source: {src}");
    }
    Box::into_raw(Box::new(engine)) as *mut c_void
}

/// 候选查询,返回 JSON 数组 [{"word":"...","weight":N},...](调用方须 free_string)
#[no_mangle]
pub extern "C" fn prisir_ime_query(handle: *mut c_void, input: *const c_char) -> *mut c_char {
    let Some(inp) = cstr(input) else {
        return std::ptr::null_mut();
    };
    let Some(engine) = get(handle) else {
        return std::ptr::null_mut();
    };
    let cands = engine.query(inp);
    let arr: Vec<serde_json::Value> = cands
        .iter()
        .map(|(w, wt)| serde_json::json!({"word": w, "weight": wt}))
        .collect();
    to_c_string(serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string()))
}

/// 五笔候选查询(wubi86),返回 JSON 数组 [{"word":"...","weight":N},...](调用方须 free_string)
#[no_mangle]
pub extern "C" fn prisir_ime_query_wubi(handle: *mut c_void, input: *const c_char) -> *mut c_char {
    let Some(inp) = cstr(input) else {
        return std::ptr::null_mut();
    };
    let Some(engine) = get(handle) else {
        return std::ptr::null_mut();
    };
    let cands = engine.query_wubi(inp);
    let arr: Vec<serde_json::Value> = cands
        .iter()
        .map(|(w, wt)| serde_json::json!({"word": w, "weight": wt}))
        .collect();
    to_c_string(serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string()))
}

/// 整句智能首选,返回拼接整句(无路径返回空串;调用方须 free_string)
#[no_mangle]
pub extern "C" fn prisir_ime_smart_sentence(handle: *mut c_void, input: *const c_char) -> *mut c_char {
    let Some(inp) = cstr(input) else {
        return std::ptr::null_mut();
    };
    let Some(engine) = get(handle) else {
        return std::ptr::null_mut();
    };
    to_c_string(engine.smart_sentence(inp).unwrap_or_default())
}

/// 学习用户选择(更新词频)
#[no_mangle]
pub extern "C" fn prisir_ime_learn(handle: *mut c_void, input: *const c_char, selected: *const c_char) {
    let (Some(inp), Some(sel)) = (cstr(input), cstr(selected)) else {
        return;
    };
    if let Some(engine) = get(handle) {
        engine.learn(inp, sel);
    }
}

/// 释放 query/smart_sentence 返回的字符串
#[no_mangle]
pub extern "C" fn prisir_ime_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)) };
    }
}

/// 释放引擎句柄
#[no_mangle]
pub extern "C" fn prisir_ime_free(handle: *mut c_void) {
    if !handle.is_null() {
        unsafe { drop(Box::from_raw(handle as *mut ImeEngine)) };
    }
}
