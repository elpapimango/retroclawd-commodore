# TODO / Roadmap

Backlog for the Commodore (C64, C128, drives) family, moved from the host repository's TODO.md. Host-wide items (plugin architecture, input, shaders, parked machines) stay in the host.

## Commodore 64 roadmap

### DONE: baseline

The C64 boots real KERNAL, BASIC, and character ROMs to `READY.`. Implemented hardware includes:

- Complete NMOS 6502 core, validated against Klaus Dormann and Tom Seddon's external suites.
- PLA/6510 memory banking.
- VIC-II text mode and live raster-position registers.
- CIA1 Timer A IRQ and keyboard scanning.
- Internal save states and GUI input/video.

The live VIC raster counter is required by real KERNAL startup code; keep the existing regression
coverage for that boot-blocking bug.

### BACKLOG: external parity execution sequence (gap vs VICE/libretro VICE)

VICE/x64sc sets the practical parity bar for C64 behavior (cycle/pixel-accurate VIC-II, SID
variants/reSID path, dual-CIA behavior, broad cartridge and media workflows). libretro VICE also
surfaces a broad user-facing media matrix (`.d64/.d71/.d81/.g64/.x64/.nib`, `.tap/.t64`,
`.crt/.prg/.p00`, `.m3u/.zip/.cmd`) and disk-swap workflows.

Strict sequence for C64 parity work (do not reorder):

1. Core chips parity: SID, VIC-II, then CIA2 completion.
2. Drive and baseline media parity: IEC + 1541, then `.d64`/`.prg`/`.tap`/`.t64` workflows.
3. Broader C64 media/workflow parity toward VICE/libretro coverage.
4. Expansion and peripheral parity.
5. Configuration and input parity.

### core parity track — SID

**DONE: phase 1.** `crates/chips/sid` (package `sid`) implements the MOS
6581/8580 SID's 3 voices: 24-bit phase accumulator, triangle/sawtooth/pulse/
noise waveform generation (including hard sync and ring modulation), the
23-bit noise LFSR, the ADSR envelope generator (linear attack, exponential
decay/release matching the chip's own published rate-counter and divisor
tables), and the full 25-register read/write map. Combined-waveform
selection uses the common bitwise-AND approximation, and the real ADSR
retrigger delay bug is not modeled -- both documented in `CREDITS.md`. Pure
chip crate, no dependency on `machine`/`mos6502`/an audio backend, same
shape as `ay-3-8910`.

**DONE: phase 2 — filter.** `crates/chips/sid`'s `Filter` implements the
real state-variable (multi-mode) filter topology via the classic discrete
"Chamberlin" recurrence, stepped once per real system clock cycle in
`Sid::tick_one_cycle` (continuous integration, matching the real analog
filter's own always-running behavior -- the filter's input is the routed
voices' continuously-changing instantaneous levels, which `output()` alone
has no way to reconstruct at its own much-slower audio-sample-rate query
points). Per-voice filter routing (`$17` bits 0-2), resonance (`$17`'s high
nibble), and the LP/BP/HP output-select bits plus voice-3-off (`$18`) are
all applied to the real mix, including two real, load-bearing hardware
facts confirmed and implemented alongside the filter math itself:
voice-3-off disconnects voice 3 from the final output entirely regardless
of filter routing, and an unselected filter output means every voice
routed to the filter is inaudible (not silently bypassed). The
cutoff-to-frequency mapping (nonlinear and 6581-vs-8580-divergent on real
hardware) and the resonance-to-Q mapping (also chip-inconsistent on real
hardware, and capable of true self-oscillation) both use documented linear
approximations rather than per-chip-measured curves -- see `CREDITS.md`.
Verified numerically stable (no NaN/out-of-range output) via a throwaway
real-ROM-boot probe at maximum resonance with all 3 filter taps selected.

**DONE: phase 3 — integration.** `sid::Sid` is wired into `crates/machines/
c64`: `C64Bus` decodes the real `$D400`-`$D7FF` I/O window (`addr & 0x1F`,
matching the chip's own 5-address-line decode and its resulting 32-byte
register mirroring) for both reads and writes; `C64::step` ticks the SID by
that step's own real cycle count (no clock divider needed -- unlike
`zx_spectrum`'s AY, a real SID runs at the same clock as the CPU beside it)
and accumulates/emits samples via the same `audio_cycle_accumulator`/
`mix_audio_sample` shape `zx_spectrum` established; `Machine::drain_audio`
is overridden the same way. Save state round-trips `sid::Sid` via a direct
serde derive (same "no external-media state" tier as `ay_3_8910::
Ay3_8910`) plus the new `audio_cycle_accumulator` field. `sid_model = 6581|8580` is now an option (filter floor + master-volume DC; see `CLAUDE.md`);
per-chip DAC/filter curves and per-model combined-waveform tables remain open.

### core parity track — VIC-II coverage

Implement in hardware-dependency order:

1. **DONE: raster IRQ.** `$D019`/`$D01A` implement the raster-compare source only (bit 0):
   `Vic::tick` latches a raster interrupt on the tick the live raster position transitions onto
   the compare line built from `$D012` + `$D011` bit 7's own stored bytes, returning `true` (same
   shape `c64::cia::Cia::tick` already established for Timer A) only when that source is also
   enabled in `$D01A`; `C64::step` turns that into a real `mos6502::Cpu::request_irq()`, same
   call-site pattern CIA1 already uses. `$D019` write-1-to-clear and its live bit-7 "/IRQ asserted"
   computation are both confirmed against VICE's own real source, not guessed -- see `CREDITS.md`.
   Sprite-sprite/sprite-background collision and light pen (bits 1-3) stay inert until sprites/light
   pen exist below.
2. **DONE: bad-line cycle stealing.** `Vic::tick` implements the real bad-line condition (bad lines
   allowed this frame, raster line's low 3 bits match `$D011` YSCROLL, line inside the real
   `$30`-`$f7` DMA window) and reports a flat `40`-cycle steal via its own `TickResult` struct
   (alongside the existing raster-IRQ signal) rather than folding it into the raster generator's own
   timing -- `frame_cycle` only ever advances by the real cycle count `tick` is given, never by the
   reported steal, matching real hardware's independent raster-generator timing. `allow_bad_lines`
   (re-)latches from the DEN bit only at the instant the raster reaches `$30`, a coarser single-sample
   approximation of real hardware's per-cycle DEN latch (this project's per-instruction tick
   granularity has no finer resolution) -- matched to VICE's own simpler, non-cycle-exact core rather
   than invented. `C64::step` folds the reported steal into CIA1/SID's own `tick` calls (both keep
   running off the real phi2 clock regardless of CPU bus stalls, real hardware fact) and into its own
   returned cycle count, so the outer real-time pacing loop's budget accounting reflects the stolen
   time too. Condition and the 40-cycle figure both confirmed against VICE's own real source plus an
   independent VIC-II timing reference -- see `CREDITS.md`.
