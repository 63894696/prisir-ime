//! prisir_hw — 手写识别独立推理进程(2026-09-04,Step 3)。
//!
//! **架构**:IME DLL 不背 28MB ochw 模型 → 独立 exe,stdio 一行一 JSON-RPC。
//!   请求: {"strokes": [[[x,y],...], ...], "limit": N}
//!   响应: {"candidates": ["写", ...]}   或   {"error": "..."}
//! 笔画坐标:画板客户区像素(f32)。本进程做 bbox 归一化 → 96x96 RGB → ochw topN。
//!
//! **识别逻辑**:逐行移植安卓 `Handwriting.java`(ochw MobileNetV2 4037 类,
//! CASIA-HWDB1.0 训练,「写」字 0.9+ 置信度):
//!   toInput: bbox 归一(span 取 max,pad=8)→ 96x96 白底黑笔画(sw=3)→
//!            ImageNet normalize(mean/std)→ float[1*3*96*96]
//!   match:   softmax(logits)→ top-N 标签下标 → ochw_labels.txt 取字
//!
//! **模型加载**:ort load-dynamic,运行时载 onnxruntime.dll(随安装包带)。
//! 资产定位顺序: PRISIR_HW_DIR 环境变量 → exe 同目录\models → C:\PrisirIME\models。
//!   - ochw.ort         (模型)
//!   - ochw_labels.txt  (4037 字符表,tab 分隔取第一列)
//!   - onnxruntime.dll  (ort 运行时,load-dynamic)

use std::io::{self, BufRead, Write};

use serde::Deserialize;
use serde_json::json;

const INPUT_SIZE: usize = 96;
const PAD: f32 = 8.0;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

#[derive(Debug, Deserialize)]
struct HwRequest {
    #[serde(default)]
    strokes: Vec<Vec<[f32; 2]>>,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    10
}

// ── 资产定位 ────────────────────────────────────────────────────────────────
fn asset_dir() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("PRISIR_HW_DIR") {
        if !d.is_empty() {
            return std::path::PathBuf::from(d);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let m = dir.join("models");
            if m.join("ochw.ort").exists() {
                return m;
            }
        }
    }
    std::path::PathBuf::from(r"C:\PrisirIME\models")
}

fn log(msg: &str) {
    // 诊断落盘(stderr 在子进程管道里可能被 IME 吞,写文件可靠)。
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(r"C:\Temp\prisir_hw_log.txt")
    {
        let _ = writeln!(f, "[prisir_hw] {msg}");
    }
    eprintln!("[prisir_hw] {msg}");
}

// ── 识别引擎(懒加载常驻) ────────────────────────────────────────────────────
struct Engine {
    session: ort::session::Session,
    labels: Vec<String>,
}

static mut ENGINE: Option<Engine> = None;

fn ensure_engine() -> Result<&'static mut Engine, String> {
    unsafe {
        if ENGINE.is_some() {
            return Ok(ENGINE.as_mut().unwrap());
        }
        let dir = asset_dir();
        let model = dir.join("ochw.ort");
        let labels = dir.join("ochw_labels.txt");
        let rt = dir.join("onnxruntime.dll");
        if !model.exists() {
            return Err(format!("model not found: {}", model.display()));
        }
        if !labels.exists() {
            return Err(format!("labels not found: {}", labels.display()));
        }
        if !rt.exists() {
            return Err(format!("onnxruntime.dll not found: {}", rt.display()));
        }

        // ort load-dynamic:指向 onnxruntime.dll 所在目录。
        ort::init_from(&rt).map_err(|e| format!("ort init_from({}) fail: {e}", rt.display()))?;

        // 标签表:tab 分隔取第一列,空行跳过。
        let text = std::fs::read_to_string(&labels).map_err(|e| format!("read labels: {e}"))?;
        let mut ls = Vec::new();
        for line in text.lines() {
            let first = line.split('\t').next().unwrap_or("").trim();
            if !first.is_empty() {
                ls.push(first.to_string());
            }
        }
        log(&format!("labels loaded = {}", ls.len()));

        let session = ort::session::Session::builder()
            .map_err(|e| format!("session builder: {e}"))?
            .commit_from_file(&model)
            .map_err(|e| format!("commit_from_file({}): {e}", model.display()))?;
        log("session ready");
        ENGINE = Some(Engine { session, labels: ls });
        Ok(ENGINE.as_mut().unwrap())
    }
}

