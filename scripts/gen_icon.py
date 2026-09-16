#!/usr/bin/env python3
"""生成一呼（Yihu）各工具的应用图标。

用法：python3 scripts/gen_icon.py <样式> [目标目录]
  样式：
    yihu     — 渐变底 + 白色「呼」字（一呼主图标，默认 tools/yihu/icons）
    ping     — 渐变底 + 声波涟漪（备选方案）
    sysdash  — 渐变底 + 白色仪表盘（tools/sysdash/icons）
    autodark — 渐变底 + 白色月牙/太阳（tools/autodark/icons）
"""

import glob
import math
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

S = 512
RADIUS = 110
TOP = (245, 158, 11)    # 日落金 #F59E0B
BOT = (239, 68, 68)     # 日落红 #EF4444


def base_image() -> Image.Image:
    grad = Image.new("RGBA", (S, S))
    d = ImageDraw.Draw(grad)
    for y in range(S):
        t = y / (S - 1)
        color = tuple(round(a + (b - a) * t) for a, b in zip(TOP, BOT)) + (255,)
        d.line([(0, y), (S, y)], fill=color)

    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, S - 1, S - 1], radius=RADIUS, fill=255
    )
    img.paste(grad, (0, 0), mask)
    return img


def _load_cjk_black(size: int) -> ImageFont.FreeTypeFont:
    """加载 Noto Sans CJK Black，优先简体中文（SC）变体。"""
    paths = sorted(
        glob.glob("/usr/share/fonts/**/NotoSansCJK-Black.ttc", recursive=True)
    ) or sorted(glob.glob("/usr/share/fonts/**/NotoSansCJK-Bold.ttc", recursive=True))
    if not paths:
        raise RuntimeError("未找到 Noto Sans CJK 字体")
    path = paths[0]
    for idx in range(6):
        try:
            f = ImageFont.truetype(path, size, index=idx)
        except Exception:
            break
        if "SC" in f.getname()[0]:
            return f
    return ImageFont.truetype(path, size)


def draw_yihu(img: Image.Image) -> None:
    """一呼主图标：单个「呼」字，即品牌本身。"""
    d = ImageDraw.Draw(img)
    font = _load_cjk_black(int(S * 0.65))
    d.text((S / 2, S / 2 + S * 0.01), "呼", font=font,
           fill=(255, 255, 255, 255), anchor="mm")


def draw_ping(img: Image.Image) -> None:
    """声波涟漪：一点呼出，声浪扩散（一呼……百应）。"""
    layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    cx, cy, r = 190, 330, 62
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(255, 255, 255, 255))
    for rr, w in [(158, 34), (246, 34), (334, 34)]:
        d.arc([cx - rr, cy - rr, cx + rr, cy + rr], start=-68, end=18,
              fill=(255, 255, 255, 255), width=w)
    img.alpha_composite(layer)


def draw_gauge(img: Image.Image) -> None:
    d = ImageDraw.Draw(img)
    cx = cy = S / 2
    r, lw = 150, 46
    d.arc([cx - r, cy - r, cx + r, cy + r], start=135, end=405,
          fill=(255, 255, 255, 255), width=lw)
    ang = math.radians(315)
    length = r - lw - 18
    d.line([cx, cy, cx + length * math.cos(ang), cy + length * math.sin(ang)],
           fill=(255, 255, 255, 255), width=26)
    hub = 36
    d.ellipse([cx - hub, cy - hub, cx + hub, cy + hub],
              fill=(255, 255, 255, 255))


def draw_daynight(img: Image.Image) -> None:
    layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    d.ellipse([110, 130, 350, 370], fill=(255, 255, 255, 255))
    d.ellipse([190, 80, 430, 320], fill=(0, 0, 0, 0))
    sx, sy, sr = 355, 145, 52
    d.ellipse([sx - sr, sy - sr, sx + sr, sy + sr], fill=(255, 255, 255, 255))
    for ang in range(0, 360, 45):
        a = math.radians(ang)
        r1, r2 = sr + 22, sr + 52
        d.line([sx + r1 * math.cos(a), sy + r1 * math.sin(a),
                sx + r2 * math.cos(a), sy + r2 * math.sin(a)],
               fill=(255, 255, 255, 255), width=16)
    img.alpha_composite(layer)


STYLES = {
    "yihu": (draw_yihu, "tools/yihu/icons"),
    "ping": (draw_ping, "tools/yihu/icons"),
    "sysdash": (draw_gauge, "tools/sysdash/icons"),
    "autodark": (draw_daynight, "tools/autodark/icons"),
}


def main() -> None:
    if len(sys.argv) < 2 or sys.argv[1] not in STYLES:
        prog = Path(sys.argv[0]).name
        sys.exit(f"用法: python3 {prog} <{'|'.join(STYLES)}> [目标目录]")
    style = sys.argv[1]
    default_dir = Path(STYLES[style][1])
    icons_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_dir
    icons_dir.mkdir(parents=True, exist_ok=True)

    img = base_image()
    STYLES[style][0](img)

    for size in (32, 128, 256, 512):
        img.resize((size, size), Image.LANCZOS).save(icons_dir / f"{size}x{size}.png")

    print(f"图标[{style}] 已生成到 {icons_dir}")


if __name__ == "__main__":
    main()
