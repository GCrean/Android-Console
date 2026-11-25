"""Generate the application icon set (PNG sizes, .ico, .icns). Requires Pillow."""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter

OUT = Path(__file__).resolve().parent.parent / "assets" / "icons"
S = 2048  # supersampled canvas


def master() -> Image.Image:
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    # Background gradient clipped to a rounded square.
    grad = Image.new("RGBA", (S, S))
    px = grad.load()
    top, bot = (0x1c, 0x2a, 0x44), (0x0d, 0x11, 0x17)
    for y in range(S):
        t = y / (S - 1)
        c = tuple(round(top[i] + (bot[i] - top[i]) * t) for i in range(3)) + (255,)
        for x in range(S):
            px[x, y] = c
    margin = int(S * 0.06)
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (margin, margin, S - margin, S - margin), radius=int(S * 0.22), fill=255
    )
    img.paste(grad, (0, 0), mask)

    d = ImageDraw.Draw(img)
    # Subtle inner border.
    d.rounded_rectangle(
        (margin, margin, S - margin, S - margin),
        radius=int(S * 0.22), outline=(255, 255, 255, 40), width=int(S * 0.006),
    )
    green = (0x3f, 0xb9, 0x50, 255)
    # Prompt chevron ">"
    w = int(S * 0.085)
    pts = [(S * 0.30, S * 0.34), (S * 0.52, S * 0.50), (S * 0.30, S * 0.66)]
    d.line(pts, fill=green, width=w, joint="curve")
    for p in (pts[0], pts[2]):
        d.ellipse((p[0] - w / 2, p[1] - w / 2, p[0] + w / 2, p[1] + w / 2), fill=green)
    # Cursor underscore
    d.rounded_rectangle(
        (S * 0.58, S * 0.62, S * 0.74, S * 0.62 + w), radius=w // 2, fill=(230, 237, 243, 255)
    )
    return img


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    m = master()
    sizes = [16, 32, 48, 64, 128, 256, 512, 1024]
    for s in sizes:
        m.resize((s, s), Image.LANCZOS).save(OUT / f"{s}x{s}.png")
    m.resize((512, 512), Image.LANCZOS).save(OUT / "icon.png")
    m.resize((256, 256), Image.LANCZOS).save(
        OUT / "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    )
    m.resize((1024, 1024), Image.LANCZOS).save(OUT / "icon.icns")
    print("wrote", sorted(p.name for p in OUT.iterdir()))


if __name__ == "__main__":
    main()
