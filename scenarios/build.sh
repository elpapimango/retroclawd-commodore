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
# VIC-20 raw-program and real KERNAL cassette/disk scenarios.
"$tools/rcasm" "$here/vic20/pattern.asm" -f prg -o "$out/vic20-pattern.prg" --force
"$tools/rcasm" "$here/vic20/tape.asm" -f prg -o "$out/vic20-tape.prg" --force
for region in pal ntsc; do
  "$tools/rctape" from-prg "$out/vic20-tape.prg" "$out/vic20-pattern-$region.tap" --region "$region" --name PATTERN --force
done
"$tools/rcdisk" create "$out/vic20-pattern.d64" --name VIC20 --id V2 --force
"$tools/rcdisk" add "$out/vic20-pattern.d64" "$out/vic20-tape.prg" --name PATTERN
# A real KERNAL-started, read-only BLK5 ROM; no direct PC injection.
"$tools/rcasm" "$here/vic20/cartridge.asm" -f bin -o "$out/vic20-pattern.a0" --force
"$tools/rcasm" "$here/vic20/cartridge.asm" -f prg -o "$out/vic20-cartridge.prg" --force
# The C128 BASIC option wants the two 16K halves as one 32K image.
cat "$here/../roms/c128/basiclo-318018-04.bin" "$here/../roms/c128/basichi-318019-04.bin" > "$out/c128-basic-32k.rom"
# The unified diagnostic cartridge as a .crt of retroclawd's own type (needs software/diagnostics/unified_32k.bin).
[ -f "$here/../software/diagnostics/unified_32k.bin" ] && python3 "$here/make_unified_crt.py" "$here/../software/diagnostics/unified_32k.bin" "$out/c64-unified-diag.crt"
