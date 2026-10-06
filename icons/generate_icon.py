"""Generate the VAM VAR Deduper app icon — Amber Circuit aesthetic.

Reference: theme_ref/stitch_sleek_dedupe_dashboard/screen.png + DESIGN.md.

Design:
 - Warm charcoal squircle (#131314) with a soft inner top highlight + bottom
   lowlight to suggest a chamfered metal edge.
 - Faint right-angled PCB traces in the background (darker warm gray) for the
   "vintage hi-fi / circuit board" texture.
 - Bold V glyph with:
     * an outer glow (filament-orange Gaussian bloom),
     * a deep terracotta drop-V offset down-right (extruded depth),
     * the main V filled with a vertical gradient amber-glow ->
       filament-orange -> burnt-clay (lit from the top).
"""
from __future__ import annotations

import random
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

# ----- palette (from theme_ref/.../DESIGN.md "Amber Circuit") -----
BG          = (19, 19, 20)      # #131314  background charcoal
TRACE       = (42, 42, 43)      # #2A2A2B  PCB trace base
PRIMARY     = (255, 107, 53)    # #FF6B35  filament orange
PRIMARY_HI  = (255, 186, 60)    # #FFBA3C  amber glow / lit edge
PRIMARY_LO  = (214, 73, 51)     # #D64933  burnt clay
DEEP        = (93, 25, 0)       # #5D1900  extruded shadow under V
RIM_HI      = (255, 255, 255)   # inner top highlight (low alpha)
RIM_LO      = (0, 0, 0)         # inner bottom lowlight (low alpha)

MASTER = 1024
RADIUS_FRAC = 0.22
RNG_SEED = 42


def rounded_mask(size: int, radius: int) -> Image.Image:
    m = Image.new("L", (size, size), 0)
    ImageDraw.Draw(m).rounded_rectangle(
        (0, 0, size - 1, size - 1), radius=radius, fill=255
    )
    return m


def render_circuit_traces(S: int) -> Image.Image:
    """Faint right-angled PCB traces with small endpoint nodes."""
    layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer, "RGBA")
    rng = random.Random(RNG_SEED)

    step = S // 32  # ~32px grid at master scale

    for _ in range(22):
        # snap start to grid
        x = rng.randint(0, 31) * step
        y = rng.randint(0, 31) * step
        axis = rng.choice(("h", "v"))
        thickness = rng.choice([3, 4])
        alpha = rng.randint(70, 130)
        color = TRACE + (alpha,)

        for _seg in range(rng.randint(2, 4)):
            length = rng.randint(2, 6) * step
            sign = rng.choice((-1, 1))
            if axis == "h":
                x2 = x + sign * length
                d.line((x, y, x2, y), fill=color, width=thickness)
                x = x2
                axis = "v"
            else:
                y2 = y + sign * length
                d.line((x, y, x, y2), fill=color, width=thickness)
                y = y2
                axis = "h"
            r = thickness + 2
            d.ellipse((x - r, y - r, x + r, y + r), fill=color)

    return layer


def vertical_gradient(S: int, top: tuple, mid: tuple, bot: tuple) -> Image.Image:
    """1×S vertical gradient stretched horizontally to S×S RGBA."""
    strip = Image.new("RGBA", (1, S), (0, 0, 0, 255))
    half = S / 2
    for y in range(S):
        if y < half:
            t = y / half
            a, b = top, mid
        else:
            t = (y - half) / half
            a, b = mid, bot
        strip.putpixel(
            (0, y),
            (
                round(a[0] * (1 - t) + b[0] * t),
                round(a[1] * (1 - t) + b[1] * t),
                round(a[2] * (1 - t) + b[2] * t),
                255,
            ),
        )
    return strip.resize((S, S))


def load_v_font(size: int) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    for candidate in ("arialbd.ttf", "seguibl.ttf", "segoeuib.ttf", "arial.ttf"):
        try:
            return ImageFont.truetype(candidate, size)
        except OSError:
            continue
    return ImageFont.load_default()


