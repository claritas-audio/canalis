"""Canalis logos in the Claritas wordmark construction (claritas_ui::wordmark), written as SVG into assets/.

Run: python3 scripts/logos.py [assets]
"""
import math
import re
import sys
from pathlib import Path

# Glyph space: baseline at y = 0, x-height 1, ascender ASCENDER, y up. Same numbers as the Claritas wordmark.
ASCENDER = 1.42
STROKE = 0.15
TRACKING = 0.36
BELL_C_WIDTH = 1.05
BELL_C_SHARPNESS = 4.5
BELL_C_GAP = 0.6
PRODUCT_GAP = 0.9
STACKED_BRAND_SCALE = 0.225
PORT = STROKE * 0.85
CABLE_C_WIDTH = 0.93

DARK = {"text": "#d8dce4", "accent": "#f2c94c", "track": "#343a46", "tile": "#14161a", "border": "#343a46"}
LIGHT = {"text": "#1d2026", "accent": "#c28a00", "track": "#c9ced8", "tile": "#ffffff", "border": "#c9ced8"}


def arc(cx, cy, r, a0, a1):
    n = max(2, math.ceil(abs(a1 - a0) / 6.0))
    return [
        (cx + r * math.cos(math.radians(a0 + (a1 - a0) * i / n)), cy + r * math.sin(math.radians(a0 + (a1 - a0) * i / n)))
        for i in range(n + 1)
    ]


def wave(x0, x1, y, amp, periods, n=60):
    return [(x0 + (x1 - x0) * i / n, y + amp * math.sin(2 * math.pi * periods * i / n)) for i in range(n + 1)]


class Drawing:
    def __init__(self):
        self.strokes = []
        self.dots = []
        self.width = 0.0

    def stroke(self, ink, line, w=1.0):
        self.strokes.append((ink, list(line), w))

    def dot(self, ink, p, r=STROKE * 0.75):
        self.dots.append((ink, p, r))

    def place(self, other, scale, dx, dy):
        m = lambda p: (dx + p[0] * scale, dy + p[1] * scale)
        for ink, line, w in other.strokes:
            self.strokes.append((ink, [m(p) for p in line], w * scale))
        for ink, p, r in other.dots:
            self.dots.append((ink, m(p), r * scale))
        self.width = max(self.width, dx + other.width * scale)

    def bounds(self):
        xs, ys = [], []
        for _, line, w in self.strokes:
            for x, y in line:
                xs += [x - STROKE * w / 2, x + STROKE * w / 2]
                ys += [y - STROKE * w / 2, y + STROKE * w / 2]
        for _, (x, y), r in self.dots:
            xs += [x - r, x + r]
            ys += [y - r, y + r]
        return min(xs), min(ys), max(xs), max(ys)


def glyph(ch):
    """(lines, dots, advance) of one letter at the origin."""
    o = 0.5
    if ch == "l":
        return [[(0, 0), (0, ASCENDER)]], [], 0.0
    if ch == "a":
        return [arc(o, o, o, 0, 360), [(1, 0), (1, 1)]], [], 1.0
    if ch == "i":
        return [[(0, 0), (0, 1)]], [(0, 1.32)], 0.0
    if ch == "n":
        arch = [(0.0, 0.55)] + arc(0.45, 0.55, 0.45, 180, 0) + [(0.9, 0.0)]
        return [[(0, 0), (0, 1)], arch], [], 0.9
    if ch == "s":
        r = 0.25
        return [arc(0.38, 1 - r, r, 20, 270) + arc(0.38, r, r, 90, -160)], [], 0.72
    if ch == "r":
        bowl = [(0, 0.55)] + arc(0.45, 0.55, 0.45, 180, 60)
        return [[(0, 0), (0, 1)], bowl], [], 0.68
    if ch == "t":
        return [[(0.22, ASCENDER - 0.1), (0.22, 0)], [(0, 1), (0.5, 1)]], [], 0.5
    raise KeyError(ch)


def word(d, text, x):
    for i, ch in enumerate(text):
        if i > 0:
            x += TRACKING
        lines, dots, adv = glyph(ch)
        for line in lines:
            d.stroke("text", [(px + x, py) for px, py in line])
        for px, py in dots:
            d.dot("text", (px + x, py))
        x += adv
    return x


def bell_c():
    n = 60
    floor = math.exp(-BELL_C_SHARPNESS)
    pts = []
    for i in range(n + 1):
        t = -1 + 2 * i / n
        g = (math.exp(-BELL_C_SHARPNESS * t * t) - floor) / (1 - floor)
        pts.append((BELL_C_WIDTH * (1 - g), ASCENDER / 2 * (1 - t)))
    return pts


def claritas():
    d = Drawing()
    d.stroke("accent", bell_c())
    d.width = word(d, "laritas", BELL_C_WIDTH + BELL_C_GAP)
    return d


def cable_c(x=0.0):
    """The c as a cable: an open circle in accent ink with a jack at each end."""
    d = Drawing()
    line = [(px + x, py) for px, py in arc(0.5, 0.5, 0.5, 45, 315)]
    d.stroke("accent", line)
    d.dot("text", line[0], PORT)
    d.dot("text", line[-1], PORT)
    d.width = x + CABLE_C_WIDTH
    return d


