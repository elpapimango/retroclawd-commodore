# VIC-20 implementation and open acceptance scenarios

Status: experimental, 2026-10-20. **Real PAL/NTSC firmware boots, BASIC typing,
tone pitch, state continuation and frame budgets pass in fast and accurate
modes.** Boot/input images match the installed VICE 3.9 reference after
palette and framing normalization. Tools-built PRG output also matches.
The manifest reports **38 passed, 0 failed, 2 known gaps**.
This is not completion of the additional-machine request.

## Implemented and checked

The Machine uses the existing NMOS `mos6502` Core and two `via6522` Chips.
The new `vic-i` models programmable text origins/dimensions, 8x8/8x16
characters, reverse and multicolor characters, color RAM, raster readback,
three tone voices, noise and filtered mono audio. The Machine supplies its
own address decode, VIC/CPU address translation, expansion RAM, keyboard,
joystick and distinct VIA1 NMI / VIA2 IRQ wiring.

Fast clocks peripherals after an instruction. Accurate clocks every real CPU
bus event, including dummy accesses. A four-cycle timer read sees 20 in fast
and 16 in accurate with the same elapsed clocks. This is an observable hardware
timing difference, not slower Host pacing. VIC-I uses bus phases, not C64
bad-line stalls. PAL and NTSC are both accepted.

The dynamic Plugin exposes machine-owned Options and keys, memory spaces,
registers and status flags, disassembly, single-step, bounded step-over,
breakpoints, Chip snapshots and the overlay. The existing Host debugger owns
watches and symbols. Unsupported I/O-port requests and media operations fail
explicitly. ABI 1.2, protocol 11 and profile contract 2 remain unchanged.

Save states validate firmware identity, Region, RAM expansion, execution mode,
buffer sizes, counters and finite audio filters before assignment. Reset
preserves RAM. Raw program attachment preflights the entire RAM range before
mutation; it does not jump or patch BASIC pointers. The existing Host parses
PRG/P00 headers, so no duplicate file parser was introduced.

Validation executed:

- Disk increment: `cargo test --release -p c1541 -p drive-link -p vic20
  -p vic20-machine-plugin -p c64 -p c128` passed 169 tests, with 25 explicit
  real-firmware ignores. Focused release Clippy with `-D warnings`, formatting,
  the dynamic Plugin build and all 38 real-firmware manifest cases passed.
  Additional real 1541 LOAD, dirty-media writeback and mid-transfer continuation
  checks cover both Regions and execution modes; details appear below.
- Cassette increment: `cargo test --release -p c64-tape -p vic20 -p vic20-machine-plugin -p c64`
  passed 129 tests, with 10 explicit real-drive ignores. The unchanged C64
  real-KERNAL cassette-load/save cases passed. Targeted release Clippy with
  `-D warnings`, format check and dynamic Plugin build passed.
- `rctape` passed 14 tests, including new PAL/NTSC PRG encoding, metadata,
  structure validation, exact-pulse normalization and overwrite/input refusal.
  Its existing Spectrum firmware-dependent bodies remained fixture-gated;
  those runner passes are not new Spectrum execution proof.
  That cassette-stage real-ROM VIC-20 manifest reported 30 passes and 3 known gaps.
- `cargo test --release -p vic-i -p vic20 -p vic20-machine-plugin`: 21 tests
  passed (8 Chip, 7 Machine, 6 Plugin), including the new reference-RMS
  regression, state continuation, malformed-state
  rejection, memory decode, timer visibility, interrupt routing, key combos,
  debugger execution, bounded requests and ABI-owned buffers.
- Earlier full-workspace `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --release --workspace` and
  `cargo build --release --workspace`: passed. The workspace run reported
  325 tests passed and 25 ignored, including the subsequent C16 TAP tests.
  Firmware/software-gated bodies still skip
  if their particular input is absent.
- After the mixer fix, format check, targeted release Clippy with `-D warnings`
  and the release VIC-20 Plugin build passed. No shared Core, Host source,
  state layout or ABI change was required.
