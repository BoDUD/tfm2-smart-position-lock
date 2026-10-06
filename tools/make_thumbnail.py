"""Draws package/thumbnail.png (512x512), the preview of Smart Position Lock.

    python tools/make_thumbnail.py

Needs Pillow. Fonts: DejaVu Sans Bold (Linux) or Arial Bold (Windows), else Pillow's default.
"""

import os

from PIL import Image, ImageDraw, ImageFilter, ImageFont

W = H = 512
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FONTS = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "C:/Windows/Fonts/arialbd.ttf",
]


def font(size):
    for path in FONTS:
        if os.path.exists(path):
            return ImageFont.truetype(path, size)
    return ImageFont.load_default()


def main():
    img = Image.new("RGBA", (W, H), (22, 23, 33, 255))
    d = ImageDraw.Draw(img)
    # a soft glow behind the slots
    glow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    ImageDraw.Draw(glow).ellipse((56, 70, 456, 400), fill=(76, 195, 138, 70))
    img.alpha_composite(glow.filter(ImageFilter.GaussianBlur(60)))
    d = ImageDraw.Draw(img)

    lanes = ["TOP", "JG", "MID", "BOT", "SUP"]
    filled = [True, True, False, True, False]
    x0, y0, w, h, gap = 46, 96, 76, 150, 8
    for i, (lane, full) in enumerate(zip(lanes, filled)):
        x = x0 + i * (w + gap)
        color = (76, 195, 138, 255) if full else (74, 76, 86, 255)
        d.rounded_rectangle((x, y0, x + w, y0 + h), 12, fill=(7, 8, 11, 230), outline=color, width=4)
        tw = d.textlength(lane, font=font(22))
        d.text((x + (w - tw) / 2, y0 + h - 36), lane, font=font(22), fill=color)
        if full:
            # a check mark: this position is taken
            d.line((x + 22, y0 + 58, x + 34, y0 + 72, x + 56, y0 + 44), fill=color, width=7, joint="curve")

    # the lock
    cx, cy = W // 2, 330
    d.rounded_rectangle((cx - 70, cy - 10, cx + 70, cy + 90), 16, fill=(194, 198, 206, 255))
    d.arc((cx - 48, cy - 80, cx + 48, cy + 20), 180, 360, fill=(194, 198, 206, 255), width=18)
    d.ellipse((cx - 14, cy + 22, cx + 14, cy + 50), fill=(22, 23, 33, 255))
    d.rectangle((cx - 5, cy + 40, cx + 5, cy + 70), fill=(22, 23, 33, 255))

    title = "SMART POSITION LOCK"
    f = font(36)
    tw = d.textlength(title, font=f)
    d.text(((W - tw) / 2, 34), title, font=f, fill=(232, 232, 232, 255))
    img.convert("RGB").save(os.path.join(ROOT, "package", "thumbnail.png"))


if __name__ == "__main__":
    main()
