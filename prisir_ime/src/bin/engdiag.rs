//! 诊断 exe:调 load_or_build_index 打印 index source,验证 rebuild + 双写 .idx/.midx。
//! 用法: engdiag <db_path>
use prisir_ime::engine::ImeEngine;
use std::time::Instant;

fn main() {
    let db = std::env::args().nth(1).expect("usage: engdiag <db_path>");
    println!("[engdiag] db={}", db);
    let md = std::fs::metadata(&db).expect("stat db");
    println!("[engdiag] db_len={}", md.len());

    let mut e = ImeEngine::new(&db).expect("open db");
    e.set_fuzzy_rules(vec!["z_zh", "c_ch", "s_sh"]);

    let t = Instant::now();
    let (ok, src) = e.load_or_build_index(&db);
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("[engdiag] load_or_build_index -> ok={} src={}  took={:.1}ms", ok, src, ms);

    // 2026-09-02:实测纯内存 trie 重建耗时(不 mmap/不落盘),评估能否同步建。
    let t2 = Instant::now();
    let mut e2 = ImeEngine::new(&db).expect("open db2");
    e2.set_fuzzy_rules(vec!["z_zh", "c_ch", "s_sh"]);
    let _ = e2.build_memory_index();
    println!("[engdiag] pure in-memory rebuild_index took={:.1}ms", t2.elapsed().as_secs_f64()*1000.0);
    let c2 = e2.query("zi");
    println!("[engdiag] trie query('zi') {} cands, top3={:?}",
        c2.len(), c2.iter().take(3).map(|(w,_)| w.clone()).collect::<Vec<_>>());
    let c2z = e2.query("z");
    println!("[engdiag] trie query('z') {} cands, top5={:?}",
        c2z.len(), c2z.iter().take(5).map(|(w,_)| w.clone()).collect::<Vec<_>>());

    // 抽查一次查询,确认引擎可用
    let t = Instant::now();
    let c = e.query("zi");
    println!("[engdiag] query('zi') {} cands in {:.2}ms, top3={:?}",
        c.len(), t.elapsed().as_secs_f64()*1000.0,
        c.iter().take(3).map(|(w,_)| w.clone()).collect::<Vec<_>>());

    // 单字母 z(SQLite 前缀高权桶路径)
    let cz = e.query("z");
    println!("[engdiag] query('z') {} cands, top12={:?}",
        cz.len(), cz.iter().take(12).map(|(w,_)| w.clone()).collect::<Vec<_>>());

    // 翻页稳定性:全量候选拼接取 md5,同进程多次 + 跨进程运行应完全一致。
    let full: String = c.iter().map(|(w,_)| w.as_str()).collect::<Vec<_>>().join(",");
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    full.hash(&mut h);
    println!("[engdiag] zi full50 hash={:016x} n={}", h.finish(), c.len());
    println!("[engdiag] zi full50 = [{}]", full);

    // 落盘检查
    let idx = db.replace(".db", ".idx");
    let midx = db.replace(".db", ".midx");
    for p in [&idx, &midx] {
        match std::fs::metadata(p) {
            Ok(m) => println!("[engdiag] FILE {} = {} bytes", p, m.len()),
            Err(_) => println!("[engdiag] FILE {} MISSING", p),
        }
    }

    // 复现「同 query 不同结果」检测:同进程连续多次 query('zi'),比对全量串是否每次一致。
    // 2026-09-02: TSF 翻页日志同一 page 候选内容不同,疑似 query 非确定。此处直接抓。
    let base = e.query("zi").iter().map(|(w,_)| w.clone()).collect::<Vec<_>>().join(",");
    for i in 1..=10 {
        let c = e.query("zi");
        let s: String = c.iter().map(|(w,_)| w.clone()).collect::<Vec<_>>().join(",");
        if s != base {
            println!("[engdiag] INSTABLE query#{} DIFFERS!", i);
            println!("[engdiag]   base = [{}]", base);
            println!("[engdiag]   q{}   = [{}]", i, s);
            return;
        }
    }
    println!("[engdiag] INSTABLE check: 10x identical OK");
}
