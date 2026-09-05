use prisir_ime::engine::ImeEngine;
use std::time::Instant;
const DB: &str = r"C:\Users\Administrator\voice_input\lingxi_ime\backend\ciku.db";
#[test]
fn dbg_load() {
    // 模拟首次切换: ImeEngine::new + load_or_build_index(走 cache) 全程计时
    let t = Instant::now();
    let mut e = ImeEngine::new(DB).unwrap();
    println!("[load] ImeEngine::new (open db) = {:.3}ms", t.elapsed().as_secs_f64()*1000.0);
    let t = Instant::now();
    let (ok, src) = e.load_or_build_index(DB);
    println!("[load] load_or_build_index src={} ok={} = {:.3}ms", src, ok, t.elapsed().as_secs_f64()*1000.0);
}
