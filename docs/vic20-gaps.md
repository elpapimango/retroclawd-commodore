# VIC-20 implementation and open acceptance scenarios

Status: experimental, 2026-10-20. **No real VIC-20 firmware boot or reference
comparison has been verified.** This is a working implementation increment,
not completion of the additional-machine request.

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
  322 tests passed and 25 ignored. Firmware/software-gated bodies still skip
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

## Reference and firmware prerequisites

`vic20.intake.toml` and `CREDITS.md` pin the primary manual and reference
sources. VICE 3.10.0 `xvic` was not installed or executed. No lawful local
VIC-20 firmware set was found. Existing C64/C128 firmware was used only for
their regressions. No VIC-20 firmware was downloaded. No firmware, commercial
software, generated output or screenshot golden is committed.

`vic20.manifest.toml` retains every standard scenario as a known gap. A screen
without a reference capture omits its golden and hash instead of inventing
either. The matching `rc-scenarios` change accepts this only for an explicit
known gap with nonempty reason and evidence; it reports the comparison as not
run and never as passed. Once a real capture is added, ordinary hash,
comparison and unexpected-pass rules apply. Existing captured scenarios and
tolerances are unchanged.

## S1 boot

Missing real firmware and reference executable/capture. Synthetic reset-vector
execution does not establish a stable BASIC screen. Close with both modes,
PAL/NTSC firmware identity and a pinned reference boot capture.

## S2 input

Matrix scanning works in both directions, including low-line propagation.
MAME's actual `via2_pa_r` plus `COL` inputs establish Port B selection and
Port A input. KeyS selects PB5 / reads PA1. The earlier intake's interpretation
of the VICE diagram comment transposed the ports; the implementation follows
the actual input paths. Real KERNAL typing, shifted legends and ghosting still
need reference/diagnostic proof.

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
state continuation have unit checks. The manifest's prospective PAL tone is
1108405 / (31 * 16 * 16) = 139.67 Hz from the pinned divider/feedback model.
Its amplitude threshold is provisional acceptance data, not a measured reference
result. Capture and measure the real reference before removing the gap.
The mixer is linear with RC filters. Nonlinear analogue response and
sample-exact reference output are not claimed.

## S5 state and reset

Synthetic continuation and RAM-preserving reset pass. Repeat after a real
firmware boot, with held input and active sound. Media state cannot be verified
before those media devices exist.

## S6 timing

PAL: 1108405 Hz, 71 clocks x 312 lines = 22152 clocks/frame.
NTSC: 1022727 Hz, 16965 clocks/frame, including the 32/33-clock partial lines.
Chip tests and synthetic Host cycle budgets agree with these source numbers.
The manifest allows at most one ordinary instruction's frame-boundary
overshoot; it is not a raster-tolerance adjustment. A real reference execution
and pixel-aspect/crop calibration remain unperformed.

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
