from __future__ import annotations

from io import BytesIO
from pathlib import Path

from PIL import Image, ImageDraw


ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "apps" / "windows" / "assets"
SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)
NAVY = "#172033"
WHITE = "#F9FAFC"
CORAL = "#C45E3A"


def scaled(value: int, size: int, supersample: int) -> int:
    return round(value * size * supersample / 512)


def render_icon(size: int, supersample: int = 4) -> Image.Image:
    canvas = size * supersample
    image = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)

    draw.rounded_rectangle(
        (
            scaled(20, size, supersample),
            scaled(20, size, supersample),
            scaled(492, size, supersample),
            scaled(492, size, supersample),
        ),
        radius=scaled(112, size, supersample),
        fill=NAVY,
    )
    draw.rounded_rectangle(
        (
            scaled(86, size, supersample),
            scaled(116, size, supersample),
            scaled(426, size, supersample),
            scaled(354, size, supersample),
        ),
        radius=scaled(42, size, supersample),
        outline=WHITE,
        width=max(1, scaled(30, size, supersample)),
    )
    line_width = max(1, scaled(28, size, supersample))
    draw.line(
        (
            scaled(256, size, supersample),
            scaled(354, size, supersample),
            scaled(256, size, supersample),
            scaled(408, size, supersample),
        ),
        fill=WHITE,
        width=line_width,
    )
    draw.line(
        (
            scaled(180, size, supersample),
            scaled(408, size, supersample),
            scaled(332, size, supersample),
            scaled(408, size, supersample),
        ),
        fill=WHITE,
        width=line_width,
    )
    draw.ellipse(
        (
            scaled(306, size, supersample),
            scaled(130, size, supersample),
            scaled(414, size, supersample),
            scaled(238, size, supersample),
        ),
        fill=CORAL,
    )
    draw.ellipse(
        (
            scaled(341, size, supersample),
            scaled(165, size, supersample),
            scaled(379, size, supersample),
            scaled(203, size, supersample),
        ),
        fill=WHITE,
    )
    return image.resize((size, size), Image.Resampling.LANCZOS)


def main() -> None:
    ASSETS.mkdir(parents=True, exist_ok=True)
    frames = [render_icon(size) for size in SIZES]
    png = BytesIO()
    frames[-1].save(png, format="PNG", optimize=True)
    write_if_changed(ASSETS / "desklink-icon.png", png.getvalue())
    icon = BytesIO()
    frames[-1].save(
        icon,
        format="ICO",
        append_images=frames[:-1],
        sizes=[(size, size) for size in SIZES],
    )
    changed = write_if_changed(ASSETS / "desklink.ico", icon.getvalue())
    status = "updated" if changed else "unchanged"
    print(f"{status} {ASSETS / 'desklink.ico'}")


def write_if_changed(path: Path, data: bytes) -> bool:
    if path.is_file() and path.read_bytes() == data:
        return False
    path.write_bytes(data)
    return True


if __name__ == "__main__":
    main()
