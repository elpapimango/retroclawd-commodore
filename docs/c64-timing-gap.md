# C64 timing: 19706 against 19656 cycles per frame (S6 known gap)

**Closed 2026-10-19.** The C64 and the C128 report an exact frame rate of the clock over one raster frame
(`Vic::cycles_per_frame`: 312 x 63 PAL, 263 x 65 NTSC) through `Machine::frame_rate` and the plugins' `MachineMetadata::frame_rate`;
the host paces and budgets by it, so a frame is 19656 cycles (50.12 Hz) on PAL and 17095 (59.83 Hz) on NTSC. Audio pacing is
unchanged: the host still runs the clock's cycles per real second.

Found 2026-10-18 by `c64.manifest.toml` scenario S6.

| | clock | frame | per frame |
|---|---|---|---|
| VICE 3.10 PAL (x64sc) | 985248 Hz | 312 lines x 63 cycles, 50.125 Hz | 19656 |
| retroclawd (`rc-scenarios`, 100 frames: 1970631 cycles) | 985248 Hz | paced at 50 Hz | 19706 |

The VICE figure is the PAL machine's own raster (312 lines of 63 cycles); it was not read from the running VICE, which has
no command for it here, so it rests on the VICE constants and on the known C64 PAL timing. The emulator's frame is the clock
divided by exactly 50 Hz (19704.96, plus up to one instruction), 0.25 % longer than the real raster.

The C128 (40 column, 1 MHz mode, scenario S6 of `c128.manifest.toml`) shows the same: retroclawd 19706 (1970593 cycles for 100 frames),
VICE x128 19656 by the same raster.

No emulator code was changed to make the scenario pass. Next step: decide whether `frame_hz` for the PAL C64 should be
985248 / 19656 = 50.1245 Hz, and what that does to audio pacing.
