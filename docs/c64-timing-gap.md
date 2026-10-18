# C64 timing: 19706 against 19656 cycles per frame (S6 known gap)

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
