"""Regenerate committed icons: pip install pillow==12.3.0 resvg-py==0.5.0."""
from pathlib import Path
from io import BytesIO

from PIL import Image, ImageDraw, ImageFont
import resvg_py

branding = Path(__file__).resolve().parents[1] / "assets" / "branding"
png = resvg_py.svg_to_bytes(svg_path=str(branding / "opsssh-mascot.svg"), width=1024, height=1024)
(branding / "opsssh.png").write_bytes(png)
with Image.open(BytesIO(png)) as image:
    image.save(branding / "opsssh.ico", sizes=[(n, n) for n in (16, 24, 32, 48, 64, 128, 256)])
    mascot = image.convert("RGBA")

def font(size):
    for name in ("C:/Windows/Fonts/segoeuib.ttf", "DejaVuSans-Bold.ttf"):
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            pass
    return ImageFont.load_default(size=size)

# WiX reserves the left strip for artwork; its dialog text occupies the white area.
dialog = Image.new("RGB", (493, 312), "white")
draw = ImageDraw.Draw(dialog)
for y in range(312):
    t = y / 311
    draw.line((0, y, 163, y), fill=tuple(round(a + (b - a) * t) for a, b in zip((101, 10, 45), (206, 24, 83))))
mark = mascot.resize((132, 132), Image.Resampling.LANCZOS)
dialog.paste(mark, (16, 65), mark)
draw.text((82, 219), "OpsSSH", fill="white", font=font(24), anchor="mm")
dialog.save(branding / "installer-dialog.bmp")

banner = Image.new("RGB", (493, 58), "white")
mark = mascot.resize((50, 50), Image.Resampling.LANCZOS)
banner.paste(mark, (436, 3), mark)
banner.save(branding / "installer-banner.bmp")
