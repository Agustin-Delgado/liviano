"""Builds Liviano's icon from its sources.

Sources (all in assets/logo/):
  liviano.svg      the mark; drawn as-is for 40, 48, 64 and 256 px.
  pixels/NN.txt    the hand-drawn 16, 20, 24 and 32 px versions, where the
                   scaled SVG goes soft.

Outputs:
  assets/logo/liviano.ico   16, 20, 24, 32, 40, 48, 64 (BMP) and 256 (PNG)
  docs/logo/*.png           previews

Run from anywhere with the standard Python and Pillow, isolated:
  python -I tools/logo.py

The SVG reader understands only what liviano.svg uses: <rect> with rx,
<path> with absolute M/L/C/Z, and a round-capped <line>. Anything else
fails loudly instead of being drawn wrong.
"""

import io
import math
import re
import struct
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
LOGO = ROOT / "assets" / "logo"
DOCS = ROOT / "docs" / "logo"
SVG_NS = "{http://www.w3.org/2000/svg}"

ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
HAND_DRAWN = [16, 20, 24, 32]
SUPERSAMPLE = 16

# Taskbar backgrounds of Windows 11, light and dark.
LIGHT_BG = (243, 243, 243, 255)
DARK_BG = (32, 32, 32, 255)


def hex_color(value):
    value = value.lstrip("#")
    if len(value) != 6:
        raise ValueError(f"unsupported colour {value!r}")
    return tuple(int(value[i : i + 2], 16) for i in (0, 2, 4)) + (255,)


def cubic(p0, p1, p2, p3, steps=64):
    out = []
    for i in range(1, steps + 1):
        t = i / steps
        u = 1 - t
        out.append(
            (
                u**3 * p0[0] + 3 * u * u * t * p1[0] + 3 * u * t * t * p2[0] + t**3 * p3[0],
                u**3 * p0[1] + 3 * u * u * t * p1[1] + 3 * u * t * t * p2[1] + t**3 * p3[1],
            )
        )
    return out


def path_polygons(d):
    tokens = re.findall(r"[MLCZ]|-?\d+(?:\.\d+)?", d)
    polys, current, i = [], [], 0

    def nums(n):
        nonlocal i
        vals = [float(t) for t in tokens[i : i + n]]
        i += n
        return vals

    while i < len(tokens):
        cmd = tokens[i]
        i += 1
        if cmd == "M":
            if current:
                polys.append(current)
            current = [tuple(nums(2))]
        elif cmd == "L":
            current.append(tuple(nums(2)))
        elif cmd == "C":
            x1, y1, x2, y2, x, y = nums(6)
            current.extend(cubic(current[-1], (x1, y1), (x2, y2), (x, y)))
        elif cmd == "Z":
            polys.append(current)
            current = []
        else:
            raise ValueError(f"unsupported path command {cmd!r}")
    if current:
        polys.append(current)
    return polys


def capsule(x1, y1, x2, y2, width, steps=32):
    r = width / 2
    angle = math.atan2(y2 - y1, x2 - x1)
    pts = []
    for cx, cy, start in ((x2, y2, angle - math.pi / 2), (x1, y1, angle + math.pi / 2)):
        for k in range(steps + 1):
            a = start + math.pi * k / steps
            pts.append((cx + r * math.cos(a), cy + r * math.sin(a)))
    return pts


def read_svg(path, mono=None):
    """Returns (viewbox size, [(colour, polygon | ('rrect', box, radius))])."""
    root = ET.parse(path).getroot()
    _, _, vw, vh = (float(v) for v in root.get("viewBox").split())
    shapes = []
    for el in root:
        tag = el.tag.replace(SVG_NS, "")
        if tag == "title":
            continue
        if tag == "rect":
            x, y, w, h = (float(el.get(k)) for k in ("x", "y", "width", "height"))
            shapes.append((hex_color(el.get("fill")), ("rrect", (x, y, x + w, y + h), float(el.get("rx", 0)))))
        elif tag == "path":
            for poly in path_polygons(el.get("d")):
                shapes.append((hex_color(el.get("fill")), poly))
        elif tag == "line":
            if el.get("stroke-linecap") != "round":
                raise ValueError("only round-capped lines are supported")
            coords = [float(el.get(k)) for k in ("x1", "y1", "x2", "y2")]
            shapes.append((hex_color(el.get("stroke")), capsule(*coords, float(el.get("stroke-width")))))
        else:
            raise ValueError(f"unsupported SVG element <{tag}>")
    return vw, shapes


def rasterize(svg, size):
    vw, shapes = read_svg(svg)
    big = size * SUPERSAMPLE
    s = big / vw
    im = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw = ImageDraw.Draw(im)
    for colour, shape in shapes:
        if shape[0] == "rrect":
            (x0, y0, x1, y1), r = shape[1], shape[2]
            draw.rounded_rectangle([x0 * s, y0 * s, x1 * s - 1, y1 * s - 1], radius=r * s, fill=colour)
        else:
            draw.polygon([(x * s, y * s) for x, y in shape], fill=colour)
    return downsample(im, size)


