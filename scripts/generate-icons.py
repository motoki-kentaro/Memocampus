"""Regenerates the Chrome extension PNGs and the Windows host ICO from assets/icon.png.

The master is read only and never rewritten. Each size is resized directly from the master with
Lanczos resampling; colours, the background and the alpha state are kept (an opaque master stays
opaque). Requires Pillow (`python -m pip install Pillow`). Run from anywhere:

    python scripts/generate-icons.py           # write the generated files
    python scripts/generate-icons.py --check   # fail if a generated file is missing or out of date
"""

import argparse
import io
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
MASTER = ROOT / "assets" / "icon.png"
EXTENSION_SIZES = (16, 32, 48, 128)
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)
EXTENSION_ICON = ROOT / "extension" / "icons" / "icon-{size}.png"
HOST_ICO = ROOT / "native-host" / "resources" / "memocampus.ico"


def load_master():
    image = Image.open(MASTER)
    image.load()
    if image.width != image.height:
        # Padding or cropping would change the picture; that is a design decision, not a build step.
        sys.exit(f"{MASTER} is {image.width}x{image.height}; a square master is required")
    if image.mode not in ("RGB", "RGBA"):
        image = image.convert("RGBA" if "transparency" in image.info or "A" in image.mode else "RGB")
    return image


def resized(master, size):
    return master.resize((size, size), Image.Resampling.LANCZOS)


def png_bytes(image):
    out = io.BytesIO()
    # No metadata from the master (its C2PA manifest describes the master only).
    image.save(out, format="PNG", optimize=True)
    return out.getvalue()


def ico_bytes(master):
    # Windows expects 32-bit entries; an opaque master gets a fully opaque alpha channel, so nothing
    # becomes transparent. Every entry is a PNG, which Windows Vista and later read at all sizes.
    images = [resized(master, size).convert("RGBA") for size in ICO_SIZES]
    entries = [png_bytes(image) for image in images]
    header = len(entries).to_bytes(2, "little")
    out = bytearray(b"\x00\x00\x01\x00" + header)
    offset = 6 + 16 * len(entries)
    for size, data in zip(ICO_SIZES, entries):
        out += bytes([size % 256, size % 256, 0, 0])
        out += (1).to_bytes(2, "little") + (32).to_bytes(2, "little")
        out += len(data).to_bytes(4, "little") + offset.to_bytes(4, "little")
        offset += len(data)
    for data in entries:
        out += data
    return bytes(out)


def outputs():
    master = load_master()
    files = {Path(str(EXTENSION_ICON).format(size=size)): png_bytes(resized(master, size)) for size in EXTENSION_SIZES}
    files[HOST_ICO] = ico_bytes(master)
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="only verify that the generated files are current")
    args = parser.parse_args()
    stale = []
    for path, data in outputs().items():
        current = path.read_bytes() if path.exists() else None
        if current == data:
            continue
        stale.append(path)
        if not args.check:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
    for path in stale:
        print(("out of date: " if args.check else "wrote ") + str(path.relative_to(ROOT)))
    if args.check and stale:
        sys.exit(1)
    if not stale:
        print("icons are up to date")


if __name__ == "__main__":
    main()
