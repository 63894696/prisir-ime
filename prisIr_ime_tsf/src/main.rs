//! prisir_tsfsvc.exe 入口 — T7 阶段支持:
//!   --version / --help / --query-test / --register / --unregister / --status / --daemon
//!   --enable / --disable — T5 新增,改 Enable DWORD(不删 dll)
//!   --ipc / --ipc-test   — T6 新增,stdio JSON-RPC server(7 个 method,给 chrome UI / extension 调)
//!   --method             — T7 新增,切换拼音/五笔(纯内存,不写 HKCU)
//!   --about <key>        — 2026-08-29 新增,显示 关于/隐私/使用条款/反馈联系 正文(对齐 Android 端)
//!
//! 用法:
//!   prisir_tsfsvc --version
//!   prisir_tsfsvc --about about         # 显示「关于」段正文(对齐 Android 端 4 段)
//!   prisir_tsfsvc --help
//!   prisir_tsfsvc --query-test <pinyin> [db_path]
//!   prisir_tsfsvc --register [dll_path]
//!   prisir_tsfsvc --unregister
//!   prisir_tsfsvc --enable
//!   prisir_tsfsvc --disable
//!   prisir_tsfsvc --status             # T6 改成 JSON 输出 (T7 加 active_method 字段)
//!   prisir_tsfsvc --method <pinyin|wubi>  # T7 新增,纯内存切换
//!   prisir_tsfsvc --daemon
//!   prisir_tsfsvc --ipc                # T6: stdio JSON-RPC server,启动后 stdin/stdout 通信
//!   prisir_tsfsvc --ipc-test           # T6: 跑 3 个无害方法(version + status + query),打印响应
//!
//! 退出码:
//!   0 = OK (--register / --unregister / --enable / --disable / --query-test / --status REGISTERED / --daemon 干净退出 / --ipc 干净退出 / --method 切换成功)
//!   1 = 未识别参数 / --method 参数无效
//!   2 = bad utf-8 path
//!   3 = 引擎加载失败(dll 缺 / db 不存在)
//!   4 = query 返回 null (--query-test)
//!   5 = FFI bridge 本身未生效
//!  10 = --register 失败
//!  11 = --unregister 失败
//!  12 = --status 失败(读不到 HKCU, 通常是权限)
//!  20 = --daemon fatal
//!  21 = --enable 失败
//!  22 = --disable 失败
//!  30 = --ipc fatal(stdin/stdout 读写错)
//!  --status 2 = PARTIAL(部分注册)
//!  --status 3 = NOT REGISTERED

