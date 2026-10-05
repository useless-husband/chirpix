#!/bin/sh
# Download four pictures of the Kodak Lossless True Color Image Suite
# (768x512, released by Eastman Kodak for unrestricted use) into data/,
# which is not part of the repository. Each file is checked against a
# SHA-256 recorded when this script was written.
set -eu
cd "$(dirname "$0")/.."
mkdir -p data
BASE=https://r0k.us/graphics/kodak/kodak
fetch() {
  name=$1
  want=$2
  if [ -f "data/$name" ] && [ "$(shasum -a 256 "data/$name" | cut -d' ' -f1)" = "$want" ]; then
    echo "data/$name ok"
    return 0
  fi
  echo "downloading $name"
  curl -fsSL --max-time 120 -o "data/$name.part" "$BASE/$name"
  got=$(shasum -a 256 "data/$name.part" | cut -d' ' -f1)
  if [ "$got" != "$want" ]; then
    rm -f "data/$name.part"
    echo "checksum mismatch for $name: got $got" >&2
    exit 1
  fi
  mv "data/$name.part" "data/$name"
}
fetch kodim23.png e3111a2fd4da24af15d6459ef9eacfe54106b38e27b4a21821b75c3f5d2d5baf
fetch kodim19.png b7450b264b1b0a411390d8931b112c27905a992520fc90569dc4b920aa32bbdc
fetch kodim05.png 10349e963c5c813d327852f82c1795fa4148d69fedffc4c589bee458e3ac3d53
fetch kodim08.png ba23983c76b4832ee0e8af0592664756841a16779acd69f792e268fb6d13d6e7
