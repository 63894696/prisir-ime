//! 2026-09-04 模糊音殿后排序验证:确认修复 i64 后,模糊音字(负权重)仍排在原音字
//! 之后,而不是被排到最前。用法: shoudiag <db_path>
use prisir_ime::engine::ImeEngine;

fn show_full(e: &ImeEngine, q: &str) {
    let c = e.query(q);
    // 打印全部候选的 (字:权重),看原音字(正权重)是否都在模糊音字(负权重)之前。
    let list: Vec<String> = c.iter().map(|(w, wt)| format!("{}:{}", w, wt)).collect();
    // 找第一个负权重出现的位置 = 原音区与模糊区的分界。
    let first_neg = c.iter().position(|(_, wt)| *wt < 0);
    let n_pos = c.iter().filter(|(_, wt)| *wt >= 0).count();
    println!(
        "  '{}': n={} 原音(正){}个 模糊(负){}个 首个负权重在第{}位",
        q,
        c.len(),
        n_pos,
        c.len() - n_pos,
        first_neg.map(|i| i + 1).map(|i| i.to_string()).unwrap_or_else(|| "无".into())
    );
    println!("      全序: {:?}", list);
}

fn main() {
    let db = std::env::args().nth(1).expect("usage: shoudiag <db_path>");
    let mut ef = ImeEngine::new(&db).expect("open db fuzzy");
    ef.set_fuzzy_rules(vec!["z_zh".into(), "c_ch".into(), "s_sh".into()]);
    let _ = ef.load_or_build_index(&db);
    println!("== 模糊音殿后排序验证(原音字必须先于模糊音字)==");
    for q in ["shou", "sang", "cou", "zang", "ce"] {
        show_full(&ef, q);
    }
}
