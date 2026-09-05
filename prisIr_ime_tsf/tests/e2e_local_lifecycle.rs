//! e2e_local_lifecycle — 真跑开发机 HKCU 完整生命周期, 验证「注册→启用→daemon→反注册」零残留
//!
//! 背景(T8 降级路径):
//!   原 plan §第三闸是 VMware 沙盒 E2E, 本期降级为开发机 HKCU 全链路闭环验证。
//!   开发机本身就是 HKCU per-user 沙箱 — `--register` / `--unregister` 落在 HKCU,
//!   `--daemon` 跑本地进程, 不影响其它用户。
//!
//! 不污染验证:
//!   - 测试开始前先 --unregister 兜底清理(应对上次失败留下的残局)
//!   - 测试中每步都走 HKCU + reg query 验真
//!   - 测试末尾必 --unregister 清理, 后状态断言 NOT REGISTERED
//!   - Drop 兜底: 进程退出前尽力 --unregister 一次, 防 panic 路径残留
//!   - 不写 HKLM, 不动系统全局

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Drop 兜底: 测试进程退出前尽力清理一次, 防 panic 路径残留。
struct CleanupGuard {
    exe: std::path::PathBuf,
    daemon_started: bool,
}
impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if self.daemon_started {
            common::kill_daemon();
        }
        let _ = Command::new(&self.exe).arg("--unregister").output();
    }
}

/// 起 daemon, 在另一线程里实时把 stdout/stderr 行丢到共享 buffer。
/// 返回 Child + 共享 buffer。
fn spawn_daemon_with_log_capture(exe: &std::path::Path) -> (std::process::Child, Arc<Mutex<String>>) {
    let mut daemon = Command::new(exe)
        .arg("--daemon")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("--daemon spawn");

    let buf = Arc::new(Mutex::new(String::new()));

    // stdout 线程
    if let Some(stdout) = daemon.stdout.take() {
        let buf_clone = Arc::clone(&buf);
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                buf_clone.lock().unwrap().push_str(&line);
                buf_clone.lock().unwrap().push('\n');
            }
        });
    }

    // stderr 线程
    if let Some(stderr) = daemon.stderr.take() {
        let buf_clone = Arc::clone(&buf);
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                buf_clone.lock().unwrap().push_str(&line);
                buf_clone.lock().unwrap().push('\n');
            }
        });
    }

    (daemon, buf)
}