3. **DONE: bitmap and multicolor modes.** `Vic::video_mode` selects one of 5 real modes (text,
   multicolor text, hi-res bitmap, multicolor bitmap, extended color mode text) from `$D011`'s
   ECM/BMM bits and `$D016`'s MCM bit; the 3 remaining `ECM+BMM` combinations render solid black
   (`VideoMode::Invalid`) rather than reproducing real hardware's exact per-sub-case output, since
   no real software relies on one. `video_matrix_base`/`character_base`/`bitmap_base` derive real
   addresses from `$D018` (bank-0-relative only -- VIC bank selection is real CIA2 wiring, out of
   scope for this item, see "CIA2 completion" below); the new `render` dispatcher slices a
   host-supplied flat RAM array at those offsets and picks the matching per-mode renderer
   (`render_multicolor_text`/`render_bitmap`/`render_multicolor_bitmap`/
   `render_extended_color_text`), reusing the existing `MASK24_TABLE`/`PALETTE24_TABLE` SIMD blend
   where the mode's shape fits (bitmap mode, ECM text) and plain scalar code for the two true
   multicolor (2-bit-pair, doubled-pixel-width) modes, consistent with this project's
   no-optimization-without-profiling-evidence stance. `c64::C64::render_frame` now calls this single
   entry point instead of `render_text` directly -- confirmed behavior-preserving for the existing
   default case (real KERNAL boot programs `$D018` to `0x14`, computing the same `$0400` video
   matrix base the old hardcoded constant already assumed) via a real, committed-ROM boot run, not
   just the unit tests. Bit layout and every mode's pixel/color algorithm confirmed against both
   VICE's own real source and Christian Bauer's widely-cited VIC-II article -- see `CREDITS.md`.