fn main() {
    use prisir_ime_tsf::about;
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--version") => {
            // 版本号单一来源: VERSION.txt(2026-08-29 决策对齐 Android 端)
            let (v, ch, _date) = read_version_txt().unwrap_or_else(|| {
                (env!("CARGO_PKG_VERSION").to_string(), "(unknown)".to_string(), "(unknown)".to_string())
            });
            println!("prisir_tsfsvc {} {} (T10 + WTS 真消息循环 + hidden HWND)", v, ch);
            println!("crate: prisir_ime_tsf");
            println!("windows crate: 0.58");
            println!("abi: C extern C — prisir_tsf_version / _echo / _load_engine / _query / _smart_sentence / _learn / _free_string / _free_engine");
            println!("register: HKCU CTF TIP + COM InprocServer32 (per-user, no admin)");
            println!("daemon: WTSRegisterSessionNotification + hidden HWND + 真消息循环 (T10) + DLL mtime 热重载 (T5)");
            println!("ipc: stdio JSON-RPC,7 methods (version/status/register/unregister/enable/disable/query) — T6");
            println!("input method: 拼音(默认=右Ctrl) / 五笔(默认=右Shift), --method <pinyin|wubi> 切换 — T7");
            println!("CATID: {{36C679D9-696D-4A1B-9D8B-313E62CD3C30}} (msctf.h CATID_TIP_KEYBOARD)");
            println!();
            println!("{}", about::about_links_inline());
        }
        Some("--about") => {
            // 2026-08-29 新增: 与 Android 端 UserDictActivity 底部 4 段正文对齐
            let key = match args.get(2).map(|s| s.as_str()) {
                Some(k) => k,
                None => {
                    println!("关于页 — 可选 key: about | privacy | terms | contact");
                    println!("用法: prisir_tsfsvc --about <key>");
                    println!("不传 key 时列出全部 4 段(便于一次性阅读)。");
                    println!();
                    for k in about::ABOUT_TITLES {
                        if let Some((title, body)) = about::lookup(title_to_key(k)) {
                            println!("========== {} ==========", title);
                            println!("{}", body);
                            println!();
                        }
                    }
                    std::process::exit(0);
                }
            };
            match about::lookup(key) {
                Some((title, body)) => {
                    println!("========== {} ==========", title);
                    println!("{}", body);
                    std::process::exit(0);
                }
                None => {
                    eprintln!("[about] 未知 key: {}", key);
                    eprintln!("[about] 接受: about | privacy | terms | contact");
                    std::process::exit(1);
                }
            }
        }
        Some("--help") => {
            println!("用法:");
            println!("  prisir_tsfsvc --version");
            println!("  prisir_tsfsvc --about <about|privacy|terms|contact>   # 显示 4 段关于页正文(对齐 Android)");
            println!("  prisir_tsfsvc --help");
            println!("  prisir_tsfsvc --query-test <pinyin> [db_path]");
            println!("  prisir_tsfsvc --register [dll_path]");
            println!("  prisir_tsfsvc --register-elevated  # T24: 自动 UAC 提升跑 --register");
            println!("  prisir_tsfsvc --register-status    # T24: 干跑验证 HKLM InprocServer32 + CoCreateInstance");
            println!("  prisir_tsfsvc --unregister");
            println!("  prisir_tsfsvc --enable");
            println!("  prisir_tsfsvc --disable");
            println!("  prisir_tsfsvc --activate            # T25+: ITfInputProcessorProfiles::ActivateLanguageProfile,触发 ctfmon LoadLibrary + 可能崩 +0x1525");
            println!("  prisir_tsfsvc --status                # JSON 输出 (T6 改, T7 加 active_method)");
            println!("  prisir_tsfsvc --method <pinyin|wubi>  # T7: 切换输入法(纯内存,不写 HKCU)");
            println!("  prisir_tsfsvc --daemon");
            println!("  prisir_tsfsvc --ipc                   # T6: stdio JSON-RPC server");
            println!("  prisir_tsfsvc --ipc-test              # T6: 跑 3 个无害烟囱");
            println!();
            println!("db_path 缺省查找顺序:");
            println!("  1. env PRISIR_CIKU_DB");
            println!("  2. %LOCALAPPDATA%\\Prisir\\Browser\\models\\ciku.db");
            println!("  3. voice_input\\lingxi_ime\\backend\\ciku.db (开发期兼容)");
            println!();
            println!("register / unregister 只写 HKCU(免管理员), 写完需重启 explorer.exe 才生效。");
            println!("enable / disable 翻 HKCU\\...\\Enable DWORD(dll 不动,只是 CTF 拉不拉)。");
            println!("method: 切拼音/五笔的全局标志,只在当前进程有效,下次启动回归默认(拼音)。");
            println!("        沿用 voice_input/lingxi_ime/backend 激活键: 拼音=右Ctrl(0xA3) 五笔=右Shift(0xA1)。");
            println!("daemon: 5s 轮询,WTS session 变化 + DLL mtime 变化触发热重载,Ctrl-C 退出。");
            println!("ipc: stdin/stdout 一行一 JSON-RPC, chrome extension 用 child_process 调起。");
        }
        Some("--query-test") => {
            let pinyin = args.get(2).map(|s| s.as_str()).unwrap_or("nihao");
            let db = args.get(3).cloned()
                .or_else(|| std::env::var("PRISIR_CIKU_DB").ok())
                .unwrap_or_else(|| {
                    std::env::var("LOCALAPPDATA")
                        .map(|d| format!("{}\\Prisir\\Browser\\models\\ciku.db", d))
                        .unwrap_or_else(|_| {
                            "C:/Users/Administrator/voice_input/lingxi_ime/backend/ciku.db".to_string()
                        })
                });
            run_query_test(pinyin, &db);
        }
        Some("--register") => run_register(args.get(2).cloned()),
        // 2026-09-04: IMM32 分支停用(用户拍板「Prisir 拼音」布局从架构移除)。
        // --register-imm 分发已摘;register_imm.rs/imm32.rs 源文件保留以便日后回滚。
        Some("--register-elevated") => run_register_elevated(),
        Some("--register-status") => run_register_status(),
        Some("--unregister") => run_unregister(),
        Some("--enable") => run_enable(),
        Some("--disable") => run_disable(),
        Some("--activate") => run_activate(),
        Some("--activate-mgr") => run_activate_mgr(),
        Some("--enum-profiles") => run_enum_profiles(),
        Some("--register-profile") => run_register_profile(),
        Some("--status") => run_status(),
        Some("--method") => {
            // T7: 切换进程级 active_method。纯内存,不写 HKCU,不调 dll。
            let arg = match args.get(2).map(|s| s.as_str()) {
                Some(a) => a,
                None => {
                    eprintln!("[method] 缺少参数: --method <pinyin|wubi>");
                    std::process::exit(1);
                }
            };
            match prisir_ime_tsf::parse_method(arg) {
                Ok(m) => {
                    prisir_ime_tsf::keystroke::set_active_method(m);
                    println!("[method] active: {}", m.as_str());
                    println!("[method] trigger: {}",
                        match m {
                            prisir_ime_tsf::InputMethod::Pinyin => format!("Right Ctrl (0x{:02X})", prisir_ime_tsf::PINYIN_TRIGGER_KEY),
                            prisir_ime_tsf::InputMethod::Wubi   => format!("Right Shift (0x{:02X})", prisir_ime_tsf::WUBI_TRIGGER_KEY),
                        });
                    println!("[method] 下次启动回归默认(拼音)。进程内立即生效。");
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("[method] FAIL: {e}");
                    eprintln!("[method] 用法: --method <pinyin|wubi>");
                    eprintln!("[method] 接受别名: 拼音/py / 五笔/wb (大小写不敏感)");
                    std::process::exit(1);
                }
            }
        }
        Some("--daemon") => run_daemon(),
        Some("--ipc") => {
            // T6: stdio JSON-RPC server。stdin EOF 时退出 0。
            if let Err(e) = prisir_ime_tsf::ipc::run_ipc_server() {
                eprintln!("[ipc] fatal: {e}");
                std::process::exit(30);
            }
        }
        Some("--ipc-test") => {
            // T6: 跑 3 个无害方法(version + status + query),不调 register/unregister/enable/disable,
            // 避免污染开发机 HKCU。
            run_ipc_test();
            println!("[ipc-test] all done");
        }
        _ => {
            eprintln!("未知参数: {:?}", args.get(1));
            eprintln!("用法: prisir_tsfsvc --version / --about <key> / --help / --query-test / --register / --register-elevated / --register-status / --unregister / --enable / --disable / --activate / --status / --method <pinyin|wubi> / --daemon / --ipc / --ipc-test");
            std::process::exit(1);
        }
    }
}

