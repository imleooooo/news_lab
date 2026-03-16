#!/usr/bin/env python3
"""
Generate News_LAB.icns from scratch.
Produces a tech-radar themed icon: dark background, concentric rings, "NL" label.
Output: scripts/../resources/icon.icns
"""

import os, sys, math, subprocess, shutil, tempfile
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:
    sys.exit("Pillow not installed. Run: pip3 install Pillow")

# ── Paths ──────────────────────────────────────────────────────────────────────
SCRIPT_DIR = Path(__file__).parent
RESOURCES   = SCRIPT_DIR.parent / "resources"
RESOURCES.mkdir(exist_ok=True)
ICONSET     = RESOURCES / "icon.iconset"
ICNS_OUT    = RESOURCES / "icon.icns"

# ── Palette ────────────────────────────────────────────────────────────────────
BG          = (13,  18,  38)       # deep navy
RING_COLORS = [
    (34, 211, 238, 55),   # cyan – outermost
    (56, 189, 248, 80),
    (99, 179, 237, 110),
    (147,197,253, 140),   # lavender – innermost
]
CROSS       = (56, 189, 248, 45)
LABEL_BG    = (30, 41, 80)
LABEL_FG    = (226, 232, 240)      # slate-100


def make_icon(size: int) -> Image.Image:
    img  = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    # Rounded-rect background
    r = size // 6
    draw.rounded_rectangle([0, 0, size - 1, size - 1], radius=r, fill=BG)

    cx = cy = size / 2

    # Concentric radar rings
    ring_radii = [0.42, 0.31, 0.20, 0.09]
    for ring_r, color in zip(ring_radii, RING_COLORS):
        rp = ring_r * size
        lw = max(1, size // 128)
        draw.ellipse(
            [cx - rp, cy - rp, cx + rp, cy + rp],
            outline=color, width=lw
        )

    # Cross-hair lines
    margin = ring_radii[0] * size
    lw = max(1, size // 200)
    draw.line([cx - margin, cy, cx + margin, cy], fill=CROSS, width=lw)
    draw.line([cx, cy - margin, cx, cy + margin], fill=CROSS, width=lw)

    # Blip dots (N, E quadrants – decorative)
    dot_specs = [
        (0.28, 50,  (74, 222, 128, 230)),   # green  – adopt
        (0.20, 340, (250,204, 21, 220)),    # yellow – trial
        (0.37, 130, (56, 189, 248, 210)),   # cyan   – assess
        (0.12, 200, (248,113,113, 200)),    # red    – hold
    ]
    for rf, angle_deg, color in dot_specs:
        rad  = math.radians(angle_deg)
        # polar → cartesian (clockwise from top)
        dx = rf * size * math.sin(rad)
        dy = -rf * size * math.cos(rad)
        dp = max(4, size // 55)
        draw.ellipse(
            [cx + dx - dp, cy + dy - dp, cx + dx + dp, cy + dy + dp],
            fill=color
        )

    # "NL" label pill at the bottom
    pill_w  = size * 0.36
    pill_h  = size * 0.14
    pill_x0 = cx - pill_w / 2
    pill_y0 = size * 0.74
    pill_r  = pill_h / 2
    draw.rounded_rectangle(
        [pill_x0, pill_y0, pill_x0 + pill_w, pill_y0 + pill_h],
        radius=pill_r, fill=LABEL_BG
    )

    # Text "NL" – try bold system fonts, fall back to default
    font_size = int(size * 0.10)
    font = None
    for path in [
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/System/Library/Fonts/SFNS.ttf",
    ]:
        if os.path.exists(path):
            try:
                font = ImageFont.truetype(path, font_size)
                break
            except Exception:
                pass
    if font is None:
        font = ImageFont.load_default()

    # Center text in pill
    bbox  = draw.textbbox((0, 0), "NL", font=font)
    tw    = bbox[2] - bbox[0]
    th    = bbox[3] - bbox[1]
    tx    = pill_x0 + (pill_w - tw) / 2 - bbox[0]
    ty    = pill_y0 + (pill_h - th) / 2 - bbox[1]
    draw.text((tx, ty), "NL", font=font, fill=LABEL_FG)

    return img


def build_iconset(iconset_dir: Path):
    iconset_dir.mkdir(exist_ok=True)
    sizes = [16, 32, 64, 128, 256, 512, 1024]
    for s in sizes:
        img = make_icon(s)
        img.save(iconset_dir / f"icon_{s}x{s}.png")
        # @2x variants (Retina)
        if s <= 512:
            img2 = make_icon(s * 2)
            img2.save(iconset_dir / f"icon_{s}x{s}@2x.png")
    print(f"  iconset → {iconset_dir}")


def iconset_to_icns(iconset_dir: Path, out: Path):
    subprocess.run(
        ["iconutil", "-c", "icns", str(iconset_dir), "-o", str(out)],
        check=True
    )
    shutil.rmtree(iconset_dir)
    print(f"  icns    → {out}  ({out.stat().st_size // 1024} KB)")


if __name__ == "__main__":
    print("🎨  Generating icon...")
    build_iconset(ICONSET)
    iconset_to_icns(ICONSET, ICNS_OUT)
    print("✅  Done.")
