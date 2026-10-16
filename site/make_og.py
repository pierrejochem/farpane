"""Make the social preview image (1200 x 630) from the Glass backdrop, the
app icon and the Inter font. Run once; the result is committed."""
from PIL import Image, ImageDraw, ImageFont, ImageFilter

root = __file__.rsplit("/site/", 1)[0]
bg = Image.open(f"{root}/crates/farpane-ui/assets/glass-backdrop.png").convert("RGB")
bg = bg.resize((1200, 750)).crop((0, 60, 1200, 690))
bg = Image.eval(bg, lambda v: min(255, int(v * 1.6)))
panel = Image.new("RGBA", (1200, 630), (5, 5, 7, 150))
img = bg.convert("RGBA")
img.alpha_composite(panel)
icon = Image.open(f"{root}/docs/brand/farpane-viewer-1024.png").convert("RGBA").resize((200, 200), Image.LANCZOS)
img.alpha_composite(icon, (90, 120))
d = ImageDraw.Draw(img)
semibold = f"{root}/crates/farpane-ui/assets/fonts/Inter-SemiBold.ttf"
regular = f"{root}/crates/farpane-ui/assets/fonts/Inter-Regular.ttf"
d.text((330, 120), "Farpane", font=ImageFont.truetype(semibold, 112), fill=(244, 244, 245))
d.text((332, 262), "VNC-compatible remote desktop", font=ImageFont.truetype(regular, 44), fill=(255, 138, 115))
d.text((332, 322), "viewer and server, written in Rust", font=ImageFont.truetype(regular, 44), fill=(220, 220, 224))
d.text((90, 520), "Encrypted. Open source. MIT or Apache-2.0.", font=ImageFont.truetype(regular, 34), fill=(190, 190, 196))
img.convert("RGB").save(f"{root}/site/static/og.png", optimize=True)