- `cargo test --release -p c64 -p c128 -- --ignored`: C128 boot/cycle/drive
  groups passed (7 + 4 + 4). Two C64 drive cases initially lacked their local
  software files. After making the existing ignored software folder available,
  `cargo test --release -p c64 --test real_1541 -- --ignored` passed all 10.
  Missing-file failures were not hidden or fixed with emulator changes.
- The release Host loaded `vic20_machine_plugin.dll` and discovered its
  Options. Synthetic firmware programmed VIC video/audio and polled VIA2.
  All four PAL/NTSC x fast/accurate combinations executed successfully.
  Five PAL frames produced a 284x312 image and 4407 audio samples; five NTSC
  frames produced 260x262 and 3657 samples. A scripted KeyS press read `$FD`
  through the emulated CPU. Saving at frame 5 and running 3 more frames
  produced the exact same state as an uninterrupted 8-frame run. Cross-mode
  state loading failed. These are integration checks, **not firmware boots**.

No controlled performance benchmark was run. Short synthetic executions took
milliseconds and are not a meaningful fast/accurate comparison.

## Reference and firmware verification

`vic20.intake.toml` and `CREDITS.md` pin the primary manual and reference
sources. The Host fetched the four exact firmware images from the configured
RomM through `--firmware auto`; every cached file passed its SHA-1 check.
The same bytes were used by the reference. Firmware identity is recorded in
the intake; availability does not establish redistribution permission.
No firmware, commercial software, generated output or screenshot golden is
committed.

The installed GTK3 Win64 `xvic.exe` identifies itself in its logfile as
**VICE 3.9**. Its SHA-256 is
`a385207b21df29459f5304bf6ed1da90c974512ead3a7ce1e340c46b01d3134f`.
This is the identity of the executable actually used, not a claim that its
build was independently reproduced from the separately pinned 3.10 sources.

`vic20.manifest.toml` retains all standard scenarios. S1/S2/S3-program/S3-tape/S3-disk/S4/S5/S6
cover PAL/NTSC and fast/accurate; S4 also checks PAL volume 7 in both modes.
Cartridge and S7 remain explicit known gaps.
An uncaptured screen omits its golden and hash instead of inventing either.
The matching `rc-scenarios` change reports these as **not run**, never passed.
The initial real-ROM manifest run correctly rejected the now-passing S4/S5/S6
gap markers; the updated manifest passes without unexpected closed gaps.

### Reference capture recipe

Use the command in the manifest with the matching BASIC, regional KERNAL and
character ROM. Boot, input, Program, tape and audio captures disable all four
Drives; disk captures enable a real 1541 as described below. `-limitcycles` deliberately
returns exit code 1; accept a capture only when its logfile identifies VICE
3.9 and says `cycle limit reached.`, and the requested screenshot exists.

| Region | S1: 180 frames | S2: 500 frames | S4: 1000 frames | Text rectangle in raw VICE PNG |
|---|---:|---:|---:|---|
| PAL | 3987360 | 11076000 | 22152000 | `[96, 48, 448, 232)` in 568x284 |
| NTSC | 3053700 | 8482500 | 16965000 | `[40, 22, 392, 206)` in 520x234 |

S2 adds `-keybuf 'print 12*34\n'`. For S4 replace `+sound -warp` with
`-sound +warp -sounddev wav -soundarg OUTPUT.wav`, and add
`-keybuf 'poke36878,15:poke36874,224\n'`. Use the WAV output device directly:
the dummy device with a separate WAV recorder produced only an invalid
44-byte, zero-channel header, even outside warp mode. Those files were not
accepted as audio evidence. The successful captures are 48000 Hz mono PCM16.

The four committed golden hashes identify **176x184 logical-pixel text
images**, not the raw doubled-width reference PNGs. Crop the rectangle above
and remove one column from each identical pair. The following conversion uses
the existing tools PNG codec and refuses nonidentical pairs. Save it outside
the repository and invoke it as
`python convert.py <retroclawd-tools> <raw.png> <golden.png> <pal|ntsc>`.

