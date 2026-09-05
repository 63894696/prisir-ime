use rusqlite::Connection;
const DB: &str = r"C:\Users\Administrator\voice_input\lingxi_ime\backend\ciku.db";
#[test]
fn dbg_bucket() {
    let conn = Connection::open(DB).unwrap();
    // z 开头的 jp 首字母词组数(= 反混 z 桶大小)
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM phrase WHERE jp >= 'z' AND jp < '{'", [], |r| r.get(0)).unwrap();
    println!("[bucket] jp 首字母=z 的 phrase 行数 = {}", n);
    // key 以 o 结尾的 z 开头词组数(= zo 反混命中数)
    let m: i64 = conn.query_row("SELECT COUNT(*) FROM phrase WHERE jp >= 'z' AND jp < '{' AND key LIKE '%o'", [], |r| r.get(0)).unwrap();
    println!("[bucket] 其中 key 以 'o' 结尾 = {}", m);
    // 全 phrase 行数
    let t: i64 = conn.query_row("SELECT COUNT(*) FROM phrase", [], |r| r.get(0)).unwrap();
    println!("[bucket] phrase 总行数 = {}", t);
}
