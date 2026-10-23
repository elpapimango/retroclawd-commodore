# Credits

Third-party references this codebase's implementation is based on -- none
of this code is copied from these sources; each is a fresh Rust
implementation of documented behavior, cited here because that behavior
(file layouts, byte offsets, register constants) came from reading them,
not from independent derivation.

## C16/Plus4 TAP-v2 prerequisite (`c64-tape::ted`)

The [VICE manual, section 17.1](https://vice-emu.sourceforge.io/vice_17.html)
specifies the 20-byte header, `C16-TAPE-RAW` version-2 identity, platform/video
fields, half-wave polarity/order and cycle encoding. VICE 3.10.0 revision
`4d283a2e7dd59b7e378524878e81ecc7826b700c`, `vice/src/tape/tap.c`,
independently confirms platform 2 and slow TED timer clocks (886724 PAL,
894886 NTSC). That file's block-import helper joins half-wave pairs; this
format API intentionally retains individual exact durations instead.

Strict length checks, rejection of unknown/unsupported header values and
zero durations, and the 32 MiB payload limit are local safety policy, not
claims that VICE rejects the same inputs. Canonical writing has synthetic
round-trip proof, not a VICE executable or real tape-load verification.
No source code, firmware or software image was copied.

## Experimental VIC-20 (`vic-i`, `vic20`, `vic20-machine-plugin`)

The VICE raw-tape specification (manual section 17.1) establishes VIC-20
platform 1, PAL/NTSC timer clocks and TAP-v0/v1 pulse encoding.
At the pinned 3.10.0 revision below, `vic20/vic20via1.c` confirms PA6
SENSE and active-low CA2 motor control; `vic20/vic20via2.c` confirms the
cassette wiring. Playback reuses this family's existing full-pulse player
and Commodore KERNAL encoder. Exact-duration writing is shared with TED,
but full pulses and TED half-waves remain separate APIs.
The installed VICE 3.9 was also run with `+virtualdev1`, real TAP playback
and the same firmware; both regional cassette outputs match retroclawd.

At the same source pin, `vic20/vic20iec.c` (`iec_pa_write`,
`iec_pcr_write`, `resolve_bus_signals`) establishes the inverting IEC buffers:
VIA1 PA7 and VIA2 CA2/CB2 high assert physical ATN/CLOCK/DATA low.
The existing 1541 Core, GCR mechanism and `drive-link` are reused.
Installed VICE 3.9 was run with `-drive8truedrive +virtualdev8`,
the same hash-verified 1541 DOS and tools-built D64. Both regional disk LOADs
produce the same glyph. No reference source or DOS ROM was copied.

At that same 3.10.0 source revision,
[`vic20/cart/vic20-generic.c`](https://github.com/VICE-Team/svn-mirror/blob/4d283a2e7dd59b7e378524878e81ecc7826b700c/vice/src/vic20/cart/vic20-generic.c)
documents generic ROM layouts, half-block selection, mirrors, BLK1/2/3/5
ordering and read-only stores; `vic20cart.c` documents reset-on-change and
explicit block flags. These files are GPL-2.0-or-later reference sources.
The private regional KERNALs confirm the signature/vector startup used by the
first-party cartridge fixture. Installed VICE 3.9 was independently captured
with `-cartA`; all 64 glyph pixels match in both execution modes and Regions.
`vic20-cartridge` is a separate pure Format crate; no commercial ROM is included.

[VIC-20 Dead Test v1.1.01](https://github.com/piersfinlayson/Vic20-dead-test/tree/e4e4bdf8f9758e63da6f2754a576d4bab6eb1631)
is an external GPL-3.0 diagnostic by Simon Rowe, Greg McCarthy and Piers
Finlayson (license and authorship read at that release source pin). Its released
BLK5 cartridge and regional replacement KERNALs run unmodified, with all eight
mode/Region/configuration results matching independently captured VICE memory.
The Host's existing diagnostic downloader pins their SHA-256 hashes; the
intake and S7 notes record scope, missing-expansion output and self-CRC limits.
No diagnostic source/binary or reference golden is included in this repository.

`vic20.intake.toml` records hardware facts and unresolved prerequisites.
`docs/vic20-gaps.md` distinguishes implemented behavior from reference proof.
References read for this implementation:

- Commodore, *VIC-20 Programmer's Reference Guide*, etext 1.1, archived at
  <https://www.zimmers.net/anonftp/pub/cbm/manuals/vic20/vic20-programmers-reference-guide-1.1.txt>.
  Printed pages 174-177 identify the memory map and VIA wiring; pages 212-216
  describe VIC-I registers, character modes, video, and sound. The transcription
  has conflicting timer labels and duplicated color-RAM wording; it must not
  override the Chip datasheet or verified behavior.
- VICE 3.10.0, revision `4d283a2e7dd59b7e378524878e81ecc7826b700c`,
  `vice/src/vic20/vic20.h`, for PAL and NTSC timing, including the partial
  first/last NTSC raster lines. VICE revision
  `d928b850b7ef4c1b1826a6efd175d8eaa80344d8`, `vic20mem.c`, `vic20via1.c`,
  `vic20via2.c`, and `data/VIC20/gtk3_sym.vkm`, for open bus, keyboard
  matrix, shifted legends and VIA wiring. The diagram's row/column comment
  must be read together with `rev_keyarr` and the keymap, not in isolation.
  At the 3.10.0 revision, `vic/vic-mem.c` and `vic20/vic20sound.c` inform
  register behavior, divider/shift-register sound, noise taps and filtering.
  The palette uses VICE's original VIC-I RGB calibration, not VIC-II colors;
  the nonlinear mixer now uses 39 interpolation knots derived from its 436
  published voltage-calibration entries. Maximum checked f32 reconstruction
  error is 0.008516 PCM16 units. Calibration attribution: Rami Rasanen and
  Ville-Matias Heikkila, GPL-2.0-or-later, compatible with this repository's
  GPL-3.0-or-later license. Filter update order follows the same RC network.
  <https://github.com/VICE-Team/svn-mirror> (GPL-2.0-or-later).
- MAME revision `848a0b5b0ab016f2fb22f259c9fc7eba0cfe8605`,
  `src/mame/commodore/vic20.cpp`, for independent address-decode checks and
  firmware SHA-1/size metadata. Its `COL` inputs and `via2_pa_r` independently
  establish the Port B selection / Port A input orientation used here.
  <https://github.com/mamedev/mame> (driver: BSD-3-Clause).

Firmware and generated reference images are not committed. The calibration
data above are reused with attribution; no reference emulator algorithm was copied.
Firmware hashes establish identity only; firmware redistribution permission
has not been established. The configured RomM supplied the four hash-verified
VIC-20 images for private execution in both retroclawd and VICE.
Boot/input captures and tone recordings were made with the installed VICE 3.9
GTK3 Win64 `xvic.exe`, SHA-256
`a385207b21df29459f5304bf6ed1da90c974512ead3a7ce1e340c46b01d3134f`.
This executable identity is separate from the 3.10.0 hardware-source pin above.
`docs/vic20-gaps.md` records capture commands, logical-pixel conversion,
measured pitch/RMS, the original gain regression and remaining gaps. Golden hashes, not images,
are stored in the manifest.

## Commodore 64 hardware (`crates/machines/c64`, `crates/chips/vic-ii`)

PLA/6510-port memory banking (which of RAM/BASIC ROM/KERNAL ROM/
character ROM/I/O registers is visible in the `$A000-$BFFF`/
`$D000-$DFFF`/`$E000-$FFFF` windows, keyed off the `LORAM`/`HIRAM`/
`CHAREN` bits of the 6510's on-chip I/O port at `$0001`) -- the
commonly-cited Commodore 64 Programmer's Reference Guide banking table,
echoed consistently across many independent C64 emulator implementations
and hardware references (same "how the machine works, not attributed to
one project's specific choice" tier as this project's AY-3-8910/6502
opcode-semantics entries above). Not cross-checked against
`frodo-libretro`/`vice-libretro` source in this session -- same
no-outbound-network caveat as the 6502 illegal-opcode entry above;
`TODO.md`'s "Commodore 64 support" section still names both as the
intended references once fetchable.

VIC-II's 16-color palette (`crates/chips/vic-ii`'s `PALETTE` constant) --
the widely-cited "Pepto" RGB approximation used across many independent
emulators. Real C64 video output is analog composite/RF through a CRT,
with no single "correct" digital RGB value -- every emulator picks its
own calibration; this is a commonly-cited approximation, not a
hardware-measured constant (same epistemic tier as this project's own
`SpeakerType` filter cutoffs, see `CLAUDE.md`).

MOS 6526 CIA Timer A semantics (start/stop bit, one-shot vs. continuous
run mode, force-load, the latch-vs-live-counter split between a `TA_HI`
write while the timer is running vs. stopped, and the ICR's set/clear-
mask write protocol) -- the 6526's own well-documented, widely-published
register behavior, same tier as the banking table above; also not
cross-checked against a real datasheet or `frodo-libretro`/
`vice-libretro` source in this session.

VIC-II raster timing (`crates/chips/vic-ii`'s `CYCLES_PER_LINE` = 63,
`TOTAL_RASTER_LINES` = 312, real PAL C64 values) -- commonly documented
across independent VIC-II references, same tier as the entries above;
added specifically because a real KERNAL ROM's boot sequence was
confirmed live to busy-wait on the raster register (`$D012`/`$D011` bit
7) during its own init, not because raster IRQ support was planned yet
(that's still deferred, see `TODO.md`) -- without a live, advancing
raster counter, that boot-time wait never ends.

## VIC-II raster IRQ (`crates/chips/vic-ii`, 2026-09-27)

`$D019`/`$D01A`'s bit layout (bit 0 = raster-compare match, bit 1 =
sprite-background collision, bit 2 = sprite-sprite collision, bit 3 =
light pen, bit 7 of `$D019` = the overall "/IRQ asserted" flag), the
`$D019` write-1-to-clear acknowledgment protocol, and the raster
*compare* register's construction (`$D012`'s stored byte as the low 8
bits, bit 7 of `$D011`'s stored byte shifted up as the 9th bit, distinct
from the *live* raster position those same two registers report on
read) were all confirmed directly against VICE's own real source
(`VICE-Team/svn-mirror`, `vice/src/vicii/vicii-irq.c`'s `d019_store`/
`vicii_irq_set_line`/`vicii_irq_check_state` and `vicii-mem.c`'s
`$D019` write case, fetched this session via `WebFetch` -- GitHub raw
content was reachable through this session's network egress policy
when several other independent VIC-II reference sites (c64-wiki.com,
zimmers.net, oxyron.de, en.wikipedia.org, a janklingel.de PDF mirror of
the MOS 6567/6569 datasheet) were not), not guessed and not carried over
from the earlier AY-3-8910/6502-tier "commonly documented, not
cross-checked" caveat this file's other C64 entries above still carry.
Sprite-sprite/sprite-background collision and light pen stay
unimplemented (nothing in this milestone's scope ever sets those latch
bits) -- only the raster-compare source is wired end to end, per
`TODO.md`'s VIC-II coverage sequence (raster IRQ before bad-line cycle
stealing, bitmap/multicolor modes, or sprites).

## VIC-II bad-line cycle stealing (`crates/chips/vic-ii`, 2026-09-27)

The bad-line condition (bad lines allowed this frame AND the raster
line's low 3 bits match `$D011`'s YSCROLL bits AND the line falls inside
the real `$30`-`$f7` DMA window), the DEN (Display ENable, `$D011` bit
4) latch that (re-)decides "allowed this frame" specifically at the
moment the raster reaches `$30` (not at raster line 0/frame start), and
the flat 40-stolen-cycles figure (cycles 15-54 inclusive of the 63-cycle
line, one c-access per text column) were all confirmed directly against
real sources fetched this session via `WebFetch`: VICE's own source
(`VICE-Team/svn-mirror`, `vice/src/vicii/vicii-fetch.c`'s bad-line
condition and `vicii.c`'s `vicii_raster_draw_alarm_handler`/
`vicii_powerup` for the DEN latch -- its simpler, non-cycle-exact core,
not the separate cycle-exact `x64sc` engine's finer per-cycle DEN OR
described in `randyrossi/vicii-kawari-vice`'s fork of `vicii-badline.c`/
`vicii-mem.c`'s `d011_store`, which this project's own per-instruction
tick granularity has no way to reproduce) and an independent VIC-II
timing reference (a c64os.com FLI-timing article, surfaced via
`WebSearch`, giving the same "cycles 15-54 = 40 stolen cycles" figure).
Several other independent VIC-II reference sites (c64-wiki.com,
zimmers.net, oxyron.de, en.wikipedia.org, janklingel.de) were blocked by
this session's network egress policy, same as the raster-IRQ entry
above.

This project's own DEN-latch approximation (a single sample of DEN's
value at the instant the raster reaches `$30`, rather than a true
per-cycle OR across that whole line) is a real, coarser tier than
hardware's genuine per-cycle latch -- deliberately matched to VICE's own
simpler `vicii.c` core rather than invented, since this project has no
sub-raster-line CPU/VIC timing resolution to do better with (see
`vic_ii::Vic::allow_bad_lines`'s own doc comment). The stolen-cycle
count itself (a flat 40, always) is likewise a documented simplification
of a real figure that can vary by a cycle or two depending on which
phase of a multi-cycle CPU instruction the steal begins on -- not
reachable without true per-cycle CPU/VIC bus arbitration, which
`mos6502::Cpu`/`c64::C64Bus` don't implement (unlike `z80`/`cpc::CpcBus`'s
own `contend_memory`/`advance_tstates` hooks).

