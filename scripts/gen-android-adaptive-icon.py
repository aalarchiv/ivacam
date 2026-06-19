#!/usr/bin/env python3
"""Regenerate the Android adaptive launcher icon as proper layers (ivac-m5a9).

`cargo tauri icon` emits a single full-bleed image used as the adaptive
foreground, so the round-button logo's dark circle reached ~71% of the
108dp canvas and got clipped under the launcher's round / squircle masks
(the "ivaCAM" text sat right on the cut line). Android adaptive icons want
two layers instead:

  - background: a solid brand color (the dark button fill) — the OS mask
    shapes THIS, so there's nothing to clip;
  - foreground: the MARK ONLY (colorful arrows + wordmark, transparent bg),
    scaled to sit inside the 66dp safe zone so no mask touches it.

This script rebuilds only the adaptive layers from the tracked source art;
the legacy (pre-API-26) ic_launcher.png / ic_launcher_round.png keep the
self-contained round-button design, which already masks correctly.

Run from the repo root after `cargo tauri android init` regenerates gen/
(then `git add -f` the outputs — gen/ is git-ignored). Requires Pillow.
"""

from pathlib import Path

from PIL import Image

REPO = Path(__file__).resolve().parent.parent
SOURCE = REPO / "assets" / "ivaCAM-Logo-2048.png"  # mark-only, transparent bg
RES = REPO / "crates/ivac-tauri/gen/android/app/src/main/res"

# Solid brand background = the round button's fill (sampled from
# assets/ivaCAM-Logo-button-2048.png center). The mask shapes this layer.
BRAND_BG = "#535366"

# The mark must stay inside the adaptive-icon SAFE CIRCLE: diameter 66dp of
# the 108dp canvas, i.e. radius 0.611 of the half-canvas. A circular mask
# clips anything past it, so we constrain the mark's furthest opaque pixel
# (its radius from the icon center), NOT its bounding box — the wordmark's
# corners reach further than its width/height suggest. 0.58 leaves a hair of
# margin inside the 0.611 safe radius for launcher parallax/zoom.
SAFE_RADIUS_FRACTION = 0.58

# Adaptive foreground is 108dp; px per density bucket = 108 * dpi/160.
FOREGROUND_PX = {
    "mdpi": 108,
    "hdpi": 162,
    "xhdpi": 216,
    "xxhdpi": 324,
    "xxxhdpi": 432,
}


def build_foreground(src_mark: Image.Image, canvas_px: int) -> Image.Image:
    """Center the mark on a transparent canvas, scaled into the safe circle.

    Scales so the mark's furthest opaque pixel from the bbox center lands on
    the safe-radius circle, guaranteeing no opaque pixel escapes it once
    centered on the canvas.
    """
    import numpy as np

    mark = src_mark.crop(src_mark.getbbox())  # trim transparent margins
    alpha = np.array(mark)[:, :, 3]
    ys, xs = np.nonzero(alpha)
    cx, cy = mark.width / 2, mark.height / 2
    max_radius = float(np.sqrt((xs - cx) ** 2 + (ys - cy) ** 2).max())
    target_radius = SAFE_RADIUS_FRACTION * (canvas_px / 2)
    scale = target_radius / max_radius
    new_size = (max(1, round(mark.width * scale)), max(1, round(mark.height * scale)))
    mark = mark.resize(new_size, Image.LANCZOS)
    canvas = Image.new("RGBA", (canvas_px, canvas_px), (0, 0, 0, 0))
    canvas.paste(mark, ((canvas_px - mark.width) // 2, (canvas_px - mark.height) // 2), mark)
    return canvas


def main() -> None:
    src = Image.open(SOURCE).convert("RGBA")
    for bucket, px in FOREGROUND_PX.items():
        out = RES / f"mipmap-{bucket}" / "ic_launcher_foreground.png"
        build_foreground(src, px).save(out)
        print(f"wrote {out.relative_to(REPO)} ({px}x{px})")

    bg = RES / "values" / "ic_launcher_background.xml"
    bg.write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n'
        "<resources>\n"
        f'  <color name="ic_launcher_background">{BRAND_BG}</color>\n'
        "</resources>\n"
    )
    print(f"wrote {bg.relative_to(REPO)} ({BRAND_BG})")


if __name__ == "__main__":
    main()