#[test]
#[ignore] // 显式 `cargo test --test e2e_local_lifecycle -- --ignored` 触发
fn lifecycle_register_daemon_unregister_clean() {
    let e = common::exe_path();

    // (0) 兜底清理(应对上次失败留下的残局)
    let _ = Command::new(&e).arg("--unregister").output();

    let mut guard = CleanupGuard { exe: e.clone(), daemon_started: false };

    // (1) 预状态: NOT REGISTERED
    let o = Command::new(&e).arg("--status").output().expect("--status pre");
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.contains("\"tip_key_exists\": false"),
        "预状态应 NOT REGISTERED, 实际: {}",
        stdout
    );

    // (2) --register
    let o = Command::new(&e).arg("--register").output().expect("--register");
    assert!(
        o.status.success(),
        "--register 失败: stderr={}",
        String::from_utf8_lossy(&o.stderr)
    );
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.contains("OK (10 entries written)"),  // T9 hotfix #3: 加 DisplayDescription, 8→10
        "register 输出: {}",
        stdout
    );

    // (3) reg query 真验证 CATID = msctf.h CATID_TIP_KEYBOARD
    let r = Command::new("reg")
        .args(&[
            "query",
            r"HKCU\SOFTWARE\Microsoft\CTF\TIP\{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}",
            "/v",
            "Category",
        ])
        .output()
        .expect("reg query Category");
    let rs = String::from_utf8_lossy(&r.stdout);
    assert!(
        rs.contains("{36C679D9-696D-4A1B-9D8B-313E62CD3C30}"),
        "Category 应真 CATID, 实际: {}",
        rs
    );

    // (4) --enable
    let o = Command::new(&e).arg("--enable").output().expect("--enable");
    assert!(o.status.success(), "--enable 失败");
    let r = Command::new("reg")
        .args(&[
            "query",
            r"HKCU\SOFTWARE\Microsoft\CTF\TIP\{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}",
            "/v",
            "Enable",
        ])
        .output()
        .expect("reg query Enable");
    let rs = String::from_utf8_lossy(&r.stdout);
    assert!(rs.contains("0x1"), "Enable 应 0x1, 实际: {}", rs);

    // (5) --daemon 跑 ~16s 后台 + rename 触发的 dll touch + 看 hot reload 日志
    //
    // 重要: 直接 `File::set_modified` 在 daemon LoadLibrary 后失败(error 32, 文件被锁)。
    // 验证可行的路子: **rename-over-file** — 拷 dll → 改 tmp mtime → rename(tmp, dll, replace)。
    // rename 不需要 dll 写权限, 只用 FILE_SHARE_DELETE(LoadLibrary 默认带), 就可替换。
    //
    //  daemon 主循环: sleep 5s → check → sleep 5s → check → ...
    //  时序安排:
    //    t=0:   启动 daemon (拿到当前 mtime 作 last_mtime) + 后台 stdout 实时收
    //    t=6:   rename 替换 dll (新 mtime = now+10s)
    //    t=12+: test 等 daemon 进第二次主循环(必见 DLL mtime changed)
    //    t=15:  taskkill + 等子线程收尾
    let (mut daemon, log_buf) = spawn_daemon_with_log_capture(&e);
    guard.daemon_started = true;
    std::thread::sleep(Duration::from_secs(6));

    // rename 替换 dll: 拷 → 改 tmp mtime → 调用 bash mv 覆盖 dll
    //
    // 注意: Rust `std::fs::rename` 在 daemon LoadLibrary 锁 dll 的情况下失败(error 5),
    // 因为它调 MoveFileExW 没拿到足够的 share mode。但 Git Bash 的 `mv` (MSYS 实现)
    // 用的是 _wrename 调 MoveFileExW 并经 MSYS 内部打开 tmp 时带 FILE_SHARE_DELETE 等
    // 兼容, 实测可绕过 LoadLibrary 锁。所以从 Rust test 里 spawn `mv` (PATH 里有 Git Bash)。
    let dll = common::dll_path();
    assert!(dll.exists(), "dll 不存在: {:?}", dll);
    let tmp = dll.with_extension("dll.tmp_touch");
    let touch_result: Result<(), String> = (|| {
        std::fs::copy(&dll, &tmp).map_err(|e| format!("copy: {e}"))?;
        // 把 tmp mtime 推到 now+10s, 保证 daemon 必见变化
        use std::time::{SystemTime, UNIX_EPOCH};
        let new_mtime = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| UNIX_EPOCH + d + std::time::Duration::from_secs(10))
            .unwrap_or(SystemTime::now());
        let f = std::fs::File::options()
            .write(true)
            .open(&tmp)
            .map_err(|e| format!("open tmp: {e}"))?;
        f.set_modified(new_mtime)
            .map_err(|e| format!("set_modified tmp: {e}"))?;
        drop(f);

        // 用 Git Bash `mv` 替换 dll(避开 Rust rename 的 share mode 限制)
        let mv_out = Command::new("mv")
            .arg("-f")
            .arg(&tmp)
            .arg(&dll)
            .output()
            .map_err(|e| format!("spawn mv: {e}"))?;
        if !mv_out.status.success() {
            return Err(format!(
                "mv failed: stderr={} status={:?}",
                String::from_utf8_lossy(&mv_out.stderr),
                mv_out.status.code()
            ));
        }
        Ok(())
    })();
    if let Err(e) = touch_result {
        eprintln!("[e2e_local_lifecycle] rename-based touch 失败: {}", e);
    } else {
        eprintln!("[e2e_local_lifecycle] rename-based touch OK, 等 daemon hot reload");
    }

    // 等第二次主循环(check2 命中 hot reload)
    std::thread::sleep(Duration::from_secs(10));

    common::kill_daemon();
    guard.daemon_started = false;
    let _ = daemon.wait();

    // 等收尾线程把最后几行读完
    std::thread::sleep(Duration::from_millis(500));

    let combined = log_buf.lock().unwrap().clone();
    eprintln!(
        "[e2e_local_lifecycle] daemon captured log:\n{}",
        combined
    );
    assert!(
        combined.contains("DLL mtime changed") || combined.contains("hot reload"),
        "daemon 应报 hot reload, 实际: {}",
        combined
    );

    // (6) --disable
    let o = Command::new(&e).arg("--disable").output().expect("--disable");
    assert!(o.status.success(), "--disable 失败");
    let r = Command::new("reg")
        .args(&[
            "query",
            r"HKCU\SOFTWARE\Microsoft\CTF\TIP\{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}",
            "/v",
            "Enable",
        ])
        .output()
        .expect("reg query Enable post");
    let rs = String::from_utf8_lossy(&r.stdout);
    assert!(rs.contains("0x0"), "Enable 应 0x0, 实际: {}", rs);

    // (7) --unregister + reg query 0 残留
    let o = Command::new(&e).arg("--unregister").output().expect("--unregister");
    assert!(o.status.success(), "--unregister 失败");
    let r = Command::new("reg")
        .args(&["query", r"HKCU\SOFTWARE\Microsoft\CTF\TIP\{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}"])
        .output()
        .expect("reg query post");
    let rs = String::from_utf8_lossy(&r.stdout);
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(
        rs.contains("系统找不到指定的注册表项或值")
            || stderr.contains("系统找不到指定的注册表项或值")
            || rs.contains("ERROR")
            || stderr.contains("ERROR")
            || !r.status.success(),
        "应找不到 key, 实际 stdout={} stderr={}",
        rs,
        stderr
    );

    // (8) 后状态: NOT REGISTERED
    let o = Command::new(&e).arg("--status").output().expect("--status post");
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.contains("\"tip_key_exists\": false"),
        "后状态应 NOT REGISTERED, 实际: {}",
        stdout
    );
    assert!(
        stdout.contains("\"clsid_key_exists\": false"),
        "后状态应 NOT REGISTERED (clsid), 实际: {}",
        stdout
    );
}