// ── 笔画 → 输入张量(移植安卓 toInput) ────────────────────────────────────────
/// 多笔画 → 白底黑笔画 96x96,ImageNet normalize,CHW 排布 float[1*3*96*96]。
/// 用离屏 RGB 栅格(纯软件画线,不依赖 GDI),与安卓 Bitmap 行为一致。
fn to_input(strokes: &[Vec<[f32; 2]>]) -> Option<Vec<f32>> {
    let mut minx = f32::MAX;
    let mut miny = f32::MAX;
    let mut maxx = f32::MIN;
    let mut maxy = f32::MIN;
    for st in strokes {
        for p in st {
            if p[0] < minx { minx = p[0]; }
            if p[0] > maxx { maxx = p[0]; }
            if p[1] < miny { miny = p[1]; }
            if p[1] > maxy { maxy = p[1]; }
        }
    }
    if minx == f32::MAX {
        return None;
    }
    let mut span = (maxx - minx).max(maxy - miny);
    if span <= 0.0 {
        span = 1.0;
    }
    let scale = (INPUT_SIZE as f32 - 2.0 * PAD) / span;
    let offx = PAD - minx * scale;
    let offy = PAD - miny * scale;
    let sw = 2.max(INPUT_SIZE / 32) as i32; // 笔画宽 = 3(对齐安卓 setStrokeWidth(3))

    // 96x96 白底栅格(0=白 255=黑,先单色,后展开 RGB)。
    let mut grid = vec![0u8; INPUT_SIZE * INPUT_SIZE];
    // 圆点粗化:以 (x,y) 为中心画实心圆。**半径取 sw(直径≈2*sw≈6)** ——
    // 2026-09-04 识别率修复:此前 r=sw/2=1 只画 3x3 断续小点,笔画不连贯,
    // 「个」「一」全认错(出「我/哦/▽/丁」)。安卓 Canvas drawPath strokeWidth=3
    // 是以线为中心向两侧扩的连续实心粗线;要匹配其笔画覆盖率,r 取 sw 让相邻
    // 采样点的圆盘重叠成连续粗笔画(用户实测「一」「个」识别率差的根因)。
    let mut plot = |x: i32, y: i32| {
        // 半径 sw/2=1(直径≈3px),对齐安卓 strokeWidth=3。2026-09-04 dump 实证:
        // r=sw=3 笔画直径 6-7px 太肥,「写」的冖/㇉糊成团(黑占比 17%),识别跑偏;
        // r=1 直径 3px 接近安卓 Canvas 实画粗度。
        let r = (sw / 2).max(1);
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    let xx = x + dx;
                    let yy = y + dy;
                    if xx >= 0 && yy >= 0 && (xx as usize) < INPUT_SIZE && (yy as usize) < INPUT_SIZE {
                        grid[yy as usize * INPUT_SIZE + xx as usize] = 255;
                    }
                }
            }
        }
    };
    // Bresenham 画线(相邻点连线)+ 单点画点。
    for st in strokes {
        if st.len() >= 2 {
            for w in st.windows(2) {
                let (x0, y0) = (
                    (w[0][0] * scale + offx) as i32,
                    (w[0][1] * scale + offy) as i32,
                );
                let (x1, y1) = (
                    (w[1][0] * scale + offx) as i32,
                    (w[1][1] * scale + offy) as i32,
                );
                let dx = (x1 - x0).abs();
                let dy = -(y1 - y0).abs();
                let sx = if x0 < x1 { 1 } else { -1 };
                let sy = if y0 < y1 { 1 } else { -1 };
                let mut err = dx + dy;
                let (mut x, mut y) = (x0, y0);
                loop {
                    plot(x, y);
                    if x == x1 && y == y1 {
                        break;
                    }
                    let e2 = 2 * err;
                    if e2 >= dy {
                        err += dy;
                        x += sx;
                    }
                    if e2 <= dx {
                        err += dx;
                        y += sy;
                    }
                }
            }
        } else if st.len() == 1 {
            plot(
                (st[0][0] * scale + offx) as i32,
                (st[0][1] * scale + offy) as i32,
            );
        }
    }

    // 栅格 → CHW float,RGB 同值(黑白图),ImageNet normalize。
    // debug:PRISIR_HW_DUMP 设置时把 96x96 栅格落成 PGM 眼见为实(对照安卓画布)。
    if std::env::var("PRISIR_HW_DUMP").is_ok() {
        if let Ok(mut f) = std::fs::File::create(r"C:\Temp\hw_dump.pgm") {
            use std::io::Write as _;
            let _ = write!(f, "P5\n{} {}\n255\n", INPUT_SIZE, INPUT_SIZE);
            let _ = f.write_all(&grid);
        }
    }
    let mut t = vec![0f32; 3 * INPUT_SIZE * INPUT_SIZE];
    // A/B 调试:PRISIR_HW_INVERT=1 时不翻转(白笔画黑底直送),验证 ochw 真实
    // 训练分布。默认翻转(黑字白底,对齐安卓 eraseColor(WHITE)+BLACK 笔画)。
    let invert = std::env::var("PRISIR_HW_INVERT").is_ok();
    for i in 0..INPUT_SIZE * INPUT_SIZE {
        let v = grid[i] as f32 / 255.0; // 1.0=笔画黑,0.0=白底
        // 安卓:黑笔画 rgb≈0,白底≈1。这里 v=1 是笔画,默认翻转:像素值 = 1-v。
        let px = if invert { v } else { 1.0 - v };
        let row = i / INPUT_SIZE;
        let col = i % INPUT_SIZE;
        t[0 * INPUT_SIZE * INPUT_SIZE + row * INPUT_SIZE + col] = (px - MEAN[0]) / STD[0];
        t[1 * INPUT_SIZE * INPUT_SIZE + row * INPUT_SIZE + col] = (px - MEAN[1]) / STD[1];
        t[2 * INPUT_SIZE * INPUT_SIZE + row * INPUT_SIZE + col] = (px - MEAN[2]) / STD[2];
    }
    Some(t)
}

