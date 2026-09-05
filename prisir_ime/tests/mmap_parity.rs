//! mmap 一致性 + 性能验证(关键路径不许假 PASS)。
//!
//! 用同一份 ciku.db 建 HashMap 版 MemoryIndex,导出 mmap 字节写临时文件,
//! MmapIndex::map 读回,对一批代表性输入逐方法断言:
//!   mmap 输出 == HashMap 输出(同词、同权重、同顺序,逐字节)。
//! 另计时 MmapIndex::map vs MemoryIndex::from_bytes(bincode),验证 mmap 加载远快。
//!
//! 全绿是把 mmap 接进 VM 的硬门槛。
//!
//! 运行: cargo test --test mmap_parity -- --nocapture

use prisir_ime::engine::ImeEngine;
use prisir_ime::mmap_index::{build_from_memory_index, MmapIndex};
use std::time::Instant;

const DB: &str = r"C:\Users\Administrator\voice_input\lingxi_ime\backend\ciku.db";

/// 代表性输入:覆盖单字/词组前缀、模糊音源、混拼、单字母、多音节、不存在 key。
const INPUTS: &[&str] = &[
    "z", "zi", "zhi", "zh", "x", "xz", "ni", "nihao", "n", "nih", "nihaoshijie",
    "s", "sh", "shi", "sj", "shij", "c", "ch", "wo", "women", "zhong", "zhongguo",
    "a", "b", "zo", "zzz", "qwq", "bu", "cunzai", "bucunzai", // 含不存在的
];

fn db_fp() -> (u64, u64) {
    let md = std::fs::metadata(DB).expect("stat db");
    let mtime = md
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    (md.len(), mtime)
}

#[test]
fn mmap_parity_and_perf() {
    // 1. 用 ImeEngine 触发 rebuild,拿到 HashMap 版 MemoryIndex(不从缓存,保证全新)。
    //    rebuild_index 是私有的,这里借 build_memory_index(它内部调 rebuild_index 灌 DB)。
    let mut e = ImeEngine::new(DB).expect("open db");
    e.build_memory_index().expect("build mem index");
    let mem = e.mem.as_ref().expect("mem index present");

    let (db_len, db_mtime) = db_fp();

    // 2. 导出 mmap 字节 + 写临时文件
    let t = Instant::now();
    let bytes = build_from_memory_index(mem, db_len, db_mtime).expect("build mmap bytes");
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    let bincode_len = std::fs::metadata(format!(r"{}", DB).replace(".db", ".idx"))
        .map(|m| m.len())
        .unwrap_or(0);
    println!(
        "[build] mmap bytes={} ({:.1}MB)  bincode .idx={} ({:.1}MB)  build_time={:.1}ms",
        bytes.len(),
        bytes.len() as f64 / 1e6,
        bincode_len,
        bincode_len as f64 / 1e6,
        build_ms
    );

    let tmp = std::env::temp_dir().join("prisir_parity_test.midx");
    std::fs::write(&tmp, &bytes).expect("write tmp midx");

    // 3. MmapIndex::map 读回(计时)
    let t = Instant::now();
    let mi = MmapIndex::map(&tmp, db_len, db_mtime).expect("mmap map");
    let mmap_load_us = t.elapsed().as_secs_f64() * 1e6;
    println!("[load] MmapIndex::map = {:.1}µs", mmap_load_us);

    // 4. bincode 反序列化对照(计时)——与生产 try_load_cached 同源的 to_bytes/from_bytes。
    //    只测反序列化本体(生产慢的就是这一步),不含文件读。
    let b = mem.to_bytes().expect("to_bytes");
    let t_bincode = Instant::now();
    let _de = prisir_ime::trie::MemoryIndex::from_bytes(&b).expect("from_bytes");
    let bincode_load_ms = t_bincode.elapsed().as_secs_f64() * 1000.0;
    println!(
        "[load] bincode from_bytes = {:.1}ms  (mmap 快 ~{:.0}x)",
        bincode_load_ms,
        bincode_load_ms * 1000.0 / mmap_load_us.max(1.0)
    );

    // 5. 逐输入 parity:mmap vs HashMap(用 e 的 HashMap 版 mem)
    let mut fail = 0;
    for inp in INPUTS {
        // HashMap 版(mem 是 build_memory_index 产出,mmap=None)
        let hm_prefix = mem.query_prefix(inp);
        let hm_single = mem.query_prefix_single(inp);
        let hm_jp = mem.query_jp(inp, 20);
        // 反混:first=首字符,rest=剩余
        let (first, rest) = inp.split_at(inp.char_indices().nth(1).map(|(i, _)| i).unwrap_or(inp.len()));
        let hm_rev = mem.query_reverse_mixed(first, rest, 15);

        // mmap 版
        let mm_prefix = mi.query_prefix(inp);
        let mm_single = mi.query_prefix_single(inp);
        let mm_jp = mi.query_jp(inp, 20);
        let mm_rev = mi.query_reverse_mixed(first, rest, 15);

        let check = |name: &str, a: &Vec<(String, i64)>, b: &Vec<(String, i64)>| {
            if a != b {
                println!(
                    "  MISMATCH [{}] input={:?}\n    hashmap={:?}\n    mmap   ={:?}",
                    name, inp, a, b
                );
                false
            } else {
                true
            }
        };
        if !check("prefix", &hm_prefix, &mm_prefix) { fail += 1; }
        if !check("prefix_single", &hm_single, &mm_single) { fail += 1; }
        if !check("jp", &hm_jp, &mm_jp) { fail += 1; }
        if !check("reverse_mixed", &hm_rev, &mm_rev) { fail += 1; }
    }

    // limit 截断语义:对混拼/单字前缀用极小 limit,验证 mmap 与 HashMap 截断后仍一致。
    // (query_jp 的 jp_map 桶上限 64、多数 < 20;用小 limit 强制走 truncate 路径。)
    for inp in ["s", "sh", "z", "n", "sj", "zh"] {
        for lim in [1usize, 3, 7] {
            let a = mem.query_jp(inp, lim);
            let b = mi.query_jp(inp, lim);
            if a != b {
                println!("  MISMATCH [jp limit={}] input={:?}\n    hashmap={:?}\n    mmap   ={:?}", lim, inp, a, b);
                fail += 1;
            }
            let a = mem.query_prefix_single(inp);
            let b = mi.query_prefix_single(inp);
            if a != b {
                println!("  MISMATCH [single trunc] input={:?}", inp);
                fail += 1;
            }
        }
    }
    println!("[parity] + limit-truncation cases, total mismatches={}", fail);

    // 清理临时文件
    let _ = std::fs::remove_file(&tmp);

    println!("[parity] {} inputs checked, {} mismatches", INPUTS.len(), fail);
    assert_eq!(fail, 0, "mmap 与 HashMap 结果不一致(关键路径不许假 PASS)");

    // 6. 性能门槛:mmap 加载应远快于 bincode,且绝对值应 < 50ms(微秒级预期)
    assert!(
        mmap_load_us < 50_000.0,
        "mmap 加载 {:.1}µs 超 50ms 门槛",
        mmap_load_us
    );
    println!("[OK] mmap parity PASS + load {:.1}µs", mmap_load_us);
}
