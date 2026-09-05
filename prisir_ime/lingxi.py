# -*- coding: utf-8 -*-
"""灵犀输入法三法启动器(2026-08-24)

一个命令起任一输入法,激活键已错开不冲突:
  拼音  右Ctrl   Rust 引擎(prisir_ime.dll)
  五笔  右Shift  Rust 引擎(prisir_ime.dll, wubi86 表)
  语音  右Alt    SenseVoice 本地模型(voice_input/lingxi_app.py)

用法:
  python lingxi.py pinyin      # 起拼音(右Ctrl)
  python lingxi.py wubi        # 起五笔(右Shift)
  python lingxi.py voice       # 起语音(右Alt)
  python lingxi.py pinyin wubi # 同起拼音+五笔(键位错开可并行)
"""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
VOICE_APP = r"C:\Users\Administrator\voice_input\lingxi_app.py"

METHODS = ("pinyin", "wubi", "voice")


def _spawn(method):
    if method == "voice":
        if not os.path.isfile(VOICE_APP):
            print(f"[ERROR] 语音入口不存在: {VOICE_APP}")
            return None
        return subprocess.Popen([sys.executable, VOICE_APP])
    # pinyin / wubi 走本壳 Rust 引擎
    shell = os.path.join(HERE, "shell_rust.py")
    return subprocess.Popen([sys.executable, shell, "--method", method])


def main():
    args = [a for a in sys.argv[1:] if a in METHODS]
    if not args:
        print(__doc__)
        print("可用法: " + ", ".join(METHODS))
        return 1
    procs = []
    for m in args:
        p = _spawn(m)
        if p:
            tag = {"pinyin": "拼音(右Ctrl)", "wubi": "五笔(右Shift)", "voice": "语音(右Alt)"}[m]
            print(f"[OK] 起 {tag}  pid={p.pid}")
            procs.append(p)
    if not procs:
        return 1
    # 等任一退出即返回(后台进程独立存活)
    try:
        for p in procs:
            p.wait()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
