# -*- coding: utf-8 -*-
"""Prisir 输入法三法 · 共享热键配置层(2026-08-19)

拼音 / 五笔 / 语音 三个输入法的「激活/切法」键位集中定义在此,改键位只改这里,无需改壳代码。
照 lingxi_config.py 的原子配置风格:每个区块标注【验证状态】。

架构约定(与浏览器版对齐):
  - 同族文本输入(拼音/五笔)互斥:同时只激活一个,抢键盘会串。
  - 语音与拼音/五笔并行:语音是「按一下起、再按一下停」的瞬时录制态,
    不占持续键盘,所以语音激活期间拼音仍可待命,讲完松手直接接拼音改字。
  - 三者键位错开(默认 拼音右Ctrl / 五笔右Shift / 语音右Alt),天然不踩。
"""

# ============================================================
# 虚拟键码常量(Windows)GetAsyncKeyState / WH_KEYBOARD_LL 通用
# ============================================================
VK_LSHIFT, VK_RSHIFT = 0xA0, 0xA1
VK_LCTRL, VK_RCTRL = 0xA2, 0xA3
VK_LALT, VK_RALT = 0xA4, 0xA5
VK_SHIFT, VK_CTRL, VK_ALT = 0x10, 0x11, 0x12   # 不区分左右
VK_LWIN, VK_RWIN = 0x5B, 0x5C
VK_SPACE = 0x20
# F1..F12 = 0x70..0x7B
VK_F1, VK_F12 = 0x70, 0x7B

# 可选为激活键的键位(可自定义范围)。字母/数字/空格等打字键不允许(会吃掉正常输入)。
ALLOWED_TRIGGER_VKS = frozenset(
    {VK_LSHIFT, VK_RSHIFT, VK_LCTRL, VK_RCTRL, VK_LALT, VK_RALT}
    | set(range(VK_F1, VK_F12 + 1))
)

VK_NAME = {
    VK_LSHIFT: "左Shift", VK_RSHIFT: "右Shift",
    VK_LCTRL: "左Ctrl", VK_RCTRL: "右Ctrl",
    VK_LALT: "左Alt", VK_RALT: "右Alt",
    VK_SPACE: "Space",
}
for _i in range(12):
    VK_NAME[VK_F1 + _i] = f"F{_i + 1}"


def vk_name(vk):
    return VK_NAME.get(vk, f"VK_{vk:#x}")


# ============================================================
# 三法激活键【默认已错开 2026-08-19】
# ============================================================
# mode:
#   "toggle"  持续态:按一下激活、再按一下取消(拼音/五笔打字用这种)
#   "ptt"     瞬时态(push-to-talk):按下起录、松开停录(语音可选)
#   "toggle_record" 语音默认:按一下起录→出悬浮球→讲话→再按一下停录→识别上屏
# group:
#   同 group 的 schema 互斥(同组同时只激活一个);"voice" 独立组,与文本组并行。

SCHEMAS = {
    "pinyin": {
        "label": "拼音",
        "trigger_vk": VK_RCTRL,          # 默认 右Ctrl
        "mode": "toggle",
        "group": "text",                 # 与 wubi 同组互斥
        "enabled": True,
    },
    "wubi": {
        "label": "五笔",
        "trigger_vk": VK_RSHIFT,         # 默认 右Shift(与拼音错开)
        "mode": "toggle",
        "group": "text",
        "enabled": True,
    },
    "voice": {
        "label": "语音",
        "trigger_vk": VK_RALT,           # 默认 右Alt(语音现用键, lingxi_config.TRIGGER_KEY)
        "mode": "toggle_record",         # 按一下起录→再按一下停录→识别上屏
        "group": "voice",                # 独立组:与拼音/五笔并行,不互斥
        "enabled": True,
        # 语音悬浮球(状态灯)在激活期间显示;讲完按停后隐藏,转识别上屏
        "show_ball": True,
    },
}

# 切法键:在「启用的同组文本输入法」间轮换(拼音 <-> 五笔)。语音不参与轮换(它是并行的)。
# 默认 Ctrl+Space(须修饰键+主键组合,避免单键误触)。设为 None 关闭切法键。
CYCLE_COMBO = {"ctrl": True, "shift": False, "alt": False, "vk": 0x20}  # Ctrl+Space


# ============================================================
# 配色【深色柔和版 2026-08-19】
# ============================================================
# 浅色天蓝配色(2026-08-21 用户拍板浅色路线):白/天蓝底,深灰近黑文字,三法悬浮球/候选窗统一。
COLORS = {
    "bg": "#ffffff",          # 悬浮球/候选窗主底(纯白)
    "bg2": "#e8f4fd",         # 内圈/面板分层(天蓝底)
    "border": "#b3d7f5",      # 描边(浅蓝)
    "text": "#1f2937",        # 主文字(深灰近黑)
    "text_dim": "#5b6b7c",    # 次文字(灰蓝)
    "gold": "#c47f2a",        # 品牌暖金(浅底加深保证对比度)
    "ready": "#1e9e5a",       # 绿=就绪/已激活(浅底加深)
    "recording": "#128a4a",   # 深绿=语音录音中(浅底保证醒目)
    "busy": "#b07d18",        # 深黄=识别中/处理中(浅底保证可读)
    "error": "#c13a3a",       # 红=故障(浅底加深)
}

# 悬浮球(语音)与状态灯几何
BALL = {
    "size": 56,               # 圆球直径(px)
    "inner_scale": 0.62,      # 状态灯内圈相对直径
    "draggable": True,        # 可拖动定位(三法悬浮件都可移动)
    "remember_pos": True,     # 记住拖动后的位置
}


# ============================================================
# 校验:启动时调用,键位越界/同组撞键即抛错并指出
# ============================================================
def validate(schemas=None):
    schemas = schemas or SCHEMAS
    errors = []
    seen = {}  # vk -> schema key(同组互斥的才查撞键;voice 独立组可与 text 同键但建议错开)
    for key, cfg in schemas.items():
        if not cfg.get("enabled", True):
            continue
        vk = cfg.get("trigger_vk")
        if vk not in ALLOWED_TRIGGER_VKS:
            errors.append(f"[{cfg['label']}] trigger_vk={vk_name(vk)} 不在可自定义范围"
                          f"(仅 左右Ctrl/Shift/Alt 或 F1-F12)")
        grp = cfg.get("group")
        if grp != "voice":  # voice 并行,不参与互斥撞键判定
            sig = (grp, vk)
            if sig in seen:
                errors.append(f"[{cfg['label']}] 与 [{schemas[seen[sig]]['label']}] "
                              f"同组({grp})撞键 {vk_name(vk)},会互抢")
            else:
                seen[sig] = key
    if errors:
        raise ValueError("热键配置冲突:\n  " + "\n  ".join(errors))
    return True


def active_text_schemas():
    """同组(text)里启用的文本输入法,供切法键轮换。voice 不在内。"""
    return [k for k, c in SCHEMAS.items()
            if c.get("enabled", True) and c.get("group") == "text"]


if __name__ == "__main__":
    validate()
    print("热键配置校验通过:")
    for k, c in SCHEMAS.items():
        print(f"  {c['label']:4s} 激活键={vk_name(c['trigger_vk']):6s} "
              f"mode={c['mode']:14s} group={c['group']}")
    print(f"  切法轮换: {'+'.join(k for k in ('ctrl','shift','alt') if CYCLE_COMBO[k])}"
          f"+{vk_name(CYCLE_COMBO['vk'])}  在 {active_text_schemas()} 间切")