/// 读 VERSION.txt(单一来源),对齐 Android 端的 `oi_enhancements/VERSION.txt` 字段。
///
/// 优先 <exe_dir>/VERSION.txt, 兜底取当前可执行文件目录下的 VERSION.txt; 读不到就 None。
/// 字段格式: `KEY=VALUE`(等号分隔, `#` 开头是注释行)。
fn read_version_txt() -> Option<(String, String, String)> {
    let exe = std::env::current_exe().ok()?;
    let candidates = [
        exe.parent()?.join("VERSION.txt"),
        exe.parent()?.join("../VERSION.txt").to_path_buf(),
    ];
    for path in &candidates {
        if let Ok(content) = std::fs::read_to_string(path) {
            let mut v = String::new();
            let mut ch = String::new();
            let mut date = String::new();
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') { continue; }
                if let Some((k, val)) = line.split_once('=') {
                    match k.trim() {
                        "PRIMARY_VERSION" => v = val.trim().to_string(),
                        "RELEASE_CHANNEL" => ch = val.trim().to_string(),
                        "BUILD_DATE"      => date = val.trim().to_string(),
                        _ => {}
                    }
                }
            }
            if !v.is_empty() {
                return Some((v, ch, date));
            }
        }
    }
    None
}

/// 把 "关于" / "隐私说明" / "使用条款" / "反馈联系" 这种 title 翻成 about.rs 的 key。
fn title_to_key(title: &str) -> &'static str {
    match title {
        "关于"     => "about",
        "隐私说明" => "privacy",
        "使用条款" => "terms",
        "反馈联系" => "contact",
        _ => "about",
    }
}

