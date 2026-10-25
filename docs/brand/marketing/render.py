#!/usr/bin/env python3
"""Render the marketing images from src/*.html with headless Chrome.

    python3 docs/brand/marketing/render.py [name ...]

Needs Google Chrome or Chromium on the path. Output goes to assets/, and the
Open Graph card is also copied to site/static/og.png for the site.
"""
import shutil
import subprocess
import sys
from pathlib import Path

here = Path(__file__).resolve().parent
root = here.parents[2]

# name -> (template, hash, width, height)
IMAGES = {
    "github-social-preview": ("card", "social", 1280, 640),
    "og": ("card", "og", 1200, 630),
    "x-card": ("card", "xcard", 1600, 900),
    "square": ("card", "square", 1080, 1080),
    "x-header": ("banner", "xheader", 1500, 500),
    "linkedin-banner": ("banner", "linkedin", 1584, 396),
}

def chrome():
    for name in ("google-chrome", "chromium", "chromium-browser", "chrome"):
        path = shutil.which(name)
        if path:
            return path
    sys.exit("Google Chrome or Chromium is needed")

def render(name):
    template, size, w, h = IMAGES[name]
    out = here / "assets" / f"{name}.png"
    out.parent.mkdir(exist_ok=True)
    url = (here / "src" / f"{template}.html").as_uri() + "#" + size
    subprocess.run(
        [chrome(), "--headless=new", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
         "--force-device-scale-factor=1", "--allow-file-access-from-files",
         f"--window-size={w},{h}", f"--screenshot={out}", "--virtual-time-budget=3000", url],
        check=True, capture_output=True,
    )
    print(f"{out.relative_to(root)}  {w}x{h}")

if __name__ == "__main__":
    for name in sys.argv[1:] or IMAGES:
        render(name)
    if (here / "assets" / "og.png").exists() and not sys.argv[1:]:
        shutil.copy(here / "assets" / "og.png", root / "site" / "static" / "og.png")
