"""Builds Nea's icon. This file is the source: nothing is drawn by hand.

The feather is made from a few numbers: a curved shaft, the width of each
vane along it, the angle of the barbs and the places where the barbs part.
The same numbers give the SVG files and every size of the .ico, so the two
can never differ.

Outputs:
  assets/logo/nea.svg        the icon, light: a white feather on graphite
  assets/logo/nea-dark.svg   the icon, dark: a silver feather on near-black
  assets/logo/nea-mono.svg   the feather alone, in currentColor
  assets/logo/nea.ico        16, 20, 24, 32, 40, 48, 64 (BMP) and 256 (PNG)
  docs/logo/*.png                previews

Below 48 px the fine parts would turn to grey mud, so the small sizes are
drawn from a plainer feather (see DETAIL): larger in the tile, no barbs,
one deep split, a thicker shaft.

Run from anywhere with the standard Python and Pillow, isolated:
  python -I tools/logo.py
"""

import io
import math
import struct
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
LOGO = ROOT / "assets" / "logo"
DOCS = ROOT / "docs" / "logo"

ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]

# Taskbar backgrounds of Windows 11, light and dark.
LIGHT_BG = (243, 243, 243, 255)
DARK_BG = (32, 32, 32, 255)

# The tile, in the 256-unit space every shape is described in.
VIEW = 256
TILE_BOX = (8, 8, 248, 248)
TILE_RADIUS = 56
CENTRE = (128, 128)


# --- colours ------------------------------------------------------------------

def rgb(value):
    value = value.lstrip("#")
    return tuple(int(value[i : i + 2], 16) for i in (0, 2, 4))


def mix(a, b, t):
    """A colour between two others: 0 gives the first, 1 the second."""
    return "#" + "".join(f"{round(x + (y - x) * t):02X}" for x, y in zip(rgb(a), rgb(b)))


GRAPHITE = ("#51515C", "#27272F")

# Each look: the tile from top to bottom, the two vanes from quill to tip,
# the shaft, the barbs (colour, opacity) and the shadow (colour, opacity).
LIGHT = {
    "tile": GRAPHITE,
    "lit": ("#FFFFFF", "#FFFFFF"),
    "shade": (mix("#FFFFFF", GRAPHITE[0], 0.26), mix("#FFFFFF", GRAPHITE[0], 0.07)),
    "shaft": "#FFFFFF",
    "barbs": (mix(GRAPHITE[1], "#000000", 0.15), 0.16),
    "shadow": (mix(GRAPHITE[1], "#000000", 0.45), 0.4),
}
DARK = {
    "tile": (mix("#25252D", GRAPHITE[1], 0.1), mix("#121217", GRAPHITE[1], 0.06)),
    "lit": ("#FFFFFF", "#F1F1F4"),
    "shade": ("#A9A9B4", "#D5D5DC"),
    "shaft": "#FFFFFF",
    "barbs": ("#121217", 0.22),
    "shadow": ("#000000", 0.5),
}


# --- the feather --------------------------------------------------------------

# `from`, `bend` and `to` are the shaft, a quadratic curve from the quill to
# the tip. `left` and `right` are the widest each vane gets. `splits` are
# where the barbs part: side (-1 left, 1 right), where along the vane, how
# long the gap takes to close, and how deep it goes.
FULL = {
    "from": (58, 204),
    "bend": (104, 132),
    "to": (200, 50),
    "left": 30,
    "right": 44,
    "sweep": 0.55,
    "bare": 0.17,
    "peak": 0.75,
    "shaft": 3.4,
    "splits": [(1, 0.3, 0.16, 0.5), (1, 0.6, 0.12, 0.38), (-1, 0.5, 0.12, 0.42)],
    "barbs": True,
    "scale": 1.0,
    "shadow": 1.0,
}

# What changes as the icon gets smaller, keyed by the largest size it holds for.
DETAIL = [
    (20, {"scale": 1.2, "shaft": 6.5, "barbs": False, "shadow": 0.0, "left": 32, "right": 46,
          "splits": [(1, 0.34, 0.26, 0.62)]}),
    (32, {"scale": 1.14, "shaft": 5.2, "barbs": False, "shadow": 0.5, "left": 31, "right": 45,
          "splits": [(1, 0.32, 0.22, 0.58), (-1, 0.52, 0.16, 0.5)]}),
    (64, {"scale": 1.06, "shaft": 4.0, "barbs": False}),
]


