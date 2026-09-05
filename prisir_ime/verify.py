# -*- coding: utf-8 -*-
"""IME P0 验证:Rust cdylib 经 ctypes 跑通 词库加载+候选查询+整句首选+学习,
与 Python 引擎逻辑同源对齐,并测性能(对标 Python ~2μs 目标)。"""
import ctypes, json, time, os, sys

DLL = os.path.join(os.path.dirname(__file__), "target", "release", "prisir_ime.dll")
CIKU = r"C:\Users\Administrator\voice_input\lingxi_ime\backend\ciku.db"

lib = ctypes.CDLL(DLL)
lib.prisir_ime_load.restype = ctypes.c_void_p
lib.prisir_ime_load.argtypes = [ctypes.c_char_p, ctypes.c_int]
lib.prisir_ime_query.restype = ctypes.c_void_p
lib.prisir_ime_query.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
lib.prisir_ime_smart_sentence.restype = ctypes.c_void_p
lib.prisir_ime_smart_sentence.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
lib.prisir_ime_learn.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p]
lib.prisir_ime_free_string.argtypes = [ctypes.c_void_p]
lib.prisir_ime_free.argtypes = [ctypes.c_void_p]

def _str(ptr):
    if not ptr:
        return None
    s = ctypes.cast(ptr, ctypes.c_char_p).value.decode("utf-8")
    lib.prisir_ime_free_string(ptr)
    return s

def query(h, inp):
    return json.loads(_str(lib.prisir_ime_query(h, inp.encode())) or "[]")

def smart(h, inp):
    return _str(lib.prisir_ime_smart_sentence(h, inp.encode())) or ""

print("== load (build_index=1) ==")
t0 = time.perf_counter()
h = lib.prisir_ime_load(CIKU.encode(), 1)
print(f"  handle={h}  build={time.perf_counter()-t0:.3f}s")
assert h, "load failed"

cases = ["nihao", "zhongguo", "shij", "sj", "jie", "l", "shurufa", "suan"]
print("\n== query 正确性 ==")
for c in cases:
    r = query(h, c)
    top = [(x["word"], x["weight"]) for x in r[:5]]
    print(f"  {c:10s} -> {top}")

print("\n== smart_sentence ==")
for s in ["nihaoshijie", "woshizhongguoren", "shurufa", "zhonghuarenmingongheguo"]:
    print(f"  {s:24s} -> {smart(h, s)}")

print("\n== 性能(热路径,内存索引) ==")
for c in ["nihao", "shij", "zhongguo", "jie"]:
    for _ in range(20):
        query(h, c)  # 预热
    t0 = time.perf_counter()
    N = 2000
    for _ in range(N):
        query(h, c)
    us = (time.perf_counter() - t0) / N * 1e6
    print(f"  {c:10s} {us:7.2f} us/query  (N={N})")

print("\n== smart_sentence 性能 ==")
s = "zhonghuarenmingongheguo"
for _ in range(5):
    smart(h, s)
t0 = time.perf_counter()
N = 500
for _ in range(N):
    smart(h, s)
print(f"  {s} {(time.perf_counter()-t0)/N*1e6:7.2f} us/call (N={N})")

lib.prisir_ime_free(h)
print("\nOK")