## VIC-II bitmap and multicolor modes (`crates/chips/vic-ii`, 2026-09-27)

The exact bit layout of `$D016` (XSCROLL bits 0-2, CSEL bit 3, MCM bit 4),
`$D011` (YSCROLL bits 0-2, RSEL bit 3, DEN bit 4, BMM bit 5, ECM bit 6,
raster bit 8 at bit 7), and `$D018` (VM13-VM10 at bits 7-4 selecting the
video matrix base in 1KB steps, CB13-CB11 at bits 3-1 selecting the
character-generator base in 2KB steps for text-family modes, CB13 alone
at bit 3 selecting one of two 8KB-aligned bitmap bases for either bitmap
mode) -- plus the pixel/color-decode algorithm for all four new modes
(multicolor text's per-cell bit-3 mode-select and 2-bit-pair-to-4-colors
mapping; standard bitmap's screen-RAM-nibble ink/paper split; multicolor
bitmap's four-color-source assembly from background 0, screen-RAM's two
nibbles, and color RAM; extended color mode's per-cell background
selection from the character code's top 2 bits plus 6-bit-masked
character addressing) -- were all confirmed directly against two real
sources fetched this session via `WebFetch`/`WebSearch`: VICE's own
source (`VICE-Team/svn-mirror`, `vice/src/vicii/vicii-draw.c`'s real
per-mode drawing routines and `vicii-mem.c`'s `d011_store`/`d016_store`
register handlers) and Christian Bauer's "The MOS 6567/6569 video
controller (VIC-II) and its application in the Commodore 64" -- the
single most widely-cited unofficial VIC-II reference, used by virtually
every independent VIC-II implementation including VICE's own (fetched
from a GitHub mirror, `FrankBoesing/Teensy64`'s `extras/docs/
VIC-Article.txt`, after the article's own original host,
`ist.uwaterloo.ca`, was blocked by this session's network egress policy,
same as several other VIC-II reference sites the raster-IRQ/bad-line
entries above already list as blocked).

This project's own scope decisions, not hardware facts: VIC bank
selection (real CIA2 Port A wiring) stays unmodeled, so every address
this pass computes is relative to a fixed bank 0 -- the same "CIA2
completion is a separate, still-deferred roadmap item" boundary the
bad-line entry above already accepts. Character-ROM ROM-shadow behavior
(real hardware substitutes its internal character generator for RAM
specifically when `$D018`'s character base lands at bank-relative
`$1000`/`$9000`, which is what lets other addresses hold a custom
redefined character set) is not modeled either -- `character_base` is
computed for completeness/documentation but the actual render calls
still take a host-supplied `char_rom` directly, exactly matching
`render_text`'s own pre-existing scope boundary, not a regression
introduced by this pass. The "invalid" `ECM+BMM` mode combinations
render solid black rather than attempting real hardware's exact
per-sub-case output, since no real C64 software deliberately depends on
one and neither source above documents a byte-exact result to reproduce
instead of guessing.

Verified against real hardware after implementing: booting this
project's own real, committed KERNAL/BASIC/character-generator ROMs
(`roms/c64/kernal-901227-03.bin` etc.) through a throwaway (not
committed) test confirmed the real KERNAL boot sequence itself programs
`$D018` to `0x14` (matching this pass's own expected default exactly)
and that `Machine::render_frame` still produces a real, non-blank
picture through the new `$D018`-derived addressing -- not simply
inferred from the unit tests alone.

## VIC-II hardware sprites (`crates/chips/vic-ii`, 2026-09-27)

The full sprite register map ($D000-$D00F X/Y positions, $D010 X MSBs,
$D015 enable, $D017 Y-expand, $D01B priority, $D01C multicolor enable,
$D01D X-expand, $D025/$D026 shared multicolor colors, $D027-$D02E
individual colors), sprite dimensions (24x21, 3 bytes/row, 63 bytes
total), the sprite-data-pointer mechanism (the video matrix's own last 8
bytes, one per sprite, each byte times 64 giving that sprite's data
address), the screen-position offsets (raw `MxX`/`MxY` of `24`/`50`
place a sprite's top-left corner at the active display area's own
top-left corner), sprite-to-sprite priority (sprite 0 highest, sprite 7
lowest, always independent of each sprite's own `$D01B` bit), the
multicolor pixel-pair color mapping (`00` transparent, `01` `$D025`,
`10` the sprite's own color, `11` `$D026`), X/Y expansion (each source
pixel/row doubled, resolution unchanged), and the foreground/background
priority rule (decided purely by the chip-wide MCM bit -- `1`-bit
granularity when clear, `10`/`11` 2-bit-pair granularity when set --
independent of BMM/ECM and independent of any *per-cell* multicolor
text opt-in) were all confirmed directly against Christian Bauer's VIC-II
article (fetched this session, same GitHub mirror the display-mode
entry above already cites, `FrankBoesing/Teensy64`'s `VIC-Article.txt`).
The `24` X-offset constant was additionally cross-checked as a literal
present in VICE's own real source (`vicii-sprites.c`'s
`vicii_sprite_offset`); the border's own real highest-display-priority
fact (it overlays sprites too) was confirmed the same way and is what
justifies this crate's own sprite-clipping-at-the-320x200-canvas-edge
behavior as a real hardware fact, not merely an architectural
convenience.

Two real, deliberate scope boundaries: VIC bank selection stays fixed at
bank 0 (real CIA2 wiring, a separate still-deferred roadmap item, same
boundary the bitmap/multicolor-mode entry above already states), and
the real "sprite crunching" glitch (mid-render Y-expansion changes can
corrupt a real chip's row counter) isn't modeled, a rare, deliberately-
exploited timing trick rather than baseline behavior.

Sprite-sprite and sprite-background **collision detection**
(`$D01E`/`$D01F`, and the `$D019` IRQ bits they'd otherwise drive) is
explicitly NOT implemented this pass, for an architectural reason
distinct from every other still-deferred item in this file: real
collision detection happens continuously as the chip scans out each
raster line, but this crate's `render()` (and the `Machine::
render_frame` trait method every machine here implements, `&self` by
design) is a once-per-*display*-frame snapshot decode, never called on
anything synchronized with `tick()`'s own per-CPU-instruction cadence
the way raster IRQ/bad-line detection both genuinely are. Computing
collision state only when a display frame happens to be rendered would
be real but arbitrarily stale relative to actual gameplay timing, and
could never drive a real CPU IRQ (only `c64::C64::step`, never
`render()`, ever calls `mos6502::Cpu::request_irq`) -- shipping a
misleadingly-partial collision implementation was rejected in favor of
leaving it fully deferred until a real per-scanline sprite pipeline
exists (see `TODO.md`).

Verified against real hardware after implementing: booting this
project's own real, committed KERNAL/BASIC/character-generator ROMs
through a throwaway (not committed) test confirmed the real KERNAL boot
sequence leaves `$D015` (sprite enable) at `0x00` as expected (the
KERNAL itself never uses sprites), and that manually enabling a sprite
afterward still renders a real frame through `Machine::render_frame`
without panicking.

## C64 keyboard matrix (`crates/machines/c64/src/keyboard.rs`)

The 8x8 column/row matrix layout (which physical key sits at which
CIA1 Port A column / Port B row) -- the standard, widely-published C64
keyboard matrix table, commonly cited across independent references
(e.g. VICE's own documentation). Not cross-checked against a real
KERNAL ROM disassembly or a datasheet in this session -- same
no-outbound-network caveat as this project's other not-yet-verified C64
entries above.

## SID chip emulation, phase 1 (`crates/chips/sid`, 2026-09-27)

The MOS 6581/8580 SID's internal architecture -- the 24-bit phase
accumulator and deriving triangle/sawtooth/pulse waveforms from its top 12
bits, the 23-bit noise LFSR and its specific tap pattern (feedback from
bits 22/17, output taken from bits 22/20/16/13/11/7/4/2), the 3-voice hard
sync/ring-mod wiring (each voice's sync/ring source is the *previous* voice
in the ring, voice 1's is voice 3), and the ADSR envelope generator's
linear rate-counter table plus its exponential decay/release divisor
breakpoints (255-93: /1, 93-54: /2, 54-26: /4, 26-14: /8, 14-6: /16, 6-0:
/30) -- is the chip's own widely-published, widely-republished internal
behavior (echoed consistently across reSID, jsSID, TinySID, and many
independent SID-emulation write-ups going back decades), not attributed to
one specific source, same "how the chip works" epistemic tier as this
project's AY-3-8910 entry above. No SID emulator source was copied; this is
an independent implementation from the published architecture.

Two deliberate approximations, not hardware measurements: combined-waveform
selection (more than one waveform bit set in a voice's control register)
uses the common "bitwise AND of the selected waveforms' own values"
simplification most non-cycle-exact emulators use, since the real combined
output is a chip-specific analog artifact no simple formula reproduces
exactly; and the real "ADSR delay bug" (retriggering the envelope while its
rate counter is mid-count can delay the next state change by one extra
cycle) is not modeled. The analog filter (cutoff/resonance/routing
registers) is stored for round-tripping but not yet applied to the mixed
signal -- a separate, later roadmap phase per `TODO.md`.

## CIA2 completion: Timer B, TOD clock, serial register, VIC bank select (`crates/machines/c64/src/cia.rs`, `crates/machines/c64/src/bus.rs`, 2026-09-27)

The MOS 6526 CIA's remaining register behavior -- Timer B (including its
own INMODE field, which can clock it from Timer A's underflow output
instead of phi2, real hardware's way of chaining the two into an effective
32-bit timer), the Time-of-Day clock's BCD tenths/seconds/minutes/1-12-hour
registers with a PM flag, its real read-latch (reading hours freezes all 4
registers for a consistent snapshot; reading tenths releases the latch,
still returning the frozen snapshot value on that exact read, not live
time -- the entire point of the latch is a guaranteed-atomic 4-byte read)
and write-halt (writing hours stops the clock; writing tenths resumes it,
so software can't race a live rollover mid-write) mechanisms, the alarm
compare-and-interrupt registers (selected via CRB bit 7 instead of the
clock registers), the output-mode serial shift register (clocked by Timer A
underflow, one bit per underflow, firing its own ICR bit once a full byte
has shifted), and the force-load control-register bit reading back as a
self-clearing strobe rather than stored state -- are the chip's own well-
documented, widely-published register behavior, same "how the chip works"
trusted-reference tier as this project's AY-3-8910/SID entries above, not
independently checked against a datasheet scan in this session.

The real, load-bearing hardware fact this pass fixed rather than just
implemented net-new: **CIA2's own IRQ output is wired to the 6510's NMI
input, not its IRQ input** (CIA1 gets the real IRQ pin instead) -- per the
commonly-cited C64 Programmer's Reference Guide's own interrupt-wiring
description. Before this pass, `c64::bus::C64Bus`'s `cia2` field was a
pure register-storage stub that `C64::step` never even called `tick` on,
so its timers/TOD/serial register never actually ran regardless of what
software wrote to them; `C64::step` now ticks it every step exactly like
CIA1, but routes a pending interrupt to `mos6502::Cpu::request_nmi()`
instead of `request_irq()`.

**Real VIC bank selection** (CIA2 Port A bits 0-1, `$DD00`, an inverted
2-bit field: `0b11` = bank 0 `$0000`, `0b10` = bank 1 `$4000`, `0b01` =
bank 2 `$8000`, `0b00` = bank 3 `$C000` -- also per the C64 Programmer's
Reference Guide's own memory map chapter) is now wired too, closing the
gap `vic-ii`'s own bitmap/multicolor and sprite entries above both
explicitly flagged as "stays fixed at bank 0, a separate still-deferred
item". The fix needed no `vic_ii` crate change at all: `Vic::render`'s own
internal offsets (`video_matrix_base`/`bitmap_base`/the sprite-pointer
table) were already bank-relative (never past `$3FFF`) -- they were only
ever bank-0-hardcoded because `C64Bus::render_frame` always handed `render`
a 64K slice starting at absolute `$0000`. `C64Bus::vic_bank_base` derives
the real bank base from CIA2's effective Port A value (reusing the same
"driven bits reflect what was written, undriven bits float high" pin
convention `port01`/`keyboard_port_b` already established), and
`render_frame` now slices `self.bus.ram` at that base instead. Confirmed
behavior-preserving for the existing default case via a throwaway,
uncommitted probe: booting the real, already-committed KERNAL/BASIC/
character-generator ROMs under `roms/c64/` for 2,000,000 real cycles shows
the KERNAL leaving CIA2's Port A DDR partially configured (`$3F`) but never
touching bits 0-1's drive state, so `vic_bank_base()` still resolves to
bank 0 exactly as every pre-existing test/boot path here already assumed.

**Explicitly not modeled** (a real, accepted scope cut, not an oversight):
the CNT and SP pins are real external lines this project has no host wiring
for on either CIA yet (no cassette-read-edge signal, no real IEC serial-bus
clock, no RS-232 line driver) -- every CNT-gated timer input mode and the
shift register's own CNT-clocked *input* mode stay inert, storage-only,
same "don't approximate a signal this project has no real source for"
stance the rest of this codebase already takes elsewhere (e.g. ZX81's
unmodeled audio). The real, documented MOS6526 AM/PM erratum around the
exact 12:00:00 hour rollover is not reproduced -- `tod_inc_hour` always does
the straightforwardly-intended toggle, same "trusted reference, not a claim
of one universal real-chip result" tier as `mos6502`'s own unstable-
illegal-opcode group. `Cia` stays local to `c64::cia` rather than becoming
a shared `crates/chips/cia-6526` crate -- CIA1 and CIA2 are both `Cia`
instances wired differently by one host bus, not two different crate
consumers, so `TODO.md`'s own "extract only once a second real use case
exists" condition still isn't met by this pass alone.

## SID chip emulation, phase 2 — analog filter (`crates/chips/sid`, 2026-09-27)

The real SID filter is an analog state-variable (multi-mode) filter -- two
cascaded integrators around a feedback loop, built from the chip's own
op-amps -- not a digital biquad. `Filter` implements this using the classic
discrete "Chamberlin" state-variable filter recurrence: a standard,
independently-derivable digital approximation of exactly this analog
topology (`high = input - low - q_inv*band; band += w0*high; low +=
w0*band`), widely taught in digital signal processing references rather
than attributed to one specific SID-emulator source. Stepped once per real
system clock cycle inside `Sid::tick_one_cycle` (continuous integration,
matching how the real analog filter is always running rather than being
resampled at the host's much slower audio sample rate) -- a genuinely
different granularity than most of this crate's other per-cycle mechanisms
needed, since the filter's *input* is the routed voices' own continuously-
changing instantaneous levels, not something that can be reconstructed
lazily from `output()` alone (which is only ever queried once per audio
sample, with no per-cycle history to integrate over).

Two real, load-bearing hardware facts confirmed and implemented, not just
the filter math itself: **"voice 3 off"** (`$18` bit 7) fully disconnects
voice 3 from the final mix regardless of filter routing (real hardware's
own way of using voice 3 purely as an inaudible modulation/OSC3-randomness
source), and **an unselected filter output** (none of `$18`'s LP/BP/HP
select bits set) means the filter's own output doesn't reach the mixer at
all -- every voice routed to a filter with no tap selected is silently
inaudible, not silently bypassed back to the direct mix. Both were
confirmed against the commonly-cited SID register reference alongside the
filter topology itself, and both are covered by dedicated regression tests
distinguishing them from a simple "filter has no effect" bug.

Two new, explicitly documented approximations, same epistemic tier as this
crate's existing combined-waveform/ADSR-delay-bug notes above: the real
cutoff-register-to-frequency mapping is well known to be nonlinear and to
differ significantly between the 6581 and 8580 revisions (driven by real
DAC nonlinearity that even varies between individual 6581 chips) --
reproducing either curve exactly needs a per-chip-measured lookup table
(reSID's own approach); this instead uses a single linear approximation
across both revisions, spanning the commonly cited real range of roughly
30Hz-12kHz. And the real resonance register's behavior is likewise
documented as inconsistent across individual chips and can reach true
self-oscillation at its top value -- this uses a documented, clearly-
approximate linear mapping deliberately kept short of self-oscillation, for
numerical stability of the discrete recurrence rather than any claim of
matching one specific chip's real resonance curve. The external-audio-input
routing bit (`$17` bit 3, "EXT IN") is stored but never contributes
anything -- no external audio input exists anywhere in this project, same
"no real source to approximate" stance as `c64::cia::Cia`'s own unwired
CNT/SP pins.

Verified numerically stable (no NaN/out-of-range output) via a throwaway,
uncommitted probe: booting the real, already-committed KERNAL/BASIC/
character-generator ROMs under `roms/c64/` for 2,000,000 real cycles with
a voice routed through the filter at *maximum* resonance and all 3 filter
taps selected simultaneously produced only finite, in-range samples.

## MOS 6522 VIA chip emulation (`crates/chips/via-6522`, package `via6522`, 2026-09-27)

First piece of the "IEC bus and 1541 drive" parity-extension track (see
`TODO.md`) -- a standalone, host-agnostic VIA chip crate, same shared-
chip-crate reasoning as `ay-3-8910`/`sid` (the 6522 is the Commodore
1541's own I/O chip, but also the VIC-20's, the C64's own user port's, and
many other 6502-family machines'). The full 16-register map (`$0`-`$F`),
both parallel ports with data-direction registers, Timer 1 (continuous
and one-shot modes, plus its optional dedicated `PB7` square-wave output
-- which real hardware fact takes over that Port B bit entirely,
independent of `DDRB`/`ORB`), Timer 2 (one-shot timed-interrupt mode plus
pulse-counting mode, counting real negative edges on `PB6`), the `CA1`/
`CA2`/`CB1`/`CB2` peripheral-control-register modes (edge-select
interrupt inputs, independent vs. non-independent IFR-clear-on-port-
access semantics, handshake output, pulse output, and manual output),
and the IER's set/clear-mask write protocol (mirroring `c64::cia::Cia`'s
own `$0D` ICR protocol) are all the chip's own widely-published, widely-
republished register behavior -- same "trusted external reference, not
verified against this project's own silicon" epistemic tier as this
project's other chip crates, not attributed to one specific source.

A genuine, distinctive hardware difference from `c64::cia::Cia` (this
project's other 6502-family timer/port chip) is implemented precisely,
not glossed over: the 6522 has **no start/stop control bit for either
timer at all** -- both are always counting down every real phi2 cycle
from the moment the chip exists (Timer 2 only in its one-shot sub-mode;
pulse-counting mode instead only decrements on a real `PB6` edge).
"Starting" a timer is simply writing its high-order counter register,
which loads the counter from the latch and clears that timer's own
interrupt flag -- there's no separate enable/disable concept the way
CIA's `CRA`/`CRB` start bits work. `Via::tick` therefore unconditionally
decrements Timer 1 every cycle in both modes, unlike `Cia::tick`'s own
running-flag gate -- confirmed via a dedicated regression test that a
never-started, freshly-constructed VIA's Timer 2 still counts down and
fires purely from repeated `tick` calls.

A second real, easy-to-get-wrong distinction implemented and directly
regression-tested: register `$01` ("ORA with handshake" -- a CPU access
clears `CA1`'s pending flag and can trigger `CA2`'s handshake/pulse
output) versus register `$0F` (the exact same Port A data, but with none
of those side effects at all) are genuinely different addresses in the
real register map, not two names for one behavior -- most of this
project's own other chip register maps don't have this particular
mirrored-but-behaviorally-distinct quirk, so it was deliberately
verified with its own test (`ora_handshake_register_clears_ca1_but_the_
no_handshake_alias_does_not`) rather than assumed correct by symmetry
with `$00`/Port B (which has no such alias at all -- another real,
asymmetric hardware fact).

**Explicitly simplified, documented scope cut** (same tier as `c64::
cia::Cia`'s own unwired CNT/SP pins and `sid::Sid`'s unwired EXT IN):
the shift register's real 8 distinct modes are collapsed into
"disabled", "clocked by an already-ticked internal source (T2 or phi2)"
(faithfully modeled, since both are already ticked every cycle), and
"clocked by an external `CB1` pulse" (left inert -- no real external
serial-data device is wired to anything in this project yet). Only the
completion timing (`IFR` bit 2 after 8 clock pulses) is modeled, not the
actual bit-by-bit data movement over a real `CB2` serial line -- the
same scope cut `Cia`'s own output-mode serial shift register already
made, for the identical reason. No host wiring exists yet at all (no
1541/IEC machine built this pass) -- `set_ca1`/`set_ca2_input`/
`set_cb1`/`set_cb2_input`/`set_external_port_a`/`set_external_port_b`/
`pulse_pb6` are the future host-facing hooks, tested standalone here
exactly the way `sid`'s own phase 1 (oscillators/envelope) was built and
verified before phase 2 (the filter) wired it into anything.

## Commodore 1541 machine and real IEC bus wiring (`crates/machines/1541`, `crates/machines/c64/tests/iec_1541.rs`, 2026-09-27)

Second piece of the "IEC bus and 1541 drive" parity-extension track (see
`via6522`'s own entry above and `TODO.md`): the 1541 modeled as its own
real computer (`c1541::C1541` -- `mos6502::Cpu` + 2 `via6522::Via`
instances + its own ROM/RAM), wired to `c64::C64` over a real,
software-visible IEC serial bus, not a shortcut abstraction.

The 1541's own memory map's VIA base addresses and mirroring, and VIA1's
exact Port B bit layout, were confirmed directly against VICE's own real
source this session (shallow-fetched, not recalled): `drive/drivemem.c`'s
own `"VIA1", 0x1800, 0x180f` / `"VIA2", 0x1c00, 0x1c0f` registrations, and
`drive/iec/via1d1541.c`'s own documented bit layout (bit 7 ATN IN, bits
6:5 device-address jumpers, bit 4 ATN ACK, bit 3 CLOCK OUT, bit 2 CLOCK
IN, bit 1 DATA OUT, bit 0 DATA IN -- with the exact real DDRB directional
masks `0xe5`/`0x1a` also confirmed there and used verbatim in this
session's own tests, not arbitrary fixture values). The RAM (`$0000`-
`$07FF`, 2K) and ROM (`$C000`-`$FFFF`, 16K) ranges are the commonly-cited
real 1541 memory map, not independently re-verified against source this
session -- same "commonly documented" epistemic tier this project already
uses for e.g. the ZX Spectrum 128K `0x7ffd` decode. No real 1541 ROM image
exists under `roms/` (this project has no legally-suppliable copy to add,
unlike the C64's own user-supplied KERNAL/BASIC/character ROMs) -- every
test this pass uses synthetic ROM content, same as this project's other
machines' own unit tests already do before/without a real ROM attached.

The C64 side's own CIA2 Port A (`$DD00`) bit layout and its real polarity
convention were also confirmed directly against VICE's real source this
session: `c64/c64cia2.c`'s own comment block ("Bit 7 Serial Bus Data
Input / Bit 6 Serial Bus Clock Pulse Input / Bit 5 Serial Bus Data Output
/ Bit 4 Serial Bus Clock Pulse Output / Bit 3 Serial Bus ATN Signal
Output"), and `store_ciapa`'s own `~byte` inversion before handing the
value to VICE's internal bus-combination logic -- read correctly, this
confirms (rather than contradicts) the expected real hardware fact: the
3 `*_OUT` bits use an inverting driver, so writing `1` to `ATN`/`CLOCK`/
`DATA` OUT *asserts* that line (pulls it low on the real, physical open-
collector bus), while the 2 `*_IN` bits read the real, current wired-AND
bus level directly (`1` = released/high). The exact same convention holds
on the drive's own VIA1 Port B, a genuine, pleasing hardware symmetry
(both ends of the same open-collector bus use the identical inverting-
driver circuit) rather than a coincidence.

`c64::bus::C64Bus::cia2_port_a_read` is the one real code change on the
C64 side: CIA2 Port A's bits 6/7 (`CLOCK IN`/`DATA IN`), when configured
as input (the real KERNAL's own default), now reflect the actual external
IEC bus level instead of just floating high -- same "host wires the
chip-specific bits into an otherwise-generic port read" split
`keyboard_port_b` already established for CIA1's own Port B. `C64` has no
`ATN IN` bit or wiring at all -- a real hardware fact, not an oversight:
the C64 is always the bus's `ATN` master, no peripheral drive on stock
IEC hardware ever drives it back.

`c1541::IecBus` (the 3-line, wired-AND shared bus type) lives in the
`c1541` crate, not `c64` or `machine` -- the disk drive is the thing that
gives this bus its reason to exist, and `c64` depends on `c1541` purely as
a `[dev-dependencies]` entry to reach this one shared type for its own
integration test, never as a real production dependency in either
direction (same "neither machine crate needs to know the other exists at
runtime" stance this project's `Machine` trait abstraction already
generally holds to). Modeled with exactly the 2 real participants this
project has today -- generalizing to more (a real second drive, a
printer) is deferred until a second real device exists to prove that
shape against, same "don't invent a shared shape from one data point"
stance already applied elsewhere in this project (e.g. `zx-tape`'s own
undecided `Loader` trait).

A real, subtle bug was found and fixed while building this, the kind this
project's own testing methodology exists to catch: `via6522::Via::new()`'s
bare `#[derive(Default)]` leaves its internal `CA1`-edge-tracking state at
`false` ("pin already low"), which mismatched this crate's own assumed
idle-high (`true`) baseline for the IEC `ATN` line -- without an explicit
`via1.set_ca1(true)` seed call in `C1541Bus::new`, the very first real
`ATN` assertion after construction would have looked like "already low, no
edge" and silently failed to fire VIA1's own `CA1` interrupt flag. Caught
by this session's own first attempt at the
`atn_input_reaches_both_port_b_and_the_real_ca1_edge_interrupt` test
failing, not anticipated in advance. The fix belongs in `c1541` (the host
wiring the chip into its own real assumed initial bus state), not
`via6522` itself -- a generic chip crate has no way to know what initial
line level any particular host's own wiring should assume, same "host
wires the chip-specific bits, the chip crate stays generic" split this
project already applies everywhere else.

