# VIC-20 implementation and open acceptance scenarios

Status: experimental, 2026-10-20. **Real PAL/NTSC firmware boots, BASIC typing,
tone pitch, state continuation and frame budgets pass in fast and accurate
modes.** Boot/input images match the installed VICE 3.9 reference after
palette and framing normalization. The manifest reports **20 passed, 0 failed,
5 known gaps**. This is not completion of the additional-machine request.

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

- `cargo test -p vic-i -p vic20 -p vic20-machine-plugin`: 20 tests passed
  (7 Chip, 7 Machine, 6 Plugin), including state continuation, malformed-state
  rejection, memory decode, timer visibility, interrupt routing, key combos,
  debugger execution, bounded requests and ABI-owned buffers.
- `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --release --workspace` and
  `cargo build --release --workspace`: passed. The workspace run reported
  325 tests passed and 25 ignored, including the subsequent C16 TAP tests.
  Firmware/software-gated bodies still skip
  if their particular input is absent.
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

`vic20.manifest.toml` retains all standard scenarios. S1/S2/S4/S5/S6 each
cover PAL/NTSC and fast/accurate. S3 and S7 remain explicit known gaps.
An uncaptured screen omits its golden and hash instead of inventing either.
The matching `rc-scenarios` change reports these as **not run**, never passed.
The initial real-ROM manifest run correctly rejected the now-passing S4/S5/S6
gap markers; the updated manifest passes without unexpected closed gaps.

### Reference capture recipe

Use the command in the manifest with the matching BASIC, regional KERNAL and
character ROM. All four drive types are disabled. `-limitcycles` deliberately
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

Cassette, cartridge and disk attachment remain unsupported. The existing
Commodore TAP/drive crates are reuse candidates, but sharing a Format does not
prove VIC-20 timing or device wiring. IEC pin accessors alone are not a drive.
No HDD interface is invented. Tape recording, media export/writeback and
external snapshot formats are likewise not implemented.

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

Pitch is measured over rising zero crossings after removing the window mean.
All four S4 cases pass the existing 1% pitch and 0.01 minimum-amplitude gates;
the NTSC window ends before its shorter 1000-frame run finishes.
These are tone-presence/pitch checks, **not amplitude fidelity**: Host gain is
lower than VICE. The mixer remains linear with RC filters. Nonlinear analogue
response, the volume curve, noise fidelity and sample-exact output remain open.

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