def render_master() -> Image.Image:
    S = MASTER
    canvas = Image.new("RGBA", (S, S), BG + (255,))

    # PCB traces (these will get clipped by the squircle later)
    canvas = Image.alpha_composite(canvas, render_circuit_traces(S))

    # V glyph metrics
    font = load_v_font(int(S * 0.72))
    measure = ImageDraw.Draw(canvas)
    bbox = measure.textbbox((0, 0), "V", font=font)
    tw = bbox[2] - bbox[0]
    th = bbox[3] - bbox[1]
    cx = S / 2
    cy = S / 2
    tx = cx - tw / 2 - bbox[0]
    ty = cy - th / 2 - bbox[1]

    # 1) Outer glow — render V in primary orange, blur heavily, boost alpha.
    glow_src = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(glow_src, "RGBA").text(
        (tx, ty), "V", font=font, fill=PRIMARY + (255,)
    )
    glow = glow_src.filter(ImageFilter.GaussianBlur(radius=S // 22))
    r, g, b, a = glow.split()
    a = a.point(lambda v: min(255, int(v * 2.2)))
    glow = Image.merge("RGBA", (r, g, b, a))
    canvas = Image.alpha_composite(canvas, glow)

    # 2) Deep drop V — offset down-right, gives 3D extrusion.
    drop = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    off = max(6, S // 110)
    ImageDraw.Draw(drop, "RGBA").text(
        (tx + off, ty + off), "V", font=font, fill=DEEP + (230,)
    )
    canvas = Image.alpha_composite(canvas, drop)

    # 3) Main V with vertical gradient (amber top → orange mid → burnt bot).
    v_mask_layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(v_mask_layer, "RGBA").text(
        (tx, ty), "V", font=font, fill=(255, 255, 255, 255)
    )
    v_mask = v_mask_layer.split()[3]
    gradient = vertical_gradient(S, PRIMARY_HI, PRIMARY, PRIMARY_LO)
    v_filled = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    v_filled.paste(gradient, (0, 0), v_mask)
    canvas = Image.alpha_composite(canvas, v_filled)

    # Squircle clip
    radius = int(S * RADIUS_FRAC)
    mask = rounded_mask(S, radius)
    out = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    out.paste(canvas, (0, 0), mask)

    # Chamfered rim — soft 1px-ish inner top highlight + bottom lowlight.
    rim = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    rim_d = ImageDraw.Draw(rim, "RGBA")
    border_w = max(2, S // 256)
    rim_d.rounded_rectangle(
        (border_w, border_w, S - 1 - border_w, S - 1 - border_w),
        radius=radius - border_w,
        outline=RIM_HI + (24,),
        width=border_w,
    )
    out = Image.alpha_composite(out, rim)
    out.putalpha(mask)
    return out


def write_svg(path: Path) -> None:
    """Editable SVG source — flat approximation of the raster design."""
    svg = """<?xml version='1.0' encoding='UTF-8'?>
<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 1024 1024' width='1024' height='1024'>
  <defs>
    <clipPath id='clip'>
      <rect x='0' y='0' width='1024' height='1024' rx='225' ry='225'/>
    </clipPath>
    <linearGradient id='vfill' x1='0' y1='0' x2='0' y2='1'>
      <stop offset='0%' stop-color='#FFBA3C'/>
      <stop offset='55%' stop-color='#FF6B35'/>
      <stop offset='100%' stop-color='#D64933'/>
    </linearGradient>
    <filter id='glow' x='-20%' y='-20%' width='140%' height='140%'>
      <feGaussianBlur stdDeviation='34' result='blur'/>
      <feMerge>
        <feMergeNode in='blur'/>
        <feMergeNode in='SourceGraphic'/>
      </feMerge>
    </filter>
  </defs>
  <g clip-path='url(#clip)'>
    <rect width='1024' height='1024' fill='#131314'/>
    <!-- a few representative PCB traces -->
    <g stroke='#2A2A2B' stroke-width='4' fill='#2A2A2B' opacity='0.55'>
      <polyline points='64,128 240,128 240,256' fill='none'/>
      <circle cx='240' cy='256' r='6'/>
      <polyline points='896,160 720,160 720,320' fill='none'/>
      <circle cx='720' cy='320' r='6'/>
      <polyline points='128,768 128,640 320,640' fill='none'/>
      <circle cx='320' cy='640' r='6'/>
      <polyline points='896,800 896,672 704,672' fill='none'/>
      <circle cx='704' cy='672' r='6'/>
    </g>
    <!-- extruded shadow V -->
    <text x='521' y='772' font-family='Arial Black, Arial, sans-serif' font-weight='900'
          font-size='760' text-anchor='middle' fill='#5D1900' opacity='0.9'>V</text>
    <!-- main V (gradient + glow) -->
    <text x='512' y='763' font-family='Arial Black, Arial, sans-serif' font-weight='900'
          font-size='760' text-anchor='middle' fill='url(#vfill)' filter='url(#glow)'>V</text>
    <!-- chamfer rim -->
    <rect x='4' y='4' width='1016' height='1016' rx='221' ry='221'
          fill='none' stroke='#FFFFFF' stroke-opacity='0.10' stroke-width='4'/>
  </g>
</svg>
"""
    path.write_text(svg, encoding="utf-8")


def main() -> None:
    repo = Path(__file__).resolve().parents[1]
    icons_dir = repo / "icons"
    tauri_icons_dir = repo / "src-tauri" / "icons"
    icons_dir.mkdir(parents=True, exist_ok=True)
    tauri_icons_dir.mkdir(parents=True, exist_ok=True)

    master = render_master()

    def at(size: int) -> Image.Image:
        return master.resize((size, size), Image.LANCZOS)

    png_targets = {
        "32x32.png": 32,
        "128x128.png": 128,
        "128x128@2x.png": 256,
    }
    for name, size in png_targets.items():
        img = at(size)
        for d in (icons_dir, tauri_icons_dir):
            img.save(d / name, format="PNG", optimize=True)

    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    ico_source = at(256)
    for d in (icons_dir, tauri_icons_dir):
        ico_source.save(d / "icon.ico", format="ICO", sizes=[(s, s) for s in ico_sizes])

    for d in (icons_dir, tauri_icons_dir):
        try:
            at(1024).save(d / "icon.icns", format="ICNS")
        except Exception as exc:
            print(f"ICNS write failed in {d}: {exc}")

    master.save(icons_dir / "icon-1024.png", format="PNG", optimize=True)
    write_svg(icons_dir / "icon.svg")
    print("done.")


if __name__ == "__main__":
    main()
