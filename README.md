# retroclawd-commodore

Commodore (experimental VIC-20, C64, C128 and the 1541/1570/1571/1581 drives) emulation for [retroclawd](https://github.com/elpapimango/retroclawd), split out of the main repository.
The machine logic lives here; the retroclawd application links none of it. It loads the
shared libraries this workspace builds (`vic20_machine_plugin`, `c64_machine_plugin`, `c128_machine_plugin`) at run time.

## Machines

- `c64` -- 6510 with a VIC-II (line or per-cycle accuracy, `--vic-accuracy`), SID (6581/8580), two CIAs, PAL and NTSC,
  keyboard and joysticks, paddles and 1351 mouse, `.tap`/`.t64` tape and `.prg`/`.p00` programs, `.d64`/`.d71`/`.d81`
  disks (writable; `.g64`, `.x64`, `.m3u` playlists), a large set of `.crt` cartridges, REU, GeoRAM, Swiftlink and
  RR-Net, a printer, KERNAL fast-load traps or a real 1541/1570/1571/1581 on the IEC bus (`--true-drive`).
- `c128` -- 8502 with the MMU, VIC-IIe and the VDC 8563 (40 or 80 columns), the Z80 boot hand-off, C64 mode, the same
  drives with burst mode, `--run` auto-typing and debugger disassembly that follows the running CPU (8502 or Z80).

The ROM files and how to call each machine: `docs/firmware.md`.

`vic20` is experimental: NMOS 6502, VIC-I video/sound, two VIAs, PAL/NTSC,
keyboard, joystick, selectable RAM expansion, raw/PRG programs, read-only VIC-20
TAP-v0/v1 playback, a real 1541 for writable D64 Media, read-only generic ROM cartridges and deterministic
save states. It reuses `mos6502` and `via6522`, not VIC-II or SID. Real PAL/NTSC
boots, BASIC typing, tone pitch, state continuation and frame budgets pass in
both execution modes: 54 scenarios pass, with boot/input/program/cassette/disk/cartridge output
compared against hash-pinned VICE 3.9 captures. External diagnostics remain
an explicit gap. Tape headers must declare platform 1 and
the machine's PAL/NTSC region. Attach presses PLAY; VIA1 controls the motor.
Use `--true-drive --disk game.d64`; supply `--rom-1541` or use `--firmware auto`.
The existing 1541/IEC implementation runs the real DOS, without a fast-load trap.
Drive stepping remains instruction-granular in both execution modes.
Disk export, writeback, swapping, reset and complete Drive states are wired.
`--cartridge game.a0` accepts raw `.20/.40/.60/.a0/.b0` or load-address-prefixed
`.prg/.bin/.rom` generic ROMs. Attach/eject resets through the KERNAL; Save states
require the same mapped ROM. Overlapping RAM expansion is rejected.
Other disk formats/Drive models, recording, banked/flash/RAM cartridges and
VIC CRT containers are unsupported. Exact layouts are in `docs/vic20-gaps.md`.
Calibrated single-voice audio RMS agrees within 1%; full hardware fidelity is not claimed.
See `vic20.intake.toml`, `vic20.manifest.toml` and `docs/vic20-gaps.md`.

## Execution modes

The C64 and C128 Plugins accept `--execution-mode fast|accurate` (configuration key
`execution_mode`). Fast is the default and uses the existing line engine:
CPU instructions batch raster and peripheral advancement, including approximate
bad-line cycle stealing. Accurate selects the existing cycle engine: each
1 MHz 6510/8502 bus access advances the VIC-II/VIC-IIe schedule, including
mid-line register effects and BA read stalls for bad lines and sprite DMA.
This changes hardware behavior, not Host pacing. Accurate remains PAL-only;
requesting it on NTSC is an error.

The C128 Z80 and 2 MHz 8502 paths remain instruction-batched and are not stalled
by BA. The VDC path does not gain additional fidelity from this Option.
Tape fast loading and drive emulation remain independent Options, unchanged
for compatibility. On the C64, disable `fast_tape_load` in configuration and
use `--true-drive` with the corresponding drive firmware for real-time media.
Accurate does not claim that every peripheral is cycle-exact.

The older `--vic-accuracy line|cycle` Option remains supported. It selects the
same engines (`line` = `fast`, `cycle` = `accurate`). With neither Option set,
the Plugin selects fast/line; neither schema injects an implicit wire value
that could mask the other. Contradictory explicit selections are errors,
including selections from different configuration layers: precedence applies
per Option key, not across aliases. Prefer one key in a layered configuration.

Save states already record and validate the engine; a state made in one mode
cannot load in the other. No state format, ABI, or protocol change is needed.
The Plugin tests compare the new and legacy engines, deterministic continuation
after state restoration, cross-mode rejection, and invalid or conflicting
Options. Existing Machine cycle tests cover raster timing and BA behavior.
No relative performance measurements have been collected for these aliases.

The VIC-20 uses the same Option. Fast advances Chips after each instruction;
accurate advances them on each CPU read, write and dummy cycle. A timer-read
regression demonstrates different bus-visible results with identical elapsed
clocks. Both modes work on PAL and NTSC. Neither models sub-clock VIC-I fetch
phases, interlace, analogue paddles or light-pen latching. The sound mixer uses
VICE's nonlinear voltage calibration with RC filters. Save states
reject different firmware, Region, RAM expansion or execution mode. No controlled
relative-performance benchmark or real-game compatibility run has been performed.

## Build

```
cargo build --release --workspace
```

produces, in `target/release/`, one library per machine
(`lib<id>_machine_plugin.so`, `<id>_machine_plugin.dll`, `lib<id>_machine_plugin.dylib`).
Put them in the `plugins/` folder next to the `retroclawd` executable (or point
`RETROCLAWD_MACHINE_PLUGIN_DIRS` at the folder) and run `retroclawd <machine> ...`.

## Shared code

Everything that is not specific to this family -- `machine`, `plugin-api`, the `z80`/`mos6502` cores and the
chips/formats several families use -- comes from
[`retroclawd-crates`](https://github.com/elpapimango/retroclawd-crates) as a git dependency pinned to the commit
`30bbca84f53177148c6ed4ae2b37597747dec654` (the `git = ..., rev = ...` entries in each crate's `Cargo.toml`). To move to a newer commit, run
`tools/bump_pin.sh` in the application repository (it rewrites the pins, runs the tests and records the result), or
by hand: change the rev in every manifest, run `cargo update`, and check the plugin ABI policy in
`retroclawd-crates/docs/plugin-abi.md` (a plugin must be built with the same ABI major, and a protocol version the
application speaks).

The shared crates come from the public repository `retroclawd-crates`, pinned by commit (`rev = ...` in the
`Cargo.toml` files); `tools/bump_pin.sh` in the host repository moves the pin.

This feature branch temporarily pins the pushed shared execution-mode change
for review. Merge its shared-crates PR first, then verify or refresh this pin
to the merged shared revision before merging this Family.

## C16/Plus4 tape-format prerequisite

`c64-tape::ted::TedTape` reads and writes `C16-TAPE-RAW` version-2 half-wave
recordings. It reuses the existing Commodore pulse decoder while keeping C16
half-waves separate from C64 playback. Durations use the TED slow timer clock:
886724 Hz PAL or 894886 Hz NTSC, independent of CPU speed.

The new API rejects mismatched sizes, unsupported header fields, zero/oversized
durations, truncated escapes and payloads above 32 MiB. Writing preserves exact
duration/order without rounding or clipping; encoding is canonical, not a
byte-identical copy of noncanonical input. It does not decode tape files or
connect a C16/Plus4 Machine, and it performs no writeback. The legacy C64 API
and its compatibility behavior are unchanged.

## Layout

The directory layout is the same as in the original repository, because the tests find ROMs and
software relative to `CARGO_MANIFEST_DIR/../../../`.

- `crates/machines/c64`
- `crates/machines/vic20`
- `crates/machines/c128`
- `crates/machines/1541`
- `crates/machines/drive-link`
- `crates/plugins/c64-machine-plugin`
- `crates/plugins/vic20-machine-plugin`
- `crates/plugins/c128-machine-plugin`
- `crates/plugin-dlls/c64`
- `crates/plugin-dlls/vic20`
- `crates/plugin-dlls/c128`
- `crates/formats/c64-tape`
- `crates/formats/c64-cartridge`
- `crates/formats/vic20-cartridge`
- `crates/formats/d64`
- `crates/formats/d71`
- `crates/formats/d81`
- `crates/formats/g64`
- `crates/formats/cbm-dos`
- `crates/chips/sid`
- `crates/chips/vic-ii`
- `crates/chips/vic-i`
- `crates/chips/via-6522`
- `crates/chips/wd1770`
- `crates/chips/cia-6526`
- `crates/chips/acia-6551`
- `crates/chips/cs8900a`
- `crates/chips/vdc-8563`

ROM images and software are never committed: `roms/` and `software/` are git-ignored folders for your
own copies. Tests that need them skip when they are absent.

## Tests

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Long-running validation against the real 1541 DOS is `#[ignore]`d: `cargo test --release -p c64 --test real_1541 -- --ignored`.
The ROM-gated boot tests read the files under `roms/` and skip without them.

## License

Free software under the **GNU General Public License v3.0 or later** (see `LICENSE`). No ROM image or commercial
software is part of this repository or its history; supply your own (see `docs/firmware.md`). Third-party sources
that were read for reference are credited in `CREDITS.md`.

## Contributing / issues

Not accepted right now -- this is a personal work-in-progress project. Pull requests and issues will be closed
unreviewed.