/// `--register [dll_path]`: 把 dll 路径写到 HKCU CTF TIP + COM InprocServer32。
///
/// `dll_path` 缺省 = `<exe_dir>\prisir_ime_tsf.dll`。
fn run_register(dll_override: Option<String>) {
    let dll = dll_override.unwrap_or_else(|| {
        let exe = std::env::current_exe().expect("current_exe");
        exe.parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("prisir_ime_tsf.dll")
            .to_string_lossy()
            .into_owned()
    });
    println!("[register] dll: {}", dll);
    println!("[register] mode: HKCU only (per-user, no admin required)");

    match prisir_ime_tsf::register::do_register(&dll) {
        Ok(r) => {
            println!("[register] OK ({} entries written)", r.entries_written);
            println!("[register] CLSID: {}", prisir_ime_tsf::register::CLSID_PRISIR_IME_STR);
            println!("[register] 重要: Windows CTF 注册后必须重启 explorer.exe 才生效");
            println!("[register] 验证: 运行 `prisir_tsfsvc --status` 或 `reg query \"HKCU\\SOFTWARE\\Microsoft\\CTF\\TIP\\{}\\;`", prisir_ime_tsf::register::CLSID_PRISIR_IME_STR);
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("[register] FAIL: {e}");
            eprintln!("[register] 提示: 当前进程无 admin,HKLM 路径写不了。改用:");
            eprintln!("[register]   `prisir_tsfsvc --register-elevated` (自动 UAC 提权)");
            eprintln!("[register]   或 `regsvr32 \"C:\\Program Files\\PrisirIME\\prisir_ime_tsf.dll\"` (T24 P3 DllRegisterServer)");
            std::process::exit(10);
        }
    }
}

/// `--register-elevated`: 自动 UAC 提升跑 --register。
///
/// 流程:
///   1. 当前进程已 admin → 直接调 do_register(避免双 launch)
///   2. 未 admin → ShellExecuteExW("runas") 启动新进程跑 --register
///
/// 新进程从同一 exe 启动,**自己再解析 argv** (跟 fork 不同)。
/// 因此我们传 `--register`(而不是 `--register-elevated`,会无限递归)。
fn run_register_elevated() {
    use prisir_ime_tsf::elevate::{run_elevated, ElevateResult};

    let exe = std::env::current_exe()
        .expect("current_exe")
        .to_string_lossy()
        .into_owned();
    let args = ["--register"];

    match run_elevated(&exe, &args) {
        ElevateResult::AlreadyAdmin => {
            // 当前进程已 admin,直接调 do_register
            println!("[register-elevated] 已是 admin,直接跑 do_register");
            let dll = exe.replace("prisir_tsfsvc.exe", "prisir_ime_tsf.dll");
            match prisir_ime_tsf::register::do_register(&dll) {
                Ok(r) => {
                    println!("[register-elevated] OK ({} entries)", r.entries_written);
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("[register-elevated] FAIL: {e}");
                    std::process::exit(10);
                }
            }
        }
        ElevateResult::UacLaunched => {
            // UAC 已触发,新进程已 fork 出,父进程干净退出
            // (新进程会自己跑 --register 并写输出)
            println!("[register-elevated] UAC launched — 等用户在弹框点\"是\"");
            println!("[register-elevated] 父进程退出。新 admin 进程会自己写注册表并退出。");
            println!("[register-elevated] 验证: 等 5s 跑 `prisir_tsfsvc --register-status`");
            std::process::exit(0);
        }
        ElevateResult::Failed => {
            eprintln!("[register-elevated] UAC 失败");
            eprintln!("[register-elevated] 兜底: 手动 admin PowerShell:");
            eprintln!("[register-elevated]   Start-Process {} -ArgumentList '--register' -Verb RunAs", exe);
            std::process::exit(5); // UAC denied
        }
    }
}

