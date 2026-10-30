"""生成 TermHarbor 应用图标源图（1024x1024 PNG）。

不依赖 Pillow，直接算像素 + zlib 打包 PNG，便于在任意机器上复现：
    python scripts/app-icon.py
然后：
    cargo tauri icon app-icon.png

图形语言：深海军蓝圆角方块 + 青绿色 ">" 提示符 + 浅蓝光标条。
"""

import math
import struct
import zlib
from pathlib import Path

SIZE = 1024
BG = (15, 42, 71)  # #0F2A47 深海军蓝
PROMPT = (93, 202, 165)  # #5DCAA5 青绿
CURSOR = (181, 212, 244)  # #B5D4F4 浅蓝

CORNER_RADIUS = 200.0
CHEVRON = [(300.0, 320.0), (500.0, 512.0), (300.0, 704.0)]
CHEVRON_WIDTH = 80.0
BAR = (560.0, 624.0, 730.0, 704.0)  # x0, y0, x1, y1
BAR_RADIUS = 40.0


def sd_round_rect(px, py, x0, y0, x1, y1, r):
    cx = (x0 + x1) / 2.0
    cy = (y0 + y1) / 2.0
    hx = (x1 - x0) / 2.0 - r
    hy = (y1 - y0) / 2.0 - r
    dx = abs(px - cx) - hx
    dy = abs(py - cy) - hy
    ox = max(dx, 0.0)
    oy = max(dy, 0.0)
    return math.hypot(ox, oy) + min(max(dx, dy), 0.0) - r


def sd_segment(px, py, ax, ay, bx, by):
    vx, vy = bx - ax, by - ay
    wx, wy = px - ax, py - ay
    denom = vx * vx + vy * vy
    t = 0.0 if denom == 0.0 else max(0.0, min(1.0, (wx * vx + wy * vy) / denom))
    return math.hypot(wx - t * vx, wy - t * vy)


def sd_polyline(px, py, points, width):
    best = float("inf")
    for (ax, ay), (bx, by) in zip(points, points[1:]):
        best = min(best, sd_segment(px, py, ax, ay, bx, by))
    return best - width / 2.0


def coverage(distance):
    """1px 抗锯齿过渡带。"""
    return max(0.0, min(1.0, 0.5 - distance))


def blend(dst, src, alpha):
    if alpha <= 0.0:
        return dst
    a = alpha
    return (
        int(src[0] * a + dst[0] * (1 - a)),
        int(src[1] * a + dst[1] * (1 - a)),
        int(src[2] * a + dst[2] * (1 - a)),
        int(255 * a + dst[3] * (1 - a)),
    )


def render():
    rows = []
    bx0, by0, bx1, by1 = BAR
    for y in range(SIZE):
        py = y + 0.5
        row = bytearray()
        for x in range(SIZE):
            px = x + 0.5
            pix = (0, 0, 0, 0)

            d_bg = sd_round_rect(px, py, 0.0, 0.0, SIZE, SIZE, CORNER_RADIUS)
            pix = blend(pix, BG + (255,), coverage(d_bg))

            if d_bg < 1.0:
                d_bar = sd_round_rect(px, py, bx0, by0, bx1, by1, BAR_RADIUS)
                if d_bar < 2.0:
                    pix = blend(pix, CURSOR + (255,), coverage(d_bar))

                d_chev = sd_polyline(px, py, CHEVRON, CHEVRON_WIDTH)
                if d_chev < 2.0:
                    pix = blend(pix, PROMPT + (255,), coverage(d_chev))

            row += bytes(pix)
        rows.append(row)
    return rows


def write_png(path, rows):
    width = SIZE
    height = len(rows)
    raw = bytearray()
    for row in rows:
        raw.append(0)  # filter type 0 (None)
        raw += row

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )
    path.write_bytes(png)


if __name__ == "__main__":
    out = Path(__file__).resolve().parent.parent / "app-icon.png"
    write_png(out, render())
    print(f"written {out} ({out.stat().st_size} bytes)")