def shape_for(size):
    for largest, changes in DETAIL:
        if size <= largest:
            return {**FULL, **changes}
    return FULL


def feather(shape):
    """The polygons of the feather, in the 256-unit space."""
    p0, p1, p2 = shape["from"], shape["bend"], shape["to"]
    bare, sweep, peak = shape["bare"], shape["sweep"], shape["peak"]

    def at(t):
        return tuple((1 - t) ** 2 * p0[k] + 2 * (1 - t) * t * p1[k] + t * t * p2[k] for k in (0, 1))

    def along(t):
        d = [2 * (1 - t) * (p1[k] - p0[k]) + 2 * t * (p2[k] - p1[k]) for k in (0, 1)]
        length = math.hypot(*d)
        return d[0] / length, d[1] / length

    def width(s, most, side):
        # Nothing at the quill, most a little before the middle, a point at the tip.
        w = most * max(0.0, math.sin(math.pi * s**peak)) ** 0.72
        for split_side, where, length, depth in shape["splits"]:
            x = (s - where) / length
            # Where the barbs part, the edge drops at once and comes back slowly.
            if split_side == side and 0 <= x <= 1:
                w *= 1 - depth * (1 - x) ** 1.5
        return w

    def edge(s, most, side):
        t = bare + s * (1 - bare)
        x, y = at(t)
        tx, ty = along(t)
        w = width(s, most, side)
        return x - ty * side * w + tx * w * sweep, y + tx * side * w + ty * w * sweep

    steps = 240

    def vane(most, side):
        spine = [at(bare + i / steps * (1 - bare)) for i in range(steps + 1)]
        rim = [edge(i / steps, most, side) for i in range(steps + 1)]
        return spine + rim[::-1]

    def barbs(most, side):
        lines = []
        s = 0.05
        while s < 0.97:
            x, y = at(bare + s * (1 - bare))
            ex, ey = edge(s, most, side)
            lines.append(((x, y), (x + (ex - x) * 0.93, y + (ey - y) * 0.93)))
            s += 0.03
        return lines

    # The shaft: thick at the quill, a hair at the tip.
    near, far = [], []
    for i in range(81):
        t = i / 80
        x, y = at(t)
        tx, ty = along(t)
        half = shape["shaft"] * (1 - t) ** 0.8 + 0.35
        near.append((x - ty * half, y + tx * half))
        far.append((x + ty * half, y - tx * half))

    parts = {
        "left": vane(shape["left"], -1),
        "right": vane(shape["right"], 1),
        "shaft": near + far[::-1],
        "cap": (p0, shape["shaft"] + 0.35),
        "barbs": barbs(shape["left"], -1) + barbs(shape["right"], 1) if shape["barbs"] else [],
        "axis": (p0, p2),
    }
    return scaled(parts, shape["scale"])


def scaled(parts, k):
    """The feather grown about the centre of the tile."""
    def move(point):
        return CENTRE[0] + (point[0] - CENTRE[0]) * k, CENTRE[1] + (point[1] - CENTRE[1]) * k

    return {
        "left": [move(p) for p in parts["left"]],
        "right": [move(p) for p in parts["right"]],
        "shaft": [move(p) for p in parts["shaft"]],
        "cap": (move(parts["cap"][0]), parts["cap"][1] * k),
        "barbs": [(move(a), move(b)) for a, b in parts["barbs"]],
        "axis": (move(parts["axis"][0]), move(parts["axis"][1])),
    }


# --- raster -------------------------------------------------------------------

def ramp(big, start, end, first, last):
    """An image that goes from `first` at `start` to `last` at `end`, held beyond both."""
    # A strip that is 0, then a ramp, then 255; the transform lays it along the axis.
    strip = Image.new("L", (1, 768))
    strip.putdata([0] * 256 + list(range(256)) + [255] * 256)
    dx, dy = end[0] - start[0], end[1] - start[1]
    span = dx * dx + dy * dy
    row = (255 * dx / span, 255 * dy / span, 256 - 255 * (start[0] * dx + start[1] * dy) / span)
    t = strip.transform((big, big), Image.Transform.AFFINE, (0, 0, 0) + row, Image.Resampling.BILINEAR)
    return Image.composite(
        Image.new("RGBA", (big, big), rgb(last) + (255,)),
        Image.new("RGBA", (big, big), rgb(first) + (255,)),
        t,
    )