`c64::C64::iec_output`/`set_iec_input` and `c1541::C1541::iec_output`/
`set_iec_input` are the two machines' own public half of the wiring;
`crates/machines/c64/tests/iec_1541.rs` is a genuine, real-DDR-value
integration test proving 4 real protocol properties end to end: the idle
bus reads high on both sides, the C64 asserting `ATN` reaches the drive as
both a real Port B level *and* a real `CA1`-triggered interrupt-flag
edge, the drive asserting `DATA OUT` is seen by the C64 as `DATA IN`
(and releases correctly), and two participants pulling the same line
low at once correctly wire-AND (only releasing once *both* let go). No
`retroclawd`-level (CLI/GUI/dual-machine-stepping) integration exists yet
-- explicitly out of scope for this pass, see `TODO.md`'s own remaining
items for this track (`.d64`, `.prg`, `.tap`/`.t64`).

## `.d64` disk image format (`crates/formats/d64`, 2026-09-27)

First piece of the "drives and baseline media" parity-extension track's
`.d64` item (see `TODO.md`): a machine-agnostic format crate parsing the
standard Commodore 1541 disk image format, same "format crate only parses
bytes into a plain struct" precedent as `z80-snapshot`/`zx81-snapshot`/
`zx-tape`/`dsk` -- no dependency on `machine`, any CPU crate, or `c1541`.
Wiring an actual parsed `D64` into `c1541::C1541`'s VIA2 (the real 1541
disk mechanism's own GCR shift-register hardware, genuinely different
from `nec765`'s NEC765-compatible-controller-command protocol CPC/+3 use)
is a separate, larger, not-yet-started item -- this crate only had to get
the disk's *logical* track/sector/directory contents right, the same
split `dsk`/`nec765` already have for CPC/+3.

