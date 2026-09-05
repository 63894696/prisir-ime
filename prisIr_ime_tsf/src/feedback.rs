//! 反馈诊断包(2026-09-04)——学 PrisirAI ⚙反馈问题(_build_feedback_zip)。
//!
//! **目的**:给用户省下描述,给我们精确看故障记录。菜单点「反馈问题」→
//! 打包 logs + system_info + 版本元数据 + 脱敏设置到桌面 zip,用户自己上传论坛/发邮件。
//!
//! **实现约束**:IME 无 zip 依赖(不引 flate2/zip crate),手写**无压缩(store)zip 容器**。
//! zip 是一种可手写的简单容器:local file header + file data + central directory + EOCD。
//! store 法压缩率=1,但日志文本小,够用。失败时 PowerShell Compress-Archive 兜底。
//!
//! **隐私**:settings.json 脱敏(抠 key 值留存在性布尔,学 _sanitize_settings_for_zip);
//! system_info 只含 OS/CPU/磁盘,不含 IP/MAC/会话正文。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 论坛反馈页:灵犀输入法子版(forum_relay.py BOARDS browser/ime)。
pub const FORUM_URL: &str = "https://bbs.babelspan.com/forum.html#board=browser/ime&hint=prisirai";
/// 反馈邮箱(与 about.rs 一致)。
const CONTACT_EMAIL: &str = "lsjdlijie@outlook.com";

fn log_fb(msg: &str) {
    crate::com_class_factory::log_dll_entry(&format!("[feedback] {}", msg));
}

/// 入口:打诊断 zip 到桌面,返回 zip 路径。失败静默返回 None(日志留因)。
pub fn build_feedback_zip() -> Option<PathBuf> {
    let ts = chrono_lite_timestamp();
    let desktop = desktop_dir();
    let zip_path = desktop.join(format!("PrisirIME-feedback-{}.zip", ts));
    log_fb(&format!("build start -> {}", zip_path.display()));

    // 收集条目 (name_in_zip, bytes)
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    entries.push(("description.txt".into(), b"(no description - pack via IME menu)".to_vec()));
    entries.push(("system_info.txt".into(), collect_system_info().into_bytes()));
    entries.push(("repo_meta.json".into(), collect_repo_meta().into_bytes()));
    entries.push(("settings.json".into(), collect_sanitized_settings().into_bytes()));
    for (name, data) in collect_logs() {
        entries.push((format!("logs/{}", name), data));
    }

    match write_zip_store(&zip_path, &entries) {
        Ok(()) => {
            log_fb(&format!("build done entries={} size={}", entries.len(),
                std::fs::metadata(&zip_path).map(|m| m.len()).unwrap_or(0)));
            Some(zip_path)
        }
        Err(e) => {
            log_fb(&format!("write_zip FAIL {:?}, trying powershell fallback", e));
            powershell_zip_fallback(&zip_path, &entries)
        }
    }
}

/// 打包后打开论坛页(ShellExecute 交给系统浏览器),让用户自己上传 zip。
pub fn open_forum() {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;
    let url: Vec<u16> = FORUM_URL.encode_utf16().chain(std::iter::once(0)).collect();
    let open: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = ShellExecuteW(None, PCWSTR(open.as_ptr()), PCWSTR(url.as_ptr()),
            PCWSTR::null(), PCWSTR::null(), SW_SHOW);
    }
}

/// 打包 + 打开论坛(菜单「反馈问题」一站式)。
/// **异步**:zip 打包 + 网络无关,但打包可能读大日志,放后台线程,不卡菜单/UI。
pub fn feedback_and_open() {
    std::thread::spawn(|| {
        let z = build_feedback_zip();
        open_forum();
        // 若打包成功,同时打开资源管理器选中 zip,让用户能找到它去上传。
        if let Some(p) = z {
            open_in_explorer(&p);
        }
    });
}

fn open_in_explorer(p: &Path) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;
    let arg: Vec<u16> = format!("/select,\"{}\"", p.display())
        .encode_utf16().chain(std::iter::once(0)).collect();
    let exe: Vec<u16> = "explorer.exe".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = ShellExecuteW(None, PCWSTR::null(), PCWSTR(exe.as_ptr()),
            PCWSTR(arg.as_ptr()), PCWSTR::null(), SW_SHOW);
    }
}

