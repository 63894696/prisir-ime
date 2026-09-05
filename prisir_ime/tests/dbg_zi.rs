use prisir_ime::engine::ImeEngine;
const DB: &str = r"C:\Users\Administrator\voice_input\lingxi_ime\backend\ciku.db";
#[test]
fn dbg_zi() {
    let mut e = ImeEngine::new(DB).unwrap();
    e.set_fuzzy_rules(vec!["z_zh","c_ch","s_sh"]);
    let _ = e.load_or_build_index(DB);
    let c = e.query("zi");
    println!("[zi] total cands = {}", c.len());
    // 查重复
    let mut seen = std::collections::HashSet::new();
    let mut dup = Vec::new();
    for (w,_) in &c {
        if !seen.insert(w.clone()) { dup.push(w.clone()); }
    }
    println!("[zi] duplicates = {:?}", dup);
    // 找 恣赀吱耔辎 在候选里的位置
    for target in ["恣","赀","吱","耔","辎"] {
        let pos: Vec<usize> = c.iter().enumerate().filter(|(_,(w,_))| w==target).map(|(i,_)| i).collect();
        println!("[zi] '{}' at idx {:?}", target, pos);
    }
    // 全量打印(带页号, 每页5)
    for (i,(w,wt)) in c.iter().enumerate() {
        println!("[zi] p{}#{} idx{} {} w={}", i/5, i%5, i, w, wt);
    }
}
