# -*- coding: utf-8 -*-
"""Prisir 灵犀输入法壳 — Rust 引擎驱动(拼音/五笔双法)
外挂式输入法:全局键盘钩子捕获输入 → 光标位置候选窗 → SendInput 上屏。
引擎用 prisir_ime.dll(Rust,逻辑同源 Python lingxi_ime),用于实机验证。
自包含:光标定位/候选窗/上屏内联,不依赖 lingxi_ime backend。

三法拆分(2026-08-24):拼音/五笔在本壳内按 --method 切换,激活键错开;
语音独立在 voice_input/lingxi_app.py(右Alt)。
  拼音  --method pinyin  右Ctrl (0xA3)
  五笔  --method wubi    右Shift(0xA1)

用法:
  python shell_rust.py [--method pinyin|wubi] [--db path] [--no-index] [--no-tray]
  激活键切换激活;激活态打字出候选;数字/空格选词;回车上屏原文;Esc 取消
"""
import ctypes, json, os, sys, time, threading
from ctypes import wintypes
import tkinter as tk

try:
    import lingxi_hotkeys as HK
    C = HK.COLORS
except Exception:  # 配置层缺失时兜底,壳仍能跑
    C = {"bg": "#ffffff", "bg2": "#e8f4fd", "border": "#b3d7f5",
         "text": "#1f2937", "text_dim": "#5b6b7c", "gold": "#c47f2a",
         "ready": "#1e9e5a", "recording": "#128a4a", "busy": "#b07d18",
         "error": "#c13a3a"}

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_DB = r"C:\Users\Administrator\voice_input\lingxi_ime\backend\ciku.db"
DLL = os.path.join(HERE, "target", "release", "prisir_ime.dll")
LOG = os.path.join(HERE, "shell_debug.log")

# ---- Windows 常量 ----
WH_KEYBOARD_LL = 13
WM_KEYDOWN = 0x0100
WM_SYSKEYDOWN = 0x0104
VK_BACK, VK_RETURN, VK_ESCAPE, VK_SPACE = 0x08, 0x0D, 0x1B, 0x20
VK_0, VK_9 = 0x30, 0x39
VK_RCONTROL, VK_RSHIFT = 0xA3, 0xA1

# 三法激活键(与 lingxi_hotkeys.SCHEMAS 对齐):拼音右Ctrl / 五笔右Shift / 语音右Alt(独立进程)
METHODS = {
    "pinyin": {"label": "拼音", "trigger": VK_RCONTROL, "tag": "P"},
    "wubi":   {"label": "五笔", "trigger": VK_RSHIFT,   "tag": "W"},
}

# 候选窗扩展样式:WS_EX_NOACTIVATE = 鼠标点选不抢前台焦点。
# 缺了它,点候选词时焦点切到候选窗,SendInput 的字会上屏进候选窗自己而不是目标应用。
GWL_EXSTYLE = -20
WS_EX_NOACTIVATE = 0x08000000
WS_EX_TOOLWINDOW = 0x00000080


def log(msg):
    try:
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(f"{time.strftime('%H:%M:%S')} {msg}\n")
    except Exception:
        pass


def _crash_hook(exc_type, exc, tb):
    """未捕获异常兜底:写栈到日志,避免无声闪退。"""
    import traceback
    log("[CRASH] " + "".join(traceback.format_exception(exc_type, exc, tb)))


def _thread_crash_hook(args):
    import traceback
    log("[THREAD-CRASH] " + "".join(traceback.format_exception(args.exc_type, args.exc_value, args.exc_traceback)))


sys.excepthook = _crash_hook
threading.excepthook = _thread_crash_hook