// ---------- 数据收集 ----------

fn chrono_lite_timestamp() -> String {
    // 用 Win32 GetLocalTime 拿本地时区的 YYYYMMDD-HHMMSS(不引 chrono)。
    use windows::Win32::System::SystemInformation::GetLocalTime;
    unsafe {
        let st = GetLocalTime();
        format!("{:04}{:02}{:02}-{:02}{:02}{:02}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond)
    }
}

fn desktop_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    let d = home.join("Desktop");
    if d.exists() { d } else { std::env::temp_dir() }
}

fn collect_system_info() -> String {
    let mut s = String::new();
    s.push_str(&format!("app: PrisirIME (prisir_ime_tsf) v{}\n", env!("CARGO_PKG_VERSION")));
    s.push_str(&format!("timestamp: {}\n", chrono_lite_timestamp()));
    s.push_str(&format!("os: Windows ({})\n", std::env::consts::OS));
    s.push_str(&format!("arch: {}\n", std::env::consts::ARCH));
    if let Some(h) = computer_name() {
        s.push_str(&format!("hostname: {}\n", h));
    }
    if let Ok(d) = std::fs::metadata(desktop_dir()) {
        let _ = d; // 磁盘信息略,不引 sysinfo
    }
    s.push_str("cpu_count: ");
    s.push_str(&std::thread::available_parallelism().map(|n| n.get().to_string()).unwrap_or("?".into()));
    s.push('\n');
    s
}

fn computer_name() -> Option<String> {
    // GetComputerNameW 在 windows 0.58 位于 Win32::System::SystemInformation? 实测不在。
    // 改用环境变量 COMPUTERNAME(同义,零 API 依赖,容错)。
    std::env::var("COMPUTERNAME").ok().filter(|s| !s.is_empty())
}

fn collect_repo_meta() -> String {
    let meta = serde_json::json!({
        "ime_version": env!("CARGO_PKG_VERSION"),
        "build": option_env!("PRISIR_BUILD").unwrap_or("dev"),
        "log_path": r"C:\Temp\prisir_dll_log.txt",
        "contact": CONTACT_EMAIL,
        "forum": FORUM_URL,
    });
    serde_json::to_string_pretty(&meta).unwrap_or_else(|_| "{}".into())
}

/// 脱敏设置:若存在 PrisirIME 配置含敏感字段,抠值留存在性布尔。
/// 当前 IME 无 settings.json,占位返回最小安全视图(与 PrisirAI 口径一致)。
fn collect_sanitized_settings() -> String {
    // IME 尚无持久 settings;若未来加,务必走 _sanitize 同口径(mask key 值)。
    serde_json::json!({
        "_note": "PrisirIME has no settings.json; keys never leave native layer",
    }).to_string()
}

/// 收集日志:C:\Temp\prisir_dll_log.txt(dllentry_log feature 写入)。
fn collect_logs() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let candidates = [
        (r"C:\Temp\prisir_dll_log.txt", "prisir_dll_log.txt"),
    ];
    for (path, name) in candidates {
        if let Ok(data) = std::fs::read(path) {
            // 截尾:日志可能很大,只留最后 512KB(最近故障最相关)。
            let tail = if data.len() > 512 * 1024 {
                data[data.len() - 512 * 1024..].to_vec()
            } else {
                data
            };
            out.push((name.to_string(), tail));
        }
    }
    out
}

// ---------- store zip 容器(无压缩,手写) ----------

