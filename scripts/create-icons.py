"""Generate original Qwen Desk application icons. Requires Pillow."""
from pathlib import Path
from PIL import Image, ImageDraw
root = Path(__file__).resolve().parents[1] / "src-tauri" / "icons"
root.mkdir(parents=True, exist_ok=True)
im = Image.new("RGBA", (512, 512))
d = ImageDraw.Draw(im)
d.rounded_rectangle((16, 16, 496, 496), radius=104, fill="#3159C6")
d.rounded_rectangle((108, 104, 392, 376), radius=66, outline="white", width=32)
d.line((270, 276, 405, 411), fill="white", width=36)
d.rounded_rectangle((177, 184, 211, 246), radius=16, fill="white")
d.rounded_rectangle((280, 184, 314, 246), radius=16, fill="white")
for size in (32, 128, 256):
    im.resize((size,size),Image.Resampling.LANCZOS).save(root / f"{size}x{size}.png")
im.save(root / "icon.ico",sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)])
