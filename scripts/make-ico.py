"""重新打包 src-tauri/icons/icon.ico，产出 Windows 认得全的图标。

为什么要单独一个脚本
--------------------
`cargo tauri icon app-icon.png` 生成的 ICO 里，**每个尺寸都是 PNG 压缩的**。
而 Windows 只保证支持「256x256 用 PNG」，小尺寸条目必须是 BMP(DIB)：

- 资源管理器按大图标渲染时还能凑合，所以桌面快捷方式看起来是正常的；
- 任务栏按 32x32（或 24/48）取图时会失败，退回系统那张默认的「空白窗口」图。

本脚本用 PIL 把 icons/icon.png 重采样成 16/24/32/48/64/128 的 32bpp DIB，
256 保留 PNG（官方唯一支持的 PNG 尺寸），再按 ICO 容器格式拼起来。
不依赖 PIL 的 ICO 写入器：它要么全 PNG，要么把 256 也写成 DIB（多 264 KB）。

用法：
    python scripts/make-ico.py
校验（会打印每个条目是 PNG 还是 DIB）：
    python scripts/make-ico.py --check
"""

from __future__ import annotations

import struct
import sys
from io import BytesIO
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src-tauri" / "icons" / "icon.png"
OUT = ROOT / "src-tauri" / "icons" / "icon.ico"

# 小尺寸走 DIB，256 走 PNG（Windows 在这一个尺寸上支持 PNG 压缩）
DIB_SIZES = (16, 24, 32, 48, 64, 128)
PNG_SIZE = 256


def dib_bytes(image: Image.Image) -> bytes:
    """32bpp 图 → ICO 用的 DIB（BITMAPINFOHEADER + 自下而上的 BGRA + AND 掩码）。"""
    size = image.width
    pixels = image.convert("RGBA").tobytes()  # 逐像素 RGBA，行优先、自上而下

    header = struct.pack(
        "<IiiHHIIiiII",
        40,  # biSize
        size,  # biWidth
        size * 2,  # biHeight：DIB 高度要算上 AND 掩码那一半
        1,  # biPlanes
        32,  # biBitCount
        0,  # biCompression = BI_RGB
        0,  # biSizeImage
        0,  # biXPelsPerMeter
        0,  # biYPelsPerMeter
        0,  # biClrUsed
        0,  # biClrImportant
    )

    xor = bytearray()
    for y in range(size - 1, -1, -1):  # DIB 是自下而上
        row = pixels[y * size * 4 : (y + 1) * size * 4]
        for x in range(size):
            r, g, b, a = row[x * 4 : x * 4 + 4]
            xor += bytes((b, g, r, a))  # BGRA

    # 32bpp 用 alpha 通道即可，掩码全 0；但仍要占位，否则有些老组件算错步长
    stride = ((size + 31) // 32) * 4
    and_mask = bytes(stride * size)

    return header + bytes(xor) + and_mask


def png_bytes(image: Image.Image) -> bytes:
    buf = BytesIO()
    image.save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def render(images: dict[int, bytes]) -> bytes:
    entries = []
    offset = 6 + 16 * len(images)
    for size in sorted(images):
        blob = images[size]
        entries.append(
            struct.pack(
                "<BBBBHHII",
                size if size < 256 else 0,
                size if size < 256 else 0,
                0,  # 调色板颜色数：真彩图为 0
                0,  # 保留
                1,  # 平面数
                32,  # 位深
                len(blob),
                offset,
            )
        )
        offset += len(blob)
    head = struct.pack("<HHH", 0, 1, len(images))
    return head + b"".join(entries) + b"".join(images[size] for size in sorted(images))


def check(path: Path) -> int:
    data = path.read_bytes()
    _, kind, count = struct.unpack_from("<HHH", data, 0)
    print(f"{path.name}: type={kind} entries={count} size={len(data)}")
    bad = 0
    for i in range(count):
        w, h, _, _, _, bpp, size, off = struct.unpack_from("<BBBBHHII", data, 6 + i * 16)
        pixel = w or 256
        blob = data[off : off + size]
        png = blob[:4] == b"\x89PNG"
        # PNG 只允许出现在 256；其余必须能读出 BITMAPINFOHEADER 的 biSize=40
        ok = (png and pixel == 256) or (not png and struct.unpack_from("<I", blob, 0)[0] == 40)
        if not ok:
            bad += 1
        print(
            f"  {pixel}x{pixel} bpp={bpp} bytes={size} "
            f"{'PNG' if png else 'DIB'} {'ok' if ok else 'BAD'}"
        )
    return bad


def main() -> None:
    if "--check" in sys.argv:
        sys.exit(1 if check(OUT) else 0)

    master = Image.open(SRC).convert("RGBA")
    if master.width != master.height:
        raise SystemExit(f"{SRC} 不是正方形：{master.size}")

    images: dict[int, bytes] = {}
    for size in DIB_SIZES:
        if size > master.width:
            raise SystemExit(f"源图只有 {master.width}px，做不出 {size}px")
        images[size] = dib_bytes(master.resize((size, size), Image.LANCZOS))
    if PNG_SIZE <= master.width:
        images[PNG_SIZE] = png_bytes(master.resize((PNG_SIZE, PNG_SIZE), Image.LANCZOS))

    OUT.write_bytes(render(images))
    print(f"written {OUT} ({OUT.stat().st_size} bytes)")
    check(OUT)


if __name__ == "__main__":
    main()