def wordmark():
    """"canalis" with the cable-c."""
    d = cable_c()
    d.width = word(d, "analis", CABLE_C_WIDTH + TRACKING)
    return d


def logo():
    """The brand line small over the product name, like the EQ logo."""
    d = wordmark()
    d.place(claritas(), STACKED_BRAND_SCALE, 0.0, ASCENDER + 0.12 * ASCENDER)
    return d


def inline():
    d = claritas()
    d.place(wordmark(), 1.0, d.width + PRODUCT_GAP, 0.0)
    return d


def canal(lead=0.22, banks=0.95, periods=1.0, amp=0.16):
    """The mark: audio in, the stream between the banks, audio out."""
    d = Drawing()
    y = ASCENDER / 2
    x0, x1 = lead, lead + banks
    w = x1 + lead
    d.stroke("text", [(x0, ASCENDER * 0.84), (x1, ASCENDER * 0.84)])
    d.stroke("text", [(x0, ASCENDER * 0.16), (x1, ASCENDER * 0.16)])
    d.stroke("accent", [(0, y)] + wave(x0 + 0.12, x1 - 0.12, y, amp, periods) + [(w, y)])
    d.dot("text", (0, y), PORT)
    d.dot("text", (w, y), PORT)
    d.width = w
    return d


def canal_vertical(lead=0.26, banks=0.9, periods=1.5, amp=0.15, half=0.46):
    """The mark standing up: the stream falls through the chain, top to bottom."""
    d = Drawing()
    cx = 0.5
    h = lead + banks + lead
    y1, y0 = h - lead, lead
    d.stroke("text", [(cx - half, y1), (cx - half, y0)])
    d.stroke("text", [(cx + half, y1), (cx + half, y0)])
    n = 60
    stream = [(cx + amp * math.sin(2 * math.pi * periods * i / n), y1 - 0.1 - (banks - 0.2) * i / n) for i in range(n + 1)]
    d.stroke("accent", [(cx, h)] + stream + [(cx, 0.0)])
    d.dot("text", (cx, h), PORT)
    d.dot("text", (cx, 0.0), PORT)
    d.width = cx + half
    return d


def lockup():
    """The mark beside the name, for a title bar or a README."""
    d = canal()
    d.place(wordmark(), 1.0, d.width + 0.5, 0.0)
    return d


def svg(d, pal, pad=STROKE):
    x0, y0, x1, y1 = d.bounds()
    w, h = x1 - x0 + 2 * pad, y1 - y0 + 2 * pad
    m = lambda p: (p[0] - x0 + pad, y1 - p[1] + pad)
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w:.3f} {h:.3f}" width="{w*40:.0f}" height="{h*40:.0f}">']
    out.append('  <g fill="none" stroke-linecap="round" stroke-linejoin="round">')
    for ink, line, scale in d.strokes:
        path = " ".join(f"{'M' if i == 0 else 'L'}{m(p)[0]:.3f} {m(p)[1]:.3f}" for i, p in enumerate(line))
        out.append(f'    <path stroke="{pal[ink]}" stroke-width="{STROKE*scale:.3f}" d="{path}"/>')
    out.append("  </g>")
    for ink, p, r in d.dots:
        x, y = m(p)
        out.append(f'  <circle fill="{pal[ink]}" cx="{x:.3f}" cy="{y:.3f}" r="{r:.3f}"/>')
    out.append("</svg>")
    return "\n".join(out) + "\n"


def tile(d, pal, pad=0.42):
    """A desktop icon: the drawing centred on a square rounded tile."""
    s = svg(d, pal)
    vb = re.search(r'viewBox="0 0 ([\d.]+) ([\d.]+)"', s)
    w, h = float(vb.group(1)), float(vb.group(2))
    side = max(w, h) + 2 * pad
    dx, dy = (side - w) / 2, (side - h) / 2
    head = f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {side:.3f} {side:.3f}" width="{side*40:.0f}" height="{side*40:.0f}">\n'
    rect = f'  <rect width="{side:.3f}" height="{side:.3f}" rx="{side*0.22:.3f}" fill="{pal["tile"]}" stroke="{pal["border"]}" stroke-width="0.03"/>\n'
    body = s[s.index("  <g fill"):s.index("</svg>")]
    return head + rect + f'  <g transform="translate({dx:.3f} {dy:.3f})">\n' + body + "  </g>\n</svg>\n"


def main(out_dir):
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    items = {
        "canalis-icon": canal(),
        "canalis-icon-vertical": canal_vertical(),
        "canalis-wordmark": wordmark(),
        "canalis-logo": logo(),
        "canalis-inline": inline(),
        "canalis-lockup": lockup(),
    }
    for name, d in items.items():
        (out / f"{name}.svg").write_text(svg(d, DARK))
        (out / f"{name}-light.svg").write_text(svg(d, LIGHT))
    (out / "canalis-app-icon.svg").write_text(tile(canal_vertical(), DARK))
    (out / "canalis-app-icon-light.svg").write_text(tile(canal_vertical(), LIGHT))
    for f in sorted(out.glob("canalis-*.svg")):
        print(f.name)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "assets")
