"""Fixtures for tests/sstv_crosscheck.rs, made with two SSTV implementations
that have nothing to do with this repository: pySSTV (encoder) and the `sstv`
package (decoder, Python bindings to a Rust crate).

    python sstv_crosscheck.py <chirpix binary> <output directory>

For each built-in test picture (320x240, so nobody resizes anything):
  <name>.png                    the picture
  <name>_ycbcr.png              Pillow's YCbCr of it, stored as R=Y, G=Cb, B=Cr
  pysstv_<name>.wav             pySSTV's Robot 36, 48 kHz 16-bit
  sstvpy_pysstv_<name>.png      the sstv package's decoding of that
  chirpix_<name>_<conv>.wav     chirpix sstv-encode, --convention spec / pysstv
  sstvpy_chirpix_<name>_<conv>.png  the sstv package's decoding of those
"""

import subprocess
import sys
from pathlib import Path

import sstv
from PIL import Image
from pysstv.color import Robot36


def run(*args):
    subprocess.run([str(a) for a in args], check=True, stdout=subprocess.DEVNULL)


def decode(wav, png):
    pictures = sstv.decode_from_wav(str(wav))
    if len(pictures) != 1 or pictures[0].info.get("sstv_mode") != sstv.Mode.ROBOT_36:
        sys.exit(f"{wav}: the sstv package found {len(pictures)} Robot 36 picture(s), expected 1")
    pictures[0].convert("RGB").save(png)


def main(chirpix, out):
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    for name in ("scene", "clouds"):
        png = out / f"{name}.png"
        run(chirpix, "testimage", name, "-o", png, "--size", "320x240")
        picture = Image.open(png).convert("RGB")
        Image.merge("RGB", picture.convert("YCbCr").split()).save(out / f"{name}_ycbcr.png")
        Robot36(picture, 48000, 16).write_wav(str(out / f"pysstv_{name}.wav"))
        decode(out / f"pysstv_{name}.wav", out / f"sstvpy_pysstv_{name}.png")
        for conv in ("spec", "pysstv"):
            wav = out / f"chirpix_{name}_{conv}.wav"
            run(chirpix, "sstv-encode", png, "-o", wav, "--convention", conv)
            decode(wav, out / f"sstvpy_chirpix_{name}_{conv}.png")
    print(f"fixtures in {out}")


if __name__ == "__main__":
    main(*sys.argv[1:3])
