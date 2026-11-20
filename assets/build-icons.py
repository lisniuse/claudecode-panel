"""Rebuild the embedded Windows resources from the SVG files."""

import argparse
from io import BytesIO
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory

import cairosvg
from PIL import Image


ASSETS = Path(__file__).resolve().parent


def write_ico(svg, output, sizes):
    frames = []
    for size in sizes:
        data = cairosvg.svg2png(url=str(svg), output_width=size, output_height=size)
        with Image.open(BytesIO(data)) as frame:
            frames.append(frame.convert("RGBA"))
    frames[-1].save(
        output, sizes=[(size, size) for size in sizes], append_images=frames[:-1]
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rc", default="rc.exe", help="Windows SDK resource compiler")
    args = parser.parse_args()

    with TemporaryDirectory() as directory:
        work = Path(directory)
        write_ico(ASSETS / "logo.svg", work / "app.ico", (16, 20, 24, 32, 40, 48, 64, 128, 256))
        write_ico(ASSETS / "tray.svg", work / "tray.ico", (16, 20, 24, 32, 40, 48, 64))
        script = work / "icons.rc"
        script.write_bytes((ASSETS / "icons.rc").read_bytes())
        subprocess.run([args.rc, "/nologo", "/fo", "icons.res", script.name], cwd=work, check=True)
        (ASSETS / "icons.res").write_bytes((work / "icons.res").read_bytes())


if __name__ == "__main__":
    main()
