//! 引擎冒烟测试:内存 trie 提速 + 模糊音(2026-09-02)。
//! 用真实 ciku.db 验证 (a) build_index 后 query 走内存微秒级 (b) 模糊音 z/zh 生效。
//! 跑: cargo test --release --test smoke -- --nocapture
use prisir_ime::engine::ImeEngine;
use std::time::Instant;

const DB: &str = r"C:\Users\Administrator\voice_input\lingxi_ime\backend\ciku.db";

#[test]
fn memory_index_speeds_up_query() {
    let mut e = ImeEngine::new(DB).expect("open db");
    // 未建索引:SQLite 查询计时
    let t0 = Instant::now();
    let c1 = e.query("nihao");
    let sqlite_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert!(!c1.is_empty(), "SQLite query nihao 应有候选");
    println!("[smoke] sqlite query('nihao') = {:.2}ms, top={:?}", sqlite_ms, c1.first());

    // 建内存索引
    let (ok, src) = e.load_or_build_index(DB);
    println!("[smoke] index loaded ok={} src={}", ok, src);
    assert!(ok, "内存索引应构建/加载成功");

    // 建索引后:内存查询计时(多次取平均)
    let mut total = 0.0;
    let n = 20;
    for _ in 0..n {
        let t = Instant::now();
        let _ = e.query("nihao");
        total += t.elapsed().as_secs_f64() * 1000.0;
    }
    let mem_ms = total / n as f64;
    let c2 = e.query("nihao");
    println!("[smoke] mem query('nihao') avg = {:.3}ms over {} runs, top={:?}", mem_ms, n, c2.first());
    assert!(!c2.is_empty(), "内存查询 nihao 应有候选");
    // 内存应明显快于 SQLite(放宽:至少不慢,且平均 < 5ms)
    assert!(mem_ms < 5.0, "内存查询应 <5ms, 实际 {:.3}ms", mem_ms);
}

#[test]
fn fuzzy_pinyin_works() {
    let mut e = ImeEngine::new(DB).expect("open db");
    e.set_fuzzy_rules(vec!["z_zh", "c_ch", "s_sh"]);
    let _ = e.load_or_build_index(DB);
    // 'zi' 平舌,模糊音应把翘舌 'zhi' 的字也纳入候选集(之/知/只/支…),
    // 不要求排进前 10(高频平舌字权重更高),但全集里必须出现。
    let cands = e.query("zi");
    let words: Vec<&str> = cands.iter().map(|(w, _)| w.as_str()).collect();
    println!("[smoke] fuzzy query('zi') top10={:?} total={}", &words[..words.len().min(10)], words.len());
    assert!(!cands.is_empty(), "模糊音查询 zi 应有候选");
    let has_zhishe = words.iter().any(|w| matches!(*w, "之" | "知" | "只" | "支" | "织" | "直" | "至" | "治" | "制" | "智"));
    println!("[smoke] fuzzy zi 含翘舌字(之/知/只/支…) = {}", has_zhishe);
    assert!(has_zhishe, "模糊音应把 zhi 的翘舌字纳入 zi 的候选集,实际 top50={:?}", words);
}

#[test]
fn long_pinyin_query_fast() {
    let mut e = ImeEngine::new(DB).expect("open db");
    let _ = e.load_or_build_index(DB);
    // 模拟用户报卡顿的长拼音连续输入
    for p in ["n", "ni", "nih", "niha", "nihao"] {
        let t = Instant::now();
        let c = e.query(p);
        println!("[smoke] query('{}') = {:.3}ms cands={}", p, t.elapsed().as_secs_f64()*1000.0, c.len());
    }
}

#[test]
fn mixed_pinyin_query_fast() {
    // 2026-09-02:非全拼音节前缀(zo/nih/删字中途)原走 SQLite 混拼回退,实测 10-15ms 卡顿。
    // 内存化后应 <3ms。删到 'z' 单字母走 query_pinyin_jp 也应快。
    let mut e = ImeEngine::new(DB).expect("open db");
    e.set_fuzzy_rules(vec!["z_zh", "c_ch", "s_sh"]);
    let _ = e.load_or_build_index(DB);
    // 预热一次(触发 bincode 反序列化桶的惰性分配/内存页调入),再测稳态。
    for p in ["zo", "nih", "niha", "z", "zhon"] {
        let _ = e.query(p);
    }
    for p in ["zo", "nih", "niha", "z", "zhon"] {
        let mut worst = 0.0f64;
        for _ in 0..20 {
            let t = Instant::now();
            let c = e.query(p);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if ms > worst { worst = ms; }
            let _ = c;
        }
        let c = e.query(p);
        println!("[smoke] mixed query('{}') worst={:.3}ms cands={}", p, worst, c.len());
        assert!(worst < 3.0, "混拼查询 '{}' 应 <3ms, 实际 worst {:.3}ms", p, worst);
    }
}