```python
import sys
from pathlib import Path

sys.path.insert(0, str(Path(sys.argv[1]) / "scripts"))
from capture_vice_screen import decode_png, encode_png

source, target, region = sys.argv[2:]
left, top = {"pal": (96, 48), "ntsc": (40, 22)}[region]
_, _, rows = decode_png(Path(source).read_bytes())
logical = []
for row in rows[top:top + 184]:
    row = row[3 * left:3 * (left + 352)]
    pixels = [row[x:x + 3] for x in range(0, len(row), 3)]
    assert pixels[::2] == pixels[1::2]
    logical.append(b"".join(pixels[::2]))
Path(target).write_bytes(encode_png(176, 184, logical))
```

The Host rectangles are `[48, 76, 224, 260)` in PAL 284x312 and
`[20, 50, 196, 234)` in NTSC 260x262. Manifest crop fractions sit a quarter
pixel inside each integer boundary to avoid floating-point truncation selecting
the previous pixel. Each selected interval remains exactly 176x184.
Goldens use the full image. Normalize luminance to remove palette differences;
the manifest permits at most 0.01 mean difference, not a glyph tolerance.
An independent full-resolution foreground-mask comparison also found **zero
of 32384 pixels different** in every boot/input region/mode combination.

Run the manifest with `RETROCLAWD_BIN` pointing to the release Host,
`RETROCLAWD_MACHINE_PLUGIN_DIRS` to the built libraries, `RC_ROMS` to the
private fixture tree named by `[files]`, and `RC_GOLDENS` to the private
golden folder:

```
rc-scenarios run vic20.manifest.toml
```

## S1 boot

Both modes and regions reach `CBM BASIC V2`, `3583 BYTES FREE` and `READY.`
after 180 frames. The four S1 cases pass against hash-pinned VICE captures.
The comparison covers the entire text area, including the captured cursor,
not the differently cropped border or full palette calibration.

## S2 input

Matrix scanning works in both directions, including low-line propagation.
MAME's actual `via2_pa_r` plus `COL` inputs establish Port B selection and
Port A input. KeyS selects PB5 / reads PA1. The earlier intake's interpretation
of the VICE diagram comment transposed the ports; the implementation follows
the actual input paths. All four S2 cases type `PRINT 12*34` through the real
KERNAL and display `408`, matching VICE after 500 frames. This exercises the
shifted asterisk combination as well as ordinary keys. Other shifted legends,
simultaneous-key ghosting and unusual scan code still need diagnostic proof.

## S3 media

Raw programs, PRG/P00 Host parsing and raw Plugin attachment are implemented.
`scenarios/vic20/pattern.asm` can be assembled with existing `rcasm`:

```
rcasm scenarios/vic20/pattern.asm -f prg -o software/scenarios/vic20-pattern.prg
```

Create the ignored output folder first. The program initializes its own
display; this is not a firmware-booted BASIC program or a loader trap.

The tools-built Program has a 55-byte payload at `$1000-$1036` (57-byte PRG
including its load-address header). It sets a visible
origin at CPU picture `(48, 76)` in both regions, draws character 1 in red
on white, enables a tone and loops. All four S3-program cases match the
reference's full 8x8 glyph after palette normalization.

For VICE, put the PRG in the capture working folder and add
`-initbreak ready -moncommands program.mon` to the bounded S1 command:

```
load "vic20-pattern.prg" 0
g 1000
```

`-initbreak ready` matters: the default startup monitor runs before the
KERNAL reset routine, which overwrote the first attempted PRG capture with
the BASIC screen. That capture was rejected. The corrected captures show the
program's red glyph, not the boot banner. This startup behavior is documented
in <https://vice-emu.sourceforge.io/vice_6.html>, section 6.17.2.
Crop raw VICE `[96, 48, 112, 56)` and remove duplicated columns to make an
8x8 golden. Use the earlier PNG conversion with height 8, raw width 16 and
final width 8. The Host crop is `[48, 76, 56, 84)` in either region.
All 64 foreground-mask pixels match independently in each mode and region.

