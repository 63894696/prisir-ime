//! 每日活跃信号(2026-09-04)——学 PrisirAI oiagent-shell 的 checkBrandUpdates。
//!
//! **口径**:每日(本地日切换)仅向我们自己的网站发一个 HTTPS GET
//! `https://www.babelspan.com/updates.json`,不带任何 ID/内容/参数。
//! 服务端用 nginx 给该路径单独切 access_log 数 IP(粗活跃,文档明说动态IP/NAT 不准)。
//! 与 PrisirAI v2.6.0 同口径、零服务端改动、隐私友好。
//!
//! **挂点**:daemon 常驻进程 prisir_tsfsvc 的 5s 轮询线程里检测「本地日是否切换」,
//! 切换即发一次。不发在 IME DLL 里(DLL 不应做网络 IO,且每次打字都触发会爆炸)。
//!
//! **容错静默**:任何失败(无网/超时/DNS)都静默返回,绝不崩 daemon、绝不弹窗、不重试轰炸。
//! 失败只在日志留一行,明天再试。
//!
//! **隐私**:这是唯一的联网行为。隐私页文案改为「每日一次匿名更新检查」,
//! 与本实现严格一致(无 ID、无内容、无行为数据、一日一次)。

use std::sync::atomic::{AtomicI64, Ordering};

/// 信号地址:与 PrisirAI 同一个 updates.json(同口径,nginx 切日志)。
const ACTIVE_URL_HOST: &str = "www.babelspan.com";
const ACTIVE_URL_PATH: &str = "/updates.json";

/// 上次发信号的本地「日序号」(days since epoch)。进程级,重启后重发一次可接受
/// (nginx 去重按日,重启重发不会产生第二个 IP 记录——同一 IP)。
static LAST_ACTIVE_DAY: AtomicI64 = AtomicI64::new(-1);

/// 本地当前「日序号」(UTC epoch-days,粗粒度足够,只为「一日一次」去重)。
fn local_day_index() -> i64 {
    // SystemTime -> unix secs -> /86400。不引入 chrono,够用。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (secs / 86400) as i64
}

/// 若今日还没发过活跃信号,spawn 一次性后台线程发一次并记下日期。每日至多一次。
/// **异步**:立即返回,网络 IO 在后台线程,绝不卡 IME 激活/打字线程。
/// 返回 true=本次触发了发送,false=今日已发(不重复 spawn)。
pub fn tick_daily_active() -> bool {
    let today = local_day_index();
    let last = LAST_ACTIVE_DAY.load(Ordering::Relaxed);
    if last == today {
        return false; // 今日已发
    }
    // 先记再发:失败也记 today,避免无网时每次激活都狂试轰炸(明天再试)。
    LAST_ACTIVE_DAY.store(today, Ordering::Relaxed);
    std::thread::spawn(|| {
        let ok = send_active_signal();
        crate::com_class_factory::log_dll_entry(&format!("[active] daily signal sent={}", ok));
    });
    true
}

/// 实际发 HTTPS GET。容错静默,返回是否 2xx。
fn send_active_signal() -> bool {
    use windows::Win32::Networking::WinHttp::*;
    use windows::core::PCWSTR;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe {
        let agent = w("PrisirIME-Active/0.7");
        let session = match WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ) {
            h if !h.is_null() => h,
            _ => {
                log_active("WinHttpOpen FAIL");
                return false;
            }
        };
        // 超时:解析/连接/发送/接收各 3s,绝不 hang daemon。
        let _ = WinHttpSetTimeouts(session, 3000, 3000, 3000, 3000);

        let host = w(ACTIVE_URL_HOST);
        let conn = WinHttpConnect(session, PCWSTR(host.as_ptr()), 443, 0);
        if conn.is_null() {
            log_active("WinHttpConnect FAIL");
            let _ = WinHttpCloseHandle(session);
            return false;
        }

        let path = w(ACTIVE_URL_PATH);
        let get = w("GET");
        let req = WinHttpOpenRequest(
            conn,
            PCWSTR(get.as_ptr()),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE, // HTTPS
        );
        if req.is_null() {
            log_active("WinHttpOpenRequest FAIL");
            let _ = WinHttpCloseHandle(conn);
            let _ = WinHttpCloseHandle(session);
            return false;
        }

        // 0.58 签名: headers 是 Option<&[u16]>,无独立 headerslength 参数。
        let sent = WinHttpSendRequest(req, None, None, 0, 0, 0);
        let mut sent_ok = false;
        if sent.is_ok() && WinHttpReceiveResponse(req, std::ptr::null_mut()).is_ok() {
            let mut code: u32 = 0;
            let mut sz = std::mem::size_of::<u32>() as u32;
            let flag = WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER;
            if WinHttpQueryHeaders(
                req,
                flag,
                PCWSTR::null(),
                Some(&mut code as *mut _ as *mut _),
                &mut sz,
                std::ptr::null_mut(),
            )
            .is_ok()
            {
                sent_ok = (200..300).contains(&code);
                log_active(&format!("active signal HTTP {}", code));
            }
        } else {
            log_active("WinHttpSend/Receive FAIL (no network?)");
        }

        let _ = WinHttpCloseHandle(req);
        let _ = WinHttpCloseHandle(conn);
        let _ = WinHttpCloseHandle(session);
        sent_ok
    }
}

fn log_active(msg: &str) {
    crate::com_class_factory::log_dll_entry(&format!("[active] {}", msg));
}