Every geometry and byte-offset constant was confirmed directly against
VICE's own real source this session (shallow-fetched, not recalled --
several attempted fetches against the classic Peter Schepers d64.html
spec page and other reference sites were blocked by this sandbox's
network egress policy, consistent with prior sessions' own experience,
so VICE's source was the fallback, same as several earlier VIC-II/CIA2/
1541 entries above): `diskimage/diskimage.c`'s own `sector_map_d64` table
(`{17, 18, 19, 21}` -- confirming the 4-speed-zone track layout: tracks
1-17 get 21 sectors, 18-24 get 19, 25-30 get 18, 31+ get 17, a real
hardware fact -- a 1541 writes at constant linear bit density on a disk
spinning at constant angular velocity, so outer tracks physically hold
more data than inner ones), `diskconstants.h`'s own `#define`s
(`NUM_BLOCKS_1541`=683/`EXT_BLOCKS_1541`=768 confirming the exact 35-
and 40-track total sector counts and therefore the 4 real recognized
file sizes; `BAM_TRACK_1541`=18/`BAM_SECTOR_1541`=0/`DIR_TRACK_1541`=18/
`DIR_SECTOR_1541`=1/`BAM_NAME_1541`=144/`BAM_ID_1541`=162/
`BAM_EXT_BIT_MAP_1541`=192), and `vdrive/vdrive-dir.h`'s own directory-
slot offsets (`SLOT_TYPE_OFFSET`=2/`SLOT_FIRST_TRACK`=3/
`SLOT_FIRST_SECTOR`=4/`SLOT_NAME_OFFSET`=5/`SLOT_NR_BLOCKS`=30/
`SLOT_SIZE`=32).

