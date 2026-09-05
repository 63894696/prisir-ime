//! JNI 导出(Android),叠加于现有 C ABI(ffi.rs)之上,不动其形态。
//! Java 侧: com.lingxi.ime.Engine 声明 native 方法,System.loadLibrary("prisir_ime")。
//! 内部直接复用 engine::ImeEngine 的 query/smart_sentence/learn,不重复逻辑。

use crate::engine::ImeEngine;
use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jlong, jstring};
use jni::JNIEnv;

/// 句柄: jlong 即 *mut ImeEngine。
fn engine_of(handle: jlong) -> Option<&'static ImeEngine> {
    if handle == 0 {
        return None;
    }
    Some(unsafe { &*(handle as *const ImeEngine) })
}

fn jstr(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s)
        .map(|v| v.into())
        .unwrap_or_else(|_| String::new())
}

fn out(env: &mut JNIEnv, s: String) -> jstring {
    env.new_string(s)
        .map(|v| v.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// 加载词库,返回引擎句柄(0=失败)。buildIndex!=0 时加载/构建内存 Trie 索引。
#[no_mangle]
pub extern "system" fn Java_com_lingxi_ime_Engine_nativeLoad(
    mut env: JNIEnv,
    _cls: JClass,
    db_path: JString,
    build_index: jboolean,
) -> jlong {
    let path = jstr(&mut env, &db_path);
    if path.is_empty() {
        return 0;
    }
    let mut engine = match ImeEngine::new(&path) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    if build_index != 0 {
        let (_ok, src) = engine.load_or_build_index(&path);
        eprintln!("[prisir_ime] index source: {src}");
    }
    Box::into_raw(Box::new(engine)) as jlong
}

/// 候选查询,返回 JSON 数组 [{"word":"...","weight":N},...]。
#[no_mangle]
pub extern "system" fn Java_com_lingxi_ime_Engine_nativeQuery(
    mut env: JNIEnv,
    _cls: JClass,
    handle: jlong,
    input: JString,
) -> jstring {
    let Some(engine) = engine_of(handle) else {
        return out(&mut env, "[]".to_string());
    };
    let inp = jstr(&mut env, &input);
    let cands = engine.query(&inp);
    let arr: Vec<serde_json::Value> = cands
        .iter()
        .map(|(w, wt)| serde_json::json!({"word": w, "weight": wt}))
        .collect();
    out(&mut env, serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string()))
}

/// 整句智能首选(DP/Viterbi),无路径返回空串。
#[no_mangle]
pub extern "system" fn Java_com_lingxi_ime_Engine_nativeSmartSentence(
    mut env: JNIEnv,
    _cls: JClass,
    handle: jlong,
    input: JString,
) -> jstring {
    let Some(engine) = engine_of(handle) else {
        return out(&mut env, String::new());
    };
    let inp = jstr(&mut env, &input);
    out(&mut env, engine.smart_sentence(&inp).unwrap_or_default())
}

/// 学习用户选择(更新词频)。
#[no_mangle]
pub extern "system" fn Java_com_lingxi_ime_Engine_nativeLearn(
    mut env: JNIEnv,
    _cls: JClass,
    handle: jlong,
    input: JString,
    selected: JString,
) {
    if let Some(engine) = engine_of(handle) {
        let inp = jstr(&mut env, &input);
        let sel = jstr(&mut env, &selected);
        engine.learn(&inp, &sel);
    }
}

/// 释放引擎句柄。
#[no_mangle]
pub extern "system" fn Java_com_lingxi_ime_Engine_nativeFree(
    _env: JNIEnv,
    _cls: JClass,
    handle: jlong,
) {
    if handle != 0 {
        unsafe { drop(Box::from_raw(handle as *mut ImeEngine)) };
    }
}
