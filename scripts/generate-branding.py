"""Regenerate committed icons: pip install pillow==12.3.0 resvg-py==0.5.0."""
from pathlib import Path
from io import BytesIO

from PIL import Image
import resvg_py

branding = Path(__file__).resolve().parents[1] / "assets" / "branding"
png = resvg_py.svg_to_bytes(svg_path=str(branding / "opsssh-mascot.svg"), width=1024, height=1024)
(branding / "opsssh.png").write_bytes(png)
with Image.open(BytesIO(png)) as image:
    image.save(branding / "opsssh.ico", sizes=[(n, n) for n in (16, 24, 32, 48, 64, 128, 256)])
