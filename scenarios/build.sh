#!/usr/bin/env bash
# Builds the media the scenario manifests load, with retroclawd-tools, into software/scenarios/ (not committed).
#   scenarios/build.sh [path to the retroclawd-tools target dir]   default ../../../../retroclawd-tools/target/debug
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tools="${1:-$here/../../../../retroclawd-tools/target/debug}"
out="$here/../software/scenarios"
mkdir -p "$out"
for ext in prg d64; do
  "$tools/rcld" build "$here/c64/hello.toml" -o "$out/c64-hello.$ext" --force
done
# The C128 BASIC option wants the two 16K halves as one 32K image.
cat "$here/../roms/c128/basiclo-318018-04.bin" "$here/../roms/c128/basichi-318019-04.bin" > "$out/c128-basic-32k.rom"
