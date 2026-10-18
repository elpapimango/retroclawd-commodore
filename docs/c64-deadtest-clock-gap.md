# C64: the dead test's clocks run at a different rate (S7-clock known gap)

Found 2026-10-18 by `c64.manifest.toml` scenario S7-clock.

crystalct's Unified Diagnostic Cartridge starts with a dead test that keeps two clocks at the bottom of the screen
(`00-00-11AM 00-00-11PM`). Run for 2000 frames:

| | clocks |
|---|---|
| VICE 3.10 (`x64sc -cartultimax` with the first 16 KiB of the image) | 00-00-11 |
| retroclawd (the same two 8K halves as its own cartridge type, `--frames 2000`) | 00-00-14 |

The title and the first two results (ZERO PAGE OK, STACK PAGE OK) are identical (scenario S7 passes). The dead test counts time
from a hardware time base; which one (CIA timer interrupts or the TOD clock) was not looked up here. Next step: read the
test's source or disassemble its clock routine, and compare how VICE and `cia-6526` run that source.
