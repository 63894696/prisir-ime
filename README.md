# Prisir 灵犀拼音输入法

跨平台拼音输入法,Windows / Android / Linux 三端。

## 目录结构

```
prisir_ime/          — 引擎(跨平台 Rust, C ABI + JNI)
  src/engine.rs      — 查询/排序/模糊音/智能整句
  src/db.rs          — SQLite 词库
  src/trie.rs        — 内存前缀索引
  src/mmap_index.rs  — mmap 持久化索引(.midx)
  src/ffi.rs         — C ABI(17 exports)
  src/jni.rs         — Android JNI

prisIr_ime_tsf/      — Windows TSF 壳
  src/tsf_input_processor.rs — ITfTextInputProcessor COM
  src/keystroke.rs           — 拼音状态机
  src/candidate_window.rs    — 候选窗
  src/status_bar.rs          — 悬浮工具栏 + 插件按钮
  src/plugin.rs              — 进程外插件框架
  src/handwriting_panel.rs   — 手写画板(Windows Ink)
  src/feedback.rs            — 反馈诊断包
  src/register.rs            — CTF 注册

installer/           — Windows 安装器(NSIS)
  lingxi_ime.nsi     — 安装脚本
  stage/             — 打包素材
```

## 构建

### Windows
```bash
cd prisIr_ime_tsf
cargo build --release
```

### Android(引擎交叉编译)
```bash
cd prisir_ime
cargo build --target aarch64-linux-android --release
```

## 插件框架

输入法支持进程外插件(独立 exe),通过 `plugins.json` 声明:

```json
{
  "plugins": [
    {
      "id": "ai",
      "name": "AI 助手",
      "exe": "plugins/ai/PrisirAI.exe",
      "event": "PrisirLingXi_AiToggle_Event",
      "button": "AI",
      "enabled": true
    }
  ]
}
```

插件放在 `%LOCALAPPDATA%\Prisir\plugins\` 目录,删目录即卸载。

## License

见 [LICENSE.txt](installer/stage/LICENSE.txt)