def mask_of(big, draw_shape):
    mask = Image.new("L", (big, big), 0)
    draw_shape(ImageDraw.Draw(mask))
    return mask


def render(size, look):
    shape = shape_for(size)
    parts = feather(shape)
    supersample = 16 if size <= 64 else 8
    big = size * supersample
    s = big / VIEW

    def pts(points):
        return [(x * s, y * s) for x, y in points]

    box = [v * s for v in TILE_BOX]
    box[2] -= 1
    box[3] -= 1
    tile_mask = mask_of(big, lambda d: d.rounded_rectangle(box, radius=TILE_RADIUS * s, fill=255))

    out = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    out.paste(ramp(big, (0, 8 * s), (0, 248 * s), *look["tile"]), (0, 0), tile_mask)

    # A line of light on the top edge of the tile, gone by its middle.
    rim = mask_of(big, lambda d: d.rounded_rectangle(box, radius=TILE_RADIUS * s, outline=255, width=max(1, round(s))))
    fade = ramp(big, (0, 8 * s), (0, 128 * s), "#383838", "#000000").convert("L")
    out.paste(Image.new("RGBA", (big, big), (255, 255, 255, 255)), (0, 0), ImageChops.multiply(rim, fade))

    start, end = pts(parts["axis"])
    plume = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    for side, tone in (("left", "lit"), ("right", "shade")):
        vane = mask_of(big, lambda d, side=side: d.polygon(pts(parts[side]), fill=255))
        plume.paste(ramp(big, start, end, *look[tone]), (0, 0), vane)
    if parts["barbs"]:
        colour, opacity = look["barbs"]
        lines = Image.new("RGBA", (big, big), (0, 0, 0, 0))
        draw = ImageDraw.Draw(lines)
        for a, b in parts["barbs"]:
            draw.line(pts([a, b]), fill=rgb(colour) + (round(255 * opacity),), width=max(1, round(0.9 * s)))
        lines.putalpha(ImageChops.multiply(lines.getchannel("A"), plume.getchannel("A")))
        plume.alpha_composite(lines)
    draw = ImageDraw.Draw(plume)
    draw.polygon(pts(parts["shaft"]), fill=rgb(look["shaft"]) + (255,))
    (cx, cy), r = parts["cap"]
    draw.ellipse([(cx - r) * s, (cy - r) * s, (cx + r) * s, (cy + r) * s], fill=rgb(look["shaft"]) + (255,))

    colour, opacity = look["shadow"]
    opacity *= shape["shadow"]
    if opacity:
        cast = ImageChops.offset(plume.getchannel("A"), 0, round(5 * s)).filter(ImageFilter.GaussianBlur(6 * s))
        cast = ImageChops.multiply(cast.point(lambda v: round(v * opacity)), tile_mask)
        out.alpha_composite(Image.merge("RGBA", [Image.new("L", (big, big), c) for c in rgb(colour)] + [cast]))
    out.alpha_composite(plume)

    # A box filter is exact coverage; Pillow premultiplies RGBA while resizing,
    # so transparent edges don't darken.
    return out.resize((size, size), Image.Resampling.BOX)


# --- SVG ----------------------------------------------------------------------

def path(points):
    return "M" + "L".join(f"{x:.2f} {y:.2f}" for x, y in points) + "Z"


TILE_PATH = "M64 8H192A56 56 0 0 1 248 64V192A56 56 0 0 1 192 248H64A56 56 0 0 1 8 192V64A56 56 0 0 1 64 8Z"