# ---------- Rust 引擎封装(ctypes) ----------
class RustIMEEngine:
    def __init__(self, db_path, build_index=True):
        lib = ctypes.CDLL(DLL)
        lib.prisir_ime_load.restype = ctypes.c_void_p
        lib.prisir_ime_load.argtypes = [ctypes.c_char_p, ctypes.c_int]
        lib.prisir_ime_query.restype = ctypes.c_void_p
        lib.prisir_ime_query.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        lib.prisir_ime_query_wubi.restype = ctypes.c_void_p
        lib.prisir_ime_query_wubi.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        lib.prisir_ime_smart_sentence.restype = ctypes.c_void_p
        lib.prisir_ime_smart_sentence.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        lib.prisir_ime_learn.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p]
        lib.prisir_ime_free_string.argtypes = [ctypes.c_void_p]
        self.lib = lib
        t0 = time.perf_counter()
        self.h = lib.prisir_ime_load(db_path.encode(), 1 if build_index else 0)
        self.load_ms = (time.perf_counter() - t0) * 1000
        if not self.h:
            raise RuntimeError(f"prisir_ime_load 失败: {db_path}")
        log(f"[engine] loaded index={build_index} in {self.load_ms:.0f}ms")

    def _str(self, ptr):
        if not ptr:
            return None
        s = ctypes.cast(ptr, ctypes.c_char_p).value.decode("utf-8")
        self.lib.prisir_ime_free_string(ptr)
        return s

    def query(self, inp):
        ptr = self.lib.prisir_ime_query(self.h, inp.encode())
        return json.loads(self._str(ptr) or "[]")

    def query_wubi(self, inp):
        ptr = self.lib.prisir_ime_query_wubi(self.h, inp.encode())
        return json.loads(self._str(ptr) or "[]")

    def smart_sentence(self, inp):
        return self._str(self.lib.prisir_ime_smart_sentence(self.h, inp.encode())) or ""

    def learn(self, inp, sel):
        self.lib.prisir_ime_learn(self.h, inp.encode(), sel.encode())


# ---------- 光标定位(GetGUIThreadInfo) ----------
class GUITHREADINFO(ctypes.Structure):
    _fields_ = [("cbSize", ctypes.c_ulong), ("flags", ctypes.c_ulong),
                ("hwndActive", ctypes.c_void_p), ("hwndFocus", ctypes.c_void_p),
                ("hwndCapture", ctypes.c_void_p), ("hwndMenuOwner", ctypes.c_void_p),
                ("hwndMoveSize", ctypes.c_void_p), ("hwndCaret", ctypes.c_void_p),
                ("rcCaret", ctypes.c_long * 4)]


def caret_position():
    # 取前台窗口所属线程的 caret(传 0=本线程拿不到目标应用光标,会退化成固定坐标)。
    u = ctypes.windll.user32
    gui = GUITHREADINFO()
    gui.cbSize = ctypes.sizeof(GUITHREADINFO)
    hwnd = u.GetForegroundWindow()
    tid = u.GetWindowThreadProcessId(hwnd, None)
    try:
        if tid and u.GetGUIThreadInfo(tid, ctypes.byref(gui)) and gui.hwndCaret:
            left, top, right, bottom = gui.rcCaret
            pt = wintypes.POINT(left, bottom)
            u.ClientToScreen(gui.hwndCaret, ctypes.byref(pt))
            if pt.x or pt.y:
                return pt.x, pt.y
    except Exception:
        pass
    # 兜底:鼠标位置偏下(不挡文字)
    pt = wintypes.POINT()
    u.GetCursorPos(ctypes.byref(pt))
    return pt.x, pt.y + 24


# ---------- SendInput Unicode 上屏 ----------
# 64位 Windows INPUT 结构体:type(4)+填充(4)+union(32,最大是 MOUSEINPUT)=40 字节。
# ctypes 自动布局常算出 32(union 对齐错),SendInput 校验 cbSize 不符 → err=87 整批拒收。
# 手工铺平布局,强制 sizeof=40。
class INPUT(ctypes.Structure):
    _fields_ = [("type", wintypes.DWORD),
                ("_pad0", wintypes.DWORD),                # 对齐 union 到偏移 8
                ("wVk", wintypes.WORD), ("wScan", wintypes.WORD),
                ("dwFlags", wintypes.DWORD), ("time", wintypes.DWORD),
                ("dwExtraInfo", ctypes.c_ulonglong),      # ULONG_PTR
                ("_pad1", ctypes.c_byte * 8)]             # 补齐到 40(MOUSEINPUT 大小)


def _make_kinput(scan, flags):
    i = INPUT()
    i.type = 1  # INPUT_KEYBOARD
    i.wVk = 0
    i.wScan = scan
    i.dwFlags = flags
    i.time = 0
    i.dwExtraInfo = 0
    return i