/// `--register-status`: 干跑验证器,输出 JSON。
///
/// 只读 HKLM\..\InprocServer32 + 真跑 CoCreateInstance,输出 verdict。
/// 不写注册表,失败也不会污染系统。
///
/// agent 通道可凭此判定 "TIPC 激活链路前置条件" 是否就绪 — 关键路径不许假 PASS。
fn run_register_status() {
    match prisir_ime_tsf::register::read_hklm_inprocserver32_status() {
        Ok(s) => {
            let payload = serde_json::json!({
                "hklm_inprocserver32_default": s.inprocserver32_default,
                "hklm_inprocserver32_default_len": s.inprocserver32_default.as_ref().map(|s| s.len()).unwrap_or(0),
                "hklm_threading_model": s.threading_model,
                "hklm_key_exists": s.key_exists,
                "cocreate_inproc_server_hr": format!("0x{:08X}", s.cocreate_hr),
                "verdict": s.verdict,
                "clsid": prisir_ime_tsf::register::CLSID_PRISIR_IME_STR,
                "expected_dll_path": "C:\\Program Files\\PrisirIME\\prisir_ime_tsf.dll",
            });
            println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()));
            // 退出码: OK = 0, BROKEN = 13 (T24 新增,跟现有 10/11/12 不冲突)
            if s.verdict == "OK" {
                std::process::exit(0);
            } else {
                std::process::exit(13);
            }
        }
        Err(e) => {
            eprintln!("[register-status] FAIL: {e}");
            std::process::exit(14);
        }
    }
}

/// `--unregister`: 删除 HKCU 下 Prisir IME 的全部 key。幂等。
fn run_unregister() {
    println!("[unregister] removing HKCU CTF TIP + COM CLSID keys for CLSID={}", prisir_ime_tsf::register::CLSID_PRISIR_IME_STR);
    match prisir_ime_tsf::register::do_unregister() {
        Ok(()) => {
            println!("[unregister] OK");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("[unregister] FAIL: {e}");
            std::process::exit(11);
        }
    }
}

/// `--status`: 检查 HKCU 下两个 key 是否存在, 输出 **JSON**(T6 破坏性变更, 给 chrome settings UI 解析)。
///
///   两 key 都在   → exit 0
///   一个在        → exit 2
///   两个都不在    → exit 3
///
/// **T6 破坏性变更**: T5 之前 `--status` 输出 `[status] ...` 纯文本。T6 改 JSON, 给 chrome UI 用。
/// 退出码契约保持不变。
///
/// **T7 增量**: JSON 新增 `active_method` 字段(拼音/五笔),反映进程内 atomic 状态。
fn run_status() {
    match prisir_ime_tsf::register::do_status() {
        Ok(s) => {
            let active = prisir_ime_tsf::keystroke::active_method();
            let payload = serde_json::json!({
                "tip_key_exists": s.tip_key_exists,
                "clsid_key_exists": s.clsid_key_exists,
                "clsid": s.clsid,
                "version": env!("CARGO_PKG_VERSION"),
                "pid": std::process::id(),
                "active_method": active.as_str(),
            });
            println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()));
            if s.tip_key_exists && s.clsid_key_exists {
                std::process::exit(0);
            } else if s.tip_key_exists || s.clsid_key_exists {
                std::process::exit(2);
            } else {
                std::process::exit(3);
            }
        }
        Err(e) => {
            eprintln!("[status] FAIL: {e}");
            std::process::exit(12);
        }
    }
}