def svg(look):
    parts = feather(FULL)
    (x1, y1), (x2, y2) = parts["axis"]
    run = f'gradientUnits="userSpaceOnUse" x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}"'
    barbs = "".join(f"M{a[0]:.2f} {a[1]:.2f}L{b[0]:.2f} {b[1]:.2f}" for a, b in parts["barbs"])
    (cx, cy), r = parts["cap"]

    def stops(pair):
        return f'<stop offset="0" stop-color="{pair[0]}"/><stop offset="1" stop-color="{pair[1]}"/>'

    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">
  <title>Nea</title>
  <!-- Written by tools/logo.py. Change the numbers there, not this file. -->
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">{stops(look["tile"])}</linearGradient>
    <linearGradient id="lit" {run}>{stops(look["lit"])}</linearGradient>
    <linearGradient id="shade" {run}>{stops(look["shade"])}</linearGradient>
    <linearGradient id="rim" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity="0.22"/><stop offset="0.5" stop-color="#fff" stop-opacity="0"/></linearGradient>
    <filter id="cast" x="-30%" y="-30%" width="160%" height="160%"><feDropShadow dx="0" dy="5" stdDeviation="6" flood-color="{look["shadow"][0]}" flood-opacity="{look["shadow"][1]}"/></filter>
    <clipPath id="inside"><path d="{TILE_PATH}"/></clipPath>
  </defs>
  <path d="{TILE_PATH}" fill="url(#tile)"/>
  <path d="{TILE_PATH}" fill="none" stroke="url(#rim)" stroke-width="2" clip-path="url(#inside)"/>
  <g filter="url(#cast)">
    <path d="{path(parts["left"])}" fill="url(#lit)"/>
    <path d="{path(parts["right"])}" fill="url(#shade)"/>
    <path d="{barbs}" fill="none" stroke="{look["barbs"][0]}" stroke-opacity="{look["barbs"][1]}" stroke-width="0.9" stroke-linecap="round"/>
    <g fill="{look["shaft"]}"><path d="{path(parts["shaft"])}"/><circle cx="{cx}" cy="{cy}" r="{r:.2f}"/></g>
  </g>
</svg>
"""


def svg_mono():
    parts = feather({**FULL, "barbs": False})
    (cx, cy), r = parts["cap"]
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">
  <title>Nea</title>
  <!-- The feather alone, in the colour of the text around it. Written by tools/logo.py. -->
  <g fill="currentColor">
    <path d="{path(parts["left"])}"/>
    <path d="{path(parts["right"])}"/>
    <path d="{path(parts["shaft"])}"/>
    <circle cx="{cx}" cy="{cy}" r="{r:.2f}"/>
  </g>
</svg>
"""


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


def stacked(sheets):
    out = Image.new("RGBA", (max(s.width for s in sheets), sum(s.height for s in sheets)))
    y = 0
    for sheet in sheets:
        out.paste(sheet, (0, y))
        y += sheet.height
    return out


def build():
    (LOGO / "nea.svg").write_text(svg(LIGHT), encoding="utf-8", newline="\n")
    (LOGO / "nea-dark.svg").write_text(svg(DARK), encoding="utf-8", newline="\n")
    (LOGO / "nea-mono.svg").write_text(svg_mono(), encoding="utf-8", newline="\n")

    light = {size: render(size, LIGHT) for size in ICO_SIZES}
    dark = {size: render(size, DARK) for size in ICO_SIZES}
    write_ico([light[s] for s in ICO_SIZES], LOGO / "nea.ico")

    DOCS.mkdir(parents=True, exist_ok=True)
    light[256].save(DOCS / "logo-256.png")
    dark[256].save(DOCS / "logo-256-dark.png")
    small = [16, 20, 24, 32, 40, 48, 64]
    size_row([light[s] for s in small], LIGHT_BG, 1).save(DOCS / "sizes-light.png")
    size_row([dark[s] for s in small], DARK_BG, 1).save(DOCS / "sizes-dark.png")
    tiny = [16, 20, 24, 32, 48]
    stacked(
        [
            size_row([light[s] for s in tiny], LIGHT_BG, 8),
            size_row([light[s] for s in tiny], DARK_BG, 8),
            size_row([dark[s] for s in tiny], LIGHT_BG, 8),
            size_row([dark[s] for s in tiny], DARK_BG, 8),
        ]
    ).save(DOCS / "small-sizes-enlarged.png")


if __name__ == "__main__":
    build()