def send_unicode(text):
    u = ctypes.windll.user32
    KEYEVENTF_UNICODE, KEYEVENTF_KEYUP = 0x0004, 0x0002
    arr = []
    for ch in text:
        arr.append(_make_kinput(ord(ch), KEYEVENTF_UNICODE))
        arr.append(_make_kinput(ord(ch), KEYEVENTF_UNICODE | KEYEVENTF_KEYUP))
    n = len(arr)
    bufsz = ctypes.sizeof(INPUT)
    sent = u.SendInput(n, (INPUT * n)(*arr), bufsz)
    if sent != n:
        err = ctypes.windll.kernel32.GetLastError()
        log(f"[send][ERR] unicode {text!r} sent={sent}/{n} sizeof={bufsz} err={err}")
    else:
        log(f"[send] unicode {text!r} events={n} sent={sent} sizeof={bufsz}")


def _set_no_activate(win):
    """给候选窗加 WS_EX_NOACTIVATE:点选候选不把前台焦点抢到自己。
    HWND 是 64 位指针,必须显式 argtypes,否则 ctypes 默认 c_int 截断。"""
    u = ctypes.windll.user32
    u.GetWindowLongW.argtypes = [wintypes.HWND, ctypes.c_int]
    u.GetWindowLongW.restype = ctypes.c_long
    u.SetWindowLongW.argtypes = [wintypes.HWND, ctypes.c_int, ctypes.c_long]
    u.SetWindowLongW.restype = ctypes.c_long
    u.SetWindowPos.argtypes = [wintypes.HWND, wintypes.HWND,
                               ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.UINT]
    try:
        hwnd = int(win.frame(), 16)  # toplevel 的外层 HWND(十六进制字符串)
    except Exception:
        hwnd = win.winfo_id()
    old = u.GetWindowLongW(hwnd, GWL_EXSTYLE)
    new = old | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW
    if new != old:
        u.SetWindowLongW(hwnd, GWL_EXSTYLE, new)
        # NOMOVE|NOSIZE|NOZORDER|NOACTIVATE|FRAMECHANGED 让样式立刻生效
        u.SetWindowPos(hwnd, 0, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0004 | 0x0010 | 0x0020)
    log(f"[win] hwnd={hwnd:#x} exstyle {old:#x} -> {new:#x}")


class KBDLLHOOKSTRUCT(ctypes.Structure):
    _fields_ = [("vkCode", wintypes.DWORD), ("scanCode", wintypes.DWORD),
                ("flags", wintypes.DWORD), ("time", wintypes.DWORD),
                ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong))]