def downsample(im, size):
    # A box filter is exact coverage; Pillow premultiplies RGBA while resizing,
    # so transparent edges don't darken.
    return im.resize((size, size), Image.Resampling.BOX)


# --- hand-drawn sizes ---------------------------------------------------------

def read_pixels(size, palette):
    path = LOGO / "pixels" / f"{size}.txt"
    rows = [line.rstrip("\n") for line in path.read_text(encoding="utf-8").splitlines() if not line.startswith("#")]
    rows = [r for r in rows if r.strip()]
    if len(rows) != size or any(len(r) != size for r in rows):
        raise ValueError(f"{path} must be {size}x{size}")
    im = Image.new("RGBA", (size, size))
    for y, row in enumerate(rows):
        for x, ch in enumerate(row):
            if ch not in palette:
                raise ValueError(f"{path}:{y + 1}: unknown pixel {ch!r}")
            im.putpixel((x, y), palette[ch])
    return im


def pixel_palette(brand, ink):
    def mix(t):
        return tuple(round(brand[i] + (ink[i] - brand[i]) * t) for i in range(3)) + (255,)

    def fade(alpha):
        return brand[:3] + (alpha,)

    return {
        ".": (0, 0, 0, 0),
        # Tile corners: the brand colour at partial coverage.
        "1": fade(64),
        "2": fade(128),
        "3": fade(192),
        "B": brand,
        # The mark over the tile, from a hint of ink to full ink.
        "a": mix(0.25),
        "b": mix(0.5),
        "c": mix(0.75),
        "W": ink,
    }


# --- .ico ---------------------------------------------------------------------

def bmp_entry(im):
    size = im.width
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    pixels = bytearray()
    for y in range(size - 1, -1, -1):
        for x in range(size):
            r, g, b, a = im.getpixel((x, y))
            pixels += bytes((b, g, r, a))
    # The AND mask is ignored for 32-bit images but must be present.
    stride = ((size + 31) // 32) * 4
    return header + bytes(pixels) + bytes(stride * size)


def png_entry(im):
    buf = io.BytesIO()
    im.save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def write_ico(images, path):
    entries = [(im, png_entry(im) if im.width >= 256 else bmp_entry(im)) for im in images]
    offset = 6 + 16 * len(entries)
    out = bytearray(struct.pack("<HHH", 0, 1, len(entries)))
    for im, data in entries:
        dim = 0 if im.width >= 256 else im.width
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    for _, data in entries:
        out += data
    path.write_bytes(bytes(out))


# --- previews -----------------------------------------------------------------

def size_row(images, bg, zoom):
    gap = 16
    width = sum(im.width * zoom for im in images) + gap * (len(images) + 1)
    height = max(im.height for im in images) * zoom + 2 * gap
    sheet = Image.new("RGBA", (width, height), bg)
    x = gap
    for im in images:
        big = im.resize((im.width * zoom, im.height * zoom), Image.Resampling.NEAREST)
        sheet.alpha_composite(big, (x, height - gap - big.height))
        x += big.width + gap
    return sheet


def build():
    svg = LOGO / "liviano.svg"
    _, shapes = read_svg(svg)
    brand = shapes[0][0]
    ink = shapes[1][0]
    palette = pixel_palette(brand, ink)

    images = {}
    for size in ICO_SIZES:
        images[size] = read_pixels(size, palette) if size in HAND_DRAWN else rasterize(svg, size)
    write_ico([images[s] for s in ICO_SIZES], LOGO / "liviano.ico")

    DOCS.mkdir(parents=True, exist_ok=True)
    images[256].save(DOCS / "logo-256.png")
    small = [images[s] for s in [16, 20, 24, 32, 40, 48, 64]]
    size_row(small, LIGHT_BG, 1).save(DOCS / "tamanos-claro.png")
    size_row(small, DARK_BG, 1).save(DOCS / "tamanos-oscuro.png")
    hand = [images[s] for s in HAND_DRAWN]
    zoomed = Image.new("RGBA", (1, 1))
    light = size_row(hand, LIGHT_BG, 8)
    dark = size_row(hand, DARK_BG, 8)
    zoomed = Image.new("RGBA", (light.width, light.height + dark.height))
    zoomed.paste(light, (0, 0))
    zoomed.paste(dark, (0, light.height))
    zoomed.save(DOCS / "tamanos-chicos-ampliados.png")


def preview(out_dir):
    """Scaled SVG at every size, zoomed, to draw the pixel versions from."""
    svg = LOGO / "liviano.svg"
    out = Path(out_dir)
    for bg_name, bg in (("light", LIGHT_BG), ("dark", DARK_BG)):
        ims = [rasterize(svg, s) for s in [16, 20, 24, 32, 48]]
        size_row(ims, bg, 8).save(out / f"preview-zoom-{bg_name}.png")
        size_row([rasterize(svg, 256)] + ims, bg, 1).save(out / f"preview-{bg_name}.png")


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--preview":
        preview(sys.argv[2])
    elif len(sys.argv) == 1:
        build()
    else:
        sys.exit("usage: logo.py [--preview OUT_DIR]")