/// 写一个合法 zip(store 法,compression=0)。返回 Err 触发 powershell 兜底。
fn write_zip_store(path: &Path, entries: &[(String, Vec<u8>)]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    let mut central: Vec<u8> = Vec::new();
    let mut offset: u32 = 0;

    for (name, data) in entries {
        let name_b = name.as_bytes();
        let crc = crc32(data);
        let size = data.len() as u32;

        // Local file header (30 bytes + name + data)
        let mut lh: Vec<u8> = Vec::with_capacity(30 + name_b.len() + data.len());
        lh.extend_from_slice(&0x04034b50u32.to_le_bytes()); // signature
        lh.extend_from_slice(&20u16.to_le_bytes());          // version needed
        lh.extend_from_slice(&0u16.to_le_bytes());           // flags
        lh.extend_from_slice(&0u16.to_le_bytes());           // method = 0 (store)
        lh.extend_from_slice(&0u16.to_le_bytes());           // mod time
        lh.extend_from_slice(&0u16.to_le_bytes());           // mod date
        lh.extend_from_slice(&crc.to_le_bytes());
        lh.extend_from_slice(&size.to_le_bytes());           // compressed = size
        lh.extend_from_slice(&size.to_le_bytes());           // uncompressed
        lh.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        lh.extend_from_slice(&0u16.to_le_bytes());           // extra len
        lh.extend_from_slice(name_b);
        lh.extend_from_slice(data);
        f.write_all(&lh)?;

        // Central directory record
        let mut cd: Vec<u8> = Vec::with_capacity(46 + name_b.len());
        cd.extend_from_slice(&0x02014b50u32.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes()); // version made by
        cd.extend_from_slice(&20u16.to_le_bytes()); // version needed
        cd.extend_from_slice(&0u16.to_le_bytes());  // flags
        cd.extend_from_slice(&0u16.to_le_bytes());  // method
        cd.extend_from_slice(&0u16.to_le_bytes());  // time
        cd.extend_from_slice(&0u16.to_le_bytes());  // date
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&size.to_le_bytes());
        cd.extend_from_slice(&size.to_le_bytes());
        cd.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes()); // extra
        cd.extend_from_slice(&0u16.to_le_bytes()); // comment
        cd.extend_from_slice(&0u16.to_le_bytes()); // disk start
        cd.extend_from_slice(&0u16.to_le_bytes()); // internal attr
        cd.extend_from_slice(&0u32.to_le_bytes()); // external attr
        cd.extend_from_slice(&offset.to_le_bytes()); // local header offset
        cd.extend_from_slice(name_b);
        central.extend_from_slice(&cd);

        offset += lh.len() as u32;
    }

    let cd_start = offset;
    f.write_all(&central)?;
    offset += central.len() as u32;

    // End of central directory
    let count = entries.len() as u16;
    let mut eocd: Vec<u8> = Vec::with_capacity(22);
    eocd.extend_from_slice(&0x06054b50u32.to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes()); // disk
    eocd.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
    eocd.extend_from_slice(&count.to_le_bytes());
    eocd.extend_from_slice(&count.to_le_bytes());
    eocd.extend_from_slice(&(central.len() as u32).to_le_bytes());
    eocd.extend_from_slice(&cd_start.to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes()); // comment len
    f.write_all(&eocd)?;
    let _ = offset;
    Ok(())
}

/// PowerShell 兜底:写临时目录后 Compress-Archive。手写 zip 失败时才走。
fn powershell_zip_fallback(zip_path: &Path, entries: &[(String, Vec<u8>)]) -> Option<PathBuf> {
    use std::process::Command;
    let stage = std::env::temp_dir().join(format!("prisir_fb_{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs()).unwrap_or(0)));
    if std::fs::create_dir_all(stage.join("logs")).is_err() {
        return None;
    }
    for (name, data) in entries {
        let p = stage.join(name.replace('/', "\\"));
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&p, data).is_err() {
            return None;
        }
    }
    let status = Command::new("powershell")
        .args(["-NoProfile", "-Command",
            &format!("Compress-Archive -Path '{}\\*' -DestinationPath '{}' -Force",
                stage.display(), zip_path.display())])
        .status()
        .ok()?;
    let _ = std::fs::remove_dir_all(&stage);
    if status.success() && zip_path.exists() {
        Some(zip_path.to_path_buf())
    } else {
        log_fb("powershell fallback also failed");
        None
    }
}

/// CRC-32(IEEE),zip 需要。不引 crc crate,查表法。
fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for i in 0..256u32 {
        let mut c = i;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
        }
        table[i as usize] = c;
    }
    let mut crc = 0xFFFFFFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFFFFFF
}
