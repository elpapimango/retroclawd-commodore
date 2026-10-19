# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repo.

## What this repository is

The Commodore (C64, C128, drives) machine family of retroclawd, split out of the main repository (`elpapimango/retroclawd`, the application, referred to as "the host"). Everything here builds into `*_machine_plugin` shared libraries that the host application loads at run time. Shared crates (`machine`, `plugin-api`, `z80`, `mos6502`, `ay-3-8910`, `nec765`, `dsk`, `zx-tape`) come from `elpapimango/retroclawd-crates` as a git dependency pinned to a commit (`rev`); changing that rev means checking `docs/plugin-abi.md` in that repository. Directory layout is the same as in the host (tests find ROMs/software at `CARGO_MANIFEST_DIR/../../../roms|software`).

## Commands

- `cargo build --release --workspace` — builds the plugin libraries into `target/release/`
- `cargo test --workspace` — all tests (ROM-gated tests skip when a ROM is absent; `-- --ignored` runs the slow real-firmware ones, use `--release`)
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`
- Private host: `CARGO_NET_GIT_FETCH_WITH_CLI=true` and git credentials for `github.com/elpapimango/retroclawd`

## Scenario manifests (2026-10-18)

`c64 and c128` have `<id>.manifest.toml` files here: the playbook's standard scenarios (S1 boot, S2 typed input, S3 media, S4 audio, S5 state, S6
timing, S7 hardware test) against VICE 3.10, run by `rc-scenarios` of `retroclawd-tools` (see the host's `CLAUDE.md` for the command and
environment). The test media come from `scenarios/c64/hello.asm`, the combined C128 BASIC image and the unified diagnostic `.crt` (`scenarios/make_unified_crt.py`); `scenarios/build.sh` builds them into the git-ignored `software/scenarios/`. Goldens are
not committed. Known gaps are `status = "known-gap"` entries with a note under `docs/`: S6 (`docs/c64-timing-gap.md`), the fast loader's missing SEARCHING/LOADING lines (`docs/c64-load-messages-gap.md`) and the dead test's clock (`docs/c64-deadtest-clock-gap.md`).

## Crate notes

The notes below were written in the host repository before the split and refer to it in places (`retroclawd`'s GUI, `HostedMachine`, `emu_thread`, the CLI flags): those parts are host code and are documented in the host's CLAUDE.md.

- **`vic-ii`** (`crates/chips/vic-ii`) — MOS 6567/6569 VIC-II video chip.
  Raster IRQ, bad-line CPU cycle stealing, all 5 real display modes
  (text, multicolor text, hi-res bitmap, multicolor bitmap, extended
  color mode text), and all 8 hardware sprites (position/priority/color/
  expansion -- see this bullet's own later paragraphs) are implemented;
  only sprite-sprite/sprite-background collision detection remains
  done (see the end of this bullet). Base text mode (40x25 char/color RAM read, char-ROM bitmap
  lookup -- same
  architectural move as `zx81::render_frame`'s char-code-to-ROM-bitmap
  decode) is `render_text`, whose glyph-bit-to-pixel expansion
  uses the `wide` crate's portable SIMD (`u8x32`, SSE/AVX on x86_64/NEON
  on ARM64/scalar elsewhere) via `MASK24_TABLE`/`PALETTE24_TABLE` --
  precomputed once (only 256 and 16 distinct patterns exist), then a
  single blend + slice copy per scanline; measured 1.40x on this
  project's own x86_64 dev machine (`cargo test --release -p vic-ii --
  --ignored --nocapture bench_render_text_simd_vs_naive_scalar` --
  a first version that rebuilt those patterns per-pixel instead of once
  measured *slower*, 0.73x, see that test's own doc comment and
  `TODO.md`'s Performance section). `Vic::tick(cycles)` advances a live
  raster-position counter (`63` cycles/line * `312` PAL lines, wrapping)
  that `$D012`/`$D011` bit 7 compute from live rather than returning
  stored bytes -- a real bug fix, not a day-one design: real KERNAL boot
  code busy-waits on the raster register during its own init sequence,
  and without this the wait never ends (see `TODO.md`'s "Commodore 64
  support" section for the full diagnosis). No dependency on `machine`
  or a CPU crate, same host-agnostic-chip convention as `ay-3-8910`;
  `render_text` returns a plain RGB tuple in `Machine::render_frame`'s
  own shape, the host machine (`c64`) is what actually implements that
  trait method and pads the result with `machine::with_border`. 16-color
  `PALETTE` is the widely-cited "Pepto" approximation, not a
  hardware-measured constant -- same epistemic tier as
  `zx_spectrum::SpeakerType`'s filter cutoffs.
  **Raster IRQ** (first item in `TODO.md`'s VIC-II coverage sequence,
  ahead of bad lines/bitmap modes/sprites): `$D019`'s low nibble is a
  real latched-interrupt-status field (`Vic::irq_status`), not plain
  register storage -- set by `tick` when the live raster position
  transitions onto the compare line built from `$D012`'s and `$D011`
  bit 7's own already-stored raw bytes (`raster_compare_line`, no
  separate cached field needed), cleared by a CPU write-1-to-clear to
  the matching bit (`write`'s own special case). `$D019` bit 7 (the
  overall "/IRQ asserted" flag) is never stored -- computed live from
  `irq_status & $D01A`, same "derive, don't cache" approach the
  raster-position read already used. `tick` returns `true` only on the
  exact tick a *new* match happens *and* `$D01A` currently enables the
  raster source -- an edge-triggered one-shot signal, deliberately the
  same shape `c64::cia::Cia::tick`'s own return value already
  established for Timer A underflow (see that method's own doc comment
  for the accepted "not a level held across every later call" caveat
  this mirrors), not a new IRQ-delivery concept. `c64::C64::step` turns
  a `true` return into a real `mos6502::Cpu::request_irq()`, same
  call-site shape CIA1 already uses -- both can fire the same step with
  no conflict, since `request_irq` just sets one pending flag. Bit
  layout and both write-1-to-clear and raster-compare-construction
  semantics were confirmed directly against VICE's own real source
  (`vicii-irq.c`/`vicii-mem.c`, fetched live this session), not carried
  over from the "commonly documented, not cross-checked" tier the rest
  of this crate's C64 entries in `CREDITS.md` still use -- see
  `CREDITS.md`'s own entry for the exact functions read. Sprite
  collision bits 1-2 are live (end of this bullet); the light pen bit (3)
  stays inert, no light pen exists.
  **Bad-line cycle stealing** (second item in `TODO.md`'s VIC-II
  coverage sequence): `tick` now returns a small `TickResult` struct
  (`raster_irq: bool` + `stolen_cycles: u32`) instead of a bare `bool` --
  `stolen_cycles` is a flat `40` (the real, widely-documented c-access
  count) whenever the tick just transitioned onto a real bad line (bad
  lines allowed this frame, raster line's low 3 bits match `$D011`
  YSCROLL, line inside the real `$30`-`$f7` DMA window,
  `is_bad_line`/`FIRST_DMA_LINE`/`LAST_DMA_LINE`), `0` otherwise.
  `allow_bad_lines` (whether bad lines can occur *this frame* at all) is
  real hardware's own DEN latch -- (re-)evaluated only at the exact tick
  the raster reaches `FIRST_DMA_LINE`, from whatever `$D011` bit 4 (DEN)
  reads as at that instant, not stored anywhere else or reset
  separately at frame start (real hardware just keeps reassigning it
  every time the raster passes `$30`, once per frame, which is
  equivalent). This single-sample check is a real, coarser tier than
  hardware's genuine per-cycle DEN latch (the mechanism the well-known
  "FLD"/border-removal demo trick exploits) -- deliberately matched to
  VICE's own simpler, non-cycle-exact core rather than invented, since
  this project's per-CPU-instruction tick granularity has no sub-line
  resolution to do better with (see `allow_bad_lines`'s own doc
  comment). Critically, `frame_cycle` (the live raster-position counter)
  only ever advances by the real `cycles` a `tick` call is given, never
  by `stolen_cycles` on top -- real hardware's raster generator runs at
  a fixed pace fully independent of CPU bus stalls, so `Vic` itself must
  never see its own reported steal fed back in. `c64::C64::step` is what
  actually applies the steal: it feeds `cpu_cycles + stolen_cycles` (not
  just `cpu_cycles`) into CIA1's and the SID's own `tick` calls (both
  keep running off the real phi2 clock the whole time, regardless of the
  CPU being bus-stalled -- real hardware fact, not this project's
  choice) and into the audio-accumulator/sample-pacing math, and returns
  that same inflated total as its own `Machine::step` cost -- the
  correct fix within this project's existing "the caller's outer
  real-time pacing loop trusts `step`'s own returned cycle count"
  architecture, same shape `zx_spectrum::ZxSpectrum::step`'s fast-tape
  trap already established (substituting an artificial cycle count that
  represents real elapsed time, not literally how many cycles a single
  CPU operation cost). The bad-line condition and the flat 40-cycle
  figure are both confirmed directly against VICE's own real source and
  an independent VIC-II timing reference (see `CREDITS.md`) -- a real
  figure that can genuinely vary by a cycle or two in truly cycle-exact
  hardware depending on which phase of a multi-cycle CPU instruction the
  steal begins on, not reachable without true per-cycle CPU/VIC bus
  arbitration (`mos6502::Cpu`/`c64::C64Bus` have none, unlike `z80`/
  `cpc::CpcBus`'s own `contend_memory`/`advance_tstates` hooks) -- an
  explicitly accepted, documented simplification, not an oversight.
  **Bitmap and multicolor modes** (third item in `TODO.md`'s VIC-II
  coverage sequence): `video_mode` (`VideoMode` enum) selects one of the
  chip's 5 real display modes from `$D011`'s ECM/BMM bits and `$D016`'s
  MCM bit -- `Text` (already existing), `MulticolorText`, `Bitmap`,
  `MulticolorBitmap`, `ExtendedColorText`; the 3 remaining `ECM+BMM`
  combinations are `VideoMode::Invalid`, rendered solid black
  (`render_invalid_mode`) rather than reproducing real hardware's exact
  per-sub-case output, since no real software deliberately relies on
  one. `video_matrix_base`/`character_base`/`bitmap_base` derive real
  addresses from `$D018` (VM13-10 at bits 7-4, 1KB steps; CB13-11 at
  bits 3-1, 2KB steps, text-family modes; CB13 alone at bit 3, one of
  two 8KB-aligned bases, bitmap modes) -- bank-relative offsets (never
  past `$3FFF`), the *host* (`c64::C64Bus::render_frame`) is what picks
  which real 16K bank those offsets land in, via CIA2 Port A (see
  `c64`'s own bullet below, "CIA2 completion" in `CREDITS.md`/`TODO.md`
  -- this crate itself needed no change for that wiring).
  `render(ram, char_rom, color_ram)` is the one dispatcher
  `c64::C64::render_frame` now actually calls: it slices the host's flat
  RAM at those offsets itself (so the host no longer duplicates any
  address arithmetic) and picks the matching per-mode render method.
  `render_bitmap`/`render_extended_color_text` reuse the exact same
  `MASK24_TABLE`/`PALETTE24_TABLE` SIMD blend `render_text` already
  established, since both are still a "per-cell ink/paper source, one
  bit = one pixel" shape underneath (bitmap mode's ink/paper come from
  the screen-RAM byte's own nibbles instead of color RAM; ECM's
  background varies per cell from a 4-entry table instead of being
  chip-wide, and its character code is masked to 6 bits, 64 addressable
  characters, real hardware fact). `render_multicolor_text`/
  `render_multicolor_bitmap` are deliberately plain scalar code instead
  -- the real doubled-pixel-width (2-bit-pair-to-4-colors) shape doesn't
  fit that single-bit table at all, and there's no profiling evidence
  yet that either mode is a hot path worth the same investment (see
  `TODO.md`'s Performance section: no optimization without a measured
  bottleneck). Switching `render_frame` from a hardcoded `$0400` video
  matrix base to this real `$D018`-derived one is confirmed
  behavior-preserving for every existing default case: real KERNAL boot
  code itself programs `$D018` to `0x14` (verified live, not assumed,
  via a throwaway run against this project's own real, committed
  KERNAL/BASIC/character-generator ROMs under `roms/c64/`), which
  computes the exact same `$0400` base the old hardcoded constant
  already assumed. Every bit layout and per-mode pixel/color algorithm
  came from two real sources fetched this session (VICE's own
  `vicii-draw.c`/`vicii-mem.c`, and Christian Bauer's widely-cited VIC-II
  article via a GitHub mirror once its original host was blocked by this
  session's network egress policy) -- see `CREDITS.md`. Character-ROM
  shadow behavior is modeled (2026-10-12): `Vic::chargen` resolves the glyph
  data as the host's char ROM when the generator base is `$1000`/`$1800` in
  VIC banks 0 and 2 (`Vic::set_chargen_shadow`, driven by
  `C64Bus::sync_vic_chargen_shadow` on every CIA2 write and state load),
  otherwise as RAM at the `$D018` base -- so custom charsets and the
  lowercase set render (before this, text modes always read the ROM at
  offset 0, ignoring `$D018`'s character-base bits).
  **Hardware sprites** (fourth item in `TODO.md`'s VIC-II coverage
  sequence): `render_sprites` composites all 8 real sprites onto
  `render`'s own background picture, in real hardware's own fixed
  priority order (sprite 0 highest, sprite 7 lowest -- confirmed
  completely independent of each sprite's own `$D01B` bit, which only
  ever decides visibility against *background* graphics). Position
  comes from `$D000`-`$D00F` plus `$D010`'s 9th-bit X extension,
  corrected by the real, widely-cited `24`/`50` screen offsets (`MxX`=
  `24`/`MxY`=`50` puts a sprite's top-left corner at the active area's
  own top-left corner -- the `24` cross-checked as a literal constant
  in VICE's own real `vicii-sprites.c`, not just web-sourced). Sprite
  data comes from the video matrix's own last 8 bytes (one pointer byte
  per sprite, `* 64` for the real data address, bank-0-relative same as
  every other address this crate computes) via the new
  `sprite_render_info`/`sprite_pixel_color`. Multicolor sprites decode
  2-bit pixel pairs (`00` transparent, `01` `$D025`, `10` the sprite's
  own `$D027`-`$D02E` color, `11` `$D026`); X/Y expansion doubles each
  source pixel/row without changing resolution. `$D01B`
  (behind-background) sprites are hidden only where the new
  `background_pixel_is_foreground` query says so -- a real, easy-to-
  get-wrong hardware quirk confirmed directly against source rather than
  assumed: that decision is made purely by the chip-wide MCM bit (`1`
  bit vs. a `10`/`11` 2-bit pair), *not* by whether the specific cell
  underneath opted into multicolor text's own per-cell rendering flag,
  which only changes that cell's *color*, never its priority.
  `background_pixel_is_foreground` is computed on demand for the
  specific pixels a behind-background sprite actually covers, mirroring
  each display mode's own bit-extraction logic rather than
  precomputing/allocating a whole-canvas mask -- no existing render
  method's signature needed to change to carry this data. Sprites are
  simply clipped at the existing `320x200` canvas edge -- real
  hardware's own border has the *highest* display priority of all
  (overlaying sprites too), so this is a real hardware fact this
  project's existing separate border-compositing architecture (see
  `machine::with_border`) reproduces for free, not an approximation.
  VIC bank selection is now real (see `c64`'s own bullet below, "CIA2
  completion"), not fixed at bank 0 -- this crate's own sprite/mode
  addressing already worked unmodified once the host started slicing the
  right 16K window. (Sprite crunch, X wrap and line-mode sprite DMA stealing:
  see "Sprite gaps closed" at the end of this bullet.) **Sprite-sprite/sprite-background collision detection
  (`$D01E`/`$D01F`, `$D019` bits 1-2, 2026-10-11)**: done as the per-raster-line
  pipeline the earlier note said it needed. `Vic::tick` reports the line it
  just entered (`TickResult::entered_line`); `c64::C64::step` hands that line
  to `Vic::scan_sprite_collisions(line, ram, char_rom, color_ram)` with the
  16K bank the VIC currently sees, before the next instruction, and requests
  the CPU IRQ if it returns `true`. Per line it builds each enabled sprite's
  opaque pixels (same `sprite_pixel_color`/expansion/multicolor rules as
  `render_sprites`), marks sprite-sprite hits wherever two or more share an
  x (also in the border, where the 320x200 window does not reach), and
  sprite-background hits where a sprite pixel sits on a *foreground* pixel
  (`background_pixel_is_foreground`, window only; `MxDP` behind-background
  sprites still collide). `$D01E`/`$D01F` OR new bits in until a CPU read
  clears them (`Vic::read_clearing`, used by `C64Bus::io_read`; `Vic::read`
  and debugger peeks do not clear), CPU writes are ignored, and the `$D019`
  latch (bit 2 sprite-sprite, bit 1 sprite-background) is set only when the
  register goes from zero to non-zero, requesting an IRQ only if enabled in
  `$D01A`. Accuracy tier: one sample at the start of each raster line from
  registers/RAM as they stand then (like the bad-line check), no per-cycle
  sprite DMA (X wrap since added, see below). The `Vic`
  save state gained two fields (bincode is not self-describing, so `.state`
  files saved before this change no longer load -- they were always
  internal-only).
  **Cycle-accurate engine (2026-10-12)**: `Vic::cycle(&VicMemory)` (`crates/chips/
  vic-ii/src/cycle.rs`, enabled by `Vic::enable_cycle_engine`, PAL only) runs one clock
  of the real 63-cycle line: it advances the raster itself (`frame_cycle`, so `$D011`/`$D012` stay
  derived), latches the raster IRQ in cycle 1 (cycle 2 of line 0), keeps the DEN latch (set if DEN is
  seen anywhere in line `$30`), and performs every access on its cycle -- p-/s-accesses for sprites
  (cycles 58-63, 1-10), refresh, c-accesses in 15-54 (started at the bad-line cycle + 3, never before
  15), g-accesses in 16-55 (idle address `$3FFF`, `$39FF` with ECM), with VC/VCBASE/RC/VMLI, the
  display/idle state (RC check and VCBASE <- VC in cycle 58) and the sprite DMA state machine (MC/
  MCBASE, MxYE flip flop, DMA start in 55/56, display start in 58, stop at MCBASE 63). The fetched data is
  kept (`line_matrix`, `line_graphics`, `sprite_bytes`). **Pixel side**: each cycle also emits its
  eight pixels (`run_sequencer`) into a 384x264 frame (`Vic::cycle_frame`, the 320x200 window with a
  32-pixel border, published when the last visible line ends): a graphics sequencer for the five modes
  with XSCROLL (cells latched from the g-access, the previous cell's tail shows through the shift,
  the idle cell -- zero c-data, `$3FFF`/`$39FF` -- fills gaps and opened borders), the eight sprite
  shifters (priority, `$D01B`, multicolor, expansion; rows latched in cycle 1 for sprites 0-2 and as the
  s-accesses finish for 3-7) and the border unit (main and vertical flip flops, CSEL/RSEL comparisons
  at x 24/31, 335/344 and lines 51/55, 247/251, Bauer 3.9 items 1-6, plus the end-of-line vertical
  check and a set at frame wrap) -- so a mid-line write to `$D011`/`$D016`/colours changes the picture
  from the next 8-pixel cell: border opening, FLD, split screens and sprite multiplexing show up.
  Verified against the static renderer pixel for pixel in every display mode and with sprites. The
  sprite collision flags stay the per-line scan. `Vic::ba_low()` is the per-cycle bus-request line. The
  line-accurate `tick` model is unchanged and the default.
  **Sprite gaps closed (2026-10-12)**: (1) **X wrap**: the raster line is 504 pixels (`RASTER_LINE_PIXELS`), so a
  sprite's pixels land at `(MxX + offset) mod 504` (`sprite_canvas_x`) and an `MxX` of 504-511 never starts (the
  chip's X counter never equals it). `render_sprites` therefore shows an X-expanded sprite at `MxX` 503 re-entering
  at the left of the window, and `scan_sprite_collisions` keys its overlap table by raster X (`0..504`, border
  included) so a sprite wrapped to the left collides with one parked there; the cycle engine already wrapped.
  (2) **Sprite crunch**: `cycle.rs` follows Bauer's flip-flop rules, now completely -- MCBASE is a 6-bit counter
  (`& 0x3F`, so a misaligned sprite that skips 63 keeps running instead of stopping at 8-bit wrap) and `Vic::write`
  forces the Y-expansion flip flop *set* when a write clears MxYE (it stays set when the bit is set again), which
  is the real mechanism of the glitch: clearing MxYE before cycle 15 makes MCBASE advance a row it should have
  repeated (`cycle::tests::clearing_mxye_before_the_mcbase_update_crunches_the_sprite`, mutation-checked),
  between cycles 15 and 16 only the "+1" happens and the sprite never reaches 63 on schedule. Crunch only shows in
  cycle mode (line mode draws sprites from the registers). (3) **Line-mode sprite DMA stealing**: the line model
  keeps the same DMA flag / MCBASE / flip flop per sprite (`LineSprites`, stepped once per raster line in
  `Vic::tick` by `step_line_sprites`, every line a long tick crossed) and `TickResult::stolen_cycles` now carries
  2 cycles per sprite that fetches in the line plus 3 BA-warning cycles when the sprite before it in time did not
  (sprites 3-7 fetch at the start of a line, 0-2 at the end, 2 runs into 3 across the boundary) -- measured
  against the cycle engine: a lone sprite 21 x 5, three adjacent 21 x 9, a Y-expanded one 42 x 5, sprites 0 and 2
  two warnings. A bad line's 40 is added on top without merging (on silicon the BA warning inside a bad line is
  free, so a bad line with sprites over-charges by up to 3 cycles per run). The `Vic` save state gained
  `line_sprites` (older `.state` files no longer load). Tests: `vic-ii` lib (collision wrap, MxX 504, wrapped
  render, 7 line-mode cost cases) and `cycle` (crunch, 6-bit MCBASE).
- **C64 CPU port read-back (2026-10-16)**: `C64Bus::port01` masks the result with `0x3F`: the 6510 in the C64 has six port
  pins, bits 6 and 7 do not exist and read 0, so `PEEK(1)` is 55 (`$37`) in the standard configuration (c64-wiki "Memory
  Map"), not 247 as it was while those bits floated high. Found by the `rc-suite` C64 cases (BASIC `PRINT PEEK(1)`);
  unit test `the_cpu_port_reads_back_only_its_six_pins`. `firmware.toml` is checked against each plugin's declared file options by
  `firmware_toml_names_only_file_options_the_plugin_declares` in the `c64` and `c128` plugin crates.
- **`c64`** (`crates/machines/c64`) — thin, same shape as `zx81`/
  `zx-spectrum`: `mos6502::Cpu` + `C64Bus` (private to the crate, same
  "Bus impl stays internal, only the `Machine` impl is exported"
  convention `zx_spectrum::ZxSpectrumBus` already established) + PAL-only
  clock/frame rate (985248Hz/50Hz -- no `Region` enum yet, same "may not
  need the full enum" reasoning flagged as an open question for the CPC
  track). Real PLA/6510-port memory banking lives entirely in
  `C64Bus::read`/`write`'s own address-decode logic (`$A000-BFFF`/
  `$D000-DFFF`/`$E000-FFFF` banked windows, keyed off the 6510's on-chip
  I/O port at `$0000`/`$0001` -- see `CREDITS.md`) -- confirmed a pure
  bus-level concern needing zero `mos6502` core change, the actual
  real-hardware-quirk proof this track set out to find. A ROM window
  being visible for *reads* never stops the real RAM underneath from
  being *written* (true C64 hardware behavior) -- `write` special-cases
  only the `$D000-$DFFF` I/O window (real chip registers, not
  RAM-shadowed) to intercept writes there too; everywhere else always
  writes straight through to `ram`, which is what makes
  `Machine::load`'s "every byte lands" case actually true here, unlike a
  machine with genuine unmapped space. `crate::cia::Cia` (now the full
  MOS 6526 -- Timer A/B, TOD clock with its real read-latch/write-halt
  mechanism and alarm compare, the output-mode serial shift register, and
  a self-clearing force-load strobe; CNT/SP-pin-gated modes stay inert,
  no real cassette/IEC/RS-232 line is wired to either CIA yet -- see
  `CREDITS.md`'s "CIA2 completion" entry) is kept local rather than a
  shared `crates/chips/cia-6526` -- CIA1 and CIA2 are both `Cia`
  instances wired differently by this one host bus, not two different
  crate consumers, so extraction still isn't warranted, same "don't
  extract from one example" principle used elsewhere. `C64::step` ticks
  both every step; CIA1's interrupt drives `mos6502::Cpu::request_irq`
  (enough for the KERNAL's own boot-time jiffy-clock IRQ), while CIA2's
  drives `request_nmi` instead -- a real hardware fact (CIA2's IRQ output
  is wired to the CPU's NMI input, not its IRQ input), not a modeling
  choice; CIA2 was a pure register-storage stub never even ticked before
  this pass. CIA2's Port A also now drives real VIC bank selection (see
  `vic_bank_base` below). `save_state`/`load_state`
  (`crate::save_state`, bincode) derive serde directly onto `mos6502::
  Cpu`/`vic_ii::Vic`/`crate::cia::Cia` -- unlike `zx_spectrum`/`zx81`,
  none of them hold or are generic over external media, so none need the
  small dump/restore mirror-struct shape those two use for `nec765::Fdc`/
  `zx_tape::TapePlayer`. `--rom-kernal`/`--rom-basic`/`--rom-char` (three
  separate flags -- a real C64 has no single ROM the way every Sinclair
  machine here does, so it doesn't fit `retroclawd`'s existing
  single-`--rom` flag; since the option rework the three paths are plain
  `File` options, so `--game-config`'s `[options] rom_kernal/rom_basic/
  rom_char` set them like any other machine option, and a game-config that
  names the machine is prescanned before the clap command is built). Real
  keyboard now wired too: `crate::keyboard::KeyMatrix` (own type, kept
  local rather than extending `sinclair-keyboard` -- CIA1 Port A/B
  general-purpose-port scanning is a genuinely different mechanism from
  the Sinclair machines' `IN`-port address decode) is scanned through
  `C64Bus::keyboard_port_b` (DDR-aware: input-configured Port B bits
  reflect the matrix, output-configured bits reflect whatever was
  written -- same "host wires the chip-specific bits, `Cia` stays
  generic" split as everything else in `crate::cia`); `C64::press_key`/
  `release_key` + `AnyMachine`'s `c64_key` mapping (letters/digits/
  space/enter/shift/cursor/delete/run-stop, plus the direct-glyph-match
  punctuation/function keys -- comma/period/slash/semicolon/minus/
  equals/F1/F3/F5/F7/Control/Home/Alt; `Colon`/`Plus`/`At`/`Pound`/
  `UpArrow` deliberately unmapped, no unambiguous host-key equivalent
  without picking a positional-vs-symbolic keyboard convention first,
  see `TODO.md`) wire it into `retroclawd`. Real hardware quirk reused,
  not reinvented:
  `CursorRight`/`CursorDown` are the *only* cursor matrix positions on
  real hardware (Cursor Left/Up are Shift+CursorRight/Shift+CursorDown),
  so both arrow-key directions map onto the same single position each
  and the existing Sinclair-machine `SHIFT_SOURCE_KEYS` combined-shift
  mechanism handles the rest unmodified. VIC-II sprite collisions are wired in `C64::step`
  (see `vic-ii`'s own bullet above) -- see `TODO.md`'s "Commodore 64 support" section for the full backlog.
  `C64Bus::vic_bank_base` decodes CIA2's effective Port A bits 0-1 (`$DD00`,
  an inverted 2-bit field) into the real VIC-II-visible 16K bank base;
  `render_frame` slices `self.bus.ram` at that base before calling
  `vic_ii::Vic::render` -- needed no `vic_ii` crate change at all, since
  its own internal offsets were already bank-relative and only ever
  looked bank-0-hardcoded because this bus always handed it a slice
  starting at absolute `$0000`.
  Raster IRQ, bad-line cycle stealing, bitmap/multicolor modes, and
  hardware sprites are all wired (see `vic-ii`'s own bullet above for
  what each actually does): `C64::step` turns
  `bus.vic.tick(cpu_cycles)`'s `TickResult::raster_irq` into a real
  `mos6502::Cpu::request_irq()`, same shape CIA1's own Timer A IRQ
  already uses -- both sources can request the same step with no
  conflict. It also adds `TickResult::stolen_cycles` on top of the
  CPU's own real cycle cost before feeding that total into CIA1's/CIA2's/
  the SID's own `tick` calls, the audio-accumulator math, and `Machine::
  step`'s own returned cycle count -- `bus.vic.tick` itself is called
  with only the CPU's own real cost, never the inflated total, since the
  raster generator's own timing must stay independent of the steal it
  reports (see that method's own doc comment).
  **`vic_accuracy = line | cycle` (2026-10-12)**: plugin option / `--vic-accuracy` /
  `C64::set_vic_accuracy(VicAccuracy)`. `line` (default) is the per-instruction model above. `cycle`
  runs `mos6502::Cpu::step` with `C64Bus::in_cpu` set so every bus cycle (`read`, `write`, or the
  core's `Bus::dummy_cycle`) calls `cpu_cycle`: one `tick_cycle` (VIC engine, tape, cartridge, CIA1/
  CIA2, SID, controllers advance one clock) and, for a *read* while the VIC's BA is low, further cycles
  until BA rises -- the 6510's RDY stalls reads, never writes. `step` returns the clock cycles that
  passed (CPU + stalls); interrupt requests raised inside an instruction wait in `pending_irq`/
  `pending_nmi` and reach the CPU after it. `C64::ba_stalled_cycles` counts the waiting. Measured:
  43 cycles per bad line on read-only code, 5 per lone sprite line, 9 for three adjacent sprites;
  writes overlapping the BA warning proceed. PAL only (refused on NTSC), a save state only loads onto
  a machine in the same mode, the REU/traps/debugger never tick the clock. Real KERNAL boots to READY.
  with the same screen as line mode. Tests: `src/cycle_tests.rs` (exact stall counts, alignment-based
  write/read cases, raster lockstep, same results in both modes, save-state/NTSC checks) and
  `tests/cycle_boot.rs`. `render_frame` returns the engine's own picture (border included) in cycle mode
  (`Vic::cycle_frame`), falling back to the register-decoded one until the first frame completes; a 6502
  program that flips RSEL in lines 248/255 opens the lower border through the whole machine
  (`cycle_tests::a_6502_program_opens_the_lower_border_in_cycle_mode_only`), which line mode cannot show.
  **PAL/NTSC (2026-10-11)**: `C64::set_region(machine::Region)` (called by the
  plugin at creation from the host's `--region`, `retroclawd.toml`/game-config
  `region`; `Japan` counts as NTSC) switches `Vic::set_ntsc` (6567R8: 65 cycles x
  263 lines instead of 63 x 312; display window, bad-line range and sprite offsets
  are identical), both CIAs' TOD divider (`Cia::set_ntsc`, 1 022 727 / 10) and
  `Machine::clock_hz`/`frame_hz` (1 022 727 Hz / 60 Hz instead of 985 248 / 50).
  The older 6567R56A (64 x 262) is not modelled. The KERNAL detects the standard
  itself by timing the raster and stores it at `$02A6` (`1` PAL, `0` NTSC) --
  **only the `-03` KERNAL (`kernal-901227-03.bin`) does this; `-01` has no
  detection and keeps PAL timing values**. `crates/machines/c64/tests/region.rs`
  boots the real `-03` ROM both ways (flag and `READY.` on each), and a live
  `retroclawd c64 --region ntsc|pal --headless --dump-memory 0x02A6:1` gives `00`/`01`.
  The new `ntsc` fields change the `.state` layout, so older save states
  no longer load (internal-only format).
  **Cartridges (`.crt`, 2026-10-11)**: new format crate `c64-cartridge`
  (`parse(bytes) -> Crt { hardware_type, exrom_level, game_level, name, chips }`;
  header levels are 0 = low = active) and `c64::cartridge::Cartridge`
  (`C64::load_cartridge[_crt]`/`eject_cartridge`, host `--cartridge <file.crt>`,
  `apply_media("crt")`, plugin `MediaKind::Cartridge` attach/detach). The
  cartridge drives the EXROM/GAME lines in VICE's *active* sense (`(true,
  false)` 8K, `(true, true)` 16K, `(false, true)` Ultimax, `(false, false)`
  off) and `C64Bus::pla_cfg` folds them into the 6510 port bits as VICE's
  5-bit index (bit 4 GAME active, bit 3 EXROM active, then CHAREN/HIRAM/
  LORAM). `PLA_*` bitmasks transcribe VICE's `c64meminit.c` ROML/ROMH/IO
  arrays (ROML in 8K/16K/Ultimax configs, ROMH at `$A000` for 16K with HIRAM,
  at `$E000` for Ultimax, I/O in the Ultimax configs too); the old no-cartridge
  map is configs 0-7 of the same tables (all earlier tests pass unchanged).
  `read` = PLA-routed `mapped_read` + CPU-only side effects (`read_roml`, I/O),
  `peek` has none. Ultimax leaves `$1000-$7FFF`/`$A000-$CFFF` open (reads
  `$FF`, writes dropped, RAM only below `$1000`); writes under ROML/ROMH/KERNAL
  still land in RAM. Mappers (behaviour from VICE's `simonsbasic.c`/`ocean.c`/
  `funplay.c`/`supergames.c`/`epyxfastload.c`/`gs.c`/`dinamic.c`/`zaxxon.c`/
  `magicdesk.c`/`easyflash.c`): 0 Normal, 4 Simons' BASIC ($DE00 read = 8K
  mode, write = 16K), 5 Ocean ($DE00 bank, one 8K bank in both windows;
  512K = 8K config), 7 Fun Play (register-encoded banks, `$86` = off),
  8 Super Games ($DF00 bank/disable/latch), 10 Epyx Fastload (512-cycle
  capacitor, ROML/IO1 reads recharge, last ROM page always at $DF00),
  15 System 3 (bank = IO1 address & $3F, read or write), 17 Dinamic (IO1
  read of $DE00-$DE0F), 18 Zaxxon (ROML reads pick the ROMH bank), 19 Magic
  Desk ($DE00 bank, bit 7 off), 32 EasyFlash (jumper-off boot decode table,
  Ultimax at reset, `$DE00` bank, `$DE02` mode, 256 bytes RAM at `$DF00`;
  flash programming is modelled, see below), and **0xC640, crystalct's C64 Unified Diagnostic Cartridge**
  (`Mapper::UnifiedDiag`, 2026-10-17; our own number, the board has no `.crt` type): 8K chips at `$8000` (the standard diagnostic) and
  `$E000` (the modified dead test), the header's EXROM/GAME levels give the start mode (Ultimax by default, the JPE/JPG jumpers), and a
  write to the I/O1 window sets the mode from data bit 0 (1 = ordinary 8K cartridge, 0 = Ultimax; the flip-flop is read off the dead
  test's own switch routine `LDA #$01 / STA $DE00 / JMP $FCE2`, no schematic here). A reset returns to the start mode; the mode is in the
  save state; the second 16K image (jumper JPS) is not modelled. Any other hardware type is a clear error listing the supported
  ones. `Machine::reset` also resets the cartridge (banked types return to
  bank 0, EasyFlash keeps its RAM); loading a cartridge resets the machine so
  the KERNAL finds `CBM80` at `$8004` or an Ultimax cartridge supplies the
  vectors. Save states carry only `CartState` (registers, EasyFlash RAM),
  applied onto the same image when attached again. Tests:
  `cartridge::tests` (18, per mapper), `bus::tests` (PLA maps, I/O routing,
  reset/eject, Epyx timing, save state), and the real-KERNAL
  `tests/cartridge.rs` (CBM80 autostart, Ultimax reset vector, EasyFlash
  Ultimax boot then switch to 8K, the unified cartridge's Ultimax-to-standard switch and KERNAL restart); live through the binary a hand-made
  `.crt` autostarted (`--dump-memory` showed its marker). `.p00` (PC64
  container: `C64File\0` + 16-byte name + 2 bytes, then `.prg` data) is
  handled next to `.prg` in `main.rs::resolve_program_load`
  (`p00`..`p99`; `.s00`/`.u00`/`.r00` pass through) and `--run` treats it like a
  `.prg`.
  **Freezers, cartridge RAM, flash, Ultimax VIC view, GeoRAM (2026-10-12)**: new
  mappers 1 Action Replay (4x8K ROM, 8K RAM over ROML via `$DE00` bit 5 and in
  `$DF00`; freeze = Ultimax from bank 0 with RAM; a read of `$DE00` does *not*
  corrupt the register as it does in VICE), 2 KCS Power (mode from the `$DE00`
  address bit 1 and R/W, 128 bytes RAM + GAME/EXROM status read at `$DF80`), 3
  Final Cartridge III (4 or 16 banks, register `$DFFF`: bank, EXROM/GAME, NMI bit,
  hide bit; the freezer maps Ultimax for the CPU while the VIC keeps RAM),
  36 Retro Replay (8 banks, 32K RAM, `$DE00`/`$DE01` with write-once flags,
  REU-compatible map, freeze with NoFreeze; Retro Replay revision only, no
  clockport, no flash jumper) and 62 Expert (8K RAM image from the CRT; switch
  `--expert-switch prg|on|off`: prg = 8K game with writable RAM at `$8000`, on =
  RAM overlaid at `$8000`/`$E000` through `Cartridge::overlay_read` with the
  `$DE00` toggle and the freeze button; the RESTORE-key NMI acknowledge is not
  modelled). Sources: VICE `actionreplay.c`/`kcs.c`/`final3.c`/`retroreplay.c`/
  `expert.c`/`c64cartmem.c` read as references (CREDITS.md). `Cartridge::write_hook`
  lets a cartridge swallow CPU writes into the ROM windows (cartridge RAM, flash)
  with `rom_visible`/`ultimax` from the PLA; `C64::freeze()` presses the button and
  raises the NMI (the line is a one-shot edge, there is no held level, so the
  "release freeze" register bits only clear state). The `freeze` hotkey (default
  `ctrl+pause`) sends `MachineCommand::Freeze` (protocol v9). **EasyFlash flash**
  (`crate::flash::Flash040`, AM29F040B command set: unlock `$AA/$55`, autoselect,
  byte program with bits only going 1 -> 0, sector/chip erase with 50-cycle
  timeout and 1M/8M-cycle erase times, DQ6 toggle/DQ7/DQ5/DQ3 status) answers
  Ultimax-mode writes at `$8000`/`$E000`; programmed content leaves through
  `--cartridge-save <file.crt>` (`MachineCommand::ExportCartridge`, written
  atomically at exit; without the flag a warning says the changes are dropped) and
  rides in save states while modified (`CartState::flash_data`). EAPI itself is
  just the cartridge's own code running on that flash. **Ultimax VIC view**: when
  the cartridge drives an Ultimax map (and it is not the FC3 freezer), the VIC
  fetches ROMH's upper 4K at `$3000-$3FFF` of every bank (`C64Bus::vic_ultimax_copy`,
  used for rendering and the sprite-collision scan). **GeoRAM**
  (`--georam off|512|1024|2048|4096`, `crate::georam::GeoRam`): window `$DE00`,
  page `$DFFE` (0-63), 16K block `$DFFF`, values wrap as in VICE `georam.c`; it
  shares the expansion port with `--reu` and `--cartridge`. Tests:
  `cartridge::tests` (6 new), `flash::tests` (5), `georam::tests` + `tests/georam.rs`,
  `bus::tests` (VIC view, freeze NMI through the cartridge vectors, flash
  programming through the bus), plugin ABI tests for Freeze/ExportCartridge.
  **Serial, ethernet and printer (2026-10-12)**: new chip crates `acia-6551` (package
  `acia6551`: the 6551 with status/command/control registers, character timing from the
  programmed baud, receive overrun, TX/RX interrupt edges, programmed reset; `Mode::SwiftLink`
  doubles the baud table, `Mode::Turbo232` adds the extended-control register at +7 for
  230400/115200/57600/28800) and `cs8900a` (CS8900A in I/O mode: 16 ports, 4K PacketPage, TxCMD/
  TxLength/BusST handshake, RxEvent/RxStatus/RxLength reads, individual/broadcast/multicast/
  hash/promiscuous filtering, auto-increment pointer; ports read the frame header high byte
  first as VICE's `cs8900.c` documents). Both follow VICE's `aciacore.c`/`cs8900.c`/
  `ethernetcart.c` as references (CREDITS.md). `C64Bus::acia: Option<(Acia, page)>` answers
  `$DE00`/`$DF00` (4 registers, 8 for Turbo232) and its interrupt goes to `request_nmi`;
  `C64Bus::rrnet` is `rrnet::RrNet` (chip + `loopback`; RR-Net mapping ignores offsets 0-1 and
  XORs the offset with 8) and `step` moves transmitted frames back into the receive queue when
  looping back. All expansion devices share `C64Bus::expansion_taken` (REU, GeoRAM, cartridge,
  serial, RR-Net exclude each other). The plugin's `SerialNet` (`net.rs`) is the TCP side: a
  thread dials out (`connect:host:port`, retried every 2 s) or accepts one peer on
  `127.0.0.1:port` (`listen:port`); `InstanceRecord::step` pumps bytes every 256 instructions
  and sets the carrier (DCD/DSR low) while a peer is connected, so a hang-up drops it.
  **Printer**: with `--printer` (`printer` plugin option) `try_printer_trap` answers the
  KERNAL's own serial routines `LISTEN $ED0C`, `SECOND $EDB9`, `CIOUT $EDDD`, `UNLSN $EDFE`
  (checked by their first two opcode bytes, identical on every committed KERNAL; the `$FFxx`
  jump-table entries and OPEN/CHKOUT's internal calls both land there) for devices 4/5, so
  `OPEN 1,4,7 : PRINT#1,"HI" : CLOSE 1` succeeds with no IEC device and the bytes collect in
  `printer::Printer`; `MachineCommand::ExportPrinter` (protocol v10) and `--printer-out` write
  them as ASCII (PETSCII `$0D` -> newline, secondary 7 or `$0E` = lower/upper case mode,
  graphics `?`). Without `--printer` device 4 is "not present" as before. Tests:
  `acia6551` (9), `cs8900a` (8), `printer::tests` (3), `bus::tests` (RR-Net loopback and
  isolated, SwiftLink NMI, exclusion), `tests/peripherals.rs` against the real KERNAL
  (printer through OPEN/CHKOUT/CHROUT/CLOSE, a SwiftLink send+receive program, RR-Net product
  ID through the window) and the plugin's `net::tests` (a real TCP peer echoing through a 6502
  program, hang-up drops the carrier). Not verified: real SwiftLink terminal software, real
  RR-Net drivers (e.g. contiki) -- none available here.
  **True 1541 (`--true-drive --rom-1541 <16K>`, 2026-10-12)**: opt-in; the default stays the
  instant fast-loader traps. `c64-machine-plugin/src/drive.rs::DriveLink` owns a `c1541::C1541`
  and a `c1541::IecBus`; `InstanceRecord::step` runs the C64, then publishes its ATN/CLOCK/DATA
  outputs, catches the drive up by a fractional cycle account (drive 1 MHz vs C64 985 248 Hz)
  and propagates the wired-AND levels after every drive instruction -- the loop
  `tests/iec_1541.rs` used to drive by hand. The disk goes to the drive (`.d64`/`.x64`/`.g64`;
  `.d71`/`.d81` are refused: a 1541 cannot read them) and the C64 holds none, so neither
  trap can engage. **Write head (2026-10-12)**: with VIA2 CB2 low and DDRA `$FF` (what the DOS sets
  with `PCR = (PCR & $1F) | $C0` at `$F5A0`/`$FDA3`) `disk::DiskMechanism` writes: at each 8-bit cell
  boundary the shift register takes the byte in Port A, byte-ready is pulsed (SOE-gated, CA1 + SO)
  and the track's GCR bytes are overwritten; leaving write mode decodes the track with
  `gcr::decode_track` (sync run + header + data block, checksums checked) and stores changed sectors
  into the `d64::D64` (`C1541::take_modified_disk_image`/`disk_image`/`mark_disk_saved`, wired
  through the plugin's `ExportDisk`/`MarkDiskSaved`, so `--disk-save`/`--disk-writeback` work).
  The decoder ignores the last group's two trailing off-bytes: the DOS switches the head off right
  after the final byte loads, so they are clipped on real hardware too (found live: every real
  data block failed the checksum-group check until this). Proof: `real_dos_saves_a_file_onto_the_disk_through_the_write_head`
  (the real KERNAL `SAVE` over IEC to a blank formatted image, then directory entry and
  `read_file` bytes checked), plus `disk::tests`/`gcr::tests` for the mechanism. A disk swap is a
  300 000-cycle empty gap (`drive.rs::DISK_SWAP_GAP_CYCLES`) because the DOS notices a change only
  through a write-protect-sense gap; an instant swap leaves its BAM stale. Not modelled: weak/half
  tracks, writes spanning a track change, `.g64` write-back (refused, decoded image would not
  round-trip raw tracks).
  Save states become a `TDRV` container (C64 state + drive state) and refuse to load across a
  true-drive mismatch. A real bug fixed on the way: `C1541::iec_output` lacked the 1541's
  hardware **ATN-acknowledge gate** (DATA is pulled low while the ATN ACK bit equals the ATN
  line level, as in VICE's `via1d1541.c` `store_prb`), without which the C64 would never see a
  device answer ATN. Verified with a hand-assembled drive firmware answering a C64 program
  across the bus (`drive::tests`) and the gate's own unit test.
  **Validated against the real 1541 DOS ROMs (2026-10-12)** -- `roms/1541/` holds the 1541
  (325302-01 + 901229-05), 1541-II (251968-03) and, via `roms/c64/VC1541-C`, the 1541-C ROM
  (user-supplied from VICE/Denise, same private-repo convention as the C64 ROMs). Running the
  real KERNAL against the real DOS found **three drive-model bugs the hand-made ROM could never
  show**, all fixed: (1) the drive's receivers are inverting buffers -- VIA1 Port B bits 7/2/0
  (ATN/CLOCK/DATA IN) read 1 while the line is *asserted*, and CA1 sees the inverted ATN, so
  asserting ATN is a *rising* edge (the real DOS programs PCR = $01; with the old polarity the
  drive never took the ATN interrupt); (2) VIA2 Port B bit 7 is SYNC (0 while the head is over a
  SYNC mark -- DOS polls it with `BIT $1C00 / BMI`) and bit 4 the write-protect sense (1 = writable
  disk in), neither was wired; (3) the stepper phase counts in **binary**, not Gray code
  (`$FA6E`: `INX / TXA / AND #$03 ... STA $1C00`; head moves by `((new-old+1)&3)-1`, +2 ignored) --
  the old Gray order silently discarded most steps so the head never left track 1. Proof:
  `crates/machines/c64/tests/real_1541.rs` (`--release`, `-- --ignored`; ROM-gated): the real
  KERNAL's own `SETNAM/SETLFS/LOAD` loads the `$` directory through all three DOS ROMs, and
  `LOAD"*",8,1` of every `.d64` in `software/c64` returns bytes identical to
  `d64::read_file` (28-31 KB each in ~40-85 emulated seconds, ~360 B/s as on hardware); also
  through the real `retroclawd c64 --true-drive --rom-1541 ... --headless` binary. A sibling
  test (`play_disk_image`, env `DISK`/`TRUE_DRIVE`/`CYCLE`/`SECONDS`/`FIRE_AT`/`AUDIO`/`SHOTS`) types
  `LOAD"*",8,1` + `RUN` into the real keyboard queue and writes screenshots.
  **A real C64 bug found by playing the games**: `Cia::tick` returns the interrupt *level* but
  CIA2's NMI was requested on every `true`, so a program that programs a fast CIA2 timer and
  has not yet read the ICR got an NMI after *every instruction* and never ran its handler
  (Bomb Jack's intro livelocked at `$FE43`). The 6510's NMI input is edge triggered:
  `C64Bus::cia2_nmi_edge` now requests an NMI only when the line goes from released to asserted,
  a CPU read of `$DD0D` releases it at once, and a state load re-derives it
  (`Cia::interrupt_asserted`). Games seen running (fast loader, true drive and `vic_accuracy =
  cycle` give the same screens; cycle mode additionally shows raster-split intros): Bomb Jack
  (Lurid/Tricycle intro), both Donkey Kongs, Dragon Breed side A (Crypt Wanderer; the split
  screen only draws correctly in cycle mode) and side B (a looping digi-sample intro on a black
  screen -- audio verified live), Boulder Dash `.t64` (Nostalgia intro; raster bars only in cycle
  mode) and the Attack of the Mutant Camels `.crt` (title screen). Accuracy: instruction
  granularity on both sides, no sub-instruction bus timing.
  **Plugin errors now reach the user (2026-10-12)**: every plugin error used to collapse to a
  status code on the host ("machine plugin call failed: 1 (unknown)"), hiding explanations such
  as the `.nib` refusal. `RcCallResult::error_with_message` puts the text in the error payload
  (all four machine plugins use it) and `plugin_bridge::decode_call_payload` shows and frees it;
  a plugin that attaches nothing keeps the old message, so older plugins still work (an old host
  would only leak the small buffer).
  **REU** (`--reu off|128|256|512`, `crate::reu::Reu`, `C64::attach_reu`):
  1700/1764/1750 on the expansion port, registers at `$DF00-$DF0A` (IO2),
  behaviour from VICE's `reu.c` (reference only, see `CREDITS.md`): status
  (read clears the interrupt/EOB/verify bits), command (stash/fetch/swap/
  verify, autoload, FF00-disable), C64/REU/bank/length/mask/address-control
  registers, 19-bit address counter, length `0` = 64K, fixed-address bits,
  autoload shadows, open-bus reads past the installed DRAM. A transfer runs
  to completion the instant it is started (`Reu::run` against `DmaBus`, which
  `C64Bus` implements through its own memory map, so `$D800` hits colour
  RAM) and returns the CPU cycles it took (1/byte, 2 for swap, verify stops
  at the first mismatch); `C64Bus::dma_cycles` is charged by `C64::step` in
  `DMA_CHUNK_CYCLES` chunks through `advance_peripherals`, so CIA/VIC/SID keep
  running while the CPU is held off. A command written with FF00 enabled
  waits for a write to `$FF00`. The end-of-block/verify interrupt goes to
  `request_irq`. An REU and a cartridge exclude each other (one port); the
  DRAM is part of the save state (`SaveState::reu`). Tests: `reu::tests` (11),
  `tests/reu.rs` (a 6502 program stashes, wipes and fetches a block on the real
  KERNAL machine); live: `--reu 512 --program` restores the wiped bytes, the
  same run without `--reu` leaves them zero. Not done: GeoRAM, REU
  timing against the VIC (bad lines steal differently during DMA), and the
  1764/1750 real-hardware quirks beyond the 19-bit counter.
  **Tape LOAD/SAVE traps, tape recording and the `.tap` fixes (2026-10-12)**:
  two real bugs surfaced building this, both invisible to the earlier tests
  (they never ran a real tape load): (1) `c64-tape` took the `.tap` header as
  24 bytes; it is 20 (magic 12, version, platform, video standard, reserved,
  `u32` size at offset 16), so every real file lost its first 4 pulses;
  (2) `tick_tape` toggled the READ line once per pulse, but CIA1's FLAG fires
  on *falling* edges only, so the KERNAL saw every second pulse -- each pulse
  is now a low blip on FLAG. A mutation check (old toggle back) makes the
  real-KERNAL playback test hang. `c64_tape::rom_format` is the standard
  cassette format **read off a recording of the real KERNAL's own SAVE**:
  pulses short/medium/long (recorded ~362/510/680 cycles, third-party tools
  384/528/688; classify at 436/596), a byte is a long+medium marker then nine
  bit-pairs LSB first (0 = short,medium; 1 = medium,short; odd parity),
  blocks are leader (27136 shorts before a header, 5377 before data) +
  countdown `$89..$81` (`$09..$01` for the repeat) + payload + XOR checksum +
  `L S` + 79 shorts, every block twice (the repeat rescues a damaged first
  copy); the 192-byte header payload is type (1 BASIC, 3 absolute), start,
  end (exclusive), 16-char name, `$20` padding. `decode_files`/
  `encode_program` do both directions. The trap: `try_fast_kernal_load_trap`
  now also serves device 1 from the decoded programs of an attached `.tap`
  (name = prefix match, empty name = next program, the position advances and
  the tape "rewinds" itself when nothing matches after it; a decodable tape
  that lacks the name answers FILE NOT FOUND, an undecodable one is played in
  real time as before), and `try_fast_kernal_save_trap` appends an encoded
  program to the **tape recorder** (`tape_recorder::TapeRecorder`, always
  listening on the cassette WRITE line = port bit 3, gated by the motor).
  The KERNAL writes each pulse as a symmetric full wave (two toggles half a
  pulse apart), so recorded edges are paired into waves (a falling-to-falling
  measurement would mix neighbouring pulses; the pairing parity is picked by
  which gives widths nearest 362/510/680). `C64::arm_tape_recorder` presses
  RECORD (SENSE low) for a real-time SAVE; `recorded_tape` +
  `c64_tape::write_tap` + the plugin's `ExportTape { "tap" }` give
  `--tape-save out.tap`. Option `fast_tape_load` (default on; `false` plays
  every pulse and records the real routine). `--run` with a `.tap` types
  `LOAD`, Enter, then `RUN` (`AutoRunProfile::C64Tape`). Tests: `rom_format::tests`
  (5), `tape_recorder::tests` (2), and `tests/tape_roundtrip.rs` against the
  real KERNAL: its real-time SAVE recorded and decoded back to the same
  program (name, type, addresses, bytes), an encoded tape loaded by the real
  LOAD in real time, and both traps instantly. Limits: no seq-file (OPEN) or
  header-type 4/5 handling, no turbo-loader formats, VERIFY not trapped, the
  KERNAL's SEARCHING/FOUND/LOADING messages are not printed by the trap, the
  trap does not move the real-time tape position.
  **Paddles and the 1351 mouse (2026-10-12)** (`crate::controllers::Controllers`,
  plugin option `mouse = off|1351-port1|1351-port2|paddle-port1|paddle-port2`, keys
  `Paddle<1|2><A|B><Dec|Inc|Fire>`): the SID's POTX/POTY (`$D419`/`$D41A`, in
  every mirror) read the control port(s) that CIA1 Port A bits 7:6 select --
  `%01` port 1, `%10` port 2, `%11` both in parallel (`controllers::parallel`,
  this crate's own resistance model), `%00` neither (`$FF`); nothing attached
  reads `$FF` (open pot). Selection mapping read from VICE's `c64cia1.c`/
  `joyport.c` (`set_joyport_pot_mask((b >> 6) & 3)`, mask 1 = first port).
  1351: POT = `(counter & $7F) + $40` per axis, host motion accumulates
  (up raises POTY), left button = Fire, right = Up (VICE `mouse_1351.c`).
  Mouse-as-paddle: motion halved, clamped 0..255, inverted (VICE
  `mouse_paddle.c`), left/right button = Left/Right line. Key paddles move one
  pot step per 2000 cycles while held; A's fire is the port's Left line, B's
  Right. Button lines are ANDed into CIA1 Port A/B reads next to the joystick.
  Not in save states (transient input). Tests: `controllers::tests` (6), a
  bus-level POT/CIA1 select test, and a plugin test through the real ABI.
  Not verified: real paddle software, the 1351's exact count scaling
  (VICE's formula used as is).
  **Light pen and the SID model (2026-10-12)**: mouse mode `lightpen`
  (`MouseMode::LightPen`) points a pen at the 320x200 picture (relative
  motion, clamped); the left button presses port 1's fire line and fires
  `Vic::light_pen_trigger(x, y)`, which latches `$D013` = (24 + x) / 2 and
  `$D014` = 51 + y, at most once per frame (cleared when the frame counter
  wraps), sets `$D019` bit 3 and requests the IRQ when `$D01A` bit 3 allows
  it; the registers are read-only to the CPU. A real pen's sensor lag is not
  modelled. Plugin option `sid_model = 6581|8580` (`sid::SidModel`, default
  6581): the 6581's filter never opens below ~220 Hz (reSID's measured
  `FCmin`), the 8580 keeps the 30 Hz floor, and the master-volume stage adds
  a DC term before multiplying by the volume (6581 0.10, 8580 0.01 of one
  full voice) so `$D418` writes are audible -- this is what makes 4-bit
  sample playback work on the 6581 and stay faint on the 8580; the ratio is
  the documented behaviour, the magnitudes are this project's own choice.
  Still approximations: filter curves are linear (no per-chip DAC table),
  combined waveforms use the AND rule for both models.
  **Disk containers (2026-10-11)**: `crates/formats/g64` parses `GCR-1541` and
  decodes the bitstream back to a `.d64` (`G64::to_d64`: per half-track circular scan for the
  sync run, header block `0x08` and data block `0x07`, GCR 5-to-4 decode); `D64::parse_x64` strips
  the 64-byte `43 15 41 64` header. The C64 plugin sniffs `.g64`/`.x64` next to `.d64`/`.d71`/`.d81`,
  `.nib` is a clear error, and `--run` treats them like `.d64`. `--disk x.g64 --disk-writeback`
  is refused (the decoded image would not round-trip raw tracks); use `--disk-save out.d64`.
  `--disk list.m3u` (`apps/retroclawd/src/playlist.rs`) attaches the first entry and the
  `next_disk`/`prev_disk` hotkeys swap (wrapping, unsaved-disk question as for manual swaps).
  **Disk SAVE** (`C64::try_fast_kernal_save_trap`, `d64::D64::save_program`):
  traps KERNAL SAVE at `$F5DD` (`STX $AE`, the routine `$FFD8` jumps to;
  A = zero-page pointer to the start address, X/Y = end, checked on the
  committed -01/-02/-03 ROMs) for a disk device with a `.d64` attached and
  stores the range as a closed PRG, load address first. `save_program`
  allocates sectors from the BAM (tracks 17 down to 1, then 19 up, never 18;
  interleave 10 within a track), writes the chain (last sector's link byte =
  valid bytes + 1, what `read_file` reads), fills a directory slot (grows the
  directory chain on track 18 when full) and updates the BAM; on any error
  the image is left exactly as it was. A name starting `@0:` / `@:` replaces an
  existing file (scratches its chain first), a leading `0:` is dropped. What
  the KERNAL itself sees: an empty name -> carry set, A=8; any drive-side
  failure (file exists without `@`, disk full, name over 16 characters)
  returns success with the image untouched, as the real call does -- the
  drive error channel (63/72/33) is **not** modelled, so those saves are
  silent. `.d71`/`.d81` are writable too through the shared
  `crates/formats/cbm-dos` (`Image` trait + one `save_program`; each format
  supplies its BAM bit/counter layout, allocation order and interleave: d64
  17..1 then 19.., d71 adds side 2 outward from track 53, d81 header/BAM on
  track 40, interleave 1); a tape device saves through the tape trap. The modified image
  leaves through `C64::take_modified_disk_image`/`disk_image`/
  `mark_disk_saved` and the plugin's `ExportDisk`/`MarkDiskSaved`, so
  `--disk-save`/`--disk-writeback` and the save-disk hotkey work for the C64
  like for the CPC. The image is not part of save states (it stays external,
  so a state loaded after a SAVE keeps the disk as it is now). Tests:
  `d64::tests` (6 new: round trip, multi-sector, replace, directory growth,
  disk full = unchanged image, names), `bus::tests::fast_kernal_save_*` (3),
  `tests/disk_save.rs` (real KERNAL `SETNAM`/`SETLFS`/`SAVE` calls); live:
  `--disk blank.d64 --disk-save out.d64 --program` with the same call
  sequence produced a directory entry `CLISAVE`, 1 block. Not done: tape SAVE/
  LOAD trap (done later, see the tape paragraph), VERIFY, SEQ/REL files, the drive error
  channel.
  SID audio is wired: `C64Bus::io_read`/`io_peek`/`io_write` decode the
  real `$D400`-`$D7FF` window as `sid.read`/`write((addr & 0x1F) as u8,
  ..)` -- the chip only decodes 5 address lines on real hardware, so its
  25-register map mirrors every 32 bytes across that whole 1KB window,
  same as real hardware. `C64::step` ticks `bus.sid` by that step's own
  real cycle count directly (no clock-divider math needed the way
  `zx_spectrum` needs one for its AY-3-8910 -- a real SID runs at the
  same clock as the CPU beside it) and accumulates/emits samples via the
  same `audio_cycle_accumulator`/`mix_audio_sample` shape `zx_spectrum`
  established, `Machine::drain_audio` overridden the same way. Save
  state round-trips `sid::Sid` via a direct serde derive (same "holds no
  external-media state" tier as `mos6502::Cpu`/`vic_ii::Vic`/`crate::
  cia::Cia` already get) plus the new `audio_cycle_accumulator` field.
  **Boots real KERNAL/BASIC/
  character ROMs to an actual `READY.` prompt** (user-supplied, now
  committed under `roms/c64/`) -- confirmed live via `--headless
  --dump-screen`, not just unit-tested; getting there surfaced and fixed
  one real bug (the VIC-II raster-register busy-wait hang, see
  `vic-ii`'s own bullet above).
  **Cassette tape** (`.tap`/`.t64`, new `crates/formats/c64-tape` crate,
  package `c64-tape`): real hardware splits cassette I/O across two
  genuinely different places, confirmed against VICE's own real source
  this session (not recalled) -- the READ line is CIA1's dedicated
  **FLAG** pin (`crate::cia::Cia::set_flag`, mirrors `via6522::Via::
  set_ca1`'s shape but fixed negative-edge-only, no PCR-style
  configurability, confirmed via `ciacore.c`'s `ciacore_set_flag`/
  `cia.h`'s `CIA_IM_FLG` bit), while MOTOR (bit 5) and SENSE (bit 4) live
  on the **6510 CPU's own on-chip I/O port** (`$0000`/`$0001`) -- not any
  CIA register at all, confirmed via `c64mem.c`/`c64pla.c`'s real
  `c64pla_config_changed`. A real, easy-to-get-wrong edge case reproduced
  faithfully rather than simplified: MOTOR reads ON both when driven low
  as output AND when merely left configured as input, the *one*
  combination that turns it off is bit 5 driven high as output.
  `C64Bus::new` seeds `cia1.set_flag(true)` (idle-high) at construction
  -- the same "host must seed the chip's assumed initial wiring state"
  lesson `c1541`'s own VIA1/`ATN` seed already taught (caught the same
  way: a failing test, not foresight alone, even though the doc comment
  anticipating it was written first). `C64::step` calls `bus.tick_tape`
  (which drives `cia1.set_flag`) *before* `cia1.tick`, same same-step
  ordering fix already established for the 1541's own VIA2/disk
  interaction. `.tap` ("C64-TAPE-RAW") is a real, raw pulse-capture
  format -- unrelated to ZX Spectrum's own same-named format despite the
  shared extension -- decoded per `c64-tape`'s own doc comment (a
  nonzero pulse byte is `N * 8` cycles; a `0x00` byte in a version-1 file
  is an exact 24-bit escape, confirmed against VICE's real `datasette.c`
  over a separate, non-authoritative helper that looked similar; a
  version-0 `0x00` byte has no real defined meaning, so it's a clean
  error, not a guess). `.t64` is not a real bitstream at all (a directory
  container invented by an emulator) -- treated like `.prg`: its first
  `NORMAL`-type entry is poked straight into memory at its own stored
  load address (`c64-machine-plugin`'s `AttachMedia` handler, no
  `TapePlayer` involved), never bit-banged, since inventing timing that
  was never real would be the lazy choice, not the honest one. Verified
  live through the real binary: a synthetic `.tap` boots a real KERNAL to
  `--dump-memory 0xDC0D:1` (CIA1's ICR) showing bit 4 (FLAG) set only
  with a tape attached, absent in a same-cycle-count control run with
  none; a synthetic `.t64`'s stored bytes land exactly at its own stored
  load address. Save state round-trips only the tape's own
  `TapePosition` (playback position), never its bytes, same
  `nec765::Fdc`/`zx_tape::TapePlayer`/`c1541::DiskMechanismState`
  external-media-stays-external shape. No KERNAL LOAD-routine trap exists
  for either format (out of scope, a separate feature) -- `.tap` plays
  the real pulse protocol in real time, `.t64` is a one-shot poke the
  user still `SYS`/`RUN`s manually, same as `.prg`.
  **`.d64` disk + IEC-level KERNAL LOAD fast loader**: the KERNAL-LOAD-
  routine trap the tape paragraph above says doesn't exist -- doesn't
  exist *for tape*; it does now for disk. `C64Bus::disk: Option<d64::
  D64>` is a direct attachment with no simulated drive hardware behind
  it at all (no `c1541`, no IEC bus, no GCR bitstream -- see `c1541`'s
  own bullet below for what a *real* 1541 wiring looks like, deliberately
  not reused here since nothing in this architecture steps a second
  machine alongside this one). `C64::try_fast_kernal_load_trap` (checked
  in `step`, mirroring `zx_spectrum::try_fast_tape_load_trap`'s own
  shape) traps real KERNAL LOAD's own entry point, `$F49E` -- confirmed
  byte-identical (`STX $C3`/`STY $C4` before falling through the real
  `$0330` vector) across all 5 committed KERNAL ROMs this session via a
  throwaway Python 6502 disassembler, not recalled -- *before* any of
  LOAD's own real work happens, so this trap fully substitutes for what
  would otherwise be a real, slow IEC serial-bus byte transfer (the
  actual "fast loader" -- same shape real fastloader carts/DOS wedges
  and VICE's own "no true drive emulation" mode use, replacing the
  KERNAL's own byte routine with a direct block read). Reads the real
  zero-page LOAD state the caller's own `SETLFS`/`SETNAM` already set
  (`FA`=`$BA`, `SA`=`$B9`, `FNLEN`=`$B7`, `FNADR`=`$BB`/`$BC` -- every one
  confirmed by disassembling `$F3D5`'s own `LDA ($BB),Y` filename read,
  not just cited), engages only for a real disk device (`FA >= 8`) and
  real LOAD (not VERIFY, an explicit scope cut -- VERIFY falls through to
  real, already-non-functional KERNAL execution), looks up the filename
  in `disk.directory_entries()` (`d64::D64::find_program`, a bare `*`
  matching the first closed PRG entry -- the real, universal
  `LOAD"*",8,1` "load the first program" idiom), reads its real sector
  chain (`d64::D64::read_file`, new this pass, including the real
  last-sector-length-byte convention confirmed against a cited example),
  and writes the payload at the real target address (`SA == 0` uses the
  caller's own given `X`/`Y`; `SA != 0` uses the file's own embedded
  2-byte header address -- exact real KERNAL convention, both paths
  tested). Success sets `X`/`Y` (and the real `$AE`/`$AF` scratch bytes
  some code reads directly) to the real "one past the last byte loaded"
  value, clears carry; a miss sets carry + `A = 4` (FILE NOT FOUND,
  confirmed via search). Either way, simulates the routine's own eventual
  `RTS` by popping a real return address off the 6502 stack -- never
  executes any of `$F49E`'s own instructions, same "never runs the real
  routine, only its real effect" principle the tape trap already
  established. `--disk` now accepts `.d64` for `--machine c64`
  (`c64-machine-plugin`'s own `MediaKind::Disk` arm -- previously a bare
  "isn't supported yet" stub).
  **`--run` auto-run scaffolding** (a new `run` `MachineOptionInfo`,
  mirroring CPC's own `--run`/autotype precedent in `apps/retroclawd/src/
  gui/input.rs` exactly): for an attached `.d64`, autotypes
  `LOAD"*",8,1` (the C64's own dedicated `Asterisk` key -- no natural
  host `KeyCode` exists for it, so the previously `KeyCode`-only autotype
  action queue gained a generalized `AutoRunKey::Host(KeyCode)|
  Named(&str)` to send it as a stable machine-side name instead) then,
  after a short delay, `RUN` -- real BASIC's own LOAD-statement command
  handler does the line-pointer relink a subsequent `RUN` needs
  automatically, for free, since this goes through a real, autotyped
  BASIC command, not a raw poke. A raw `.prg`/`.t64` poke has no such
  handler in the loop at all, so `c64-machine-plugin`'s new
  `finish_basic_program_load`/`relink_basic_program` synthesizes
  VARTAB/ARYTAB/STREND (`$2D`-`$32`) by hand first, mirroring real
  BASIC's own LINKPRG: walks the loaded program's real line-link format
  (`[u16 link][u16 line#][tokens...][$00]`, terminated by a real `$0000`
  link) and rewrites each line's own link to its real, freshly-computed
  address rather than trusting the file's stored one -- gated strictly
  to the real, conventional `$0801` BASIC origin, a no-op at any other
  load address (a machine-code program has no such structure at all,
  and blindly walking/rewriting its bytes would corrupt it).
  **A real, live bug found and fixed getting the GUI side of this
  working, not just unit-tested**: an actual GUI run under this sandbox's
  own software (llvmpipe) rendering showed `LOAD"*",8,1`'s own keystrokes
  never reaching the screen at all (confirmed via the debugger plugin's
  `/api/memory-bytes` endpoint dumping live screen RAM), while the
  *later*, multi-second-delayed `RUN` follow-up worked fine -- real
  GPU/shader-compile startup cost (independently documented already by
  this project's own audio-setup code) can genuinely outlast the flat
  1-second startup delay every auto-run profile already used, silently
  swallowing keystrokes sent before BASIC's own input loop starts polling
  the keyboard buffer. Fixed the same way `CpcTape`'s own pre-existing
  readiness-timeout mechanism already handles the analogous real-tape-
  load-takes-a-while problem: both C64 profiles now wait (logged
  timeout/warning, same shape) for `PC` to land inside `$E5CD`-`$E5D4`,
  the real KERNAL "wait for a keypress" idle loop (confirmed by
  disassembly: `LDA $C6/STA $CC/STA $0292/BEQ $E5CD`, `$C6` being the
  real keyboard-buffer count NDX), before typing a single character.
  **Verified live end to end through the real binary** (not just unit
  tests): a real GUI session with a synthetic `.d64` (one tokenized
  `10 POKE 49152,66` program named `HELLO`) and `--run` shows, via the
  debugger's own HTTP API, the real screen text `LOAD"*",8,1` /
  `READY.` / `RUN` / `READY.` in sequence, memory at `$0801` holding the
  exact tokenized bytes, and memory at `$C000` reading back `$42` --
  the loaded program's own `POKE` genuinely executed after a real,
  autotyped `RUN`. The fast-loader trap itself was separately verified
  live too, headless, independent of the GUI/auto-run layer: a synthetic
  6502 test program executing a real `JSR $FFD5` against the real
  committed KERNAL ROM, both by exact filename and by `*` wildcard, plus
  a no-disk-attached control run confirming the trap never engages
  without one.
- **`sid`** (`crates/chips/sid`, package `sid`) — MOS 6581/8580 SID sound
  chip, phase 1 of the C64 parity roadmap's SID track (see `TODO.md`):
  3 voices, each a 24-bit phase accumulator driving triangle/sawtooth/
  pulse/noise waveform generation (top 12 accumulator bits, same
  architectural shape as `ay_3_8910`'s tone generators) plus its own ADSR
  envelope generator. Same host-agnostic-chip convention as `ay-3-8910`/
  `vic-ii`/`i8255`: no dependency on `machine`, `mos6502`, or an audio
  backend -- `write`/`read` are the real 25-register interface
  (`$00`-`$18` relative to the chip's base address), `tick(cycles)`/
  `output() -> f32` drive sample generation at whatever clock-divided rate
  the caller passes in, same shape as `ay_3_8910::Ay3_8910::tick`/
  `output`. Noise LFSR tap pattern (23-bit, feedback bits 22/17, output
  bits 22/20/16/13/11/7/4/2), hard sync (each voice syncs to the
  *previous* voice in the 3-voice ring) and ring modulation (substitutes
  the ring source's accumulator MSB for the triangle generator's own),
  and the ADSR envelope's linear-attack/exponential-decay-and-release
  rate-counter tables are the chip's own well-documented, widely
  cross-referenced internal architecture -- same "trusted external
  reference, not verified against this project's own silicon" epistemic
  tier as `ay-3-8910`'s own algorithms (see `CREDITS.md`). Two documented
  approximations: combined-waveform selection (more than one waveform bit
  set at once) uses the common bitwise-AND simplification most
  non-cycle-exact SID emulators use rather than a per-chip-measured
  table, and the real "ADSR delay bug" (retriggering mid-rate-count can
  delay the next envelope state change by one cycle) isn't modeled. Wired
  into `crates/machines/c64`'s real `$D400`-`$D7FF` I/O window and its
  audio/save-state paths -- see `c64`'s own bullet above.
  **Analog filter** (phase 2, same session): `Filter` (the real
  state-variable/multi-mode topology -- two cascaded integrators around a
  feedback loop -- via the classic discrete "Chamberlin" recurrence, a
  standard DSP approximation of that exact analog shape, not copied from
  any one SID emulator) is stepped once per real system clock cycle inside
  `Sid::tick_one_cycle`, fed by whichever voices `$17`'s low 3 bits route
  through it (continuous integration -- the filter's input is the routed
  voices' own continuously-changing instantaneous levels, which `output()`
  alone has no way to reconstruct at its own much-slower audio-sample-rate
  query points). `$17`'s high nibble (resonance) and `$18`'s LP/BP/HP
  select bits + voice-3-off bit are all applied to the real mix. Two real
  hardware facts confirmed and implemented, not just the filter math:
  voice-3-off (`$18` bit 7) disconnects voice 3 from the final output
  entirely regardless of filter routing (real hardware's own way of using
  it purely as an inaudible modulation/OSC3-randomness source), and an
  unselected filter output (no LP/BP/HP select bit set) means every voice
  routed to the filter is inaudible, not silently bypassed back to the
  direct mix. Two new approximations, same tier as the combined-waveform/
  ADSR-delay-bug ones above: the cutoff-to-frequency mapping (real hardware
  is nonlinear and differs materially between 6581/8580, driven by
  chip-to-chip DAC nonlinearity) and the resonance-to-Q mapping (also
  chip-inconsistent on real hardware, and capable of true self-oscillation)
  both use documented linear approximations spanning the commonly-cited
  ranges, not per-chip-measured curves. `$17` bit 3 (EXT IN, external-audio
  filter routing) is stored but never contributes anything -- no external
  audio input exists anywhere in this project, same "no real source to
  approximate" stance as `c64::cia::Cia`'s own unwired CNT/SP pins.
  Verified numerically stable (no NaN/out-of-range output) via a
  throwaway, uncommitted real-ROM-boot probe at maximum resonance with all
  3 filter taps selected simultaneously.
- **`via6522`** (`crates/chips/via-6522`) — MOS 6522 VIA, first piece of
  the "IEC bus and 1541 drive" parity-extension track (see `TODO.md`).
  Same host-agnostic-chip convention as `ay-3-8910`/`sid`: no dependency
  on `machine`, a CPU crate, or any host wiring -- nothing in this
  codebase calls it yet (no 1541/IEC machine exists this pass), same
  "built and tested standalone, wired into a machine in a later pass"
  precedent `sid`'s own phase 1/phase 2 split already established. Full
  16-register map (`$0`-`$F`): both parallel ports w/ DDR and real
  driven-vs-floating semantics (`effective_port_a`/`effective_port_b`,
  same convention `c64::bus::C64Bus::port01` already uses for the 6510's
  own port); Timer 1 (continuous or one-shot, plus an optional dedicated
  `PB7` square-wave output that overrides that Port B bit entirely,
  independent of `DDRB`/`ORB`, while active); Timer 2 (one-shot timed-
  interrupt mode, or pulse-counting mode counting real negative edges on
  `PB6` via `pulse_pb6`); the `CA1`/`CA2`/`CB1`/`CB2` peripheral-control
  modes (edge-select interrupt inputs with independent-vs-non-independent
  IFR-clear-on-port-access semantics, handshake output, pulse output, and
  manual output); and the IER's set/clear-mask write protocol (mirroring
  `c64::cia::Cia`'s own `$0D` ICR protocol, plus its own documented
  "reading IER always shows bit 7 set" quirk). Two real, distinctive
  hardware facts implemented and directly regression-tested, not glossed
  over: **the 6522 has no start/stop control bit for either timer at
  all** (unlike CIA's `CRA`/`CRB`) -- both are always counting down every
  real phi2 cycle from construction, "starting" a timer is simply writing
  its high-order counter register (`Via::tick` therefore unconditionally
  decrements Timer 1 every cycle in both modes, a deliberate contrast
  with `Cia::tick`'s own running-flag gate); and register `$01` ("ORA with
  handshake" -- clears `CA1`'s flag and can trigger `CA2`'s handshake/
  pulse output on access) vs. `$0F` (the exact same Port A data, zero side
  effects) are genuinely different addresses, not two names for one
  behavior -- Port B has no such alias at all, a real, asymmetric
  hardware fact. **Explicitly simplified, documented scope cut**: the
  shift register's real 8 modes collapse into "disabled", "clocked by an
  already-ticked internal source (T2 or phi2)" (faithfully modeled), and
  "clocked by an external `CB1` pulse" (left inert, no real external
  serial-data device wired to anything yet) -- same "no real source to
  approximate" stance `c64::cia::Cia`'s own unwired CNT/SP pins and
  `sid::Sid`'s unwired EXT IN already take; only the shift's completion
  timing is modeled, not real bit-by-bit `CB2` data movement. 17 tests
  cover every mode above. Now wired into a real 1541 machine and a real
  IEC bus to `c64::C64` -- see `c1541`'s own bullet below.
- **`c1541` 1570/1571 (2026-10-12)**: `c1541::DriveModel` (`C1541`/`C1570`/`C1571`,
  `C1541::with_model`; `C1541::new` is still the 16K 1541) adds the 32K-ROM family. Memory map from
  VICE `memiec.c`: 2K RAM mirrored over `$0000-$0FFF`, VIA1 `$1800`, VIA2 `$1C00`, WD1770 window
  `$2000-$2FFF` (not wired yet, reads `$FF`), a **6526 CIA at `$4000-$7FFF`** (`cia6526`, ticked each
  step, IRQ to the CPU), ROM `$8000-$FFFF`. VIA1 Port A (VICE `via1d1541.c` 1571 branches): bit 7 reads
  0 while a disk byte waits (`BIT $180F / BMI` loops; `byte_ready_latch`, set by the mechanism, taken by
  a read **or write** of VIA2 Port A -- the write matters: without it the 1571 write loop outruns the
  head), bit 0 the track-0 sensor, bit 1 fast-serial direction (stored), bit 2 head select, bit 5 the
  **2 MHz clock = "1571 mode"** (`C1541::clock_hz` is dynamic; the disk bit clock follows it; the host
  must scale its cycle account by `drive.clock_hz()`). A 1571 on a C64 stays in 1541 mode (single sided,
  `66 ILLEGAL TRACK` for tracks over 35) until `OPEN 15,8,15,"U0>M1"` -- found by tracing the DOS
  (`$2750`: `LDA $180F / AND #$20`), not assumed. `disk::Medium` (`D64`/`D71`) is the media abstraction:
  `gcr::SectorSource` builds tracks, `side` + `logical_track()` (physical track + 35 on side 2, which is
  how a `.d71` and the DOS number them) pick the data, `write_sector` commits the write head's output into
  either image. `d71::D71::blank_formatted` makes an empty double-sided disk. Plugin: `drive_type`
  (`1541|1570|1571`) + `rom_1541` (the matching ROM: 16K or 32K from `roms/c64/VC1570`/`VC1571`),
  `.d71` accepted on a 1570/1571 only. Proof (`crates/machines/c64/tests/real_1541.rs`, `--ignored`,
  release): the real 1571 and 1570 DOS list/load a `.d64` over IEC, load a file whose first sector is on
  side 2 of a `.d71` after `U0>M1`, and SAVE onto a blank `.d71` through the write head; live through the
  binary (`--true-drive --drive-type 1571 --rom-1541 roms/c64/VC1571`) the directory loads. Correction to
  earlier notes: the 1570/1571/1581 CPUs are plain NMOS 6502s (VICE runs only the FD2000/4000/CMD HD with
  a 65C02) -- no 65C02 core is needed for the C128 drives. Not done: the WD1770 (MFM) side of the 1571,
  fast serial/burst (the C128 exists now but burst mode is not modelled, see `c128`'s notes), the 1571's CIA SDR wiring.
- **1581 and the WD1770 (2026-10-12)**: `wd1770::Wd1770<F: Floppy>` is the controller as a command
  state machine against the raw byte stream of a rotating track (one byte per 32 us, `Floppy::read/write`
  with bit 8 = sync byte, index pulses, TR00, step pulses): restore/seek/step with verify and settle
  delay, read/write sector (multi-sector, `A1 A1 A1 FB/F8`), read address, read track, write track
  (`F5`/`F6`/`F7` translation and CRC generation), force interrupt, DRQ/lost-data/INTRQ. Timing is
  scaled by the CPU MHz; CRC-CCITT presets `0xB230` (after the ID mark) and `0xE295` (after the data
  mark). Behaviour cross-checked against VICE's `wd1770.c` (reference only). `c1541::C1581`
  (`drive1581.rs`) is the drive: 6502 at 2 MHz, 8K RAM `$0000`, CIA `$4000`, WD1770 `$6000`, 32K ROM
  `$8000` (VICE `memiec.c`); the CIA's Port A is head select (bit 0), motor (bit 2, 0 = on), device
  number (bits 4:3) and disk-change (bit 7), Port B the serial bus like the 1541's VIA1 PB plus
  write-protect (bit 6) and fast direction (bit 5). **ATN must also reach the CIA's FLAG pin** -- without
  it the real DOS idles forever (found by watching its program counter sit in one loop). `floppy81.rs`
  turns a `.d81` into the MFM track the head sees (80 cylinders, 10 x 512-byte sectors per side, IDs
  `C H R N` with `H` and the half of the image the data comes from both given by CIA Port A bit 0;
  one physical sector = two DOS sectors at `(cyl*2+half)*20 + 2*(R-1)`) and decodes written tracks
  back into the image (ID + data CRCs checked), so a **format (Write Track) works too**. `c1541::IecDrive`
  is the trait the serial-bus wiring needs from any drive. Plugin: `drive_type` now also `1581`
  (`c64-machine-plugin/src/drive.rs`: `DriveKind`, `Disk::{Gcr, D81}`, a `.d81` into a GCR drive or a
  `.d64`/`.d71` into the 1581 is a clear error). Proof (`real_1541.rs`, `--ignored`, release): the real
  1581 DOS lists a `.d81` and loads a 3000-byte file over IEC, SAVEs a file through the WD1770 (read back
  with the `d81` crate) and **formats a blank all-zero image with `N:FRESH,81`** (160 Write Track
  revolutions) after which it lists as `FRESH`; live through the binary
  (`--true-drive --drive-type 1581 --rom-1541 roms/c64/VC1581 --disk x.d81`). Not done: 1581 fast serial
  (C128 burst), Read Track is implemented but nothing drives it, disk-image weak/odd sector layouts, the
  1571's WD1770 (MFM) mode, `.d81` write-back through `--disk-save` is wired but only the 1541 path has
  a live check.
- **`vdc8563`** (`crates/chips/vdc-8563`, 2026-10-12) -- the C128's 80-column video chip. Same
  host-agnostic-chip convention as `vic-ii`/`sid`: no `machine`/CPU dependency. The CPU sees two ports
  (index/status, data); everything else lives in the chip's own 16K or 64K (`Vdc::new(ram64)`) of video RAM,
  reached through register 31 (a window onto the byte at the R18/R19 update address, auto-incrementing).
  Register 28 bit 4 chooses 64K addressing, folded onto 16K chips as VICE does. Writing R30 starts a block
  **fill** (R31 value) or, with R24 bit 7 set, a **copy** from the R32/R33 block start, instantly. The status
  byte reads ready (bit 7, always -- block operations finish at once), vertical blank (bit 5, the top/
  bottom border part of the frame) and the revision in the low bits (2 = 8568, as in the 128DCR).
  `render()` decodes the registers and video RAM on demand: text with the per-character attributes (alternate
  set, reverse, underline, blink, 16 RGBI colours), the hardware cursor, reverse-screen (R24 bit 6), slow/
  fast blink (R24 bit 5) and bitmap mode (R25 bit 7). `tick` only advances the 60 Hz frame counter (blink
  phase, vblank flag). Register read-back masks (unused bits read 1) are VICE's `regmask`. Behaviour from the
  8563 data sheet and VICE's `vdc-mem.c`/`vdc-draw.c` (reference only, `CREDITS.md`). Not done: light pen,
  interlace, and anything finer than a whole frame (a mid-frame register change shows from the next
  render).
- **`c128`** (`crates/machines/c128`, 2026-10-12) -- the Commodore 128 as one `Machine`: **an 8502 and a
  Z80 on one bus** (`C128Bus`; `mmu.cpu_is_8502()` picks which of them `C128::step` runs), the VIC-IIe
  (`vic-ii`, plus the K0-K2 keyboard lines at `$D02F` and the 2 MHz bit at `$D030`), the SID, both CIAs
  (`cia6526`) and the VDC. Everything ticks in 1 MHz bus cycles whichever CPU runs; at 2 MHz (`$D030` bit
  0) the 8502 and the Z80 deliver two CPU cycles per bus cycle (`clock_remainder`).
  *MMU* (`mmu.rs`, 8722): CR (RAM bank, kernal/function-ROM/RAM per region, I/O off), PCRA-D pre-
  configuration registers (a write to `$FF01-$FF04` loads CR from them), MCR (bit 0 CPU select, bit 6 C64
  mode, bits 5:4 read the GAME/EXROM lines as high, bit 7 reads the 40/80 key), RCR (VIC RAM bank, common
  RAM share size/top/bottom), the P0/P1 zero-page/stack relocation pointers with latched high bytes, all
  at both `$D500-$D50A` and (first five) `$FF00-$FF04`. Common RAM is always bank 0. *Boot*: the machine
  powers up on the **Z80**, whose BIOS (bytes `0x1000-0x1FFF` of the 16K kernal image, visible at `$0000`
  while `CR & 0xC0 == 0`) programs the MMU and finally writes MCR bit 0 -- that write hands the machine to
  the 8502 at its reset vector (no special-casing of the hand-off: it is the real mechanism; the BIOS reads
  MCR bits 5:4 (GAME/EXROM, read as high) and bit 6 to decide between C128 and C64 mode). *C128 mode map*: `read_128` resolves each address through
  `Mmu::source_for` (RAM, BASIC lo/hi, editor, kernal, char ROM with I/O off), writes always reach RAM,
  `$D000-$DFFF` is the I/O block while CR bit 0 is clear (colour RAM has two 1K banks selected by 8502 port
  bits 0/1 for CPU and VIC, bit 2 shows the character ROM to the VIC). The Z80's `IN`/`OUT` reach the same
  chips (`z80_in`/`z80_out`, the port's high byte is the chip). *C64 mode* (`MMU MCR bit 6`, entered by
  BASIC's `GO64` or the 64 key): `read_64`/`write_64` implement the C64's own PLA map from the 8502 port
  bits (LORAM/HIRAM/CHAREN), the C64 BASIC/kernal ROMs, the lower half of the character ROM, and the VIC
  character-ROM shadow only in VIC banks 0 and 2 (`sync_vic`); the MMU keeps the C128 hardware
  underneath (the VDC and the MMU registers vanish from view as on the real machine). *Video*: `render_frame`
  shows the VDC picture (line-doubled to 704x528 with a border, so the window is 4:3 like the CPC's) when the
  40/80 DISPLAY key is held at reset (`set_80_columns`, plugin option `display = 80`), otherwise the VIC-IIe
  with the usual 32 px border. *Keyboard* (`keyboard.rs`): the C64 matrix plus three more columns
  selected by `$D02F` (keypad, ESC/TAB/ALT/HELP/LINE FEED/NO SCROLL, dedicated cursor block), CAPS LOCK
  sensed through 8502 port bit 6, the 40/80 key through the MMU. Joystick ports as on the C64
  (`press_joystick`). *Drive*: `iec_output`/`set_iec_input` give `drive-link` the CIA2 serial lines.
  Save states (`save_state.rs`: both CPUs, 128K RAM + colour RAM, MMU, VIC, CIAs, SID, VDC, keyboard,
  clock remainder; ROMs external). **Validated against the real ROMs** (`roms/c128/`, ROM-gated ignored
  tests, `--release`): `tests/boot.rs` -- the Z80 BIOS hands over to the 8502 and BASIC 7.0 reaches
  `READY.` with `122365 BYTES FREE`, typed keys reach BASIC through the matrix, the 80-column VDC screen
  shows the same banner, `GO64` + `Y` boots the C64 kernal to `64K RAM SYSTEM  38911 BASIC BYTES FREE`, a
  save state resumes bit-identically; `tests/drive.rs` -- the real C128 kernal lists a directory from a real
  **1571** (`.d64`) and a real **1581** (`.d81`) DOS over the IEC bus, and the C64-mode kernal loads
  `$` from a real 1541 DOS. Real bugs found on the way: the Z80 BIOS entered C64 mode because MCR bits 5:4 read
  0 (the GAME/EXROM lines read high), BIOS visibility is `CR & 0xC0 == 0` (not tied to the I/O bit), and a
  VDC R31 write itself stores/advances the update address.
  **`vic_accuracy = line | cycle` (2026-10-12)**: plugin option / `--vic-accuracy` / `C128::set_vic_accuracy`
  (`vic_ii::VicAccuracy`, moved out of `c64` so both machines share it). `cycle` enables the same VIC-II cycle
  engine the C64 uses. While the **8502 runs at 1 MHz** the CPU steps with `C128Bus::in_cpu` set, so each bus cycle
  (`read`, `write`, the core's `dummy_cycle`) calls `cpu_cycle`: one `tick_cycle` (VIC engine, both CIAs, SID, VDC
  advance one clock) and, for a *read* while `Vic::ba_low()`, further cycles until BA rises (`ba_stalled_cycles`).
  The **Z80 and the 8502 in 2 MHz mode** run the whole instruction and then tick the machine by the bus cycles it
  took (`step_cycle_mode`): the engine keeps its exact raster/fetch schedule, but those CPUs are not stalled (a
  2 MHz C128 blanks the VIC-II anyway, and the Z80 has no per-cycle bus hook). `VicView` gives the engine the
  VIC's RAM bank (MMU RCR), the shown colour-RAM bank and the char-ROM shadow (`vic_chargen_shadow`, same rule as
  `sync_vic`: C64 mode banks 0/2, C128 mode 8502 port bit 2); sprite collisions stay the per-line scan. `render_frame`
  returns the engine's 384x264 picture in cycle mode (80-column VDC output is unaffected). PAL only (refused on
  NTSC); a state only loads onto a machine in the same mode. Also fixed: the VIC's char-ROM shadow flag was never
  synced at construction, only when the map changed. Real-KERNAL checks (`tests/cycle.rs`, `--ignored`, release):
  BASIC 7 boots to the identical screen as line mode with BA stalls counted, `GO64` and the Commodore-key C64-mode
  boot work, a save state resumes bit-exactly; `src/cycle_tests.rs` has the exact stall counts (25 x 43 per frame on
  bad lines, 21 x 5 for a lone sprite), Z80/2 MHz lockstep, NTSC refusal and save-state tests. Accuracy: the same
  instruction/bus-cycle granularity as the C64's cycle mode, nothing finer.
  **CP/M Plus boots (2026-10-12)**: `software/c128/c128-cpm.system.622-3282252-1.d64` (a CBM-format disk whose
  T1/S0 is a `CBM` boot sector) boots to CP/M 3.0's `A>` prompt on the VDC: `--display 80 --true-drive
  --disk <that .d64>`, then `BOOT` at the BASIC prompt. The boot sector (disassembled from the disk and the
  kernal's Z80 BIOS in `kernal-318020-05` bytes `0x1000-0x1FFF`) sets `CR = $3E`, plants `JP $0008` at `$FFEE`
  and jumps to the CPU-switch stub at `$FFD0`, which the BIOS copied there from ROM `$0EE5`: its 8502 half writes
  MCR bit 0 = 0 and its Z80 half (`$FFE0`) writes bit 0 = 1, **so each processor resumes at the instruction
  after its own last switch**. The Z80 BIOS clears `$3000-$FFFF`, installs the 8502 service code at `$3000`, and
  from then on every CP/M disk call is a Z80 -> 8502 -> Z80 round trip through the real kernal and the IEC
  drive. **Real bug found and fixed**: `C128::after_instruction` reset the 8502 on *every* Z80 -> 8502 switch
  (added for the Commodore-key C64-mode boot), so the 8502 restarted at its reset vector, ran the kernal cold
  start and CP/M never got past `$0199` (the BIOS's 52K `LDIR`); it now resets it only on the first hand-over
  after power-on/`reset` (`C128::cpu_started`, in the save state) and afterwards just resumes it. Test:
  `tests/drive.rs::the_cpm_plus_system_disk_boots_to_the_a_prompt` (real ROMs + the committed disk, 120 emulated
  seconds, asserts `CP/M 3.0`, `Commodore 128` and `A>` on the VDC screen read as ASCII; `probe_disk` gained
  `TRACE=1` and `ASCII=1`). Not done for CP/M: the CP/M file system itself is MFM on a double-sided disk, which
  needs the 1571's WD1770 side (below), so only the CBM-format boot disk is exercised; no CP/M program has been
  run past the prompt.
  *Not done (C128 closed with these as the honest gaps)*: **burst/fast serial** (the
  kernal falls back to the ordinary serial protocol with a 1571/1581, which works), the 1571's MFM mode (CP/M
  disks), the external-function ROM slots (read as open bus),
  REU/cartridges/printer/datasette (the C64's expansion devices are not wired to the C128), and an
  automatic `LOAD` trap -- a disk needs `--true-drive` (see `drive-link` below), unlike the C64.
- **`drive-link`** (`crates/machines/drive-link`, extracted 2026-10-12 from `c64-machine-plugin`) -- the
  opt-in "true drive" as a library: `DriveLink` owns a `c1541::C1541` (1541/1570/1571) or `C1581`, an
  `IecBus` and the swap-gap/pending-disk logic, and steps the drive alongside any computer that implements
  `IecHost` (`iec_output`, `set_iec_input`, `clock_hz` -- each plugin wraps its own machine in a small
  adapter, since the trait cannot be implemented for a foreign type). `after_host_step(host, cycles)`
  replaces the old `after_c64_step`; `pack_drive_state`/`unpack_drive_state` are the `TDRV` save-state
  container. The C64 and C128 plugins share it, so a drive fix (see `c1541`'s notes) applies to both.
  `c128-machine-plugin` (`crates/plugins`) is modelled on the C64 plugin: five File options (`rom-basic`
  32K, `rom-kernal` 16K, `rom-char` 8K, `rom-c64-basic`/`rom-c64-kernal` 8K), `display = 40|80`, `true_drive`/
  `drive_type` (default 1571)/`rom_1541`, key names from `c128_key_table` (the C64 names plus `Tab`, `Esc`,
  `Help`, `LineFeed`, `Alt`, `NoScroll`, `CapsLock`, `Display4080`, `Numpad*`, `ArrowUp/Down/Left/Right` on the
  dedicated cursor keys and `CursorRight`/`CursorDown` for the C64 pair), joystick `Joy1*`/`Joy2*`, program
  poke at `$1C01` by default; a disk without `--true-drive` is a clear error (the message says how to get
  one). Verified live through the binary: the 40- and 80-column boot screens, `--true-drive --disk x.d64`,
  save/load state with and without a drive (a mismatch is refused).
  **`--run`, `--mode` and BASIC program injection (2026-10-12)**: option `mode = c128|c64`
  (`C128::boot_into_c64_mode`: the Commodore key is held for 1.5M bus cycles from power-on, which the BIOS
  and kernal read -- the machine then boots the C64 ROMs; found along the way that the 8502 was reset only
  once at construction, so a Z80 hand-off into C64 mode started it at PC 0: `C128::step` now resets the
  8502 again at the moment the Z80 releases the machine, so it reads the reset vector of the map it
  actually sees). **Why programs are injected late**: BASIC's cold start rewrites the head of program memory
  (a program poked at power-on has its first link zeroed -- measured), so `AttachMedia{Program}` at
  `$1C01` (C128) / `$0801` (C64) is held in `pending_program` and poked by `InstanceRecord::step` the first
  time the 8502 idles in the editor/KERNAL "wait for a key" loop (`$C25E-$C262`, sampled at the prompt on the
  real ROMs; `$E5CD-$E5D4` in C64 mode) and the MMU mode matches; any other address (machine code) is poked
  at once. `finish_basic_program_load` then relinks the line chain (as BASIC's LINKPRG does) and tells BASIC
  where the text ends: C128 BASIC 7.0 keeps its end-of-text pointer at **`$1210/$1211`** (found by diffing
  low memory before and after typing a line; its variables live in bank 1, so the C64's VARTAB/ARYTAB/STREND
  are not it), C64 mode writes VARTAB/ARYTAB/STREND (`$2D-$32`) like the C64 plugin. The host sends a
  `--program` through that path only with `--run` (`HostedMachine::attach_program`; without `--run` it is
  still poked and jumped to as before): **this also fixed the plain C64, whose `--run --program x.prg` had
  never worked** (the program was poked at power-on and wiped, or jumped to as code; the C64 plugin now has
  the same `pending_program`, `$E5CD` idle test, relink only with `run`). Host profiles
  (`resolve_auto_run_profile`, machine id `c128`): disk in C128 mode -> `AutoRunProfile::C128Disk` types
  `RUN"*"` (BASIC 7 loads the first program and runs it); `.prg` -> `C128Run` types `RUN`; `mode = c64` with a
  disk -> `C128C64Disk` types `LOAD"*",8,1` and, because a real-drive load takes tens of emulated seconds,
  waits until the C64 idle loop is back (at least 3 s, then PC in `$E5CD-$E5D4`) before typing `RUN`; `.prg`
  in C64 mode -> the existing `C64Run`. **Verified live through the GUI under Xvfb** (screen RAM over the
  debugger's `/api/memory-bytes`): `--run --program` of `The Fighter.prg` and `Baseball Trivia Quiz.prg`
  ($1C01) ran their programs, a hand-made `$0801` prg ran in C64 mode and on the plain C64,
  `--run --true-drive --rom-1541 roms/c64/VC1571 --disk "Mikes Maze 128.d64"` loaded and ran, and
  `--mode c64 --run --true-drive ... --disk "Donkey Kong (Ocean).D64"` typed `LOAD"*",8,1`, waited out the
  ~90 s load and then ran the game. Tests: `boot.rs` (`holding_the_commodore_key_at_power_on_boots_the_c64`,
  `a_basic_program_poked_at_the_prompt_runs`), plugin tests for the held-back program and the relink, host
  tests for the profile choice and the key sequences, and an exploration harness
  (`tests/drive.rs::probe_disk`, env `DISK`/`DRIVE`/`C64`/`COLS80`/`TYPE`/`SECONDS`). `software/c128/` holds
  TOSEC `.zip`s (`.d64`/`.d81`/`.prg`/`.lnx`/`.tap`); the host opens them directly (see "Zip and URL inputs" under `retroclawd`). A true-drive load is slow at the ordinary serial
  speed (~360 bytes/s, Tetris128 takes minutes of emulated time); the `fast_forward` hotkey helps in the GUI.
  Disks marked `[80 Column Mode]` need `--display 80`.
- **`d64`** (`crates/formats/d64`) — parses the standard Commodore `.d64`
  disk image format, first piece of the "drives and baseline media"
  track's `.d64` item (see `TODO.md`). Same "format crate only parses
  bytes into a plain struct" precedent as `z80-snapshot`/`zx81-snapshot`/
  `zx-tape`/`dsk` -- no dependency on `machine`, a CPU crate, or `c1541`;
  same "parse the container" vs. "protocol the real controller speaks"
  split `dsk`/`nec765` already have for CPC/+3 -- wiring an actual `D64`
  into `c1541::C1541`'s VIA2 (the real 1541 disk mechanism) is now done,
  see `c1541`'s own bullet below. Every geometry and
  byte-offset constant was confirmed directly against VICE's own real
  source this session (several attempted fetches against the classic
  Peter Schepers d64.html spec page and other reference sites were
  blocked by this sandbox's network egress policy, same experience prior
  sessions already had) -- `diskimage/diskimage.c`'s own `sector_map_d64`
  table (real hardware fact: a 1541 writes at constant linear bit density
  on a disk spinning at constant angular velocity, so it packs its 35/40
  tracks into 4 speed zones with different sector counts -- 21/19/18/17 --
  rather than a uniform count throughout), `diskconstants.h`'s own BAM/
  directory-location `#define`s, and `vdrive/vdrive-dir.h`'s own
  directory-slot offsets. That last one caught a real, easy-to-get-wrong
  subtlety before it became a bug: each 32-byte directory slot's file-type
  byte sits at slot-relative offset `2`, not the more "obvious" `0` --
  only a directory sector's very first slot's bytes 0-1 double as that
  whole sector's own chain link to the next directory sector, but every
  slot (including the other 7 with no real link data of their own) is
  still treated as a uniform 32-byte window starting 2 bytes before its
  own real per-entry data, so every slot's fields land at consistent
  offsets regardless of position -- a dedicated test locks in the `+2`
  offset specifically so a future refactor can't regress it back to `+0`.
  Supports the 2 known-real size variants (35-track/40-track, each
  optionally with a per-sector real drive error-code byte channel
  appended) -- the rarer 42-track variant VICE's own source also defines
  isn't supported, no citable byte-layout source was found for it this
  session, so `parse` errors clearly on that size rather than guessing.
  `sector_error` exposes the real error-code channel but nothing acts on
  it yet -- no real GCR-level read attempt exists to produce or consult
  one, same "expose but don't act on yet" precedent `dsk`'s own weak-
  sector `copies` field originally established before `nec765::Fdc`
  existed to consume it. `read_sector`/`write_sector` (real track/sector
  addressing: 1-indexed tracks, 0-indexed sectors within a track, matching
  real DOS convention) and `directory_entries` (walks the real chain from
  the BAM's own stored link, with a visited-set guard against a corrupt/
  circular chain hanging the parser the way it would a real drive) round
  out the API; `to_bytes` round-trips exactly, verified by test.
- **`d71`/`d81`** (`crates/formats/d71`/`d81`) — sibling crates parsing
  Commodore `.d71` (1571, double-sided) and `.d81` (1581, uniform-sector)
  disk images, own crate per format rather than folding into `d64` (same
  "own crate per real file format" precedent `dsk` established for CPC,
  chosen deliberately so the already-shipped, tested `d64` crate's public
  API needed zero changes) -- `DirectoryEntry`/chain-walking/`find_program`
  logic is duplicated across all three rather than shared, same "own shape
  per format" precedent `sna-snapshot`'s `PagingState` already set. Every
  constant confirmed against VICE's own real source this session
  (`diskconstants.h`'s `NUM_TRACKS_1571`/`BAM_TRACK_1571`/`BAM_NAME_1571`/
  `NUM_TRACKS_1581`/`NUM_SECTORS_1581`/`BAM_NAME_1581`, `vdrive-bam.c`'s own
  BAM-formatting code) -- see `CREDITS.md`. `.d71` side 1 (tracks 1-35) is
  byte-identical to a `.d64`'s own BAM/directory layout; side 2 (tracks
  36-70) repeats the same 4-zone sector table track-for-track, with its own
  second BAM sector at track 53 (`BAM_TRACK_1571 + num_tracks/2`,
  confirmed). `.d81` is a real, physically different format -- true MFM at
  a uniform 40 sectors/track, no speed zones at all (confirmed by the
  absence of any `sector_map_d81` table in VICE's source, unlike `d64`/
  `d71`'s shared `sector_map_d64`) -- with its own header sector (track
  40/sector 0, name/ID at offsets 4/22, genuinely different from `d64`/
  `d71`'s track-18 BAM) and two separate BAM sectors (track 40/sectors
  1-2); its DOS-type offset (25) was derived the same padding-consistency
  way `d64`'s own was, cross-checked against VICE's own disk-formatting
  code literally writing `"3D"` there. **Deliberately deferred, both
  formats**: the second side's/BAM sectors' own per-track free-space
  bitmap isn't decoded -- confirmed which *sector* holds it, not the exact
  *starting offset* of the per-track group table within it, and this
  session couldn't independently re-verify that arithmetic against a
  second source; undecoded since reading a disk (directory listing, file
  extraction) never consults the BAM's own allocation bitmap on real
  hardware either, same as `d64` itself already established. Wired into
  `c64::bus::C64Bus` via a new `AttachedDisk` enum (`D64`/`D71`/`D81`) and
  a small private `CbmDisk` trait (`find_program_bytes`) -- `try_fast_
  kernal_load_trap` (see `c64`'s own bullet above) needed zero format-
  specific branching to support them, since `.d71`/`.d81` share the exact
  same real CBM DOS directory/file chain format `.d64` uses, just at a
  different geometry. `c64-machine-plugin`'s `MediaKind::Disk` handler now
  extension-sniffs (`.d64`/`.d71`/`.d81`, defaulting to `.d64` when no
  filename is given) instead of unconditionally parsing as `.d64`;
  `--run`'s `C64Load` auto-run profile now also matches `.d71`/`.d81`
  (same `LOAD"*",8,1` autotype sequence, since the fast-loader trap already
  made disk format irrelevant past "a disk is attached").
- **`c1541`** (`crates/machines/1541`) — the Commodore 1541 floppy drive
  modeled as its own real computer (same "own machine, wired to another
  over a real bus" shape this project's `Machine` trait already
  generalizes), completing the "IEC bus and 1541 drive" parity-extension
  track's first item (see `via6522`'s own bullet above and `TODO.md`).
  `mos6502::Cpu` + 2 `via6522::Via` instances (VIA1 = serial/IEC bus +
  device-address jumpers, VIA2 = the real disk mechanism, see this
  crate's own `gcr`/`disk` bullet below) + 2K RAM (`$0000`-`$07FF`) + 16K
  ROM (`$C000`-`$FFFF`). VIA base addresses (`$1800`/`$1C00`, each mirrored
  across a 1KB window), VIA1's exact Port B bit layout (bit 7 ATN IN,
  bits 6:5 device jumpers, bit 4 ATN ACK, bit 3 CLOCK OUT, bit 2 CLOCK IN,
  bit 1 DATA OUT, bit 0 DATA IN), and its real DDR masks (`0xe5`
  input/`0x1a` output) were all confirmed directly against VICE's own real
  source this session (`drive/drivemem.c`/`drive/iec/via1d1541.c`), not
  recalled -- see `CREDITS.md`. RAM/ROM ranges are the commonly-cited real
  1541 memory map, not independently re-verified against source this
  session, same "commonly documented" tier as e.g. ZX Spectrum 128K's
  `0x7ffd` decode. No real 1541 ROM is committed under `roms/` (this
  project has no legally-suppliable copy, unlike the C64's own
  user-supplied ROMs) -- every test here uses synthetic ROM content.
  `c1541::IecBus` (the 3-line, wired-AND shared bus type) lives in this
  crate rather than `c64` or `machine` -- the disk drive is what gives the
  bus its reason to exist; `c64` reaches it only via a
  `[dev-dependencies]` entry for its own integration test
  (`crates/machines/c64/tests/iec_1541.rs`), never a real production
  dependency in either direction. Modeled with exactly the 2 real
  participants this project has today (real IEC hardware supports up to
  5) -- generalizing further is deferred until a second real device
  exists to prove that shape against, same "don't invent a shared shape
  from one data point" stance this project already takes elsewhere (e.g.
  `zx-tape`'s own undecided `Loader` trait). On the C64 side, the one real
  code change is `c64::bus::C64Bus::cia2_port_a_read`: CIA2 Port A bits
  6/7 (`CLOCK IN`/`DATA IN`), confirmed against VICE's own `c64cia2.c`
  (bit layout comment + `store_ciapa`'s `~byte` inversion, confirming the
  real inverting-driver polarity: writing `1` to an `*_OUT` bit asserts/
  pulls that line low, an `*_IN` bit reads the real wired-AND bus level
  directly), now reflect the real external bus level when configured as
  input instead of just floating high -- same "host wires the
  chip-specific bits" split `keyboard_port_b` already established for
  CIA1's own Port B. `C64` has no `ATN IN` wiring at all, a real hardware
  fact (the C64 is always the bus's `ATN` master, no stock peripheral
  ever drives it back), not an oversight.
  `crates/machines/c64/tests/iec_1541.rs` proves 4 real protocol
  properties end to end, using the exact real DDR values real firmware
  uses (`$3F` CIA2 DDRA, `$1A` VIA1 DDRB): idle-bus-reads-high, `ATN`
  reaching the drive as both a Port B level and a real `CA1` interrupt
  edge, `DATA OUT` reaching the C64 as `DATA IN` (and releasing
  correctly), and correct wired-AND behavior when both sides assert the
  same line at once. A real bug was found and fixed getting the `CA1`
  test to pass: `via6522::Via::new()`'s bare `Default` derive leaves its
  internal `CA1`-edge-tracking state at `false` ("pin already low"),
  mismatching this crate's own assumed idle-high baseline for `ATN` --
  fixed with an explicit `via1.set_ca1(true)` seed call in
  `C1541Bus::new`, the correct fix living in the *host* (which knows what
  initial bus state it assumes) rather than `via6522` itself (a generic
  chip crate has no way to know any particular host's initial wiring
  assumption). Explicitly out of scope for this pass: no `retroclawd`
  CLI/GUI integration (no `--machine 1541`, no dual-machine stepping
  loop, no `--rom-1541` flag) -- see `TODO.md`.

  **GCR disk mechanism wired into VIA2** (`src/gcr.rs`/`src/disk.rs`,
  same session): VIA2 now drives a real `disk::DiskMechanism` -- head
  stepper motor (the real 2-bit gray-code sequence `[00,01,11,10]`, only
  an adjacent-position transition moves the head by one real half track,
  any other transition a real hardware no-op), spindle motor, density/
  speed-zone select (`ZONE_BITS_PER_SEC` = `{250000, 266667, 285714,
  307692}` bits/sec, VICE's own `rotation.c`), and the real bit-level
  SYNC-detection + byte-assembly hardware DOS firmware bit-bangs through.
  `gcr.rs` is a direct, faithful port of VICE's own real `gcr.c`
  (`GCR_conv_data`'s 4-bit-to-5-bit code table,
  `gcr_convert_4bytes_to_GCR`'s rolling-bit-accumulator technique,
  `gcr_convert_sector_to_GCR`'s field ordering and checksums) --
  cross-checked byte-for-byte against a from-scratch reimplementation
  over 20000 random inputs before trusting the literal port (see
  `CREDITS.md`); its `decode_nibble`/`decode_5bytes_to_4` are test-only,
  since real hardware never decodes GCR itself -- that's DOS firmware's
  own job, matching this crate's own read-only, bitstream-level scope.
  VIA2's real pin assignments were cross-checked against two independent
  emulator sources (VICE's `rotation.c` + MAME's `c1541.cpp`): Port A =
  raw GCR byte, Port B bits `STP1:STP0`/`MTR`/`DS1:DS0`, `CA1` =
  byte-ready (also wired directly to the 6502's own SO pin via the new
  `mos6502::Cpu::set_overflow` -- a real, distinct hardware fact,
  confirmed independently by both sources: the same pulse reaches both
  VIA2's ordinary `CA1` edge-interrupt input and the CPU's dedicated,
  unvectored SO pin), `CA2` = SOE (Sync Output Enable, gating whether a
  completed byte's `CA1`/SO pulse actually propagates -- handled entirely
  in `bus::C1541Bus::tick_disk`, keeping the chip-generic `disk` module
  unaware of it). A real, careful subtlety confirmed by manual bit-by-bit
  tracing: while a SYNC run's own ones-count is still below the 10-bit
  recognition threshold, ordinary byte framing keeps running
  undisturbed, so every real SYNC field produces exactly one "bogus"
  assembled byte from its own lead-in bits before framing forcibly
  re-aligns -- correct, expected real hardware behavior, not a bug (real
  DOS polling loops are written to tolerate it). Read-only (no `CB2`/`OE`
  write path); round-trips through save state via a `DiskMechanismState`
  mirror struct, same "external media stays external" shape `nec765::
  Fdc`/`zx_tape::TapePlayer` already established -- see `CREDITS.md` for
  the full citation and the egress-blocked/cross-referencing story.