/// `--ipc-test`: 跑 3 个无害 method, 打印 request/response pair, 用于 `cargo run --release -- --ipc-test` 烟囱。
///
/// 故意不跑 register/unregister/enable/disable, 避免污染开发机 HKCU。
/// query 用 `nihao`, 缺省 ciku.db 路径走 ffi::prisir_tsf_load_engine, 失败时不报错退出
/// (本机可能没 dll/db, 烟囱就当 ffi 链路不通的兜底报一下, exit 0)。
fn run_ipc_test() {
    let tests: Vec<&str> = vec![
        r#"{"method":"version","params":{},"id":1}"#,
        r#"{"method":"status","params":{},"id":2}"#,
        r#"{"method":"query","params":{"pinyin":"nihao"},"id":3}"#,
    ];
    for line in tests {
        println!("[ipc-test] request : {}", line);
        let resp = prisir_ime_tsf::ipc::handle_request(line);
        println!("[ipc-test] response: {}", resp);
    }
}

/// `--enable`: `HKCU\...\Enable = 1`,告诉 CTF 拉起该 TIP。dll 仍在注册表里。
fn run_enable() {
    match prisir_ime_tsf::register::do_enable() {
        Ok(()) => {
            println!("[enable] OK (Enable=1)");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("[enable] FAIL: {e}");
            std::process::exit(21);
        }
    }
}

/// `--disable`: `HKCU\...\Enable = 0`,告诉 CTF 不要拉起。dll 仍在注册表里。
fn run_disable() {
    match prisir_ime_tsf::register::do_disable() {
        Ok(()) => {
            println!("[disable] OK (Enable=0)");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("[disable] FAIL: {e}");
            std::process::exit(22);
        }
    }
}

/// `--activate`: T25+0x1525 真因调查用。调
/// ITfInputProcessorProfiles::ActivateLanguageProfile,触发 ctfmon 走完整路径:
///   LoadLibrary(prisir_ime_tsf.dll) → CoCreateInstance → ITfInputProcessor::Activate
/// 这就是 explorer.exe 在 Settings 选中 Prisir 时调的,会触发 +0x1525 crash。
///
/// **风险**: ctfmon / explorer 可能崩。LocalDumps 已配,崩了会留 .dmp 在
/// guest `C:\Temp\dumps\`。
fn run_activate() {
    match prisir_ime_tsf::register::do_activate() {
        Ok(()) => {
            println!("[activate] OK (TIPC ActivateLanguageProfile 调通)");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("[activate] FAIL: {e}");
            std::process::exit(23);
        }
    }
}

/// `--activate-mgr`: T25「不可选」修复。用**新版**
/// ITfInputProcessorProfileMgr::ActivateProfile 激活(旧版 ActivateLanguageProfile
/// 在 Win10 对现代 TSF TIP 稳定返 E_FAIL)。详见 register::do_activate_mgr。
fn run_activate_mgr() {
    match prisir_ime_tsf::register::do_activate_mgr() {
        Ok(()) => {
            println!("[activate-mgr] OK (TIPC ActivateProfile 调通)");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("[activate-mgr] FAIL: {e}");
            std::process::exit(24);
        }
    }
}

/// `--enum-profiles`: 诊断。枚举 TIPC 看到的 zh-CN input-processor profiles。
fn run_enum_profiles() {
    match prisir_ime_tsf::register::do_enum_profiles() {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("[enum-profiles] FAIL: {e}");
            std::process::exit(25);
        }
    }
}

/// `--register-profile`: T25「不可选」修复。用新版 ITfInputProcessorProfileMgr::
/// RegisterProfile 把 Prisir 登记成可激活 input-processor(旧 AddLanguageProfile 假 PASS)。
fn run_register_profile() {
    let dll = std::env::args()
        .nth(2)
        .unwrap_or_else(|| r"C:\PrisirIME\prisir_ime_tsf.dll".to_string());
    match prisir_ime_tsf::register::do_register_profile(&dll) {
        Ok(()) => {
            println!("[register-profile] OK (TIPC RegisterProfile 调通)");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("[register-profile] FAIL: {e}");
            std::process::exit(26);
        }
    }
}