4. **DONE (partial): eight hardware sprites.** `Vic::render_sprites` composites all 8 sprites onto
   the current mode's own background picture inside `render()`: real position (9-bit X via `$D010`,
   `24`/`50` screen-offset correction), real fixed priority (sprite 0 highest, sprite 7 lowest,
   independent of each sprite's own `$D01B` bit), per-sprite `$D01B` behind/in-front-of-background
   priority (checked via a new on-demand `background_pixel_is_foreground` query -- decided purely by
   the chip-wide MCM bit, not any per-cell multicolor opt-in, a real, easy-to-get-wrong hardware
   quirk confirmed against source rather than assumed), hi-res and multicolor color decode (`$D025`/
   `$D026` shared colors, `$D027`-`$D02E` individual), and X/Y expansion. Sprites are clipped at the
   existing `320x200` canvas edge, which is real hardware-correct (the border has the *highest*
   display priority of all, overlaying sprites too), not merely an architectural shortcut. Bit
   layout and every rendering rule confirmed against Christian Bauer's VIC-II article plus a literal
   cross-check against VICE's own source -- see `CREDITS.md`.

5. **DONE (2026-10-11): sprite-sprite/sprite-background collisions.** The per-raster-line pipeline the
   earlier note asked for: `Vic::tick` reports `TickResult::entered_line`, `C64::step` calls
   `Vic::scan_sprite_collisions` with the bank the VIC sees and requests the IRQ. `$D01E`/`$D01F` latch
   until a CPU read (`Vic::read_clearing`), `$D019` bits 1-2 set on the register's zero-to-nonzero edge
   and interrupt when enabled in `$D01A`; border sprite-sprite hits included. Samples once per line (no
   per-cycle sprite DMA; X wrap past 511 added later, see `CLAUDE.md`). Tests in `vic-ii` (8) and `c64` (3).

Do not compromise live raster-register behavior or the optimized text renderer while extending the
chip.

### core parity track — CIA2 completion

**DONE.** `crates/machines/c64/src/cia.rs`'s `Cia` now models the full MOS
6526 register set, not just Timer A: Timer B (including its own INMODE
field, which can chain it to count Timer A's underflows instead of phi2 --
a real, effective-32-bit-timer hardware trick), the Time-of-Day clock (BCD
tenths/seconds/minutes/1-12-hour-with-PM-flag, the real read-latch and
write-halt mechanisms, and the alarm compare-and-interrupt registers), the
output-mode serial shift register (clocked by Timer A underflow, one bit
per underflow), and the force-load control-register bit as a real self-
clearing strobe. `C64::step` now ticks CIA2 every step (previously a pure
register-storage stub that was never ticked at all, so none of its timers
ever ran) and routes its interrupt to `mos6502::Cpu::request_nmi()` --
**CIA2's real IRQ output is wired to the CPU's NMI input, not its IRQ
input**, unlike CIA1 -- confirmed and load-bearing, not a detail glossed
over. Real VIC bank selection (CIA2 Port A bits 0-1, `$DD00`, inverted
2-bit field) is wired too, closing the "stays fixed at bank 0" gap the
VIC-II bitmap/multicolor and sprite items above both flagged -- needed zero
`vic-ii` crate change, since its own internal offsets were already bank-
relative; `C64Bus::render_frame` just hands `Vic::render` the correctly-
selected 16K slice now instead of always starting at `$0000`. See
`CREDITS.md` for the full citation and every explicit scope boundary
(CNT/SP pins unwired -- no cassette/IEC/RS-232 host wiring exists yet for
either CIA to drive them from; the real 12:00:00 AM/PM erratum not
reproduced).

`Cia` stays local to `c64::cia` rather than becoming a shared
`crates/chips/cia-6526` crate -- CIA1 and CIA2 are both `Cia` instances
wired differently by one host bus (`C64Bus`), not two different crate
consumers, so extraction still isn't warranted by this pass alone; revisit
once a second real machine (not just a second instance within this one)
needs the chip.

### parity extensions — drives and baseline media

**DONE (2026-10-12): C64 tape LOAD/SAVE fast traps, `--tape-save out.tap`, `--run` for `.tap`** (see `CLAUDE.md`; also fixed the 24-vs-20-byte `.tap` header and the per-pulse FLAG toggle).

**DONE (2026-10-11): `.d64` disk SAVE** (`C64::try_fast_kernal_save_trap`, `D64::save_program`, `ExportDisk`
through the C64 plugin). Still open: `.d71`/`.d81` BAM decode + writing, VERIFY, SEQ/REL
files, the drive error channel (63/72/33 are silent). Details in `CLAUDE.md`.

**DONE: item 1's first piece — the VIA chip.**
`crates/chips/via-6522` (package `via6522`) implements a full, standalone
MOS 6522 VIA -- both parallel ports w/ DDR, Timer 1 (continuous/one-shot +
PB7 square-wave output), Timer 2 (one-shot + PB6 pulse-counting), all
CA1/CA2/CB1/CB2 peripheral-control modes (edge-select interrupts,
independent/non-independent IFR-clear semantics, handshake/pulse/manual
output), and the IER set/clear-mask protocol -- same host-agnostic-chip
convention as `ay-3-8910`/`sid`, no dependency on `machine`/a CPU crate/any
host wiring yet. Two real, easy-to-get-wrong hardware facts confirmed and
tested: the 6522 has no start/stop bit for either timer at all (unlike
CIA's `CRA`/`CRB`) -- both always count down every phi2 cycle from
construction; and register `$01` ("ORA with handshake") vs `$0F` ("ORA, no
side effects") are genuinely different addresses for the same data, not
one behavior under two names. Shift-register modes clocked by an external
CB1 pulse stay inert (no real external serial-data device wired to
anything in this project), same scope cut `c64::cia::Cia`'s own CNT/SP
pins already established. See `CREDITS.md` for the full citation.

**DONE: item 1 (IEC bus and 1541).** `crates/machines/1541` (package
`c1541`) implements the 1541 as its own real computer: `mos6502::Cpu` + 2
`via6522::Via` instances (VIA1 for the serial/IEC bus + device-address
jumpers, VIA2 now wired to a real disk mechanism -- see the GCR item
below) + 2K RAM (`$0000`-`$07FF`) +
16K ROM (`$C000`-`$FFFF`). No real 1541 ROM image is committed (this
project has no legally-suppliable copy, unlike the C64's own
user-supplied KERNAL/BASIC/character ROMs) -- every test uses synthetic
ROM content. `c1541::IecBus` (a 3-line, wired-AND bus type modeled with
exactly the 2 real participants this project has today) connects it to
`c64::C64` over a real, software-visible serial bus: `C64::iec_output`/
`set_iec_input` and `C1541::iec_output`/`set_iec_input` are each side's
own half of the wiring, and `c64::bus::C64Bus::cia2_port_a_read` wires
CIA2 Port A's `CLOCK IN`/`DATA IN` bits (`$DD00` bits 6/7) to the real
bus level instead of just floating high. Every VIA base address, Port B
bit layout, DDR mask, and the CIA2 Port A bit layout + inverting-driver
polarity convention were confirmed directly against VICE's own real
source this session (not recalled) -- see `CREDITS.md`. A genuine,
real end-to-end integration test (`crates/machines/c64/tests/
iec_1541.rs`, `c1541` as a test-only `[dev-dependencies]` entry of `c64`,
no production dependency either direction) proves 4 real protocol
properties using the exact real DDR configuration values real firmware
uses: idle-bus-reads-high, `ATN` reaching the drive as both a Port B
level and a real `CA1` interrupt edge, `DATA OUT` reaching the C64 as
`DATA IN`, and correct wired-AND behavior when both sides assert the same
line. A real bug was found and fixed in the process (a VIA's default
`CA1`-edge-tracking state didn't match this host's own assumed
idle-high baseline) -- see `CREDITS.md`'s own entry for the full account.

Explicitly out of scope for this pass, left for the items below: no
`retroclawd` CLI/GUI integration (SUPERSEDED 2026-10-12: `--true-drive` now steps a `c1541` beside the C64, see `CLAUDE.md`; originally: no `--machine 1541`, no dual-machine
stepping loop, no `--rom-1541` flag).

**DONE: item 2's first piece — the `.d64` format crate.**
`crates/formats/d64` parses the standard Commodore 1541 disk image format
(the real 4-speed-zone track/sector layout -- 21/19/18/17 sectors/track
across 4 zones, confirmed against VICE's own `sector_map_d64` table; the
35-track and 40-track variants, each with an optional per-sector real
drive error-code byte channel), plus BAM (disk name/ID/DOS type, per-track
free-sector bitmap groups) and directory-chain reading -- every offset
confirmed against VICE's own real source this session (several attempted
fetches against the classic Peter Schepers spec page and other reference
sites were blocked by this sandbox's egress policy), not recalled. Same
"format crate only parses bytes, no machine/CPU dependency" split
`dsk`/`nec765` already established for CPC/+3 -- see `CREDITS.md` for the
full citation, including a real, easy-to-get-wrong directory-slot offset
(`+2`, not the more obvious `+0`) this pass caught via VICE's own source
before it became a bug rather than after.

**DONE: item 2 (GCR disk mechanism wired into VIA2).** `crates/machines/1541`
gained `gcr.rs` (the real 4-bit-to-5-bit GCR codec + the real sector-to-GCR
encoding algorithm, ported directly from VICE's own real `gcr.c` --
`GCR_conv_data`/`gcr_convert_4bytes_to_GCR`/`gcr_convert_sector_to_GCR`,
fetched live this session, see `CREDITS.md`) and `disk.rs` (`DiskMechanism`:
head stepper motor, spindle motor, density/speed-zone select, and the real
bit-level SYNC-detection + byte-assembly hardware DOS firmware bit-bangs
through -- a genuinely different mechanism from `nec765`'s NEC765-compatible-
controller-command protocol CPC/+3 use, modeled at the real GCR-bitstream
level rather than a shortcut "read this sector" abstraction, same principle
as `nec765` modeling the real FDC command set). VIA2's real pin assignments
(Port A = raw GCR byte; Port B bits `STP1:STP0`/`MTR`/`DS1:DS0`; `CA1` =
byte-ready, also wired directly to the 6502's own SO pin for fast,
interrupt-free polling; `CA2` = SOE gating whether that signal actually
propagates) were confirmed against two independent real emulator sources
this session (VICE's `rotation.c` and MAME's `c1541.cpp`, cross-checked
against each other), not guessed -- see `CREDITS.md`. `mos6502::Cpu` gained
`set_overflow` (the real, distinct SO pin: sets the V flag immediately,
with no interrupt-enable gating or vectoring unlike IRQ/NMI) to support this.
A real, careful subtlety confirmed by hand-tracing rather than assumed:
while a real SYNC run's own ones-count is still below the 10-bit
recognition threshold, ordinary byte framing keeps running undisturbed, so
one "bogus" byte gets assembled from the run's own lead-in bits before
SYNC is recognized and framing re-aligns -- correct, expected real
hardware behavior, not a bug (real DOS polling loops are written to
tolerate it). Read-only: no GCR *write* path exists (`CB2`/`OE` stay
unwired), matching this crate's own scope for this pass. Round-trips
through save state via a dedicated `DiskMechanismState` mirror struct
(same "external media stays external" shape `nec765::Fdc`/
`zx_tape::TapePlayer` already established) -- the attached disk image's own
bytes stay external, only the mechanism's own live mechanical/timing state
does.

**DONE: item 3 (`.prg` loading).** `retroclawd`'s existing `--program`/
`--org` path (already fully generic across every machine here, not a
C64-specific mechanism) now auto-detects a `.prg` extension (case-
insensitive): the real 2-byte little-endian load address every genuine
`.prg` file begins with (the same header a real Commodore KERNAL LOAD
reads) is parsed and used instead of `--org`, and only the bytes after
that header are loaded -- `--org` keeps applying unchanged to every other
extension. `resolve_program_load` (a small, pure, file-IO-free function,
same testing shape `resolve`/`resolve_one_machine_option` already use) is
the entire change -- no new plugin command, `MediaKind`/`MediaSlot`
variant, or per-machine handling, confirming the "does not need a general
media abstraction" call above: `.prg` is just "2-byte address + raw
bytes", and the caller already drives the exact same generic
`Machine::load`/`set_pc` calls every other `--program` file already went
through. Deliberately out of scope, matching real hardware's own
LOAD-vs-RUN distinction rather than a shortcut: no BASIC-pointer patching
or auto-`RUN` injection -- a `.prg` is loaded into memory exactly where
its header says, same as a real KERNAL LOAD, but actually running it
(`RUN` for a BASIC program at `$0801`, or a manual `SYS`) is still the
user's own next step, not simulated here. A too-short (<2-byte) `.prg` is
a clean error rather than a silent zero-address load.

**DONE: item 4 (`.tap`/`.t64` cassette loading).** New `crates/formats/
c64-tape` crate parses both -- same "format crate only parses bytes, no
machine/CPU dependency" split every other format crate here already
follows. **`.tap`** ("C64-TAPE-RAW") is a real, raw pulse-capture format,
completely unrelated to ZX Spectrum's own same-named `.tap` despite the
shared extension: a nonzero pulse byte is `N * 8` real CPU cycles; a
`0x00` byte (version 1 only) escapes to an exact 24-bit little-endian
cycle count, confirmed against VICE's own real `datasette.c` (the actual
playback engine) rather than a separate, non-authoritative helper in
`tape/tap.c` that looked similar but served a different internal purpose
-- see `CREDITS.md` for the full story of how that mismatch was caught.
A `0x00` byte in a version-0 file has no real, spec-defined meaning
(VICE's own code falls back to an arbitrary internal constant) --
`Error::ZeroByteInVersion0` errors clearly rather than guessing at it.
`TapePlayer::tick(cycles) -> u32` returns a transition *count*, not just
a level, so a caller can never collapse two real edges into one even
under an extreme turbo multiplier. **`.t64`** is not a real bitstream at
all -- a directory container invented by the C64S emulator, never
produced by real hardware -- so it's deliberately NOT bit-banged: its
first loadable (`NORMAL`-type) entry's bytes are poked directly into
memory at the entry's own stored load address, the exact same "poke
bytes, no BASIC-pointer/auto-`RUN` magic" semantic item 3's `.prg`
already established (`T64::first_program() -> Option<(u16, &[u8])>` is
the same `(address, data)` shape `resolve_program_load` uses). Real C64
cassette wiring (confirmed against VICE's own source this session, not
recalled): the READ line is CIA1's dedicated **FLAG** pin
(`Cia::set_flag`, negative-edge-only -- unlike VIA's configurable
CA1/CB1, confirmed via `ciacore.c`/`cia.h`'s `CIA_IM_FLG`), while MOTOR
(bit 5) and SENSE (bit 4) live on the **6510 CPU's own on-chip I/O
port** ($0000/$0001), not any CIA register at all (confirmed via
`c64mem.c`/`c64pla.c`'s real `c64pla_config_changed`) -- a real,
easy-to-get-wrong edge case reproduced faithfully: MOTOR reads ON both
when driven low as output AND when merely left configured as input,
the ONE combination that turns it off is bit 5 driven *high* as output.
`C64Bus::new` seeds `cia1.set_flag(true)` (idle-high) at construction,
the same "host must seed the chip's assumed initial wiring state"
lesson `c1541`'s own VIA1/`ATN` seed already taught. `.tap`/`.t64`
dispatch lives in `c64-machine-plugin`'s `AttachMedia` handler (mirrors
`zx-spectrum-machine-plugin`'s own extension-sniff pattern exactly);
`retroclawd`'s `apply_media` extension table gained `"t64"` alongside
the tape family. Verified live through the real binary, not just unit
tests: attaching a synthetic `.t64` and `--dump-memory`-ing its stored
load address shows the exact stored bytes landed there; attaching a
synthetic `.tap` and booting a real KERNAL to `--dump-memory 0xDC0D:1`
(CIA1's ICR) shows bit 4 (FLAG) set only when a tape is attached, never
in a same-cycle-count control run with no tape -- a real, confirmed
end-to-end effect, not merely unit-tested. Save state round-trips the
tape's own `TapePosition` (playback position only, never the tape's own
bytes), same `nec765::Fdc`/`zx_tape::TapePlayer`/`c1541::
DiskMechanismState` "external media stays external" shape. No KERNAL
LOAD-routine trap exists for either format (a separate, much larger
feature, deliberately deferred) -- `.tap` plays back the real pulse
protocol in real time exactly like ZX Spectrum's own tape support, and
`.t64` is a one-shot poke the user still `SYS`/`RUN`s manually, same as
`.prg`.

**DONE: item 5 (`.d64` disk attach + IEC-level KERNAL LOAD fast loader +
`.prg`/`.t64`/`.d64` auto-run scaffolding).** The exact KERNAL-LOAD-
routine-trap deferred as "a separate, much larger feature" in item 4's
own entry above is now done, in the same spirit as `zx_spectrum::
try_fast_tape_load_trap`: `C64Bus::disk: Option<d64::D64>` (direct
attachment -- deliberately no simulated 1541/IEC-bus bit-banging at all,
same "not every real mechanism needs modeling once the logical result is
right" stance `.t64`'s own entry already took) plus `C64::
try_fast_kernal_load_trap`, checked in `step()` before `cpu.step()`.
Traps the real KERNAL LOAD entry point itself (`$F49E`, confirmed by
disassembling this project's own committed `roms/c64/kernal-*.bin` files
directly this session with a throwaway Python 6502 disassembler, not
recalled or guessed -- byte-identical across all 5 committed revisions),
*before* any of LOAD's own real work happens, servicing the entire
request directly from the attached image's own logical directory/sector
contents: reads the real zero-page LOAD state (`FA`/`SA`/`FNLEN`/FNADR
at `$BA`/`$B9`/`$B7`/`$BB:$BC`, also confirmed by disassembly, not just
cited), looks up the requested filename (a bare `*` matches the first
closed PRG entry -- the real, universal `LOAD"*",8,1` idiom), reads the
file's own real sector chain (new `d64::D64::read_file`, cross-checked
against a real, cited example for the tricky last-sector-length-byte
off-by-one), and writes its payload at the real target address (`SA`
picks the file's own embedded header address vs. the caller's given
one, exact real convention), then simulates LOAD's own eventual `RTS`
back to its caller (pop the real return address off the 6502 stack,
`PC = popped + 1`) with the real carry/A/X/Y exit convention (confirmed
via search: carry clear + X/Y = one-past-the-last-byte on success;
carry set + `A = 4` FILE NOT FOUND on a miss). VERIFY (`A == 1` on
entry) is an explicit, documented scope cut, not silently mishandled --
falls through to real (already-non-functional, no simulated 1541 exists
either way) KERNAL execution. `--disk` now accepts `.d64` for
`--machine c64` (`c64-machine-plugin`'s `MediaKind::Disk` arm, previously
a bare "isn't supported yet" stub).

`.prg`/`.t64`/`.d64` auto-run scaffolding (`--run`, mirroring CPC's own
`--run`/autotype precedent in `gui/input.rs` exactly, new `c64`
`MachineOptionInfo`): for `.d64`, autotypes `LOAD"*",8,1` (the C64's own
dedicated `Asterisk` key, no natural host `KeyCode` for it, is sent via
a new `AutoRunKey::Named` variant generalizing the previously
`KeyCode`-only autotype action queue) then, after a short delay, `RUN`;
for a raw `.prg`/`.t64` poke (no real KERNAL LOAD involved at all), just
bare `RUN`. A raw poke needs one more thing a real `LOAD"...",8,1`
statement gets for free (BASIC's own command handler always relinks a
just-loaded program's line-pointer chain before returning to READY) --
`c64-machine-plugin`'s new `finish_basic_program_load`/
`relink_basic_program` walks the loaded program's own real line-link
format (`[link][line#][tokens...][$00]`, terminated by a real `$0000`
link) and synthesizes VARTAB/ARYTAB/STREND (`$2D`-`$32`), gated strictly
to the real, conventional `$0801` BASIC origin -- never touches a
machine-code load at any other address, matching real BASIC's own
LINKPRG assumption rather than inventing a looser one.

**A real, live bug found and fixed getting the GUI auto-run path working,
not just unit-tested**: an actual GUI run under this sandbox's own
software (llvmpipe) rendering showed the primary `LOAD"*",8,1` sequence's
keystrokes never reaching the screen at all (confirmed via the debugger
plugin's own `/api/memory-bytes` endpoint dumping live screen RAM), while
the *later*, multi-second-delayed `RUN` follow-up worked correctly --
real GPU/shader-compile startup cost (this project's own audio-setup
code already independently documents this same real, multi-second-
capable window) outlasted the flat 1-second startup delay every other
profile already used, silently swallowing early keystrokes sent before
BASIC's own input loop was actually polling the keyboard buffer yet.
Fixed the same way `CpcTape`'s own pre-existing readiness-timeout
mechanism already handles an analogous problem for real tape loads:
`gui/input.rs` gained a generalized `auto_run_ready`/
`auto_run_needs_readiness_wait`, and both C64 profiles now wait (with the
same logged timeout/warning shape) for `PC` to land inside `$E5CD`-`$E5D4`
-- the real KERNAL "wait for a keypress" idle loop (`LDA $C6/STA $CC/STA
$0292/BEQ $E5CD`, `$C6` being the real keyboard-buffer count NDX),
confirmed by disassembling the real ROM directly, not guessed -- before
sending a single keystroke. **Verified live end to end through the real
binary** (not just unit tests, and not just the headless fast-loader
trap in isolation): a real GUI session with a synthetic `.d64` (one
tokenized `10 POKE 49152,66` BASIC program named `HELLO`) and `--run`
shows, via the debugger's own HTTP API, the real screen text
`LOAD"*",8,1` / `READY.` / `RUN` / `READY.` in sequence, memory at
`$0801` holding the exact real tokenized bytes, and memory at `$C000`
(`49152`) reading back `$42` (`66`) -- the loaded BASIC program's own
`POKE` genuinely executed after a real, autotyped `RUN`, not merely
loaded. The headless-only fast-loader trap itself (independent of the
GUI/auto-run layer entirely) was separately verified live too: a
synthetic 6502 test program executing a real `JSR $FFD5` against the
real committed KERNAL ROM, both by exact filename and by `*` wildcard,
plus a no-disk-attached control run confirming the trap never engages
without one.

### BACKLOG: parity extensions — broader C64 media/workflow (gap vs VICE/libretro)

**DONE: item 1 (`.d71`/`.d81`).** New sibling format crates `crates/formats/d71` (package `d71`)
and `crates/formats/d81` (package `d81`) parse the 1571 (double-sided 1541) and 1581 (uniform-
sector, genuinely different header/BAM shape) disk formats, mirroring `d64`'s own "format crate
only parses bytes into a plain struct" precedent rather than folding into that crate (own shape
per real disk-format family, same "own crate per format" precedent `dsk` already established for
CPC) -- `DirectoryEntry`/directory-chain-walking/file-chain-walking/`find_program` logic is
duplicated across all three rather than shared, same "own shape per format" precedent
`sna-snapshot`'s `PagingState` (vs `z80-snapshot`'s) already set, chosen deliberately here so the
already-shipped, tested `d64` crate's public API needed zero changes.

Every geometry/offset constant was confirmed directly against VICE's own real source this session
(`diskconstants.h`'s `NUM_TRACKS_1571`/`NUM_BLOCKS_1571`/`BAM_TRACK_1571`/`BAM_NAME_1571`/
`BAM_ID_1571`/`NUM_TRACKS_1581`/`NUM_SECTORS_1581`/`BAM_TRACK_1581`/`BAM_NAME_1581`/`BAM_ID_1581`,
`vdrive-bam.c`'s own `vdrive_bam_setup_bam`/BAM-formatting code) -- see `CREDITS.md`. `.d71` side 1
(tracks 1-35) is byte-identical to a `.d64`'s own BAM/directory layout (same track 18 BAM, same
144/162 name/ID offsets); side 2 (tracks 36-70) repeats the same 4-zone sector-count table
track-for-track, with its own second BAM sector at track 53 (confirmed via VICE's own
`bam_tracks[1] = BAM_TRACK_1571 + num_tracks/2` construction). `.d81` is a real, different physical
format (true MFM at a uniform 40 sectors/track, no speed zones at all -- confirmed by the absence
of any `sector_map_d81` table in VICE's own source, unlike `d64`/`d71`'s shared `sector_map_d64`)
with its own header sector (track 40/sector 0, name at offset 4/ID at offset 22 -- genuinely
different offsets from `d64`/`d71`'s track-18 BAM, not the same struct at a different location) and
two separate BAM sectors (track 40/sectors 1-2); its DOS-type offset (25) was derived the same
padding-consistency way `d64`'s own was, cross-checked against VICE's own disk-formatting code
literally writing `"3D"` there. **Deliberately deferred, both formats**: the second side's/BAM
sectors' own per-track free-space bitmap isn't decoded (confirmed the real byte layout enough to
place the *sector*, not the exact *starting offset* of the per-track group table within it, and
this session couldn't independently re-verify that exact arithmetic against a second source) --
undecoded since attaching and *reading* a disk (directory listing, file extraction, the actual
point of this item) never consults the BAM's own allocation bitmap on real hardware either, same
as `d64::D64::directory_entries`/`read_file` already established for `.d64` itself.

Wired into `c64::bus::C64Bus` via a new `AttachedDisk` enum (`D64`/`D71`/`D81`) and a small private
`CbmDisk` trait (`find_program_bytes`) implemented once per format crate -- `try_fast_kernal_load_
trap` (see item 5's own entry above) needed zero format-specific branching to support the two new
formats: `.d71`/`.d81` share the exact same real CBM DOS directory/file chain format `.d64` uses,
just at a different real geometry, so the trap's only change was replacing its inline
`directory_entries`/`find_program`/`read_file` sequence with one `AttachedDisk::find_program_
bytes` call (behavior-identical, verified by every pre-existing `.d64` trap test still passing
unchanged). `C64::load_disk`'s own existing signature/behavior is untouched; `load_disk_d71`/
`load_disk_d81` are new, parallel entry points. `c64-machine-plugin`'s `MediaKind::Disk` handler
now extension-sniffs (`.d64`/`.d71`/`.d81`, defaulting to `.d64` when no filename is given,
preserving this handler's pre-existing name-agnostic behavior) instead of unconditionally parsing
as `.d64`; `retroclawd`'s `apply_media`/`--run`'s `resolve_auto_run_profile` both gained the two new
extensions, the latter mapping either to the existing `C64Load` auto-run profile unchanged (same
`LOAD"*",8,1` autotype sequence, since the fast-loader trap already made disk format irrelevant
past "a disk is attached"). New regression tests at every layer (`d71`: 10, `d81`: 9, `c64::bus`:
2 new fast-loader-trap tests using data sectors on tracks past `.d64`'s own 35-track range to prove
the wider geometry is genuinely reachable, `c64-machine-plugin`: 3 new attach/reject tests) --
`cargo test --workspace`/`cargo clippy --workspace --all-targets -- -D warnings` both clean.

2. **DONE (2026-10-11)** `.g64` (new `g64` crate: GCR-1541 container, circular-bitstream sync/header/data
   decode into a `.d64`; attach only, `--disk-writeback` refused, copy-protected tracks that do not decode
   to sectors are not representable) and `.x64` (64-byte header + `.d64`). `.nib` is refused with a clear
   error: its track layout could not be verified from a source, so it is not guessed.
3. **DONE (2026-10-11)** Cartridge/program container interop: `.crt` (new `c64-cartridge` crate +
   `c64::cartridge`, PLA with EXROM/GAME incl. Ultimax, mappers Normal/Simons' BASIC/Ocean/Fun Play/
   Super Games/Epyx Fastload/System 3/Dinamic/Zaxxon/Magic Desk/EasyFlash read-only, `--cartridge`)
   and `.p00`. **DONE (2026-10-12)**: GeoRAM, Action Replay/KCS Power/Final Cartridge III/Retro Replay/Expert
   (freeze button, cartridge RAM), EasyFlash flash programming with `--cartridge-save`, Ultimax VIC view.
   Not done: Retro Replay flash jumper/clockport/Nordic Replay quirks, Expert RESTORE-key NMI, other
   freezers (Atomic Power, Super Snapshot, Final Cartridge I/Plus, Magic Formel...), a held (level) NMI line.
4. **DONE (2026-10-11)** Multi-disk: `--disk list.m3u` (`#` lines skipped, relative paths against the
   playlist folder) attaches entry 0; hotkey actions `next_disk`/`prev_disk` (unbound by default) walk the
   list and wrap, through the same unsaved-disk question as a manual swap (`dialogs::FileAction::NextDisk`).
   Works for every machine with disk support. `.d71`/`.d81` SAVE also done (`cbm-dos`).
5. **DONE** `.zip` attach and `http(s)://` sources: the host's `inputs.rs` (over `file-loader`) unpacks a
   single-entry archive and downloads URLs for *every* file input (ROMs, program, disk, tape, snapshot, state,
   cartridge, game config); `archive.zip#entry` picks from a multi-file zip; unpacked files live in one temp folder removed at exit; `--progress` + toasts; `.m3u` entries may be URLs/zips; `--disk-writeback` refuses them.

### BACKLOG: parity extensions — C64 expansion and peripheral parity

Execute in this order:

1. **DONE** RAM expansion modules: REU (`--reu`) and GeoRAM (`--georam`), see `CLAUDE.md`.
2. **DONE** Cartridge/expansion-port coverage for freeze workflows (Action Replay, KCS, FC3,
   Retro Replay, Expert) plus EasyFlash flash.
3. **DONE (2026-10-12)** Serial/network: SwiftLink and Turbo232 (`--serial`, 6551 ACIA chip crate
   `acia-6551`, `--serial-net connect:host:port|listen:port` over TCP) and RR-Net (`--rrnet
   loopback|isolated`, CS8900A chip crate `cs8900a`; **chip + loopback only, no real NIC, by decision**).
   Not done: userport RS-232 (`$DD01` bit-banged by the KERNAL), real network backends for the
   RR-Net, hardware flow control lines, parity/break errors.
4. **DONE (2026-10-12)** Printer output: `--printer` traps the KERNAL serial LISTEN/SECOND/CIOUT/UNLSN
   for device 4/5, `--printer-out file.txt` writes PETSCII-converted text. Not done: MPS graphics
   modes, a real IEC printer.

### BACKLOG: parity extensions — C64 configuration and keyboard completion

Execute in this order:

1. **DONE (2026-10-12)** Game configuration represents C64's three ROM paths through the generic
   `[options]` table (`rom_kernal`/`rom_basic`/`rom_char`), no schema change for other machines. A
   game-config that selects the machine (no CLI machine argument) is prescanned before the clap command
   is built, so the machine's declared options exist; covered by a `main.rs` test.
2. **DONE (2026-10-11)** Explicit C64 PAL/NTSC selection and timing wiring (`C64::set_region`, VIC raster
   65x263, CIA TOD divider, clock/frame rate; verified against the real `-03` KERNAL's own detection).
3. Keep current keyboard behavior as the default. Resolve ambiguous C64 punctuation through the
   remapping layer below rather than hardcoding an arbitrary positional-versus-symbolic convention.
**DONE (2026-10-12):** paddles / 1351 mouse (SID POTX/POTY, CIA1 Port A bits 7:6 select, fire lines; see `CLAUDE.md`). The older joystick-port item below is covered by `press_joystick` plus these.

4. **DONE (2026-10-12)** C64 joystick ports (`Joy1*` = port 1, `Joy2*` = port 2 names, bindable in
   `[keys]` and `[pads]`, pads default to `Joy{N}*`) and paddle/mouse mapping; arrow keys + Space keep
   their old behaviour.

### BACKLOG: cross-emulator comparison addendum (VICE/libretro VICE as additional source)

Source set to consult for all future C64 work in this roadmap:

- https://vice-emu.sourceforge.io/
- https://docs.libretro.com/library/vice/
- https://github.com/libretro/vice-libretro

Continue only after completing the external parity sequence above, then execute in this order:

1. Build and keep a C64 feature-delta checklist against the source set above (chip behavior, media
   workflows, and expansion/peripheral paths).
2. For each remaining C64 backlog item, consult those sources as additional implementation references
   before coding.
3. If sources disagree, require hardware-backed behavior or executable tests before selecting
   behavior.

Do not import code blindly; treat these emulators as implementation references, not as substitutes
for hardware verification.

### C64: selectable VIC-II accuracy (`vic_accuracy = line | cycle`)

Goal: keep today's line-accurate VIC-II as the default and add a cycle-accurate mode behind a
machine option (`vic_accuracy`, same plumbing as `sid_model`: CLI, `[machines.c64]`, game-config
`[options]`). Line mode must stay bit-for-bit what it is now (all tests, current speed).
Execute in this order:

1. **DONE (2026-10-12)** CPU bus hook: `machine::Bus::dummy_cycle(addr, write, value)` (defaulted
   no-op) and a cycle-complete `mos6502` core -- every 6502 cycle is exactly one `read`, `write` or
   `dummy_cycle`, in real order (implied-op padding read, indexed fix-up read at the uncorrected
   address, `zp,X` unindexed read, RMW write-back of the old value, stack/RTS/RTI/JSR/BRK/interrupt
   dummy reads, branch wasted reads, JAM). Guard: `crates/core/6502/tests/cycle_hook.rs` checks all
   256 opcodes x 16 operand/flag scenarios emit as many events as the cycles `step` reports
   (mutation-checked: removing the indexed fix-up read fails it), plus exact event orders for INC abs,
   LDA/STA abs,X, `zp,X`, JSR/RTS and taken/page-crossing branches. Klaus Dormann and Seddon still pass.
2. **DONE (2026-10-12)** C64 bus ticks per cycle behind the option (`vic_accuracy = line | cycle`,
   plugin option / `--vic-accuracy`, `c64::VicAccuracy`, `C64::set_vic_accuracy`): in cycle mode
   `C64Bus::read`/`write`/`dummy_cycle` each run one clock of VIC/CIA1/CIA2/SID/tape/cartridge/
   controllers (`C64Bus::tick_cycle`) while `mos6502::Cpu::step` runs (`in_cpu`, so the debugger, the
   traps and the REU never tick the clock); interrupt requests raised mid-instruction are handed to the
   CPU after it (`pending_irq`/`pending_nmi`); `Machine::step` returns the clock cycles that really
   passed. Line mode is untouched. The mode is part of the save state check (a state saved in the other
   mode is refused). PAL only: `cycle` on an NTSC machine is a clear error.
3. **DONE (2026-10-12)** VIC-II cycle engine (`crates/chips/vic-ii/src/cycle.rs`, `Vic::cycle`,
   `VicMemory`): the PAL 63-cycle schedule with c-/g-/p-/s-accesses against the real 16K view
   (`VicView`: bank, char ROM shadow, Ultimax window), VC/VCBASE/RC/VMLI and display/idle state, the
   per-cycle bad-line condition with the DEN latch from line $30 (so changing YSCROLL before cycle 12
   cancels a bad line and making the next line bad re-fetches the row: FLD), sprite DMA/MC/MCBASE/
   expansion flip flop and the sprite BA windows. 13 unit tests with a mock memory.
4. **DONE (2026-10-12)** BA/AEC: BA low from cycle 12 (or the cycle a late bad line is seen) through 54,
   and 3 cycles before each DMA sprite through the cycle after its p-access; the CPU stalls on a *read*
   while BA is low and never on a write (`C64Bus::cpu_cycle`). Measured on read-only code: 43 cycles per
   bad line (replacing line mode's flat 40), 5 per lone sprite line, 9 for three adjacent sprites; a
   BRK started in cycle 10 of a bad line writes in 12-14 unstalled and waits 40 cycles at the vector
   read. Mutation-checked (writes stalling, no BA warning, no stalling each fail tests). The real
   KERNAL boots to READY. with a screen identical to line mode (`tests/cycle_boot.rs`) and the headless
   binary shows no measurable slowdown.
   Known limits of steps 2-4: IRQ/NMI lines are sampled at instruction boundaries, not on the real
   penultimate cycle; the c-accesses of a bad line that starts after cycle 15 read correct data (real
   silicon reads bus garbage for the first ones); REU DMA still runs as a block after the instruction;
   sprite collisions stay the per-line scan; dummy reads have no I/O side effects (so tricks that rely on
   a read-modify-write's dummy write to a register, e.g. `INC $D019`, do not see the extra write).
5. **DONE (2026-10-12)** Pixel output during the run (`vic-ii/src/cycle.rs`, `Vic::cycle_frame`): every
   cycle emits its eight pixels into a 384x264 frame (the 320x200 window + 32-pixel border, the same shape
   line mode hands out): graphics sequencer for all five modes with XSCROLL (the previous cell's tail shows
   through, the idle cell fills the gap), the eight sprite shifters (priority, `$D01B`, multicolor, X/Y
   expansion, 504-pixel wrap) from the s-access data, and the border unit (main + vertical flip flops,
   CSEL/RSEL comparisons at 24/31/335/344 and 51/55/247/251, Bauer 3.9 items 1-6). `C64::render_frame`
   returns it in cycle mode (the register-decoding path only until the first frame is complete).
6. **DONE (2026-10-12)** Option, save state, docs, tests: `vic_accuracy` option and the mode check on state
   load were done with steps 2-4; the extra engine state round-trips (frame buffers are transient and not
   saved; a restored state draws the same picture as the original). Tests: all five display modes
   match the static renderer **pixel for pixel** over random data, sprites match it (priority, behind-
   background, multicolor, expansion), XSCROLL, side-border opening (CSEL toggle in cycle 56) shows the
   idle byte on that line only, the lower border opened by a 6502 program (`RSEL` off in line 248, on in 255)
   through the whole C64 in cycle mode while line mode cannot show it, FLD repeating a glyph scanline,
   sprite multiplexing (the same sprite drawn twice), and the real KERNAL's boot screen equal to line mode's
   within the cursor cell. Mutation checks: wrong multicolor-bitmap colour source, no right-border compare,
   no X expansion, ignored XSCROLL tail all fail tests. Lorenz/VICE VIC-II programs were **not** run (not
   available here).
   Limits and open assumptions: a register write lands at the next 8-pixel cell (real chips switch a pixel
   or two in; demos that exploit sub-cell timing will differ); the vertical flip flop is also set when the
   frame wraps -- from how demos behave, not from the article; sprite collisions remain the per-line scan;
   sprite crunch follows from the flip-flop rules (done); PAL only.

### C64 feature-delta checklist vs VICE (2026-10-12, topic closed)

Done and tested here: 6510 + illegal opcodes, PLA banking, VIC-II (5 modes, sprites, collisions,
raster IRQ, bad lines, light pen, custom/lowercase character sets via the chargen shadow, bank
select), CIA1/CIA2 incl. TOD and serial register, SID 6581/8580 + filter, PAL/NTSC, `.prg`/`.p00`/
`.t64`/`.tap` (real-time and fast trap, recording), `.d64`/`.d71`/`.d81`/`.g64`/`.x64` with SAVE,
playlists, `.crt` with 16 mapper types, freezers, EasyFlash flash, REU, GeoRAM, SwiftLink/Turbo232,
RR-Net (chip + loopback), printer text output, paddles/1351/light pen, joysticks, opt-in true 1541.

Known gaps, none blocking normal use:
- VIC-II is line-accurate by default (`vic_accuracy = cycle` is the cycle-accurate model): no FLD/border-trick
  timing in line mode, no light-pen sensor lag. Sprite DMA stealing, X wrap past 511 and sprite crunch are done
  (2026-10-12, see `CLAUDE.md`'s vic-ii bullet).
- SID filter curves are linear (no per-chip DAC tables); combined waveforms use the AND rule.
- CIA: CNT/SP-driven modes inert apart from the serial register output mode.
- Drive: fast KERNAL traps are the default; the drive error channel is not modelled there. The true
  1541 needs the user's own 1541 ROM and was only tested with a hand-assembled drive ROM.
- Media: `.nib`, copy-protected GCR, `.tap` turbo loaders, seq/REL files, VERIFY trap.
- Hardware: userport RS-232 bit-banging, other freezer cartridges, RR-Net real NIC, MPS printer
  graphics, IEEE-488/CBM-II, tape with real `datasette` motor inertia.
- Not verified here (no hardware or desktop in the sandbox): a physical gamepad, native file dialogs,
  real paddle software, real SwiftLink/RR-Net software, a real 1541 DOS ROM.


## C64 media validation with real games and the real 1541 DOS (2026-10-12)

DONE: real 1541 DOS ROMs (`roms/1541/`, three variants) run against the real KERNAL over the
emulated IEC bus (`crates/machines/c64/tests/real_1541.rs`), `$` directory and `LOAD"*",8,1`
of all five `software/c64/*.D64` byte-identical to the image; Bomb Jack, both Donkey Kongs, both
Dragon Breed sides, Boulder Dash `.t64` and the Camels `.crt` run and show their intros in line,
cycle and true-drive modes. Bugs found and fixed: inverted drive IEC inputs, missing SYNC/WPS on
VIA2 Port B, Gray-code stepper (really binary), CIA2 NMI level-vs-edge livelock. See `CLAUDE.md`.

Still open:
- Dragon Breed is a two-sided game: play side A past its "insert side B" request with
  `--disk list.m3u` and the `next_disk` hotkey (the swap path exists and now leaves the 300 000-cycle
  empty gap the DOS needs to notice a change; the game was not played that far); its side-A loader files (`LOADER`, `00.`-`04.`) were not exercised beyond the intro.
- Custom drive loaders (M-W/M-E uploads, fast-loader protocols) only work with `--true-drive`; the
  fast KERNAL trap cannot serve them. No such game was found in `software/c64` to test with.
- True-drive disk writes: DONE for SAVE (write head, see `CLAUDE.md`); scratch/validate/format
  (`N:`) and a game saving a highscore (`BOMBJACK HI`, Donkey Kong `highscoresaver`) through the
  head were not exercised yet.
- Joystick/keyboard play-through (the harness only types `LOAD`/`RUN` and one fire press) and a
  frame-for-frame comparison with a reference emulator (VICE is installed on the owner's
  machine, `GTK3VICE-3.10`) were not done; screens were judged by eye.
- `roms/c64/` holds 1570/1571 (now modelled, see `CLAUDE.md`), 1581 and SCPU64 ROMs (from Denise); the 1581 is modelled too (WD1770, `.d81`); SCPU64 is not.

