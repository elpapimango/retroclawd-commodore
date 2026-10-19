# C64: the dead test's clocks run at a different rate (S7-clock, closed 2026-10-19)

**Closed 2026-10-19.** The time base is the CIAs' TOD clocks. A real 6526 counts the mains pulses on its TOD pin (50 Hz on a
PAL C64) and makes a tenth every 5 pulses when CRA bit 7 (`TODIN`) is set, every 6 when it is clear. The KERNAL sets `TODIN`
to match the machine; the dead test leaves it clear, so on a PAL machine its clocks run at 5/6 speed -- 14 x 5/6 = 11.7, the
`00-00-11` VICE shows. `cia-6526` used to make a tenth every 98 525 cycles whatever `TODIN` said; it now derives the mains
pulses from phi2 (exact fractional accumulator, 50 Hz PAL / 60 Hz NTSC) and divides them by 5 or 6
(`tod_divides_the_mains_pulses_by_five_or_six_as_todin_says`). After the fix both clocks read `00-00-11` at frame 2000. The
scenario's crop was also wrong (its lower edge reached the border, a third tone `normalize` cannot remove); it is now the
bottom text row only. With the new crop the old CIA fails (4.98 against a limit of 1), the fixed one passes.

The original finding:

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