/// `--daemon`: T10 真 daemon 化,主循环走 `daemon::run_daemon`:
///   - T10: WTSRegisterSessionNotification + hidden HWND + 真消息循环(GetMessageW/DispatchMessageW)
///   - WndProc 处理 WM_WTSSESSION_CHANGE:WTS_CONSOLE_CONNECT/DISCONNECT → 自动 register/unregister
///   - T5: 后台 thread 5s 轮询 DLL mtime,变化 → FreeLibrary + LoadLibraryA 真热重载
///   - Ctrl-C handler 装上,GetMessageW 返 -1 后干净退出
fn run_daemon() {
    let config = prisir_ime_tsf::daemon::DaemonConfig::default();
    if let Err(e) = prisir_ime_tsf::daemon::run_daemon(config) {
        eprintln!("[daemon] fatal: {e}");
        std::process::exit(20);
    }
}

/// 真跑 `prisir_tsf_load_engine → query → smart_sentence → free_engine`,验证 FFI 链路。
fn run_query_test(pinyin: &str, db: &str) {
    use std::ffi::CString;

    eprintln!("[main] --query-test pinyin={:?} db={:?}", pinyin, db);

    let db_c = match CString::new(db) {
        Ok(c) => c,
        Err(e) => { eprintln!("bad db path utf-8: {e}"); std::process::exit(2); }
    };
    let pinyin_c = match CString::new(pinyin) {
        Ok(c) => c,
        Err(e) => { eprintln!("bad pinyin utf-8: {e}"); std::process::exit(2); }
    };

    #[allow(unused_unsafe)]
    let h = unsafe { prisir_ime_tsf::ffi::prisir_tsf_load_engine(db_c.as_ptr()) };
    if h.is_null() {
        eprintln!("[main] prisir_tsf_load_engine({}) 返回 null", db);
        eprintln!("       可能原因: prisIr_ime.dll 未找到(检查 PATH / PRISIR_IME_DLL / 当前目录)");
        eprintln!("                   或 ciku.db 不存在 / 不可读");
        std::process::exit(3);
    }
    println!("[main] engine loaded: handle={:p} db={}", h, db);

    #[allow(unused_unsafe)]
    let json_ptr = unsafe { prisir_ime_tsf::ffi::prisir_tsf_query(h, pinyin_c.as_ptr()) };
    if json_ptr.is_null() {
        eprintln!("[main] prisir_tsf_query 返回 null");
        prisir_ime_tsf::ffi::prisir_tsf_free_engine(h);
        std::process::exit(4);
    }
    let json = unsafe { std::ffi::CStr::from_ptr(json_ptr) }.to_string_lossy().into_owned();
    println!("[main] query '{}' → {}", pinyin, json);
    prisir_ime_tsf::ffi::prisir_tsf_free_string(json_ptr);

    let sent_in = "nihaoshijie";
    let sent_c = CString::new(sent_in).unwrap();
    #[allow(unused_unsafe)]
    let sent_ptr = unsafe { prisir_ime_tsf::ffi::prisir_tsf_smart_sentence(h, sent_c.as_ptr()) };
    if sent_ptr.is_null() {
        eprintln!("[main] smart_sentence 返回 null(继续往下走)");
    } else {
        let sent = unsafe { std::ffi::CStr::from_ptr(sent_ptr) }.to_string_lossy().into_owned();
        println!("[main] smart_sentence '{}' → '{}'", sent_in, sent);
        prisir_ime_tsf::ffi::prisir_tsf_free_string(sent_ptr);
    }

    let learn_in = CString::new("nihao").unwrap();
    let learn_sel = CString::new("你好").unwrap();
    prisir_ime_tsf::ffi::prisir_tsf_learn(h, learn_in.as_ptr(), learn_sel.as_ptr());
    println!("[main] learn 'nihao' -> '你好' submitted");

    prisir_ime_tsf::ffi::prisir_tsf_free_engine(h);
    println!("[main] engine freed");
}