That last one caught a real, easy-to-get-wrong subtlety before it became
a bug rather than after: the intuitive guess is that each 32-byte
directory slot's file-type byte sits at slot-relative offset `0` --
VICE's own confirmed `SLOT_TYPE_OFFSET`=2 says otherwise. The real reason
is a genuine hardware/format quirk: only a directory *sector's* very
first slot's bytes 0-1 double as that whole sector's own directory-chain
link (the track/sector of the next directory sector) -- every slot in
that sector, including slots 2-8 which have no real link data of their
own to store there, still gets treated as a uniform 32-byte window
starting 2 bytes before its own real per-entry data begins, purely so
every slot's own meaningful fields land at the same offsets regardless
of position within the sector. Gotten right the first time here (a
dedicated test, `directory_entries_reads_from_slot_relative_offset_two_
not_zero`, locks in the `+2` offset specifically so a future refactor
can't regress it back to the more "obvious" `+0`), unlike a few other
entries in this file that had to be caught by a failing test after the
fact.

The rarer 42-track variant VICE's own `MAX_TRACKS_1541`/`MAX_BLOCKS_1541`
constants also define isn't supported -- no citable source for its own
byte layout was found this session, so `D64::parse` errors clearly on
that size rather than silently guessing at it, same "error clearly,
don't best-effort" convention `z80-snapshot`'s own `UnsupportedHardwareMode`
already established. The real drive error-code byte channel (one extra
byte appended per sector, in track/sector order, for the "with error
info" size variants) is parsed and exposed (`sector_error`) but not
otherwise acted on -- no real GCR-level read attempt exists yet to
actually produce or consult one, same "expose but don't yet act on"
precedent `dsk`'s own weak-sector `copies` field originally established
before `nec765::Fdc`'s `weak_read_counter` existed to consume it.

## GCR disk mechanism wired into VIA2 (`crates/machines/1541/src/gcr.rs`, `crates/machines/1541/src/disk.rs`, `crates/core/6502`'s `set_overflow`, 2026-09-27)

Second, larger piece of the "drives and baseline media" parity-extension
track's `.d64` item (see `TODO.md` and `d64`'s own entry above): wires an
actual parsed `d64::D64` into `c1541::C1541`'s VIA2 -- the real 1541 disk
mechanism (head stepper motor, spindle motor, density/speed-zone select,
and the bit-level GCR SYNC-detection + byte-assembly hardware real DOS
firmware bit-bangs through), genuinely different hardware from `nec765`'s
NEC765-compatible-controller-command protocol, modeled at the real
GCR-bitstream level rather than a shortcut "read this sector" abstraction --
same principle as `nec765` modeling CPC/+3's real FDC command set.

Every real fact below was confirmed against source fetched live this
session, cross-referencing two independent real emulator projects for
extra confidence where a single source's own file layout didn't
immediately yield a complete picture (several attempted fetches against
`unusedino.de`'s classic 1541 hardware reference and other candidate
sites were blocked by this sandbox's egress policy, consistent with prior
sessions' own experience, same fallback-to-emulator-source pattern
`d64`'s own entry above already used):

- **The real 4-bit-to-5-bit GCR code table and sector encoding algorithm**
  -- VICE's own real `gcr.c` (`GCR_conv_data`, `gcr_convert_4bytes_to_GCR`,
  `gcr_convert_sector_to_GCR`), fetched directly this session. `gcr.rs`'s
  `encode_4bytes_to_5` is a literal, faithful port of the source's own
  deliberately obscure rolling 32-bit-register bit-accumulator technique,
  not a reimplementation from a simplified mental model of what it does --
  cross-checked byte-for-byte equal against a from-scratch "pack 8 5-bit
  codes MSB-first into a 40-bit stream" reimplementation over 20000 random
  4-byte inputs (a throwaway Python script, not committed) before trusting
  either, specifically because a hand-port of that exact technique is easy
  to get subtly wrong. `encode_sector`'s field ordering and both checksums
  (header: XOR of sector/track/id2/id1; data: XOR of all 256 real data
  bytes) match `gcr_convert_sector_to_GCR`'s own real source exactly,
  confirmed by a dedicated round-trip test that encodes then fully decodes
  a sector and checks every field, plus a corrupted-checksum test proving
  a bit flip is actually detected, not silently accepted.
- **VIA2's real pin assignments** -- cross-checked against two independent
  real emulator sources for extra confidence, since neither alone
  initially yielded a complete picture: VICE's own `rotation.c` (fetched
  live, narrower re-fetches needed after an initial AI-summarized pass
  refused to quote its ~400-line "1541 circuit simulation" comment block
  verbatim as "exceeds reasonable fair use", so specific short passages
  were re-requested instead) confirmed the real bit-rate table
  (`rot_speed_bps[0]` = `{250000, 266667, 285714, 307692}` bits/sec for the
  4 real speed zones) and the byte-ready-also-drives-the-6502-SO-pin
  mechanism; MAME's own `c1541.cpp` (located via `WebSearch` once VICE's
  own file layout didn't yield a complete VIA2 pinout on its own)
  independently confirmed Port A = raw GCR data byte, Port B bit layout
  (`STP1:STP0`/`MTR`/`DS1:DS0`), and that byte-ready *also* drives VIA2's
  own `CA1` (`.append(m_via1, FUNC(via6522_device::write_ca1))`) --
  initially read as a possible contradiction with VICE's own SO-pin
  finding, resolved by recognizing both are real, simultaneously true
  hardware facts (the same physical byte-ready pulse reaches both the
  CPU's dedicated SO pin *and* VIA2's ordinary `CA1` edge-interrupt input,
  for two different real purposes -- fast polling vs. a slower
  interrupt-driven fallback). `CA2` as SOE (Sync Output Enable, gating
  whether a completed byte's `CA1`/SO pulse actually propagates at all)
  came from the same VICE source. Port B bits 4 and 7, and `CB1`'s real
  meaning, weren't confirmed by either source -- left unwired rather than
  guessed, see `disk.rs`'s own doc comment.
- **The real stepper-motor gray-code sequence** (`[00, 01, 11, 10]`, only
  an adjacent-position transition moves the head by one real half track,
  any other transition -- including a same-tick two-position jump -- is a
  real hardware no-op) and the real mechanical seek limit (track 42) are
  commonly-cited real 1541 facts, not independently re-derived from a
  disassembly this session -- same "commonly documented" epistemic tier
  this project already uses for e.g. the ZX Spectrum 128K `0x7ffd` decode.
  This crate's own absolute direction convention (which end of the
  sequence is "outward") is its own arbitrary choice -- no real 1541 ROM
  exists in this project to validate a specific convention against (same
  gap the crate's own module doc comment already flags for `roms/`).

**A genuine, careful hardware subtlety, verified by manual bit-by-bit
tracing rather than assumed**: while a real SYNC run's own consecutive-
ones count is still below the 10-bit recognition threshold, ordinary byte
framing keeps running completely undisturbed -- so a real SYNC field (40
bits, 5 bytes of `0xFF`) always produces exactly one "bogus" assembled
byte from its own lead-in bits *before* SYNC is recognized and framing
forcibly re-aligns to `0` for the run's remainder. This is correct,
expected real hardware behavior (real DOS polling loops are specifically
written to tolerate/ignore it while waiting for genuine SYNC), not a
modeling defect -- confirmed by an end-to-end test
(`disk::tests::a_full_real_sector_assembles_byte_for_byte_off_a_
synthesized_track`) that initially failed for the wrong reason (searching
the *raw, still GCR-encoded* assembled byte stream for a literal decoded
marker byte that can only ever appear after DOS firmware's own separate
GCR-decode step, which this hardware-level mechanism deliberately never
performs -- see `gcr.rs`'s own doc comment on why `decode_nibble`/
`decode_5bytes_to_4` are test-only) -- fixed by asserting the assembled
stream matches the independently-computed raw GCR bytes directly instead,
which is what actually exercises and proves the bit-level SYNC/byte-
assembly logic. A related, initially-backwards assumption caught the same
way: the codec's real run-length guarantee that makes SYNC detection
possible bounds consecutive **`1`** bits (max 8 across any two adjacent
real code words, confirmed by direct computation across all 256 adjacent-
code-pair combinations), not consecutive `0` bits (also bounded, at 2, but
a different real property, relevant to self-clocking recovery rather than
SYNC detection) -- `gcr.rs`'s own
`no_adjacent_pair_of_valid_codes_ever_produces_a_real_sync_run_or_more_
than_2_zeros` test checks both, correctly this time.

**Real gap-byte counts between sync/header/data fields are an accepted,
documented approximation** (`gcr::HEADER_GAP_BYTES`/`SECTOR_GAP_BYTES`),
not byte-exact per-zone values -- real firmware pads a track's gaps to
fill exactly one physical rotation at that zone's own bit rate, which
this crate doesn't attempt to reproduce; what matters for a real DOS
routine reading this synthesized track is that real SYNC marks correctly
bound each sector, not that the total track byte count matches a real
drive's own rotation exactly. Same epistemic tier as
`zx_spectrum::SpeakerType`'s filter cutoffs.

`mos6502::Cpu` gained `set_overflow` (the real, dedicated SO input pin
every 6502 datasheet documents -- sets the V flag immediately, with no
interrupt-enable gating or vectoring, unlike `request_irq`/`request_nmi`)
specifically to support this real byte-ready mechanism; a small, focused,
backward-compatible addition to an already-ZEXALL/Klaus-Dormann/Seddon-
suite-validated core, not a redesign.

Explicitly out of scope for this pass, same as `d64`'s own entry above:
no disk *write* path (`CB2`/`OE` stay unwired, no GCR encoder is ever
invoked from a CPU-driven write), and no `retroclawd` CLI/GUI integration.

## `.tap`/`.t64` cassette loading (`crates/formats/c64-tape`, `crates/machines/c64/src/cia.rs`, `crates/machines/c64/src/bus.rs`, `crates/plugins/c64-machine-plugin`, 2026-09-27)

Fourth item of the "drives and baseline media" parity-extension track
(see `TODO.md`). Several attempted fetches against `c64-wiki.com`,
`codebase.c64.org`, `sleepingelephant.com`, and `en.wikipedia.org` (all
plausible authoritative sources for the real `.tap`/`.t64` format specs
and cassette hardware wiring) were blocked by this sandbox's egress
policy this session, consistent with every prior session's own
experience -- VICE's own real source (fetched via `raw.githubusercontent.com`,
which mixes `master` and `main` as valid branch refs for different files
in the same repository) was the working fallback, same
fetch-the-emulator's-own-source pattern `d64`'s and the GCR disk
mechanism's own entries above already established.