### Cassette playback

Read-only TAP-v0/v1 playback now passes all four region/mode cases through
the real KERNAL, without a loader trap. `c64-tape::vic20` validates platform 1,
PAL/NTSC, reserved byte, exact payload size, complete escapes and positive
24-bit durations, with a 32 MiB payload limit. Platform-0 legacy recordings
and region mismatches are refused rather than silently assigned another clock.
The legacy C64 parser/player behavior is unchanged.

Attach presses PLAY: VIA1 PA6 SENSE goes low, VIA1 CA2 controls the active-low
motor, and each completed full pulse delivers one falling READ edge to VIA2
CA1. Alternating the generic player's legacy phase would discard every second
falling edge; both existing C64 wiring and the new VIC wiring use pulse counts.
Motor-off preserves pulse phase; eject releases SENSE. The runtime/debug media
snapshot and overlay report tape position, total pulses and motor state.
Reset preserves tape phase and restores idle-high READ on the recreated VIA.
The reset regression first failed because VIA reset left CA1 low, losing the
next falling edge; it passes after restoring the connected tape's idle level.

Build the cassette with the existing tools:

```
rcasm scenarios/vic20/tape.asm -f prg -o software/scenarios/vic20-tape.prg
rctape from-prg software/scenarios/vic20-tape.prg software/scenarios/vic20-pattern-pal.tap --region pal --name PATTERN
rctape from-prg software/scenarios/vic20-tape.prg software/scenarios/vic20-pattern-ntsc.tap --region ntsc --name PATTERN
```

The 55-byte machine-code payload lives at `$1200-$1236`, leaving the empty
BASIC program at `$1001` intact. An initial cassette using the raw-program
fixture at `$1000` was rejected: BASIC relinked its loaded bytes after LOAD.
Both fixtures now include one shared assembly body; the original `$1000`
PRG was verified byte-identical after that extraction.
The KERNAL encoder reuses the existing Commodore header, duplicate blocks,
parity and checksums, converting standard pulse durations to VIC timer clocks.

The Host types `LOAD"PATTERN",1,1` from frame 150, then `SYS4608` through
scripted key presses from frames 1900-1945. At frame 2200 every payload byte
matches and PC is `$1234`, the expected loop. Both modes and regions match all
64 foreground pixels of independently captured VICE glyphs; no tolerance changed.

For VICE use the same bounded command, with 48734400 PAL or 37323000 NTSC
cycles, and add `+virtualdev1 -1 TAPE -initbreak ready -moncommands cassette.mon`
and `-keybuf 'load"pattern",1,1\nsys4608\n'`. `+virtualdev1` disables the
fast tape mechanism. `cassette.mon` contains:

```
tapectrl 1
g
```

Crop and deduplicate exactly as for S3-program. The resulting cassette golden
hashes happen to equal the corresponding program hashes because the same glyph
and palette result; each was derived from its actual cassette capture.

State version 2 introduced a SHA-1 recording identity and playback position without
changing the old hardware prefix. Current version 3 retains both fields and
adds the Drive checkpoint. Versions 1/2 still load without a Drive.
Wrong/missing recordings and corrupt positions fail before mutation.
All four real-KERNAL mid-load saves at frame 600 followed by 1600 frames
produce byte-identical states to uninterrupted 2200-frame runs.

Only speed 1 is firmware-verified. `tape_speed` scales pulse durations against
CPU time; it is not a transparent stock-ROM loading accelerator. Use Host
fast-forward for faster wall-clock loading. Motor inertia and physical
rising-edge phase are not modeled: READ returns high immediately after each
falling edge. Recording and tape export remain unsupported.

### 1541 disk

`--true-drive` enables the existing `drive-link` and 1541 Core at IEC device 8.
Supply its 16 KiB DOS through `--rom-1541`, or let `--firmware auto` select
the existing hash metadata. The default remains no Drive; the Plugin refuses
disk attachment without it and refuses an unused DOS ROM without the Option.
Only D64 and Drive 0 are advertised; D71/D81/G64 and other Drive models are
not silently accepted.