/// 识别 top-N(移植安卓 match):softmax + 部分选择。
fn recognize(strokes: &[Vec<[f32; 2]>], limit: usize) -> Result<Vec<String>, String> {
    let data = to_input(strokes).ok_or("empty strokes")?;
    let eng = ensure_engine()?;
    let shape = vec![1usize, 3, INPUT_SIZE, INPUT_SIZE];
    let tensor = ort::value::Tensor::from_array((shape, data))
        .map_err(|e| format!("from_array: {e}"))?;
    let outputs = eng
        .session
        .run(ort::inputs![tensor])
        .map_err(|e| format!("session.run: {e}"))?;
    let (_shape, logits) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("extract tensor: {e}"))?;
    if logits.is_empty() {
        return Ok(vec![]);
    }
    // softmax(数值稳定:先减 max)。
    let max = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let mut sm = vec![0f32; logits.len()];
    let mut sum = 0f32;
    for (i, &v) in logits.iter().enumerate() {
        sm[i] = (v - max).exp();
        sum += sm[i];
    }
    if sum > 0.0 {
        for v in sm.iter_mut() {
            *v /= sum;
        }
    }
    // top-N 下标(简单选择,N 很小)。
    let lim = limit.max(1);
    let mut bi = vec![-1i32; lim];
    let mut bv = vec![f32::NEG_INFINITY; lim];
    for (i, &p) in sm.iter().enumerate() {
        if p > bv[lim - 1] {
            let mut j = lim - 1;
            while j > 0 && bv[j - 1] < p {
                bv[j] = bv[j - 1];
                bi[j] = bi[j - 1];
                j -= 1;
            }
            bv[j] = p;
            bi[j] = i as i32;
        }
    }
    let mut out = Vec::new();
    for &idx in bi.iter() {
        if idx >= 0 && (idx as usize) < eng.labels.len() {
            out.push(eng.labels[idx as usize].clone());
        }
    }
    Ok(out)
}

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut w = stdout.lock();
    log("prisir_hw started");
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<HwRequest>(&line) {
            Ok(req) => match recognize(&req.strokes, req.limit) {
                Ok(c) => json!({ "candidates": c }),
                Err(e) => {
                    log(&format!("recognize err: {e}"));
                    json!({ "error": e })
                }
            },
            Err(e) => json!({ "error": format!("parse: {e}") }),
        };
        let _ = writeln!(w, "{}", resp);
        let _ = w.flush();
    }
}