# ---------- 输入法主程序 ----------
class PrisirShellIME:
    def __init__(self, db_path, build_index=True, no_tray=False, method="pinyin"):
        if method not in METHODS:
            raise ValueError(f"未知输入法: {method} (可选 {list(METHODS)})")
        self.method = method
        self._mcfg = METHODS[method]
        self._trigger_vk = self._mcfg["trigger"]
        self._tag = self._mcfg["tag"]
        self.engine = RustIMEEngine(db_path, build_index)
        self._root = None
        self._input = ""
        self._cands = []
        self._sel = 0
        self._active = False
        self._hook = None
        self._hook_proc = None
        self._win = None
        self._labels = []
        self._smart = ""
        self._no_tray = no_tray
        self._traywin = None
        self._status_lbl = None
        self._drag_pos = None

    def start(self):
        self._root = tk.Tk()
        self._root.withdraw()
        # Tk 回调异常兜底(候选窗/上屏里的异常默认打 stderr,可能拖垮主循环)
        self._root.report_callback_exception = lambda et, ev, tb: _crash_hook(et, ev, tb)
        self._tray()
        if not self._install_hook():
            print("[ERROR] 键盘钩子安装失败")
            return
        print(f"[OK] Prisir 灵犀·{self._mcfg['label']}已启动 (引擎加载 {self.engine.load_ms:.0f}ms)")
        print(f"     {self._trigger_name()} 切换激活;打字出候选;数字/空格选词;回车上屏原文;Esc 取消")
        self._root.mainloop()

    def _trigger_name(self):
        return "右Ctrl" if self._trigger_vk == VK_RCONTROL else "右Shift"

    def _toggle(self):
        self._active = not self._active
        log(f"[toggle] active={self._active}")
        self._set_status(self._active)
        if not self._active:
            self._hide()
            if self._input:
                self._commit()

    def _tray(self):
        # pystray 的 win32 消息循环线程与 键盘钩子/Tk 三方并发踩 GIL(PyEval_RestoreThread 崩,
        # 连右Ctrl都没按就在启动期崩),已确认是 pystray 线程所致。改为纯 Tk 托盘(与 mainloop 同线程)。
        if self._no_tray:
            return
        try:
            self._build_tk_tray()
        except Exception as e:
            log(f"[WARN] tray: {e}")

    def _build_tk_tray(self):
        # 纯 Tk 状态窗(迷你浮窗当托盘替代):显示激活态,双击退出。单线程,无 GIL 风险。
        w = tk.Toplevel(self._root)
        w.title(f"灵犀·{self._mcfg['label']}")
        w.overrideredirect(True)
        w.attributes("-topmost", True)
        w.configure(bg=C["bg"])
        self._status_lbl = tk.Label(w, text=f"{self._tag}·未激活", font=("Microsoft YaHei UI", 9),
                                    bg=C["bg"], fg=C["text_dim"], padx=8, pady=4)
        self._status_lbl.pack()
        w.geometry("+20+760")  # 屏幕左下
        w.bind("<Double-Button-1>", lambda e: self._quit())
        self._traywin = w

    def _set_status(self, active):
        if getattr(self, "_status_lbl", None):
            try:
                self._status_lbl.config(text=f"{self._tag}·已激活" if active else f"{self._tag}·未激活",
                                        fg=C["ready"] if active else C["text_dim"])
            except Exception:
                pass

    def _quit(self, *_):
        self._root.after(0, self._root.quit)

    # 架构对齐 lingxi app_debug.py(已验证稳定):
    #   钩子回调里绝不碰 Tk/引擎,只 queue.put(vk) + return 1 吞键(纯线程安全操作);
    #   主线程 _pump_keys 每 15ms 从队列取键安全执行。
    # 之前崩是我误在钩子回调里调 root.after(在错误线程上下文撞 GIL)。queue.put 无此风险。
    # 吞键后字母/数字不进目标应用 → 无需事后退格/选中清理,首字母残留问题根除。
    PUMP_MS = 15

    def _install_hook(self):
        import queue
        self._key_queue = queue.Queue()
        u = ctypes.windll.user32
        k32 = ctypes.windll.kernel32
        k32.GetModuleHandleW.restype = wintypes.HMODULE
        u.SetWindowsHookExW.restype = wintypes.HHOOK
        u.CallNextHookEx.restype = ctypes.c_long
        u.CallNextHookEx.argtypes = [wintypes.HHOOK, ctypes.c_int, wintypes.WPARAM, wintypes.LPARAM]
        HOOKPROC = ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_int, wintypes.WPARAM, wintypes.LPARAM)
        u.SetWindowsHookExW.argtypes = [ctypes.c_int, HOOKPROC, wintypes.HMODULE, wintypes.DWORD]
        hmod = k32.GetModuleHandleW(None)

        def proc(nCode, wParam, lParam):
            # 钩子回调:尽快返回,绝不抛异常、不碰 Tk/引擎/SQLite。只入队 + 吞键。
            try:
                if nCode == 0 and wParam in (WM_KEYDOWN, WM_SYSKEYDOWN):
                    kb = ctypes.cast(lParam, ctypes.POINTER(KBDLLHOOKSTRUCT)).contents
                    if kb.flags & 0x10:  # LLKHF_INJECTED:合成键(自己 SendInput 的)放行
                        return u.CallNextHookEx(self._hook, nCode, wParam, lParam)
                    vk = kb.vkCode
                    if vk == self._trigger_vk:
                        self._key_queue.put(vk)
                        return 1  # 吞掉激活键,不切窗口焦点
                    # 任何修饰键(Ctrl/Alt/Shift/Win)按下时一律放行,保组合键(Ctrl+C/V/Space 等)
                    if self._modifier_down():
                        return u.CallNextHookEx(self._hook, nCode, wParam, lParam)
                    if self._active and self._wants_key(vk):
                        self._key_queue.put(vk)
                        return 1  # 吞掉,不进目标应用
            except Exception:
                pass  # 钩子里静默
            return u.CallNextHookEx(self._hook, nCode, wParam, lParam)

        self._hook_proc = HOOKPROC(proc)
        self._hook = u.SetWindowsHookExW(WH_KEYBOARD_LL, self._hook_proc, hmod, 0)
        if not self._hook:
            log(f"[ERROR] SetWindowsHookExW: {k32.GetLastError()}")
            return False
        self._root.after(self.PUMP_MS, self._pump_keys)
        return True

    def _modifier_down(self):
        # Ctrl(0x11/0xA2/0xA3) Alt(0x12/0xA4/0xA5) Shift(0x10/0xA0/0xA1) Win(0x5B/0x5C)
        u = ctypes.windll.user32
        for vk in (0x10, 0x11, 0x12, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C):
            if u.GetAsyncKeyState(vk) & 0x8000:
                return True
        return False

    def _win_down(self):
        """Win 键(0x5B/0x5C)是否按住 — 单独非缓存检查。

        Bug3 根因:激活态下 Win+D 的 'd' 是真实键,_wants_key 会吞掉它,
        而 _modifier_down() 在钩子回调里有竞态(Win 键 state 可能尚未置位),
        导致 D 被吞、Win+D 切桌面失效。这里对 Win 键做一次直接 GetAsyncKeyState,
        在 _wants_key 里优先放行所有 Win 组合键。"""
        u = ctypes.windll.user32
        return bool(u.GetAsyncKeyState(0x5B) & 0x8000 or u.GetAsyncKeyState(0x5C) & 0x8000)

    def _wants_key(self, vk):
        # Win 组合键(Win+D/E/R/L…)一律放行,绝不吞字母 → 系统快捷键不被吃掉
        if self._win_down():
            return False
        # 字母始终吞(激活态就是打字)
        if 0x41 <= vk <= 0x5A:
            return True
        # 数字键:有候选才吞(选词),否则放行(正常打数字)
        if VK_0 + 1 <= vk <= VK_9:
            return bool(self._cands)
        # 空格:有候选才吞(上屏首选),否则放行(正常空格)
        if vk == VK_SPACE:
            return bool(self._cands)
        # 回车/退格/Esc:有输入才吞,否则放行
        if vk in (VK_RETURN, VK_BACK, VK_ESCAPE):
            return bool(self._input)
        return False

    def _pump_keys(self):
        # 主线程泵:从队列取钩子投递的键,在主线程安全执行(可碰 Tk)
        import queue as _q
        try:
            while True:
                vk = self._key_queue.get_nowait()
                self._on_press(vk)
        except _q.Empty:
            pass
        except Exception as e:
            log(f"[pump ERROR] {e}")
        self._root.after(self.PUMP_MS, self._pump_keys)

    def _on_press(self, vk):
        # 主线程(泵回调),可直接操作 Tk
        if vk == self._trigger_vk:
            self._toggle()
            return
        if not self._active:
            return
        if 0x41 <= vk <= 0x5A:
            self._append(chr(vk).lower())
        elif VK_0 + 1 <= vk <= VK_9:
            self._pick(vk - VK_0 - 1)
        elif vk == VK_SPACE:
            self._pick(0)
        elif vk == VK_RETURN:
            self._commit_raw()
        elif vk == VK_BACK:
            self._backspace()
        elif vk == VK_ESCAPE:
            self._cancel()

    # ---- 主线程状态操作 ----
    def _append(self, ch):
        self._input += ch
        self._refresh()

    def _backspace(self):
        if self._input:
            self._input = self._input[:-1]
            self._refresh()

    def _cancel(self):
        self._input, self._cands = "", []
        self._hide()

    def _commit_raw(self):
        # 上屏原拼音(回车)。吞键模式字母未进屏,直接发
        if self._input:
            send_unicode(self._input)
        self._cancel()

    def _pick(self, idx):
        if 0 <= idx < len(self._cands):
            word = self._cands[idx]
            inp = self._input
            # 学习写库放后台线程:不挡上屏(五笔编码非拼音,跳过学习避免污染词频)
            if self.method != "wubi":
                threading.Thread(target=self._learn_safe, args=(inp, word), daemon=True).start()
            # 吞键模式:字母未进屏,直接发中文上屏
            send_unicode(word)
            self._cancel()

    def _learn_safe(self, inp, word):
        try:
            self.engine.learn(inp, word)
        except Exception as e:
            log(f"[WARN] learn: {e}")

    def _commit(self):
        if self._cands:
            self._pick(self._sel)
        else:
            self._commit_raw()

    def _refresh(self):
        if not self._input:
            self._cands = []
            self._hide()
            return
        t0 = time.perf_counter()
        if self.method == "wubi":
            rows = self.engine.query_wubi(self._input)
            self._smart = ""  # 五笔无整句智能
        else:
            rows = self.engine.query(self._input)
            # 整句首选:输入含 >=2 音节时给出(用 Rust smart_sentence,空串=无路径)
            self._smart = self.engine.smart_sentence(self._input) if len(self._input) >= 3 else ""
        qms = (time.perf_counter() - t0) * 1000
        self._cands = [r["word"] for r in rows[:9]]
        # 整句首选置顶(若与候选不同)
        if self._smart and self._smart not in self._cands:
            self._cands = [self._smart] + self._cands[:8]
        self._sel = 0
        log(f"[query] {self._input!r} -> {len(self._cands)} cands {qms:.1f}ms smart={self._smart!r}")
        self._show()

    def _show(self):
        if not self._input or not self._cands:
            self._hide()
            return
        if self._win is None:
            self._win = tk.Toplevel(self._root)
            self._win.overrideredirect(True)
            self._win.attributes("-topmost", True)
            self._win.configure(bg=C["bg"])
            self._py = tk.Label(self._win, text="", font=("Microsoft YaHei UI", 10),
                                bg=C["bg2"], fg=C["gold"], anchor="w", padx=8, pady=1)
            self._py.pack(side=tk.TOP, fill=tk.X)
            # 拼音行可拖动整个候选窗
            self._py.bind("<Button-1>", self._drag_start)
            self._py.bind("<B1-Motion>", self._drag_move)
            self._py.config(cursor="fleur")
            row = tk.Frame(self._win, bg=C["bg"])
            row.pack(side=tk.TOP, fill=tk.X)
            self._labels = []
            for i in range(9):
                lbl = tk.Label(row, text="", font=("Microsoft YaHei UI", 13),
                               bg=C["bg"], fg=C["text_dim"], anchor="w", padx=6, pady=3)
                lbl.pack(side=tk.LEFT)
                # 鼠标点选:点击第 i 个候选直接上屏。候选窗带 NOACTIVATE,点击不抢焦点。
                lbl.bind("<Button-1>", lambda e, i=i: self._pick(i))
                lbl.bind("<Enter>", lambda e, i=i: self._hover(i, True))
                lbl.bind("<Leave>", lambda e, i=i: self._hover(i, False))
                lbl.config(cursor="hand2")
                self._labels.append(lbl)
            # 窗口先映射才能拿到有效 HWND,再加 NOACTIVATE 扩展样式
            self._win.update_idletasks()
            _set_no_activate(self._win)
        self._py.config(text=self._input + ("  ⇒ " + self._smart if self._smart else ""))
        for i, lbl in enumerate(self._labels):
            if i < len(self._cands):
                cur = i == self._sel
                lbl.config(text=(f"{i+1}.{self._cands[i]}" if cur else f"{i+1} {self._cands[i]}"),
                           fg=C["gold"] if cur else C["text_dim"])
            else:
                lbl.config(text="")
        # 位置:用户拖动过就保持(_drag_pos),否则跟随光标
        if getattr(self, "_drag_pos", None):
            x, y = self._drag_pos
        else:
            x, y = caret_position()
            y += 6
        self._win.geometry(f"+{x}+{y}")
        self._win.deiconify()

    # 候选窗拖拽:按住拼音行拖动,记住位置
    def _drag_start(self, e):
        self._drag_ox, self._drag_oy = e.x, e.y

    def _hover(self, i, on):
        # 鼠标悬停高亮(不改动 _sel,避免与键盘数字选词状态互相干扰)
        if on and i < len(self._cands):
            self._labels[i].config(fg=C["gold"])
        elif i < len(self._labels):
            cur = i == self._sel
            self._labels[i].config(fg=C["gold"] if cur else C["text_dim"])

    def _drag_move(self, e):
        x = self._win.winfo_x() + e.x - self._drag_ox
        y = self._win.winfo_y() + e.y - self._drag_oy
        self._win.geometry(f"+{x}+{y}")
        self._drag_pos = (x, y)

    def _hide(self):
        if self._win:
            self._win.withdraw()


def main():
    db = DEFAULT_DB
    build_index = True
    no_tray = False
    method = "pinyin"
    args = sys.argv[1:]
    if "--no-index" in args:
        build_index = False
    if "--no-tray" in args:
        no_tray = True
    if "--db" in args:
        db = args[args.index("--db") + 1]
    if "--method" in args:
        method = args[args.index("--method") + 1]
    PrisirShellIME(db, build_index, no_tray, method).start()


if __name__ == "__main__":
    main()