The Machine owns the Drive and advances the link for each elapsed clock, so
debugger stepping and both CPU execution engines reach the same peripheral
path. The Drive CPU itself still steps whole instructions, with fractional
clock-ratio debt; this is not cycle-synchronous Drive emulation.
VICE's pinned `vic20iec.c` establishes the physical output inversion.
The original pin accessors incorrectly inverted all three assertion values;
a failing-then-passing regression checks high VIA outputs assert ATN/CLOCK/DATA
low and PA0/PA1 read the resulting physical input levels.

Build the D64 with existing tools and the safe `$1200` cassette payload:

```
rcdisk create software/scenarios/vic20-pattern.d64 --name VIC20 --id V2
rcdisk add software/scenarios/vic20-pattern.d64 software/scenarios/vic20-tape.prg --name PATTERN
retroclawd vic20 --firmware auto --true-drive --disk software/scenarios/vic20-pattern.d64
```

The Host types `LOAD"PATTERN",8,1` from frame 150 and `SYS4608` from
frames 1000-1045. After 1200 frames all 55 payload bytes match, PC is `$1234`,
and all 64 glyph foreground pixels match VICE in PAL/NTSC and fast/accurate.
No KERNAL or DOS loader trap is used. The disk goldens were independently
captured; their canonical hashes equal the corresponding Program/tape hashes.
The existing 0.01 similarity bound and crop conversion are unchanged.

For the reference command replace `-drive8type 0` with `-drive8type 1541`,
keep Drives 9-11 disabled, and add:

```
-dos1541 DOS -drive8truedrive +virtualdev8 -8 vic20-pattern.d64
-keybuf 'load"pattern",8,1\nsys4608\n'
```

Use 26582400 PAL or 20358000 NTSC cycles. Installed VICE 3.9 uses the
per-Drive `-drive8truedrive` switch, not the rejected global `-truedrive`.
Its DOS is the same hash-verified
`ab16f56989b27d89babe5f89c5a8cb3da71a82f0` used by retroclawd.
Accept only actual screenshot output plus the version and cycle-limit log
markers. Crop raw `[96, 48, 112, 56)` and deduplicate columns as above.

Attach/eject, the existing 0.3-second swap gap, runtime/debug disk status,
track/motor overlay, reset, disk export, mark-saved and Host writeback are
wired. Real DOS serial writes were checked in all four Region/mode cases:
`OPEN1,8,1,"WRITE,S,W":PRINT#1,"VIC20":CLOSE1` followed by `--disk-save`.
Existing `rcdisk extract` recovered exactly `VIC20` plus carriage return.
Restoring each dirty Save state with the original mounted D64 exported a
byte-identical modified D64, proving both medium bytes and unsaved flags survive.

Version 3 validates DOS and original-media identity before restoring a staged
Drive, then commits the Machine only after every check passes.
The new complete GCR checkpoint preserves the IEC bus, fractional clock debt,
pending insertion and countdown, mounted medium, GCR cache, dirty track, write
shifter, mechanism phase and unsaved flag. Legacy C64/C128 Drive state APIs
and layouts are unchanged. Unit regressions cover pending swaps, active writes,
both execution modes and atomic failure. Real mid-transfer saves at frame 350
(payload still unloaded) plus 850 frames equal direct 1200-frame states byte
for byte in all four cases. Four S5-disk cases also check mounted-Drive
continuation in the manifest.

Cartridge attachment, HDD interfaces, tape recording/export and external
snapshot formats remain unimplemented. Native desktop disk dialogs were not
manually exercised in this increment; they use the existing Host media path.

## S4 audio

Divider/shift-register, noise phase, volume integration, reset and deterministic
state continuation have unit checks. The real KERNAL accepts the typed POKEs
and enables voice 1 at volume 15. The divider predicts PAL
1108405 / (31 * 16 * 16) = 139.667969 Hz and NTSC
1022727 / (31 * 16 * 16) = 128.871850 Hz. Recorded measurements:

| Region/window | VICE pitch | Host pitch, both modes | VICE DC-free RMS | Host DC-free RMS |
|---|---:|---:|---:|---:|
| PAL, 17-19 s | 139.667808 Hz | 139.668161 Hz | 0.155869 | 0.085344 |
| NTSC, 13-15 s | 128.871849 Hz | 128.872164 Hz | 0.151126 | 0.082637 |

The Host column above records the **original linear-mixer regression**, not
the corrected output. The cause was the missing nonlinear voltage transfer
and filter update order, not Host volume, resampling or an arbitrary gain.
The source calibration and filter order predict an RMS ratio of 1.8287,
against observed ratios of 1.8264 PAL and 1.8288 NTSC.

The fix applies the calibrated transfer to the cycle-integrated voice/volume
level and updates the RC filter state in the reference order. Thirty-nine
knots reproduce all 436 pinned calibration entries with maximum checked f32
error 0.008516 PCM16 units. The weighted accumulator and serialized layout
keep their existing meaning. No state-format or ABI bump is involved.

| Region/volume | VICE DC-free RMS | Corrected Host RMS, both modes | Relative error |
|---|---:|---:|---:|
| PAL, 15 | 0.155869 | 0.156080 | 0.135% |
| NTSC, 15 | 0.151126 | 0.151131 | 0.004% |
| PAL, 7 | 0.075602 | 0.075613 | 0.015% |

The new focused regression failed on the old mixer (PAL RMS 0.085455).
It now checks RMS within 1% against these independently recorded reference
features, including lower volume to reject a simple fitted gain multiplier.
Actual release Host recordings meet the same bound in all six runs.

Pitch is measured over rising zero crossings after removing the window mean.
All six S4 cases pass the existing 1% pitch and 0.01 minimum-amplitude gates;
the NTSC window ends before its shorter 1000-frame run finishes.
The manifest gates tone presence/pitch; the focused regression and separate
recordings establish RMS agreement. Polyphonic phase, noise fidelity,
analogue variation beyond the reference calibration and sample-exact output
are not claimed.

## S5 state and reset

All four real-ROM S5 cases verify two identical fresh states, then byte-identical
180+50-frame saved continuation versus an uninterrupted 230-frame run.
Separate real-ROM runs type the tone POKEs, hold KeyS from frame 450, save at
500 and resume for 50 frames: every region/mode matches its uninterrupted
550-frame state byte for byte. Loading a state into the other execution mode
is refused. RAM-preserving reset has unit coverage. Media state cannot be
verified before those media devices exist.

## S6 timing

PAL: 1108405 Hz, 71 clocks x 312 lines = 22152 clocks/frame.
NTSC: 1022727 Hz, 16965 clocks/frame, including the 32/33-clock partial lines.
Chip tests and real-ROM Host cycle budgets agree with these source numbers.
The manifest allows at most one ordinary instruction's frame-boundary
overshoot; it is not a raster-tolerance adjustment. All four S6 cases pass
with the unchanged eight-cycle allowance. The 180-frame boot runs average
22153.2222/22153.2389 PAL clocks and 16966.4611/16966.4944 NTSC clocks in
fast/accurate respectively. Text crops are calibrated against real reference
captures; full-border geometry and pixel-aspect calibration remain open.

## S7 diagnostics

No external VIC-20 diagnostic has been selected and license-verified. Do not
claim the focused tests establish hardware conformance or game compatibility.

## Additional fidelity gaps

Sub-clock VIC-I fetch phases and register delays, display flip-flop tricks,
interlace, light-pen latching and analogue paddle conversion are not modeled.
Per-pixel character fetches approximate the real phase schedule. The existing
VIA shift-register model has documented limitations. Serial/printer/user-port
peripherals are not wired. The overlay reports an interlace request, but that
does not mean the video mode is emulated.