- **`.tap` ("C64-TAPE-RAW") pulse encoding** -- VICE's own real
  `src/tape/tap.c` and `src/datasette/datasette.c`, both fetched live this
  session. Initially read `tap.c`'s own `tap_get_pulse` helper as the
  authoritative decode (a nonzero byte `N` scaled `>> 3` from an assumed
  already-`*8`-scaled stored value), which appeared to contradict
  `datasette.c`'s own real playback code. Resolved by recognizing
  `datasette.c`'s `datasette_read_gap`/`fullwave` handling is the actual
  real-time playback engine (what a real Datasette motor does cycle by
  cycle), while `tap_get_pulse` serves a separate, non-playback internal
  purpose (an import/pilot-detection aid) with its own, differently-scaled
  convention -- trusting the real player over the incidental helper is
  what this crate's own `parse_tap` implements. Confirmed: a nonzero pulse
  byte is `N * 8` real CPU cycles; a `0x00` byte in a **version 1** file
  escapes to the next 3 bytes, a little-endian 24-bit *exact* cycle count
  (no further `* 8` scaling); a `0x00` byte in a **version 0** file has no
  real, spec-defined meaning at all -- `datasette.c`'s own code falls back
  to an arbitrary internal engineering constant
  (`datasette_zero_gap_delay`) for it, which this crate deliberately does
  NOT replicate (`Error::ZeroByteInVersion0` instead, matching this
  project's own "error clearly, don't guess" convention already
  established for e.g. `UnsupportedHardwareMode`). **Version 2**
  ("half-wave", real but C16/Plus4-only per VICE's own source) is out of
  scope for the C64 parser; the separate `ted` API above handles it.
  There is no separate "level" encoded
  anywhere in the file -- each pulse is a real half-wave toggle, confirmed
  via `datasette.c`'s own `fullwave[port] ^= 1`.
- **`.t64` directory container layout** -- VICE's own real
  `src/tape/t64.h` (the `T64_FILE_RECORD_*` entry-type enum, confirming
  `NORMAL == 1` is the only real, loadable-program type) and
  `src/tape/t64.c` (header/record byte offsets), both fetched live this
  session. 64-byte header (32-byte description, `u16` LE version, `u16` LE
  max/used entry counts, 2 reserved bytes, 24-byte tape name) and 32-byte
  directory records (entry type, C1541 file type, `u16` LE start/end
  address, `u32` LE absolute file offset to the program's own raw bytes,
  16-byte PETSCII name) match this source exactly. Real files are
  routinely loose about header entry counts and end-address fields (a
  known real-world quirk, not this crate's own invention) -- `parse_t64`
  is permissive: it reads however many whole 32-byte records actually fit
  in the file rather than trusting the header's own counts, and falls
  back to reading until EOF from a record's `contents_offset` when
  `end_addr - start_addr` isn't a positive length.
- **Real C64 cassette hardware wiring is split across two genuinely
  different chips**, confirmed against three separate VICE source files
  fetched live this session, not assumed from the READ-line fact alone:
  the READ line is CIA1's dedicated **FLAG** pin -- `src/core/ciacore.c`'s
  `ciacore_set_flag` (always negative-edge-triggered only, unlike VIA's
  configurable CA1/CB1 polarity) and `src/cia.h`'s `CIA_IM_FLG = 0x10`
  bit constant. MOTOR and SENSE are **not** on either CIA at all --
  confirmed by first checking `src/c64/c64cia1.c`'s own `store_ciapa` and
  finding no cassette logic there, then fetching `src/c64/c64mem.c`'s
  `tape_sense`/`tape_write_in`/`tape_motor_in` variables (which feed into
  a function from a different file entirely) and finally
  `src/c64/c64pla.c`'s exact, verbatim `c64pla_config_changed` function
  body, confirming both bits live on the **6510 CPU's own on-chip I/O
  port** ($0000 DDR / $0001 data), a real, easy-to-get-wrong hardware fact
  (a natural first guess, given the FLAG-pin precedent, would be to also
  look for MOTOR/SENSE on a CIA). Real polarity, reproduced exactly per
  that same function body rather than simplified: SENSE reads 0 (active)
  when a button is sensed and the pin is configured as input; MOTOR reads
  ON (`(dir & data) & 0x20 == 0`) in every case except the one combination
  where bit 5 is driven *high* as an output -- meaning MOTOR reads ON both
  when driven low as output AND when merely left configured as input, a
  real edge case this project's own first test attempt got backwards
  before being corrected against this exact cited formula (see
  `crates/machines/c64/src/bus.rs`'s own test module).

`Cia::set_flag` mirrors `via6522::Via::set_ca1`'s edge-detection shape,
fixed to negative-edge-only per the citation above. `C64Bus::new` seeds
`cia1.set_flag(true)` (idle-high) at construction -- the exact same
"host must seed the chip's assumed initial wiring state" lesson
`c1541::bus::C1541Bus::new`'s own `via1.set_ca1(true)` seed already
taught, caught the identical way: a test failure (`landed: None`, IRQ
never fired) despite live instrumentation proving the tape's own
playback position really had advanced, traced to `Cia`'s derived
`Default` leaving `flag_level` at `false` while the tape's real idle
level is `true`, so the tape's very first real edge was invisible to
edge-detection. The doc comment anticipating this exact risk was written
*before* the seed call itself was, and initially without it -- the
mitigation was only actually added after the test caught its absence,
consistent with this project's own stated "found and fixed by a failing
test, not guessed correctly the first time" verification ethos.

`.t64` dispatch lives entirely in `c64-machine-plugin`'s `AttachMedia`
handler (mirrors `zx-spectrum-machine-plugin`'s own `media_extension`/
extension-sniff pattern) rather than in `c64::C64`/`C64Bus` at all -- a
`.t64` load is exactly a `.prg`-shaped memory poke (`T64::
first_program() -> Option<(u16, &[u8])>`, the same `(address, data)`
shape `retroclawd`'s own `resolve_program_load` already uses for `.prg`),
with no `TapePlayer`/CIA/6510-port involvement whatsoever -- deliberately
not routed through any tape-hardware code path, since none of it applies
to a format that was never a real signal.

Verified live through the real `retroclawd` binary, not just unit tests:
a synthetic `.tap` file, attached via `--tape` alongside a real,
committed KERNAL/BASIC/character ROM set and run for 50000 real CPU
cycles, shows `--dump-memory 0xDC0D:1` (CIA1's ICR) reading `0x91` (bit 7
IRQ + bit 4 FLAG + bit 0 Timer A) -- a same-cycle-count control run with
no tape attached at all reads `0x81` (bit 7 + bit 0 only, no bit 4) over
the identical KERNAL boot sequence, proving the FLAG bit is a real,
tape-presence-dependent effect reaching all the way through the plugin
boundary into the actual CIA1 register, not merely a unit-tested
in-process fact. A synthetic `.t64` file's stored program bytes are
confirmed landing exactly at its own stored load address via
`--dump-memory` the same way `.prg` loading was verified in the previous
pass.

## `.d64` disk attach, IEC-level KERNAL LOAD fast loader, and auto-run scaffolding (`crates/machines/c64/src/bus.rs`, `crates/formats/d64/src/lib.rs`, `crates/plugins/c64-machine-plugin/src/lib.rs`, `apps/retroclawd/src/gui/input.rs`, 2026-09-28)

Fifth item of the "drives and baseline media" parity-extension track
(see `TODO.md`) -- the KERNAL-LOAD-routine trap the previous session's
own `.tap`/`.t64` entry explicitly deferred as "a separate, much larger
feature." This session's own real ROM disassembly work (below) had no
working web source to lean on for the exact routine addresses/zero-page
layout the way earlier sessions found for VICE/MAME source -- every
address here was derived directly from this project's own committed
`roms/c64/kernal-*.bin` files via a small, throwaway Python 6502
disassembler (opcode table hand-written this session, not vendored),
cross-checked against a handful of documentation fetches (below) only
for the parts a raw disassembly can't reveal on its own (the calling
convention's carry/A/X/Y meaning, which is a documented software
contract, not something encoded in the opcode bytes themselves).

- **`$FFD5` = `JMP $F49E` (real KERNAL LOAD entry), byte-identical
  across every committed KERNAL revision** -- confirmed by reading the
  raw bytes at file offset `$1FD5` (`$FFD5 - $E000`) directly from all 5
  committed `roms/c64/kernal-*.bin` files this session: every one reads
  `4C 9E F4`. Disassembling from `$F49E` onward (again, this project's
  own real ROM bytes, not a reference disassembly) shows `STX $C3`/
  `STY $C4` (saving the caller's given load address) immediately followed
  by `JMP ($0330)` -- the real KERNAL LOAD vector -- confirming `$F49E`
  itself, not some vector-indirect target, is the one stable trap point
  reachable regardless of what a caller does.
- **Zero-page LOAD state** -- `$F4A5` onward (the real default `$0330`
  vector target) was disassembled far enough to directly observe: `STA
  $93` (VERCK, the LOAD/VERIFY flag stored from the entry `A` register),
  `STA $90` (STATUS, cleared), `LDA $BA` (FA, device number, then
  compared against `3` to route tape vs. serial-bus devices), `LDY $B7`
  (FNLEN), `LDX $B9` (SA). Separately, disassembling `$F3D5` (the shared
  "open a file on the serial bus" subroutine `$F49E`'s own default vector
  target calls) shows `LDA ($BB),Y` -- confirming FNADR is zero-page-
  indirect through `$BB` (low)/`$BC` (high), and `CPY $B7`/`BNE` bounds
  that same read loop by FNLEN, cross-confirming both fields from a
  second, independent vantage point in the real ROM. These same six
  addresses (`$90`/`$93`/`$B7`/`$B9`/`$BA`/`$BB:$BC`) also appear in the
  zero-page equates section of a mirrored, commented C64 KERNAL/BASIC
  disassembly gist (`gist.github.com/cbmeeks/...`, fetched via
  `WebFetch` this session) with the same names (`FNLEN`/`SA`/`FA`/
  `FNADR`/`STATUS`/`VERCK`) -- a second, independent source agreeing with
  the direct disassembly, not the sole basis for it.
- **LOAD's real calling convention** (carry/`A`/`X`/`Y` on entry and
  exit) -- not derivable from raw opcode bytes alone without tracing the
  entire routine to its every possible exit, so confirmed instead via
  `WebSearch` against the real Commodore 64 Programmer's Reference Guide
  text (mirrored in `mist64/c64ref`'s own `kernal_prg.txt`, and
  independently via a `lemon64.com`/general web summary for the specific
  carry/error-code convention): `A` on entry selects LOAD (`0`) vs VERIFY
  (`1`); `X`/`Y` on entry give the load address only when `SA == 0`
  (ignored, using the file's own embedded header instead, when `SA !=
  0`); on exit, carry clear + `X`/`Y` = the address one past the last
  byte loaded is success, carry set + `A = 4` is FILE NOT FOUND (`A = 5`
  is DEVICE NOT PRESENT, not reachable through this trap since it only
  ever engages once a disk is already attached).
- **The real KERNAL "wait for a keypress" idle loop** (`$E5CD: LDA $C6 /
  STA $CC / STA $0292 / BEQ $E5CD`) -- found by disassembling forward
  from a real, live-observed idle `PC` (`$E5D1`, captured via the
  debugger plugin's own `/api/state` endpoint against a genuinely-booted
  real GUI session, not guessed) back to its own loop entry, confirming
  `$C6` is the real keyboard-buffer count (NDX) this same session already
  knew from BASIC's own well-known `POKE 198,...`/`PEEK(198)` convention.
  Used as `gui/input.rs`'s own C64 auto-run readiness gate -- see that
  file's own `C64_READY_PC_RANGE` doc comment for the real live bug this
  fixed (early autotype keystrokes silently dropped under this sandbox's
  own software-rendering startup cost).
- **`d64::D64::read_file`'s last-sector-length convention** -- confirmed
  via `WebSearch` (a `c64-wiki.com`/general summary citing the format's
  own worked example: a last-sector link of `$00`/`$34` means real data
  occupies bytes `$02`-`$34` *inclusive*, i.e. `link_byte - 1` bytes, not
  `link_byte` or `link_byte - 2` -- an easy off-by-one this project's own
  test (`read_file_returns_exactly_the_valid_bytes_of_a_single_sector_
  file`) locks in against regressing.

Two real, live bugs were found and fixed getting this session's own live
verification bar met, neither guessed:

1. **The GUI auto-run readiness gap** described above (`C64_READY_PC_
   RANGE`'s own doc comment in `gui/input.rs` has the full account) --
   found by actually running the GUI under this sandbox's own Xvfb +
   software (llvmpipe) Vulkan rendering (`mesa-vulkan-drivers` and the
   `libxkbcommon-x11-0` runtime library, plus a manual `libxkbcommon-x11.so`
   symlink to the versioned `.so.0` file the `xkbcommon-dl` crate's own
   `dlopen` call expects but only a `-dev`-style package normally
   provides, were all installed/created this session specifically to make
   a live GUI run possible at all in this environment) with `--run` and
   the debugger plugin's HTTP API (`--plugin debug-ui:port=N`), then
   reading live screen RAM via `/api/memory-bytes` -- the primary
   `LOAD"*",8,1` sequence produced no visible screen text at all, while
   the later, multi-second-delayed `RUN` follow-up worked, pointing
   straight at a timing race rather than a logic bug in the key-sending
   mechanism itself (already proven correct by the follow-up succeeding).
2. Not a new bug, but a live, end-to-end confirmation that no *new* one
   was introduced integrating the fast loader with real BASIC command
   parsing: a synthetic `.d64` (one tokenized `10 POKE 49152,66` program
   named `HELLO`) run through the real, fixed GUI auto-run path shows
   `LOAD"*",8,1`/`READY.`/`RUN`/`READY.` on real screen RAM, the exact
   tokenized program bytes at `$0801`, and `$C000` reading back `$42` --
   the loaded program's own `POKE` genuinely executed after a real,
   autotyped `RUN`, confirmed via the debugger plugin's own HTTP API
   against the actual running instance, not inferred from unit tests
   alone.

## `.d71`/`.d81` disk image formats (`crates/formats/d71`, `crates/formats/d81`, `crates/machines/c64/src/bus.rs`, 2026-09-28)

First item of the "broader C64 media/workflow parity" track (see
`TODO.md`). Every constant below was confirmed via `WebFetch` against
VICE's real source this session (`VICE-Team/svn-mirror` on GitHub,
`vice/src/diskconstants.h` and `vice/src/vdrive/vdrive-bam.c`) --
`WebFetch` processes fetched pages through a summarizing model rather
than returning raw bytes, so every numeric constant below was either (a)
returned as a direct, isolated quote in response to a narrowly-scoped
fetch, or (b) cross-checked against this project's own general knowledge
of the well-documented real CBM DOS format (e.g. the real, extremely
commonly-cited `.d81` "800K"/819200-byte size) before being trusted, and
scope was deliberately narrowed (see the "deliberately deferred" item
below) wherever a fetch's numeric detail couldn't be cross-checked this
way -- a genuinely lower epistemic tier than a session with direct git/
Bash access to shallow-clone and grep the source file directly (blocked
this session by a transient environment-classifier outage, not the usual
network-egress policy other entries in this file cite).

- **D71 (1571) constants** -- `NUM_TRACKS_1571 = 70`, `NUM_BLOCKS_1571 =
  1366` (exactly double `d64`'s own confirmed 683 -- internally
  consistent with two identical 4-zone sides, not just asserted),
  `DIR_TRACK_1571 = 18`/`DIR_SECTOR_1571 = 1`, `BAM_TRACK_1571 = 18`/
  `BAM_SECTOR_1571 = 0`, `BAM_NAME_1571 = 144`/`BAM_ID_1571 = 162` (both
  identical to `d64`'s own already-confirmed offsets, consistent with
  side 1 being the same DOS-family header sector, not a 1571-specific
  reinvention). The second side's own BAM sector location was confirmed
  via a second, more targeted fetch of `vdrive-bam.c` quoting the real
  construction `vdrive->bam_tracks[1] = BAM_TRACK_1571 + vdrive->
  num_tracks / 2` / `vdrive->bam_sectors[1] = BAM_SECTOR_1571` -- i.e.
  track 53 (`18 + 35`), sector 0. `BAM_EXT_BIT_MAP_1571 = 221` was also
  directly confirmed (a free-count-only array back in the *first* BAM
  sector for tracks 36-70) but its consuming logic (the actual 3-byte
  usage-bitmap layout in the *second* BAM sector) was **not** implemented
  this pass -- a follow-up fetch's paraphrased byte-offset arithmetic for
  that specific table couldn't be trusted at the precision this
  codebase's own byte-level accuracy bar requires, so `d71` ships without
  a `track_bam_group`-equivalent rather than guessing one, see `d71`'s
  own module doc comment for the full reasoning.
- **D81 (1581) constants** -- `NUM_TRACKS_1581 = 80`, `NUM_SECTORS_1581 =
  40` (a single scalar constant, not a per-zone table the way `d64`'s own
  `sector_map_d64` is -- itself real confirmation that the 1581 uses a
  uniform sector count, unlike the 1541/1571's constant-linear-bit-
  density zoning), `DIR_TRACK_1581 = 40`/`DIR_SECTOR_1581 = 3`,
  `BAM_TRACK_1581 = 40`/`BAM_SECTOR_1581 = 0`, `BAM_NAME_1581 = 4`/
  `BAM_ID_1581 = 22`. A second, more targeted fetch of `vdrive-bam.c`'s
  own real disk-formatting code confirmed the DOS-type bytes literally:
  `vdrive->bam[BAM_VERSION_1581] = 51; vdrive->bam[BAM_VERSION_1581 + 1]
  = 68` -- decimal `51`/`68` are ASCII `'3'`/`'D'`, the well-known real
  1581 DOS type -- and confirmed the two separate BAM sectors (track 40,
  sectors 1 and 2, each covering 40 tracks' worth of real 6-byte-per-
  track groups: 1 free-count byte + 5 bitmap bytes, since 40 sectors
  need exactly 5 bytes to cover as a bitmap). `d81`'s own
  `HEADER_OFFSET_DOS_TYPE = 25` was *derived* (not directly fetched) the
  same padding-consistency reasoning `d64`'s own `BAM_OFFSET_DOS_TYPE`
  doc comment already uses (`BAM_ID_1581 = 22`, `+2` id length, `+1`
  padding `= 25`), then cross-checked against the confirmed literal `51,
  68` bytes above -- a real, if partial, independent confirmation of the
  derived offset. Same "deliberately deferred" scope cut as `d71`: the
  exact starting byte offset of each BAM sector's own per-track group
  table (as opposed to its size and which sector holds it, both
  confirmed) isn't decoded.
- **Both formats' directory-entry/file-chain format** -- not
  independently re-confirmed this session; carried over unchanged from
  `d64`'s own already-confirmed 32-byte slot layout and last-sector-
  length convention, on the reasoning (itself a real, well-documented
  CBM DOS fact, not this project's own assumption) that the directory/
  file chain format is a DOS-level convention shared across the whole
  1541/1571/1581 family, not something that varies by physical disk
  geometry -- the *only* things confirmed to actually differ per format
  above are BAM/header location, size, and the sector-count/zone table.

No live GUI/binary verification was performed for this item (unlike the
previous `.d64` fast-loader entry) -- this session's own attempts to run
`cargo build`/`cargo test`/`git commit` were repeatedly blocked by a
transient "server-side auto mode classifier gave no verdict" error on
every `Bash` call for an extended stretch mid-session; verification
resumed (and `cargo test --workspace`/`cargo clippy --workspace
--all-targets -- -D warnings` both ran clean, including two new
`c64::bus` fast-loader-trap tests that plant their test file's own data
sector on a track past `.d64`'s own 35-track range specifically to prove
`AttachedDisk`'s abstraction reaches the wider real geometry, not just
that `.d71`/`.d81` parsing alone works standalone) once that outage
cleared, but no live GUI run (the standard this project holds `--run`-
class features to elsewhere in this file) was repeated for this item.

## C64 control-port joysticks (`crates/machines/c64/src/bus.rs`) and file dialogs (`apps/retroclawd/src/dialogs.rs`)

- **C64 joysticks** (2026-10-10): port 2 on CIA1 Port A (`$DC00`), port 1 on CIA1 Port B (`$DC01`, shared with the
  keyboard rows), bits 0-4 = up/down/left/right/fire, a pressed switch pulls its line low. This is the
  widely-documented standard wiring, written from memory of that common description; it was **not** cross-checked
  against VICE's source or a hardware reference in this session (the sandbox's network egress limits applied), so
  it sits at the same epistemic tier as the other "commonly documented" C64 entries above. The unit test only proves
  the code does what this comment says. Reading `$DC00` now returns the pin levels (`effective_port_a`, undriven bits
  float high) instead of the raw latch -- the real behaviour, and needed for a joystick to read correctly.
- **File dialogs** use the [`rfd`](https://github.com/PolyMeilex/rfd) crate (MIT), 0.17; its Linux XDG-portal
  backend has no message boxes (it falls back to `zenity`), which is why the unsaved-disk question is drawn in-app.

## C64 cartridges: `.crt` container and mapper behaviour (`crates/formats/c64-cartridge`, `crates/machines/c64/src/cartridge.rs`, 2026-10-11)

References read for behaviour (none copied): VICE's `src/c64/c64meminit.c` (the PLA configuration
index and the ROML/ROMH/IO tables -- bit 4 GAME active, bit 3 EXROM active), `src/c64/cart/`
`simonsbasic.c`, `ocean.c`, `funplay.c`, `supergames.c`, `epyxfastload.c`, `gs.c` (C64 Game
System / System 3), `dinamic.c`, `zaxxon.c`, `magicdesk.c` and `easyflash.c`, plus the `.crt`
header/CHIP layout from the VICE manual, Peter Schepers' `CRT.TXT` and `crt.c` (header EXROM/GAME
bytes are line levels, 0 = active). Fun Play's register decode and "off" value are marked as
guesswork in VICE's own source; Ocean's 256K/512K ROMH behaviour follows VICE (the bank register
selects one 8K bank shown in both windows; only 512K uses the 8K configuration). The Epyx
Fastload 512-cycle figure is VICE's `EPYX_ROM_CYCLES`. EasyFlash's jumper-off decode table is
VICE's; flash programming is not emulated.
Hardware type ids 1, 2, 3, 6, 9, 11-14, 16, 20-31 (Action Replay, KCS, Final Cartridge III, Expert,
Atomic Power, Westermann, Rex, Magic Formel, WarpSpeed, ...) are rejected with an error rather
than approximated.

## C64 REU (`crates/machines/c64/src/reu.rs`, 2026-10-11)

Behaviour read (not copied) from VICE's `src/c64/cart/reu.c`: register map `$DF00-$DF0A`, status bit
clearing on read, command bits (execute, autoload, FF00-disable, transfer type), the 19-bit address
counter, length `0` meaning 64K, fixed-address control bits, autoload shadow registers and the
end-of-block/verify interrupt mask rules. Transfer cycle counts (1 per byte, 2 for swap) are this
project's simplification of the real DMA, which interleaves with VIC bad lines.

## C64 disk SAVE (`crates/formats/d64`, `crates/machines/c64/src/bus.rs`, 2026-10-11)

The SAVE entry `$F5DD` and its calling convention (A = pointer to the start address in zero page, X/Y =
end address) come from disassembling the committed KERNAL images; the 1541 file-allocation habits
(interleave 10 for files, 3 for directory sectors, searching outward from track 18) are the commonly
documented ones, not checked against a drive ROM. No code was copied from any emulator.

## C64 standard cassette format (`crates/formats/c64-tape/src/rom_format.rs`, 2026-10-12)

Pulse classes, byte/bit coding, parity, countdown bytes, checksum, block repetition, leader lengths and the
192-byte header layout were measured on a recording of the committed KERNAL's own SAVE (the test
`tests/tape_roundtrip.rs` re-derives them), and the `.tap` header is 20 bytes per the VICE `tap_create`
(24-byte initial file = 20 header + 4 data bytes). No code copied.

## C64 paddles and 1351 mouse (`crates/machines/c64/src/controllers.rs`, 2026-10-12)

Behaviour read (not copied) from VICE: `c64cia1.c` (`set_joyport_pot_mask((b >> 6) & 3)`), `joyport/joyport.c`
(mask 1 = first port, 2 = second, 3 = parallel combination), `joyport/mouse_1351.c` (POT = `(count & 0x7F) +
0x40`, buttons on Fire and Up, active low) and `joyport/mouse_paddle.c` (motion / 2, clamp, invert; buttons
on Left/Right). The parallel-pot curve is this project's own approximation.

## C64 light pen and SID model (2026-10-12)

The 6581's 220 Hz cutoff floor is reSID's measured `FCmin` (its `filter.h`). The light pen latch layout
(`$D013` = X/2, `$D014` = raster line, `$D019` bit 3, one latch per frame) is the standard VIC-II behaviour
(Christian Bauer's article); the pen sits on control port 1's fire pin as VICE's CIA1 light-pen check does.

## C64 freezer cartridges, EasyFlash flash, GeoRAM (`crates/machines/c64/src/{cartridge,flash,georam}.rs`, 2026-10-12)

Register layouts and mode tables were read (not copied) from VICE's `src/c64/cart/actionreplay.c`, `kcs.c`,
`final3.c`, `retroreplay.c`, `expert.c`, `easyflash.c`, `georam.c`, `c64cartmem.c` (the `CMODE_*` mode
encoding and which write paths reach cartridge RAM) and `src/core/flash040core.c` (the AM29F040B command
state machine, its timeouts and status bits). The Ultimax VIC view (ROMH shown at `$3000-$3FFF`) follows the
`ultimax_romh_phi1_*` hooks in the same file. Deliberate differences: no bus-conflict emulation for Action
Replay's broken mode `$22`, no corruption of the register by an IO1 read, and an NMI that is a single edge.

## C64 SwiftLink/Turbo232, RR-Net and printer (`crates/chips/{acia-6551,cs8900a}`, `crates/machines/c64/src/{rrnet,printer}.rs`, 2026-10-12)

Register layouts, status/command/control bit meanings, baud tables (SwiftLink and Turbo232 double the 6551's),
the Turbo232 extended register and the transmit/receive interrupt behaviour were read (not copied) from VICE's
`src/aciacore.c`, `acia.h` and `src/c64/cart/c64acia1.c`. The CS8900A I/O ports, PacketPage map, transmit
handshake, receive status construction and filter rules were read from VICE's `src/core/cs8900.c`; the RR-Net
port mapping (offsets 0-1 unused, offset XOR 8) from `src/c64/cart/ethernetcart.c`. No network backend is
implemented (loopback only, by decision). The printer routes bytes at the KERNAL serial entry points instead of
emulating the IEC wire; its PETSCII conversion is this project's own.

## C64 cycle-accurate VIC-II and cycle-complete 6502 (`crates/chips/vic-ii/src/cycle.rs`, `crates/core/6502`, 2026-10-12)

The per-cycle VIC-II schedule is Christian Bauer's "The MOS 6567/6569 video controller (VIC-II) and its
application in the Commodore 64", reproduced from memory of the article, not fetched this session (no
network copy was read): the PAL cycle table (sprite p-/s-accesses in cycles 58-63 and 1-10, refresh in
11-15, c-accesses in 15-54, g-accesses in 16-55), the display/idle state rules and VC/VCBASE/RC/VMLI
updates (cycles 14 and 58), the bad-line condition and its DEN latch in line $30, sprite DMA start in
cycles 55/56, MC/MCBASE handling in 15/16/58 and the Y-expansion flip flop, and BA falling three cycles
before the first access. The BA/RDY behaviour (the CPU halts on the first read while BA is low, never on
a write, so at most three writes can overlap the warning window) is the same article's section on bus
arbitration. Treat the figures as the commonly documented tier; no VICE/Lorenz test program was run
against them. The 6502 per-cycle access patterns (dummy reads/writes) follow the well-known
cycle-by-cycle instruction tables (e.g. the "64doc"/MOS 6502 programming references) and are checked here
only for count and order by `crates/core/6502/tests/cycle_hook.rs`.

The Family now pins shared revision `ae53253565d8bd34ebba7bdde8acebe55b9c028c`
with the SHY/SHX/SHA/TAS high-byte store correction exposed by Atari Acid800.
The mask uses the unindexed base high byte plus one; a page carry replaces the
write-address high byte with the masked data. See the shared `CREDITS.md` for
the hardware/reference sources and instability limits. CPU State encoding and
cycle counts are unchanged. Commodore workspace tests and all 62 existing
VICE-backed VIC-20 scenarios pass on this pin; IRQ polling is not fixed here.

The pixel side of the same engine follows Bauer's sections 3.7.3 (the graphics sequencer's per-mode colour
rules), 3.8 (sprite shifters, priority and expansion) and 3.9 (the border unit's six flip-flop rules and the
24/31 and 335/344 comparison columns, 51/55 and 247/251 rows), again from memory of the article. The
raster-X to cycle mapping (cycle 14 starts at x = 0, cycle 17 at x = 24, 8 pixels per cycle, 504 pixels per
line) is derived from the display starting at x = 24 one cycle after the first g-access. The rule that the
vertical border flip flop is also set when the frame wraps is this project's own choice (demos that open only
the bottom border still show a normal top border), not from the article. The static renderer already in this
crate was used as the independent oracle for the graphics and sprite output (`cycle::tests`).

## 1541 DOS ROMs and drive behaviour found by running them (`roms/1541/`, `crates/machines/1541`, 2026-10-12)

The 1541 ROM images (`dos1541-325302-01+901229-05.bin`, `dos1541ii-251968-03.bin`; the 1541-C
image, 1570/1571/1581 and SCPU64 ROMs sit in `roms/c64/`) were copied by the project owner from
the VICE 3.10 `DRIVES` folder and from the Denise emulator's ROM set; they are Commodore
copyrighted firmware, not distributed with this repository (see `docs/firmware.md`).
Drive behaviour that the earlier VICE/MAME-sourced model had wrong was corrected from the ROM's
own code (disassembled here): the inverted IEC inputs (ATN service at `$E85B` falls through only
on PB7 = 1; PCR = $01 for CA1), the SYNC/write-protect inputs on VIA2 Port B (`$F556`, `$F57A`),
and the binary stepper phase (`$FA6E`). The CIA2 NMI edge behaviour follows the 6510 datasheet's
edge-triggered NMI input.

## 1570/1571 drive (`crates/machines/1541`, `crates/chips/cia-6526`, 2026-10-12)

Behaviour read from VICE's drive source (GPL, reference only, fetched from the VICE-Team svn mirror):
`drive/iec/memiec.c` (1570/1571 memory map), `drive/iec/via1d1541.c` (1571 Port A: byte-ready, track-0
sensor, side, 2 MHz, fast direction), `drive/iec/cia1571d.c`, `drive/iec/glue1571.c`, `drive.c`/
`drivecpu.c` (only the FD/CMD drives use a 65C02). The ROMs in `roms/c64/VC1570`/`VC1571` are from the
user's Denise/VICE downloads; the `U0>M1` mode switch and the `$2750` 1571-mode test were found by
disassembling that ROM. `cia6526` is the former `c64::cia`, moved unchanged.

## 1581 drive and WD1770 (`crates/machines/1541/src/{drive1581,floppy81}.rs`, 2026-10-12; the `wd1770` crate moved to retroclawd-crates 2026-10-21)

Behaviour read from VICE (GPL, reference only, VICE-Team svn mirror): `drive/iec/wd1770.c` (command
state machine, status bits, timing constants), `drive/iec/fdd.c` (raw track synthesis for a `.d81`, the
head/half-image mapping, flush/decode of written tracks, disk-change line), `drive/iec/cia1581d.c` (CIA pin
wiring, ATN-acknowledge gate), `memiec.c` (1581 memory map). The WD1770 datasheet gives the command set;
`roms/c64/VC1581` (user-supplied, from Denise/VICE) is the DOS that proves it. The FLAG-pin wiring of ATN
was found by running that ROM, not read from VICE.

## Commodore 128 (`crates/machines/c128`, `crates/chips/vdc-8563`, 2026-10-12)

Behaviour read from VICE (GPL, reference only, fetched from the VICE-Team svn mirror): `c128/c128mmu.c`
(MMU registers, pre-configuration, page 0/1 relocation with latched high bytes, common RAM, the MCR bits),
the C128 memory-map sources under `c128/` (the 8502 and Z80 maps, VIC bank = RCR bit 6 plus CIA2, colour RAM
banks on the 8502 port) and `vdc/vdc.c`/`vdc-mem.c`/`vdc-draw.c` (register masks, R30 fill/copy, the
update-address window, attribute and cursor rendering). The ROM layout (kernal image = editor + Z80 BIOS +
kernal, character ROM halves) was confirmed by running the ROMs. The C128 Programmer's
Reference Guide gives the MMU register table. `roms/c128/` is user-supplied (from the VICE data set). The
C64-mode mapping reuses the C64 PLA behaviour documented above. Everything was then checked by running the
real ROMs (`crates/machines/c128/tests/{boot,drive}.rs`), not by comparing with VICE output.
