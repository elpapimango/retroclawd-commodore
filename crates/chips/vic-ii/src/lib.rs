//! MOS 6567/6569 VIC-II video chip. Raster IRQ, bad-line CPU cycle
//! stealing, and all 5 real display modes (text, multicolor text,
//! hi-res bitmap, multicolor bitmap, extended color mode text -- see
//! `VideoMode`) are implemented; only sprites remain deferred (see
//! `TODO.md`'s VIC-II coverage sequence, and the "bank 0 only" scope
//! note on `video_matrix_base`, since VIC bank selection is real CIA2
//! wiring, a separate, still-deferred roadmap item). No dependency on
//! `machine`, a CPU crate, or a host window/audio backend -- same
//! "host-agnostic chip" convention as `ay-3-8910`/`nec765`.
//!
//! Unlike those two, the VIC-II doesn't drive a protocol over `tick()`
//! calls -- for text mode there's no timing-sensitive internal state to
//! step, only registers a host bus writes and a screen a host renders
//! on demand from the current register/memory content, same "static,
//! on-demand decode of current memory state" shape `zx81`/`zx_spectrum`'s
//! own `render_frame` already use (see `CLAUDE.md`'s notes on both).

use wide::u8x32;

mod cycle;
pub use cycle::{CycleResult, VicMemory};

/// The 16-color C64 palette, commonly cited across independent emulator
/// implementations (this exact RGB set is widely known as the "Pepto"
/// palette). Real C64 output is composite/RF video through an analog
/// monitor -- there is no one "correct" digital RGB value, every emulator
/// picks a calibration; this is the commonly-cited approximation, not a
/// hardware-measured constant (same epistemic tier as this project's own
/// `SpeakerType` filter cutoffs -- see `CLAUDE.md`).
pub const PALETTE: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00), // 0 black
    (0xFF, 0xFF, 0xFF), // 1 white
    (0x88, 0x00, 0x00), // 2 red
    (0xAA, 0xFF, 0xEE), // 3 cyan
    (0xCC, 0x44, 0xCC), // 4 purple
    (0x00, 0xCC, 0x55), // 5 green
    (0x00, 0x00, 0xAA), // 6 blue
    (0xEE, 0xEE, 0x77), // 7 yellow
    (0xDD, 0x88, 0x55), // 8 orange
    (0x66, 0x44, 0x00), // 9 brown
    (0xFF, 0x77, 0x77), // 10 light red
    (0x33, 0x33, 0x33), // 11 dark grey
    (0x77, 0x77, 0x77), // 12 grey
    (0xAA, 0xFF, 0x66), // 13 light green
    (0x00, 0x88, 0xFF), // 14 light blue
    (0xBB, 0xBB, 0xBB), // 15 light grey
];

const REG_LIGHT_PEN_X: usize = 0x13;
const REG_LIGHT_PEN_Y: usize = 0x14;
const IRQ_LIGHT_PEN: u8 = 0x08;
const REG_CONTROL1: usize = 0x11;
const REG_RASTER: usize = 0x12;
const REG_CONTROL2: usize = 0x16;
const REG_MEMORY_PTRS: usize = 0x18;
const REG_IRQ_STATUS: usize = 0x19;
const REG_IRQ_ENABLE: usize = 0x1A;
const REG_SPRITE_X_MSB: usize = 0x10;
const REG_SPRITE_ENABLE: usize = 0x15;
const REG_SPRITE_Y_EXPAND: usize = 0x17;
const REG_SPRITE_PRIORITY: usize = 0x1B;
const REG_SPRITE_MULTICOLOR_ENABLE: usize = 0x1C;
const REG_SPRITE_X_EXPAND: usize = 0x1D;
const REG_BORDER_COLOR: usize = 0x20;
const REG_BACKGROUND_COLOR0: usize = 0x21;
const REG_BACKGROUND_COLOR1: usize = 0x22;
const REG_BACKGROUND_COLOR2: usize = 0x23;
const REG_BACKGROUND_COLOR3: usize = 0x24;
const REG_SPRITE_MULTICOLOR0: usize = 0x25;
const REG_SPRITE_MULTICOLOR1: usize = 0x26;
/// `$D027`-`$D02E`: sprite 0-7's own individual color register (add the
/// sprite index).
const REG_SPRITE_COLOR_BASE: usize = 0x27;
const REG_COUNT: usize = 0x40;

/// Eight identical hardware sprites -- `$D000`-`$D00F` hold each sprite's
/// X (even offsets, `2*i`) and Y (odd offsets, `2*i+1`) position as plain
/// 8/9-bit registers (bit `i` of `$D010` is sprite `i`'s X coordinate's
/// 9th bit); all read/write generically via the existing passthrough
/// register file, no special-casing needed. Register map, sprite
/// dimensions, the sprite-data-pointer formula, and every rendering rule
/// below (position offsets, priority ordering, multicolor pixel-pair
/// mapping, expansion) are confirmed directly against Christian Bauer's
/// VIC-II article (see `CREDITS.md`), the same source the display-mode
/// work above already cites.
const SPRITE_COUNT: usize = 8;
/// A sprite's native (non-expanded) pixel dimensions: 24 wide (3 data
/// bytes/row, 8 bits each), 21 tall (63 data bytes / 3 bytes-per-row).
const SPRITE_WIDTH: u32 = 24;
const SPRITE_HEIGHT: u32 = 21;
/// The 8 sprite data pointers live in the *video matrix*'s own last 8
/// bytes (`video_matrix_base + 0x3F8 + sprite index`), not a separate
/// register -- real hardware fact (the same p-access cycles that fetch
/// character/color data for the last row also fetch these).
const SPRITE_POINTER_OFFSET: usize = 0x3F8;
/// A sprite data pointer's value, times 64, gives that sprite's 63-byte
/// data block's address -- bank-0-relative, same scope boundary
/// `video_matrix_base`'s own doc comment already states.
const SPRITE_DATA_STRIDE: usize = 64;
/// The real, widely-cited screen-position offsets: raw `MxX`/`MxY`
/// register values of `24`/`50` put a sprite's top-left corner exactly
/// at the active display area's own top-left corner. Confirmed both via
/// Christian Bauer's article and directly present as a literal `24` in
/// VICE's own real `vicii-sprites.c` (`vicii_sprite_offset`); `50` cross-
/// referenced against the same widely-cited value independent sources
/// (including a c64-wiki summary reachable via `WebSearch` when the site
/// itself was blocked by this session's egress policy) all agree on --
/// see `CREDITS.md`.
const SPRITE_X_OFFSET: i32 = 24;
const SPRITE_Y_OFFSET: i32 = 50;
/// Pixels in a PAL raster line (`$000`-`$1F7`). The sprite X counter wraps
/// here, so a wide sprite near the right end of the line continues at the
/// left, and a 9-bit `MxX` of `504..=511` never matches the counter at all.
const RASTER_LINE_PIXELS: i32 = 504;
/// Cycles the VIC-II keeps the bus for one sprite's data (p- and s-accesses)
/// and the warning cycles BA is low beforehand -- measured against the cycle
/// engine (a lone sprite costs 3 + 2 on read-only code, three adjacent ones
/// 3 + 3 x 2).
const SPRITE_DMA_CYCLES: u32 = 2;
const SPRITE_BA_WARNING_CYCLES: u32 = 3;

/// The canvas column (`0..320` is the display window; anything else is the
/// border) where pixel `offset` of a sprite whose [`SpriteRenderInfo::x`] is
/// `info_x` lands, with the raster X wrap applied. `None` for a sprite whose
/// `MxX` is `504` or more: the X counter never reaches it, so it is never shown.
fn sprite_canvas_x(info_x: i32, offset: i32) -> Option<i32> {
    let raw = info_x + SPRITE_X_OFFSET;
    if raw >= RASTER_LINE_PIXELS {
        return None;
    }
    Some((raw + offset).rem_euclid(RASTER_LINE_PIXELS) - SPRITE_X_OFFSET)
}

/// `$D011` bit 5 (BMM, Bitmap Mode) and bit 6 (ECM, Extended Color Mode);
/// `$D016` bit 4 (MCM, MultiColor Mode). The three combine to select one
/// of the chip's real display modes -- see `VideoMode`. Bit positions
/// confirmed directly against Christian Bauer's widely-cited "The MOS
/// 6567/6569 video controller (VIC-II) and its application in the
/// Commodore 64" article (see `CREDITS.md`), the same de facto reference
/// virtually every independent VIC-II implementation (including VICE's
/// own) cites for exactly this bit layout.
const BMM_BIT: u8 = 0x20;
const ECM_BIT: u8 = 0x40;
const MCM_BIT: u8 = 0x10;

/// `$D019`/`$D01A` bit 0: raster-compare match -- the only interrupt
/// source this milestone implements. Bits 1/2 (sprite-background/
/// sprite-sprite collision) stay unimplemented for a different reason
/// than before sprites existed: real collision detection happens
/// continuously as the chip scans out each raster line, but this
/// crate's `render()` is a `&self`, once-per-*display*-frame snapshot
/// decode (see this module's own top doc comment) called from
/// `Machine::render_frame` -- itself `&self` on every machine here, by
/// trait design -- not from anything synchronized with `tick()`'s own
/// per-CPU-instruction cadence the way raster IRQ/bad-line detection
/// both are. Computing collision state only when a display frame
/// happens to be rendered would be real but arbitrarily stale relative
/// to actual gameplay timing, and could never drive a real `$D019`
/// IRQ (`c64::C64::step`, not `render()`, is what calls
/// `mos6502::Cpu::request_irq`) -- so rather than ship a
/// misleadingly-partial implementation, sprite collision stays fully
/// deferred (see `TODO.md`'s VIC-II coverage backlog) until a real
/// per-scanline sprite/collision pipeline exists. Bit 3 (light pen)
/// stays deferred too, no light pen modeled. Bit layout confirmed
/// directly against VICE's own real `vicii-irq.c` (see `CREDITS.md`),
/// not guessed.
const IRQ_RASTER: u8 = 0x01;
/// `$D019`/`$D01A` bit 1 (IMBC): sprite-background collision.
const IRQ_SPRITE_BACKGROUND: u8 = 0x02;
/// `$D019`/`$D01A` bit 2 (IMMC): sprite-sprite collision.
const IRQ_SPRITE_SPRITE: u8 = 0x04;
const REG_COLLISION_SPRITE_SPRITE: usize = 0x1E;
const REG_COLLISION_SPRITE_BACKGROUND: usize = 0x1F;
/// Raster line the first of the 200 display rows is drawn on (the
/// default 25-row, YSCROLL 3 window). `SPRITE_Y_OFFSET`'s 50 means a
/// sprite with `MxY == 50` shows its first row on this line, which is
/// how `scan_sprite_collisions` maps a raster line onto the canvas.
const FIRST_DISPLAY_LINE: i32 = 51;

/// `$D011` bit 4: DEN (Display ENable). Standard, widely-cited `$D011`
/// bit map; confirmed here specifically via VICE's own real `d011_store`
/// (see `CREDITS.md`) rather than assumed from memory.
const DEN_BIT: u8 = 0x10;

/// The real PAL VIC-II's DMA/bad-line window -- a bad line can only
/// occur on raster lines `$30`-`$f7` (48-247 decimal), the standard
/// 25-row display's real vertical extent. Confirmed directly against
/// VICE's own real `vicii-fetch.c` (see `CREDITS.md`).
const FIRST_DMA_LINE: u32 = 0x30;
const LAST_DMA_LINE: u32 = 0xf7;

/// Real, widely-documented bad-line cycle count: the VIC-II's 40
/// c-access cycles (cycles 15-54 of the 63-cycle line, one per text
/// column) during which it holds the bus and stalls the CPU. Confirmed
/// against both VICE's own real source and an independent VIC-II timing
/// reference (see `CREDITS.md`) -- real hardware's exact figure can vary
/// by a cycle or two depending on which phase of a multi-cycle
/// instruction the steal begins on, not modeled at this project's
/// per-instruction (not per-cycle) granularity.
const BAD_LINE_STOLEN_CYCLES: u32 = 40;

const TEXT_COLS: u32 = 40;
const TEXT_ROWS: u32 = 25;
const CHAR_PX: u32 = 8;

/// One video-matrix/color-RAM byte per character cell -- `40*25`.
const SCREEN_CELLS: usize = (TEXT_COLS * TEXT_ROWS) as usize;
/// One bitmap byte per character-cell scanline in either bitmap mode --
/// `40*25*8`, the real 8000-byte hi-res/multicolor bitmap size.
const BITMAP_BYTES: usize = (TEXT_COLS * TEXT_ROWS * CHAR_PX) as usize;

/// Real PAL VIC-II timing: 63 CPU cycles per raster line, 312 raster
/// lines per frame -- commonly documented, not verified against a
/// datasheet or real emulator source in this session (see `TODO.md`'s
/// own "no outbound network access" note elsewhere in this codebase).
const CYCLES_PER_LINE: u32 = 63;
const TOTAL_RASTER_LINES: u32 = 312;
/// 6567R8 (NTSC): 65 cycles per line, 263 lines (the 6567R56A's 64 cycles x
/// 262 lines is the older revision and is not modelled).
const NTSC_CYCLES_PER_LINE: u32 = 65;
const NTSC_TOTAL_RASTER_LINES: u32 = 263;

/// How faithfully the VIC-II is modelled (the `vic_accuracy` plugin option of the machines that use it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VicAccuracy {
    /// The raster and the bad-line steal advance once per CPU instruction
    /// (fast; the default).
    #[default]
    Line,
    /// The whole machine advances one clock per CPU bus cycle and the VIC-II
    /// runs its real per-cycle schedule, stalling the CPU through BA (PAL
    /// only).
    Cycle,
}

/// The chip's own register file (`$D000`-`$D03F` on real hardware, mapped
/// by the host machine's own bus at whatever address range it decodes --
/// see `c64::bus`). Text-mode M2 scope only reads two of these
/// (`$D020` border, `$D021` background 0); every other register is
/// stored and echoed back on read (real hardware convention for most
/// VIC-II registers: unimplemented bits commonly read back as `1`, not
/// modeled here -- plain storage is the pragmatic simplification so a
/// real KERNAL boot sequence's writes to sprite/raster/mode registers
/// this milestone doesn't act on yet don't crash or get silently
/// dropped).
// `registers` is a `Vec<u8>`, not `[u8; REG_COUNT]`, purely so this type
// can `#[derive(Serialize, Deserialize)]` directly: serde's blanket array
// impls stop at length 32 without pulling in an extra crate
// (`serde-big-array`), and `REG_COUNT` (64) is past that -- a `Vec` sized
// to `REG_COUNT` once in `Default` and never resized is functionally a
// fixed-size array for every purpose this chip actually has.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Vic {
    registers: Vec<u8>,

    /// Cycles elapsed within the current frame, wrapping at
    /// `CYCLES_PER_LINE * TOTAL_RASTER_LINES` -- advanced by `tick()`.
    /// Real KERNAL boot code busy-waits on the raster position (`$D011`
    /// bit 7 + `$D012`) during its own init sequence -- confirmed live,
    /// not guessed: a real KERNAL ROM's boot got stuck forever at a
    /// `LDA $D012 / BNE` loop before this field existed, since a frozen
    /// raster register never satisfies that loop's exit condition.
    frame_cycle: u32,

    /// `$D019`'s low nibble: latched interrupt-status bits, set by real
    /// chip events (only raster-compare match implemented so far, see
    /// `IRQ_RASTER`). Cleared by writing a `1` to the corresponding bit
    /// (real hardware's write-1-to-clear convention -- see `write`'s own
    /// doc comment). Bit 7 (the overall "/IRQ asserted" flag) is never
    /// stored here -- `read`/`irq_line_asserted` compute it live from
    /// `irq_status & $D01A`, same "derive the live value, don't cache
    /// it" approach `frame_cycle`'s own raster-position read already
    /// uses above.
    irq_status: u8,

    /// A light pen trigger has latched `$D013`/`$D014` this frame; further
    /// triggers are ignored until the next frame starts (real hardware
    /// latches once per frame).
    light_pen_latched: bool,

    /// Whether bad lines are currently allowed to occur this frame --
    /// real hardware's own DEN (Display ENable, `$D011` bit 4) latch:
    /// (re-)evaluated only once per frame, at the moment the raster
    /// reaches `FIRST_DMA_LINE`, from whatever DEN reads as at that
    /// instant (`tick`'s own doc comment). This is a real, coarser
    /// approximation of a genuinely per-cycle hardware latch (real
    /// silicon ORs together *every* cycle's DEN value across that whole
    /// raster line, which is what lets a program toggle DEN mid-line for
    /// the well-known "FLD"/border-removal trick) -- this project's
    /// per-CPU-instruction tick granularity has no sub-line resolution
    /// to do that with, so a single sample at the line's start is the
    /// achievable approximation; confirmed as a real, accepted tier
    /// (not this project's own invention) against VICE's own simpler,
    /// non-cycle-exact `vicii.c` core, which takes exactly this same
    /// single-sample approach (its separate cycle-exact `x64sc` core
    /// implements the finer per-cycle version instead -- see
    /// `CREDITS.md`).
    allow_bad_lines: bool,

    /// `$D01E`: which sprites have collided with another sprite since the
    /// register was last read. Accumulates (ORs) until a CPU read clears
    /// it (`read_clearing`), as on real hardware.
    #[serde(default)]
    collision_sprite_sprite: u8,
    /// `$D01F`: same, for sprite-background (foreground pixel) collisions.
    #[serde(default)]
    collision_sprite_background: u8,

    /// 6567 (NTSC) raster geometry instead of the 6569's (PAL).
    #[serde(default)]
    ntsc: bool,

    /// Whether the character ROM shadows the bank-relative `$1000-$1FFF`
    /// window for the VIC's glyph fetches (true in VIC banks 0 and 2, false
    /// in 1 and 3 -- the host sets it from CIA2). Everywhere else, and with
    /// the flag off, glyphs come from the bank's RAM at the `$D018` base,
    /// which is how custom character sets work.
    #[serde(default = "default_chargen_shadow")]
    chargen_shadow: bool,

    /// State of the cycle-accurate engine (`cycle.rs`), idle unless the host
    /// enabled it.
    #[serde(default)]
    engine: cycle::CycleEngine,
    /// Sprite DMA bookkeeping of the line-accurate model (see `tick`); idle in
    /// cycle mode, where the engine keeps the real counters.
    #[serde(default)]
    line_sprites: LineSprites,
}

/// What the line-accurate model remembers per sprite to know which lines
/// the VIC-II takes the bus for sprite data: the same DMA flag, MCBASE and
/// Y-expansion flip flop the real chip (and `cycle.rs`) keep, stepped once a
/// line (Bauer 3.8.1) instead of once a cycle.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct LineSprites {
    dma: u8,
    expansion_ff: u8,
    mcbase: [u8; SPRITE_COUNT],
    /// Whether sprite 2 fetched at the end of the previous line (its bus
    /// window runs straight into sprite 3's at the start of this one).
    sprite2_fetched: bool,
}

fn default_chargen_shadow() -> bool {
    true
}

impl Default for Vic {
    fn default() -> Self {
        Self {
            registers: vec![0; REG_COUNT],
            frame_cycle: 0,
            irq_status: 0,
            light_pen_latched: false,
            allow_bad_lines: false,
            collision_sprite_sprite: 0,
            collision_sprite_background: 0,
            ntsc: false,
            chargen_shadow: true,
            engine: cycle::CycleEngine::default(),
            line_sprites: LineSprites::default(),
        }
    }
}

/// The real, per-tick effects `tick` reports back to the host machine --
/// a raster IRQ edge (see `tick`'s own doc comment) and any CPU cycles
/// the VIC-II's bus mastery just stole for a bad line's c-accesses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickResult {
    /// `true` only on the exact tick a raster-compare match occurs *and*
    /// it's currently enabled in `$D01A` -- same edge-triggered,
    /// one-shot shape the old plain-`bool` return used.
    pub raster_irq: bool,
    /// Extra real clock cycles (`BAD_LINE_STOLEN_CYCLES`, or `0` outside
    /// a bad line) the host should feed into every *other* peripheral's
    /// own `tick` (CIA/SID keep running off the real phi2 clock
    /// regardless of CPU bus access, real hardware fact) and into its
    /// own outer real-time cycle-budget accounting -- but must NOT feed
    /// back into a second `Vic::tick` call: the raster generator's own
    /// timing is independent of CPU bus stalls (see `tick`'s own doc
    /// comment for why `frame_cycle` never includes this value).
    pub stolen_cycles: u32,
    /// The raster line the position just moved onto, set only on a tick
    /// that crossed a line boundary. The host passes it to
    /// `scan_sprite_collisions`, which is the per-raster-line half of
    /// collision detection (see that method's doc comment).
    pub entered_line: Option<u32>,
}

/// One of the chip's real display modes, selected by `$D011`'s ECM/BMM
/// bits and `$D016`'s MCM bit (see `Vic::video_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoMode {
    /// ECM=0, BMM=0, MCM=0: the original text mode this crate already
    /// implemented (`render_text`).
    Text,
    /// ECM=0, BMM=0, MCM=1 (`render_multicolor_text`).
    MulticolorText,
    /// ECM=0, BMM=1, MCM=0 (`render_bitmap`).
    Bitmap,
    /// ECM=0, BMM=1, MCM=1 (`render_multicolor_bitmap`).
    MulticolorBitmap,
    /// ECM=1, BMM=0, MCM=0 (`render_extended_color_text`).
    ExtendedColorText,
    /// Any combination with ECM=1 not covered above (`ECM+BMM`, with or
    /// without `MCM`) -- real hardware's own "invalid" mode group.
    /// Widely documented as producing a solid-color (commonly cited as
    /// black) display area rather than anything software-meaningful;
    /// this crate renders solid black (`render_invalid_mode`) rather
    /// than attempting to reproduce whatever chip-specific garbage a
    /// real 6567/6569 shows in each exact sub-case, since no real C64
    /// software deliberately relies on one -- same "don't chase an
    /// unverifiable exact quirk" stance this project already takes
    /// elsewhere (see e.g. `sid`'s combined-waveform approximation).
    Invalid,
}

impl Vic {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advances the live raster position by `cycles` real clock cycles,
    /// latches a raster interrupt (`$D019` bit 0) if the position just
    /// transitioned onto the programmed compare line (`$D012` + `$D011`
    /// bit 7, see `raster_compare_line`), (re-)latches whether bad lines
    /// are allowed this frame if the position just reached
    /// `FIRST_DMA_LINE` (see `allow_bad_lines`'s own doc comment), and
    /// reports any CPU cycles a bad line just stole. Call once per
    /// `Machine::step()`, same "per-instruction granularity" shape
    /// `zx_spectrum::ZxSpectrum::step`'s own `tick()` calls use for its
    /// AY/tape peripherals. `frame_cycle` itself only ever advances by
    /// the real `cycles` given here, never by a returned
    /// `stolen_cycles` value -- real hardware's raster generator keeps
    /// its own fixed pace regardless of CPU bus stalls (see
    /// `TickResult::stolen_cycles`'s own doc comment).
    ///
    /// `TickResult::raster_irq` is `true` only on the tick a raster
    /// match happens *and* the raster source is currently enabled in
    /// `$D01A` -- an edge-triggered one-shot signal, same shape
    /// `c64::cia::Cia::tick`'s own return value already establishes for
    /// Timer A underflow, not a level held across every later call
    /// while unacknowledged (this codebase's existing, accepted
    /// simplification -- see that method's own doc comment). A tick
    /// batch that spans more than one full raster line
    /// (`cycles > CYCLES_PER_LINE`, never true for this project's own
    /// per-instruction call granularity) could skip past the compare
    /// line, or past `FIRST_DMA_LINE`, without the transition ever being
    /// observed -- not a concern at real 6502-instruction-sized cycle
    /// counts.
    pub fn tick(&mut self, cycles: u32) -> TickResult {
        let old_line = self.raster_line();
        let old_cycle = self.frame_cycle;
        self.frame_cycle = (self.frame_cycle + cycles) % self.cycles_per_frame();
        if self.frame_cycle < old_cycle {
            self.light_pen_latched = false;
        }
        let new_line = self.raster_line();

        let mut result = TickResult::default();
        if new_line == old_line {
            return result;
        }
        result.entered_line = Some(new_line);

        // The bus time sprite DMA takes, line by line (a tick that spans more
        // than one line -- a long DMA chunk -- steps every line it crossed).
        let total_lines = self.cycles_per_frame() / self.cycles_per_line();
        let crossed = (new_line + total_lines - old_line) % total_lines;
        for step in 1..=crossed.min(total_lines) {
            let line = (old_line + step) % total_lines;
            result.stolen_cycles += self.step_line_sprites(line);
        }

        if new_line == self.raster_compare_line() {
            self.irq_status |= IRQ_RASTER;
            result.raster_irq = self.irq_enable() & IRQ_RASTER != 0;
        }

        if new_line == FIRST_DMA_LINE {
            self.allow_bad_lines = self.den();
        }

        if self.is_bad_line(new_line) {
            result.stolen_cycles += BAD_LINE_STOLEN_CYCLES;
        }

        result
    }

    /// Advances the line-accurate model's sprite DMA state by raster line
    /// `line` and returns the cycles the VIC-II keeps the bus for sprite data
    /// in it: Bauer 3.8.1's rules taken once per line, in the order the cycles
    /// run -- sprites 3-7 fetch at the start of the line from the DMA state the
    /// previous line left, MCBASE advances (and DMA ends at 63) in cycles
    /// 15-16, the Y-expansion flip flop toggles and DMA starts in 55-56, and
    /// sprites 0-2 fetch at the end of the line. Each fetching sprite costs 2
    /// cycles, plus 3 BA warning cycles when the sprite before it in time did
    /// not fetch. A bad line's own steal is added separately and the two are
    /// not merged (on silicon the BA warning inside a bad line is free).
    fn step_line_sprites(&mut self, line: u32) -> u32 {
        let enabled = self.registers[REG_SPRITE_ENABLE];
        let y_expand = self.registers[REG_SPRITE_Y_EXPAND];
        let state = &mut self.line_sprites;
        if enabled == 0 && state.dma == 0 {
            state.sprite2_fetched = false;
            return 0;
        }

        let early = state.dma & 0xF8;
        // Cycles 15-16.
        let ff = state.expansion_ff | !y_expand;
        for n in 0..SPRITE_COUNT {
            if state.dma & (1 << n) != 0 && ff & (1 << n) != 0 {
                state.mcbase[n] = (state.mcbase[n] + 3) & 0x3F;
                if state.mcbase[n] == 63 {
                    state.dma &= !(1 << n);
                }
            }
        }
        // Cycles 55-56.
        state.expansion_ff = (state.expansion_ff ^ y_expand) | !y_expand;
        for n in 0..SPRITE_COUNT {
            let bit = 1u8 << n;
            if enabled & bit != 0
                && self.registers[n * 2 + 1] == (line & 0xFF) as u8
                && state.dma & bit == 0
            {
                state.dma |= bit;
                state.mcbase[n] = 0;
                if y_expand & bit != 0 {
                    state.expansion_ff &= !bit;
                }
            }
        }

        let late = state.dma & 0x07;
        let mut cycles = 0;
        // In time order: 3, 4, 5, 6, 7 (start of the line), then 0, 1, 2.
        let mut previous = state.sprite2_fetched;
        for n in [3usize, 4, 5, 6, 7] {
            let fetching = early & (1 << n) != 0;
            if fetching {
                cycles += SPRITE_DMA_CYCLES;
                if !previous {
                    cycles += SPRITE_BA_WARNING_CYCLES;
                }
            }
            previous = fetching;
        }
        previous = false;
        for n in 0..3usize {
            let fetching = late & (1 << n) != 0;
            if fetching {
                cycles += SPRITE_DMA_CYCLES;
                if !previous {
                    cycles += SPRITE_BA_WARNING_CYCLES;
                }
            }
            previous = fetching;
        }
        state.sprite2_fetched = late & 0x04 != 0;
        cycles
    }

    fn raster_line(&self) -> u32 {
        self.frame_cycle / self.cycles_per_line()
    }

    fn cycles_per_line(&self) -> u32 {
        if self.ntsc {
            NTSC_CYCLES_PER_LINE
        } else {
            CYCLES_PER_LINE
        }
    }

    fn cycles_per_frame(&self) -> u32 {
        if self.ntsc {
            NTSC_CYCLES_PER_LINE * NTSC_TOTAL_RASTER_LINES
        } else {
            CYCLES_PER_LINE * TOTAL_RASTER_LINES
        }
    }

    /// Switches between the PAL 6569 (63 cycles x 312 lines, the default)
    /// and the NTSC 6567R8 (65 x 263) raster. The display window, bad-line
    /// range and sprite offsets are the same on both; only how long a line
    /// and a frame last differ. Resets the raster position.
    /// Tells the chip whether the character ROM is visible at `$1000-$1FFF`
    /// of the VIC bank it is currently reading (banks 0 and 2).
    pub fn set_chargen_shadow(&mut self, shadow: bool) {
        self.chargen_shadow = shadow;
    }

    /// The 2 KB glyph table the text modes read: the character ROM when the
    /// `$D018` character base lands in the shadowed `$1000-$1FFF` window
    /// (`$1000` = the upper-case/graphics set, `$1800` = lower case), else
    /// the bank's own RAM at that base -- a program-defined character set.
    fn chargen<'a>(&self, ram: &'a [u8], char_rom: &'a [u8]) -> &'a [u8] {
        let base = self.character_base();
        if self.chargen_shadow && (0x1000..0x2000).contains(&base) {
            let at = base - 0x1000;
            return &char_rom[at..(at + 0x800).min(char_rom.len())];
        }
        &ram[base..(base + 0x800).min(ram.len())]
    }

    pub fn set_ntsc(&mut self, ntsc: bool) {
        self.ntsc = ntsc;
        self.frame_cycle = 0;
        // The cycle engine only knows the PAL schedule.
        self.engine.enabled = false;
    }

    pub fn is_ntsc(&self) -> bool {
        self.ntsc
    }

    fn den(&self) -> bool {
        self.registers[REG_CONTROL1] & DEN_BIT != 0
    }

    /// Real bad-line condition: bad lines allowed this frame (see
    /// `allow_bad_lines`), the raster line's own low 3 bits match
    /// `$D011`'s YSCROLL (bits 0-2), and the line falls inside the real
    /// DMA window. Confirmed directly against VICE's own real
    /// `vicii-fetch.c` (see `CREDITS.md`), not guessed.
    fn is_bad_line(&self, line: u32) -> bool {
        self.allow_bad_lines
            && (line & 7) == (self.registers[REG_CONTROL1] & 0x07) as u32
            && (FIRST_DMA_LINE..=LAST_DMA_LINE).contains(&line)
    }

    /// The 9-bit raster line real software programs `$D012`/`$D011` bit
    /// 7 with to select which line raises the raster interrupt --
    /// reconstructed from those registers' own *stored* raw bytes
    /// (`write` always stores the literal byte given, `read` is what
    /// overrides `$D012`/`$D011` bit 7 to report the *live* position
    /// instead -- see `read`'s own doc comment) rather than a redundant
    /// separate field. Combining rule confirmed directly against VICE's
    /// own real `vicii-irq.c` (see `CREDITS.md`), not guessed.
    fn raster_compare_line(&self) -> u32 {
        (((self.registers[REG_CONTROL1] & 0x80) as u32) << 1) | self.registers[REG_RASTER] as u32
    }

    fn irq_enable(&self) -> u8 {
        self.registers[REG_IRQ_ENABLE]
    }

    /// Whether `$D019`'s overall "/IRQ asserted" flag (bit 7) should read
    /// as set right now -- any latched source that's also enabled in
    /// `$D01A`. Computed live rather than cached, matching every other
    /// derived-not-stored value this chip already uses (`raster_line`,
    /// `raster_compare_line`).
    fn irq_line_asserted(&self) -> bool {
        self.irq_status & self.irq_enable() & 0x0F != 0
    }

    /// `reg` is already decoded to a `0..0x40` register index by the host
    /// bus (real hardware mirrors this 64-byte block across the whole
    /// `$D000`-`$D3FF` page -- the host's own address decode, not this
    /// chip's concern, same division of labor as `ay-3-8910`'s port
    /// decode living in each host machine). `$D012` (raster line low 8
    /// bits) and bit 7 of `$D011` (raster line's bit 8) are computed live
    /// from `frame_cycle` rather than stored -- real hardware always
    /// reads the *current* raster position regardless of what was last
    /// written there (a `$D012`/`$D011`-bit-7 write instead sets a
    /// raster *compare* value for IRQ purposes, reconstructed on demand
    /// by `raster_compare_line`, not this method).
    pub fn read(&self, reg: u8) -> u8 {
        match (reg as usize) % REG_COUNT {
            REG_CONTROL1 => {
                let raster_bit8 = ((self.raster_line() >> 8) & 1) as u8;
                (self.registers[REG_CONTROL1] & 0x7F) | (raster_bit8 << 7)
            }
            REG_RASTER => (self.raster_line() & 0xFF) as u8,
            REG_IRQ_STATUS => {
                (self.irq_status & 0x0F) | if self.irq_line_asserted() { 0x80 } else { 0 }
            }
            REG_COLLISION_SPRITE_SPRITE => self.collision_sprite_sprite,
            REG_COLLISION_SPRITE_BACKGROUND => self.collision_sprite_background,
            other => self.registers[other],
        }
    }

    /// A CPU read: same value as `read`, but `$D01E`/`$D01F` clear on being
    /// read (the collision registers latch until the CPU reads them).
    /// `read` stays side-effect free for debugger peeks.
    pub fn read_clearing(&mut self, reg: u8) -> u8 {
        let value = self.read(reg);
        match (reg as usize) % REG_COUNT {
            REG_COLLISION_SPRITE_SPRITE => self.collision_sprite_sprite = 0,
            REG_COLLISION_SPRITE_BACKGROUND => self.collision_sprite_background = 0,
            _ => {}
        }
        value
    }

    /// `$D019` is special-cased: real hardware clears a status bit when
    /// the CPU writes a `1` to it (bit 7 of the written value is also
    /// accepted but has no effect, since bit 7 is never stored -- see
    /// `irq_line_asserted`), rather than the plain overwrite every other
    /// register gets. Confirmed directly against VICE's own real
    /// `d019_store` (see `CREDITS.md`), not guessed.
    pub fn write(&mut self, reg: u8, val: u8) {
        let reg = (reg as usize) % REG_COUNT;
        if reg == REG_IRQ_STATUS {
            self.irq_status &= !(val & 0x0F);
        } else if reg == REG_COLLISION_SPRITE_SPRITE
            || reg == REG_COLLISION_SPRITE_BACKGROUND
            || reg == REG_LIGHT_PEN_X
            || reg == REG_LIGHT_PEN_Y
        {
            // Read-only latches: a CPU write has no effect.
        } else {
            if reg == REG_SPRITE_Y_EXPAND {
                // The Y-expansion flip flop is held set while a sprite's
                // MxYE bit is clear, and *stays* set when the bit is set
                // again: clearing MxYE mid-sprite makes the next MCBASE
                // update advance a row it should have repeated, which is the
                // "sprite crunch" glitch (Bauer 3.8.1, rule 1).
                let cleared = self.registers[reg] & !val;
                self.engine.set_expansion_ff(cleared);
                self.line_sprites.expansion_ff |= cleared;
            }
            self.registers[reg] = val;
        }
    }

    /// A light pen (or its stand-in) touches the picture at `x`/`y`, given in
    /// pixels of the 320x200 display area: latches `$D013` (the raster X
    /// position divided by two) and `$D014` (the raster line) -- at most once
    /// per frame, as on the chip -- and raises the light-pen interrupt bit
    /// (`$D019` bit 3). Returns `true` when that interrupt is enabled in
    /// `$D01A` and should be requested. The display area starts at raster X
    /// 24 and raster line `FIRST_DISPLAY_LINE`; a real pen's sensor lag
    /// (a few pixels) is not modelled.
    pub fn light_pen_trigger(&mut self, x: u16, y: u16) -> bool {
        if self.light_pen_latched {
            return false;
        }
        self.light_pen_latched = true;
        let raster_x = u32::from(x) + 24;
        self.registers[REG_LIGHT_PEN_X] = ((raster_x >> 1) & 0xFF) as u8;
        self.registers[REG_LIGHT_PEN_Y] = ((u32::from(y) + FIRST_DISPLAY_LINE as u32) & 0xFF) as u8;
        self.irq_status |= IRQ_LIGHT_PEN;
        self.irq_enable() & IRQ_LIGHT_PEN != 0
    }

    pub fn border_color(&self) -> (u8, u8, u8) {
        PALETTE[(self.registers[REG_BORDER_COLOR] & 0x0F) as usize]
    }

    pub fn background_color0(&self) -> (u8, u8, u8) {
        PALETTE[(self.registers[REG_BACKGROUND_COLOR0] & 0x0F) as usize]
    }

    /// `$D022`/`$D023`: multicolor text/bitmap modes' "Background 1"/
    /// "Background 2" color sources (see `render_multicolor_text`/
    /// `render_multicolor_bitmap`'s own doc comments for where each is
    /// used).
    pub fn background_color1(&self) -> (u8, u8, u8) {
        PALETTE[(self.registers[REG_BACKGROUND_COLOR1] & 0x0F) as usize]
    }

    pub fn background_color2(&self) -> (u8, u8, u8) {
        PALETTE[(self.registers[REG_BACKGROUND_COLOR2] & 0x0F) as usize]
    }

    /// `$D024`: extended color mode text's 4th background register (see
    /// `render_extended_color_text`'s own doc comment) -- multicolor
    /// modes never use this one, only ECM does.
    pub fn background_color3(&self) -> (u8, u8, u8) {
        PALETTE[(self.registers[REG_BACKGROUND_COLOR3] & 0x0F) as usize]
    }

    /// Which of the chip's real display modes is currently selected, from
    /// `$D011`'s ECM/BMM bits and `$D016`'s MCM bit. Five combinations are
    /// real, documented modes real software uses; the other three
    /// (`ECM+BMM` with or without `MCM`, `ECM+BMM+MCM`) are the chip's own
    /// "invalid" combinations -- see `VideoMode::Invalid`'s own doc
    /// comment for what this crate does with them. Confirmed directly
    /// against Christian Bauer's VIC-II article (see `CREDITS.md`).
    pub fn video_mode(&self) -> VideoMode {
        let ecm = self.registers[REG_CONTROL1] & ECM_BIT != 0;
        let bmm = self.registers[REG_CONTROL1] & BMM_BIT != 0;
        let mcm = self.registers[REG_CONTROL2] & MCM_BIT != 0;
        match (ecm, bmm, mcm) {
            (false, false, false) => VideoMode::Text,
            (false, false, true) => VideoMode::MulticolorText,
            (false, true, false) => VideoMode::Bitmap,
            (false, true, true) => VideoMode::MulticolorBitmap,
            (true, false, false) => VideoMode::ExtendedColorText,
            (true, _, _) => VideoMode::Invalid,
        }
    }

    /// `$D018` bits 7-4 (VM13-VM10): the video matrix's base address, in
    /// 1KB steps, relative to whichever 16K bank the VIC currently sees.
    /// Bank selection itself is real CIA2 Port A wiring -- explicitly
    /// deferred (see `TODO.md`'s "CIA2 completion" backlog, a separate
    /// item from this crate's own VIC-II coverage sequence) -- so this
    /// project's only modeled bank is bank 0, same assumption `render`'s
    /// own doc comment already states. Confirmed directly against
    /// Christian Bauer's VIC-II article (see `CREDITS.md`).
    pub fn video_matrix_base(&self) -> usize {
        (((self.registers[REG_MEMORY_PTRS] >> 4) & 0x0F) as usize) * 0x400
    }

    /// `$D018` bits 3-1 (CB13-CB11): the character generator's base
    /// address in text/ECM modes, in 2KB steps -- used only to decide
    /// which of the *host-supplied* `char_rom`'s own 2KB banks a text-
    /// family render call should treat as "the" character generator.
    /// This crate still doesn't model the real hardware's character-ROM-
    /// shadow behavior (real silicon substitutes its internal character
    /// ROM for RAM specifically when this base lands at bank-relative
    /// `$1000`/`$9000`, letting software point elsewhere in RAM for a
    /// custom redefined character set) -- same scope boundary
    /// `render_text` already established (see its own doc comment), not
    /// widened by this pass.
    pub fn character_base(&self) -> usize {
        (((self.registers[REG_MEMORY_PTRS] >> 1) & 0x07) as usize) * 0x800
    }

    /// `$D018` bit 3 (CB13 alone): in either bitmap mode, the bitmap
    /// data's base address is one of exactly two 8KB-aligned locations
    /// within the current bank (`$0000`/`$2000`) -- real hardware never
    /// substitutes ROM here, bitmap data is always real RAM. Confirmed
    /// directly against Christian Bauer's VIC-II article (see
    /// `CREDITS.md`).
    pub fn bitmap_base(&self) -> usize {
        if self.registers[REG_MEMORY_PTRS] & 0x08 != 0 {
            0x2000
        } else {
            0x0000
        }
    }

    /// Renders the standard 40x25 text-mode picture: `320x200` RGB pixels,
    /// row-major, top-to-bottom, left-to-right -- same shape
    /// `machine::Machine::render_frame` documents, though this crate has
    /// no dependency on `machine` itself (the host machine's own
    /// `render_frame` impl calls this and, if it wants a border, pads the
    /// result with `machine::with_border` using `border_color()` above --
    /// same division of labor `zx81`/`zx_spectrum` already use for their
    /// own border padding).
    ///
    /// `screen_ram`/`color_ram` must be at least `1000` bytes (`40*25`);
    /// `char_rom` at least `2048` bytes (256 chars * 8 bytes -- only the
    /// standard character set's first half is needed for text mode's own
    /// character codes `0..=255`, the real character ROM's second 2K bank
    /// is the alternate/graphics character set the KERNAL can bank-switch
    /// to via `$D018`, not modeled yet). Panics on a too-short slice
    /// (a caller error, not a runtime condition to recover from -- same
    /// stance `machine::MemoryBus` takes on out-of-range construction).
    ///
    /// Per-scanline pixel expansion (one character-ROM byte -> 8 RGB
    /// pixels = 24 bytes) uses the `wide` crate's portable `u8x32`
    /// (compiles to SSE/AVX on x86_64, NEON on ARM64, a plain scalar
    /// fallback elsewhere -- one code path, not a per-architecture
    /// `#[cfg]` split): `MASK24_TABLE`/`PALETTE24_TABLE` (below) turn a
    /// glyph byte or a 4-bit color index into precomputed 24-byte lane
    /// patterns -- only 256 and 16 distinct values exist, so every
    /// pattern is computed once ever (lazily, on first `render_text`
    /// call), not rebuilt per pixel/cell/scanline. Blending is then just
    /// `(ink & mask) | (bg & !mask)` (one vector op) and one 24-byte
    /// slice copy per scanline, instead of a per-bit branch plus 8
    /// separate 3-byte writes.
    ///
    /// A first version rebuilt both lane patterns from scratch on every
    /// cell/scanline via a small scalar loop each -- measured *slower*
    /// than the original branchy scalar code (0.73x, not a speedup) on
    /// this project's own x86_64 dev machine, because that construction
    /// cost dominated the tiny 24-byte blend it was feeding. Precomputing
    /// both tables once is what actually pays off; see
    /// `tests::bench_render_text_simd_vs_naive_scalar` for the measured
    /// numbers this doc comment is describing, not guessing at.
    pub fn render_text(
        &self,
        screen_ram: &[u8],
        color_ram: &[u8],
        char_rom: &[u8],
    ) -> (u32, u32, Vec<u8>) {
        let width = TEXT_COLS * CHAR_PX;
        let height = TEXT_ROWS * CHAR_PX;
        let mut rgb = vec![0u8; (width * height * 3) as usize];
        let bg24 =
            u8x32::new(PALETTE24_TABLE[self.registers[REG_BACKGROUND_COLOR0] as usize & 0x0F]);
        let mask_table = &*MASK24_TABLE;

        for row in 0..TEXT_ROWS {
            for col in 0..TEXT_COLS {
                let cell = (row * TEXT_COLS + col) as usize;
                let code = screen_ram[cell];
                let ink24 = u8x32::new(PALETTE24_TABLE[(color_ram[cell] & 0x0F) as usize]);
                let glyph_base = code as usize * 8;

                for line in 0..CHAR_PX {
                    let bits = char_rom[glyph_base + line as usize];
                    let mask24 = u8x32::new(mask_table[bits as usize]);
                    let blended = (ink24 & mask24) | (bg24 & !mask24);

                    let py = row * CHAR_PX + line;
                    let row_start = ((py * width + col * CHAR_PX) * 3) as usize;
                    rgb[row_start..row_start + 24].copy_from_slice(&blended.to_array()[..24]);
                }
            }
        }

        (width, height, rgb)
    }

    /// Renders the current frame according to whichever real display
    /// mode `video_mode` currently selects, slicing `ram` (this
    /// project's only modeled VIC bank -- bank 0, see
    /// `video_matrix_base`'s own doc comment for why) at the real,
    /// register-derived offsets itself rather than requiring the host
    /// to duplicate that address arithmetic. `char_rom` (used only by
    /// the text-family modes) and `color_ram` (the real,
    /// always-visible-regardless-of-bank 1KB color SRAM, a separate
    /// chip on real hardware) are supplied directly, same convention
    /// `render_text` already established -- this is the one entry point
    /// `c64::C64::render_frame` actually calls. Composites all 8
    /// hardware sprites on top of the selected mode's own background
    /// picture afterward (`render_sprites`'s own doc comment covers
    /// position/priority/expansion/color rules).
    pub fn render(&self, ram: &[u8], char_rom: &[u8], color_ram: &[u8]) -> (u32, u32, Vec<u8>) {
        let vm = self.video_matrix_base();
        let screen_ram = &ram[vm..vm + SCREEN_CELLS];
        let glyphs = self.chargen(ram, char_rom);

        let (width, height, mut rgb) = match self.video_mode() {
            VideoMode::Text => self.render_text(screen_ram, color_ram, glyphs),
            VideoMode::MulticolorText => self.render_multicolor_text(screen_ram, color_ram, glyphs),
            VideoMode::ExtendedColorText => {
                self.render_extended_color_text(screen_ram, color_ram, glyphs)
            }
            VideoMode::Bitmap => {
                let bb = self.bitmap_base();
                self.render_bitmap(&ram[bb..bb + BITMAP_BYTES], screen_ram)
            }
            VideoMode::MulticolorBitmap => {
                let bb = self.bitmap_base();
                self.render_multicolor_bitmap(&ram[bb..bb + BITMAP_BYTES], screen_ram, color_ram)
            }
            VideoMode::Invalid => self.render_invalid_mode(),
        };

        self.render_sprites(&mut rgb, ram, char_rom, color_ram);
        (width, height, rgb)
    }

    /// Multicolor text mode (ECM=0, BMM=0, MCM=1). Same `320x200`
    /// canvas/cell layout as `render_text`, but each cell's own
    /// color-RAM byte's bit 3 selects, per real hardware, between two
    /// completely different per-cell rendering rules: bit 3 clear
    /// renders exactly like `render_text` (single bit -> one pixel),
    /// except the color-RAM value is masked to its own low 3 bits first
    /// (only 8 colors reachable, not 16 -- bit 3 itself is the
    /// mode-select flag, not part of a color index, real hardware
    /// fact); bit 3 set treats each of the glyph byte's 4 adjacent
    /// bit-*pairs* as one double-width pixel (real hardware halves
    /// horizontal resolution this way), selecting one of 4 colors:
    /// `00`=Background 0 (`$D021`), `01`=Background 1 (`$D022`),
    /// `10`=Background 2 (`$D023`), `11`=the cell's own (low-3-bit)
    /// color. Confirmed directly against both VICE's own real
    /// `vicii-draw.c` and Christian Bauer's VIC-II article (see
    /// `CREDITS.md`).
    ///
    /// Deliberately plain scalar code, not the `MASK24_TABLE`/SIMD path
    /// `render_text` uses -- the doubled-pixel-width multicolor case
    /// doesn't fit that single-bit-per-pixel table shape, and there is
    /// no profiling evidence yet that this mode is a hot path worth the
    /// same investment (see `TODO.md`'s Performance section: no
    /// optimization without a measured bottleneck).
    pub fn render_multicolor_text(
        &self,
        screen_ram: &[u8],
        color_ram: &[u8],
        char_rom: &[u8],
    ) -> (u32, u32, Vec<u8>) {
        let width = TEXT_COLS * CHAR_PX;
        let height = TEXT_ROWS * CHAR_PX;
        let mut rgb = vec![0u8; (width * height * 3) as usize];
        let bg0 = self.background_color0();
        let bg1 = self.background_color1();
        let bg2 = self.background_color2();

        for row in 0..TEXT_ROWS {
            for col in 0..TEXT_COLS {
                let cell = (row * TEXT_COLS + col) as usize;
                let code = screen_ram[cell];
                let attr = color_ram[cell];
                let glyph_base = code as usize * 8;
                let multicolor = attr & 0x08 != 0;
                let own_color = PALETTE[(attr & 0x07) as usize];

                for line in 0..CHAR_PX {
                    let bits = char_rom[glyph_base + line as usize];
                    let py = row * CHAR_PX + line;
                    let row_base = ((py * width + col * CHAR_PX) * 3) as usize;

                    if multicolor {
                        for pair in 0..4u32 {
                            let shift = 6 - pair * 2;
                            let value = (bits >> shift) & 0x03;
                            let color = match value {
                                0 => bg0,
                                1 => bg1,
                                2 => bg2,
                                _ => own_color,
                            };
                            for sub in 0..2 {
                                let idx = row_base + ((pair * 2 + sub) * 3) as usize;
                                rgb[idx] = color.0;
                                rgb[idx + 1] = color.1;
                                rgb[idx + 2] = color.2;
                            }
                        }
                    } else {
                        for bit in 0..8u32 {
                            let set = bits & (0x80 >> bit) != 0;
                            let color = if set { own_color } else { bg0 };
                            let idx = row_base + (bit * 3) as usize;
                            rgb[idx] = color.0;
                            rgb[idx + 1] = color.1;
                            rgb[idx + 2] = color.2;
                        }
                    }
                }
            }
        }

        (width, height, rgb)
    }

    /// Standard (hi-res) bitmap mode (ECM=0, BMM=1, MCM=0): `320x200`,
    /// one bit per pixel same as `render_text`, but the ink/paper source
    /// is different -- each cell's *screen-RAM* byte (not color RAM)
    /// supplies both colors: the high nibble is the color for a set
    /// bit, the low nibble for a clear bit. `bitmap_data` is the real
    /// 8000-byte hi-res bitmap (`BITMAP_BYTES`, `bitmap_base`'s own
    /// slice) -- 8 bytes per cell, one per scanline, same per-cell/
    /// per-scanline order `render_text`'s own `char_rom` lookup uses.
    /// Reuses the same `MASK24_TABLE`/`PALETTE24_TABLE` SIMD blend
    /// `render_text` already established, since this is the exact same
    /// "8 bits -> 8 pixels, one of two colors" shape, just with a
    /// per-cell nibble pair standing in for `char_rom`'s glyph byte and
    /// color RAM's ink/background split. Confirmed directly against
    /// both VICE's own real `vicii-draw.c` and Christian Bauer's VIC-II
    /// article (see `CREDITS.md`).
    pub fn render_bitmap(&self, bitmap_data: &[u8], screen_ram: &[u8]) -> (u32, u32, Vec<u8>) {
        let width = TEXT_COLS * CHAR_PX;
        let height = TEXT_ROWS * CHAR_PX;
        let mut rgb = vec![0u8; (width * height * 3) as usize];
        let mask_table = &*MASK24_TABLE;

        for row in 0..TEXT_ROWS {
            for col in 0..TEXT_COLS {
                let cell = (row * TEXT_COLS + col) as usize;
                let nibbles = screen_ram[cell];
                let fg24 = u8x32::new(PALETTE24_TABLE[(nibbles >> 4) as usize & 0x0F]);
                let bg24 = u8x32::new(PALETTE24_TABLE[nibbles as usize & 0x0F]);
                let cell_base = cell * CHAR_PX as usize;

                for line in 0..CHAR_PX {
                    let bits = bitmap_data[cell_base + line as usize];
                    let mask24 = u8x32::new(mask_table[bits as usize]);
                    let blended = (fg24 & mask24) | (bg24 & !mask24);

                    let py = row * CHAR_PX + line;
                    let row_start = ((py * width + col * CHAR_PX) * 3) as usize;
                    rgb[row_start..row_start + 24].copy_from_slice(&blended.to_array()[..24]);
                }
            }
        }

        (width, height, rgb)
    }

    /// Multicolor bitmap mode (ECM=0, BMM=1, MCM=1): same doubled-pixel-
    /// width shape `render_multicolor_text`'s own multicolor cells use,
    /// applied to every cell rather than a per-cell opt-in. Each bitmap
    /// byte's 4 adjacent bit-pairs select one of 4 colors: `00`=
    /// Background 0 (`$D021`, chip-wide), `01`=the cell's screen-RAM
    /// high nibble, `10`=the cell's screen-RAM low nibble, `11`=the
    /// cell's color-RAM low nibble (a real 4-bit/16-color value here,
    /// unlike multicolor text's 3-bit/8-color "own color" -- color
    /// RAM's bit 3 isn't a mode-select flag in bitmap mode, real
    /// hardware fact). Confirmed directly against both VICE's own real
    /// `vicii-draw.c` and Christian Bauer's VIC-II article (see
    /// `CREDITS.md`). Deliberately plain scalar code, same "no
    /// profiling evidence yet" reasoning as `render_multicolor_text`.
    pub fn render_multicolor_bitmap(
        &self,
        bitmap_data: &[u8],
        screen_ram: &[u8],
        color_ram: &[u8],
    ) -> (u32, u32, Vec<u8>) {
        let width = TEXT_COLS * CHAR_PX;
        let height = TEXT_ROWS * CHAR_PX;
        let mut rgb = vec![0u8; (width * height * 3) as usize];
        let bg0 = self.background_color0();

        for row in 0..TEXT_ROWS {
            for col in 0..TEXT_COLS {
                let cell = (row * TEXT_COLS + col) as usize;
                let nibbles = screen_ram[cell];
                let colors = [
                    bg0,
                    PALETTE[(nibbles >> 4) as usize & 0x0F],
                    PALETTE[nibbles as usize & 0x0F],
                    PALETTE[color_ram[cell] as usize & 0x0F],
                ];
                let cell_base = cell * CHAR_PX as usize;

                for line in 0..CHAR_PX {
                    let bits = bitmap_data[cell_base + line as usize];
                    let py = row * CHAR_PX + line;
                    let row_base = ((py * width + col * CHAR_PX) * 3) as usize;

                    for pair in 0..4u32 {
                        let shift = 6 - pair * 2;
                        let color = colors[((bits >> shift) & 0x03) as usize];
                        for sub in 0..2 {
                            let idx = row_base + ((pair * 2 + sub) * 3) as usize;
                            rgb[idx] = color.0;
                            rgb[idx + 1] = color.1;
                            rgb[idx + 2] = color.2;
                        }
                    }
                }
            }
        }

        (width, height, rgb)
    }

    /// Extended color mode (ECM) text (ECM=1, BMM=0, MCM=0): same
    /// single-bit-per-pixel shape as `render_text` (ink still comes
    /// from color RAM's own low 4 bits, 16 colors), but only the
    /// character code's low 6 bits address `char_rom` (64 addressable
    /// characters, not 256 -- real hardware fact) and the *background*
    /// color varies per cell instead of being chip-wide: the code's own
    /// top 2 bits select one of `$D021`-`$D024` (Background 0-3).
    /// Reuses `render_text`'s own `MASK24_TABLE`/`PALETTE24_TABLE` SIMD
    /// blend, since per-cell background is exactly the same shape as
    /// `render_bitmap`'s own per-cell nibble colors, just sourced from a
    /// 4-entry table instead of a nibble. Confirmed directly against
    /// both VICE's own real `vicii-mem.c` and Christian Bauer's VIC-II
    /// article (see `CREDITS.md`).
    pub fn render_extended_color_text(
        &self,
        screen_ram: &[u8],
        color_ram: &[u8],
        char_rom: &[u8],
    ) -> (u32, u32, Vec<u8>) {
        let width = TEXT_COLS * CHAR_PX;
        let height = TEXT_ROWS * CHAR_PX;
        let mut rgb = vec![0u8; (width * height * 3) as usize];
        let bg_indices = [
            self.registers[REG_BACKGROUND_COLOR0] & 0x0F,
            self.registers[REG_BACKGROUND_COLOR1] & 0x0F,
            self.registers[REG_BACKGROUND_COLOR2] & 0x0F,
            self.registers[REG_BACKGROUND_COLOR3] & 0x0F,
        ];
        let mask_table = &*MASK24_TABLE;

        for row in 0..TEXT_ROWS {
            for col in 0..TEXT_COLS {
                let cell = (row * TEXT_COLS + col) as usize;
                let code = screen_ram[cell];
                let ink24 = u8x32::new(PALETTE24_TABLE[(color_ram[cell] & 0x0F) as usize]);
                let bg24 = u8x32::new(PALETTE24_TABLE[bg_indices[(code >> 6) as usize] as usize]);
                let glyph_base = (code as usize & 0x3F) * 8;

                for line in 0..CHAR_PX {
                    let bits = char_rom[glyph_base + line as usize];
                    let mask24 = u8x32::new(mask_table[bits as usize]);
                    let blended = (ink24 & mask24) | (bg24 & !mask24);

                    let py = row * CHAR_PX + line;
                    let row_start = ((py * width + col * CHAR_PX) * 3) as usize;
                    rgb[row_start..row_start + 24].copy_from_slice(&blended.to_array()[..24]);
                }
            }
        }

        (width, height, rgb)
    }

    /// The chip's own "invalid" ECM+BMM combinations (see
    /// `VideoMode::Invalid`'s own doc comment for why this crate
    /// doesn't attempt to reproduce real hardware's exact per-sub-case
    /// garbage): solid black across the same `320x200` canvas every
    /// other mode uses.
    pub fn render_invalid_mode(&self) -> (u32, u32, Vec<u8>) {
        let width = TEXT_COLS * CHAR_PX;
        let height = TEXT_ROWS * CHAR_PX;
        (width, height, vec![0u8; (width * height * 3) as usize])
    }

    /// Whether the *background* picture's pixel at `(x, y)` counts as a
    /// "foreground" pixel for sprite-priority purposes (`render_sprites`'
    /// own `MxDP`/behind-background handling) -- real hardware's rule is
    /// decided purely by the chip-wide MCM bit, independent of `BMM`/
    /// `ECM` and independent of any *per-cell* multicolor opt-in
    /// (multicolor text's own bit-3 flag only changes how a cell is
    /// *colored*, never how its priority is decided): `1` bit set (MCM
    /// clear) or a `10`/`11` 2-bit pair (MCM set) is foreground, `0`/
    /// `00`/`01` is background. Confirmed directly against Christian
    /// Bauer's VIC-II article's own explicit statement of this rule
    /// (see `CREDITS.md`). Recomputed on demand for a single pixel
    /// (rather than a whole precomputed mask) since it's only ever
    /// queried for the handful of pixels an enabled, behind-background
    /// sprite actually covers, not the full canvas -- no `Vec<bool>`
    /// allocation or extra pass needed for the common case (nothing
    /// behind-background enabled).
    fn background_pixel_is_foreground(
        &self,
        x: u32,
        y: u32,
        ram: &[u8],
        char_rom: &[u8],
        color_ram: &[u8],
    ) -> bool {
        let width = TEXT_COLS * CHAR_PX;
        let height = TEXT_ROWS * CHAR_PX;
        if x >= width || y >= height {
            // The border always wins over a sprite regardless of MxDP
            // (real hardware fact, see `render_sprites`'s own doc
            // comment) -- `render_sprites` itself already clips here,
            // this branch only guards against being called out of
            // range at all.
            return false;
        }

        let col = x / CHAR_PX;
        let row = y / CHAR_PX;
        let line = y % CHAR_PX;
        let bit_in_cell = x % CHAR_PX;
        let cell = (row * TEXT_COLS + col) as usize;
        let vm = self.video_matrix_base();
        let code = ram[vm + cell];

        match self.video_mode() {
            VideoMode::Invalid => false,
            VideoMode::Text => {
                let bits = char_rom[code as usize * 8 + line as usize];
                bits & (0x80 >> bit_in_cell) != 0
            }
            VideoMode::ExtendedColorText => {
                let bits = char_rom[(code as usize & 0x3F) * 8 + line as usize];
                bits & (0x80 >> bit_in_cell) != 0
            }
            VideoMode::Bitmap => {
                let bb = self.bitmap_base();
                let bits = ram[bb + cell * CHAR_PX as usize + line as usize];
                bits & (0x80 >> bit_in_cell) != 0
            }
            VideoMode::MulticolorText => {
                let _ = color_ram; // the per-cell bit-3 flag never affects this rule
                let bits = char_rom[code as usize * 8 + line as usize];
                let pair_shift = 6 - (bit_in_cell / 2) * 2;
                (bits >> pair_shift) & 0x03 >= 2
            }
            VideoMode::MulticolorBitmap => {
                let bb = self.bitmap_base();
                let bits = ram[bb + cell * CHAR_PX as usize + line as usize];
                let pair_shift = 6 - (bit_in_cell / 2) * 2;
                (bits >> pair_shift) & 0x03 >= 2
            }
        }
    }

    /// One sprite's own rendering-relevant state for the current frame,
    /// gathered once per sprite per `render_sprites` call.
    fn sprite_render_info(&self, index: usize, ram: &[u8]) -> SpriteRenderInfo {
        let x_low = self.registers[index * 2] as i32;
        let x_msb = (self.registers[REG_SPRITE_X_MSB] as i32 >> index) & 1;
        let vm = self.video_matrix_base();
        let pointer = ram[vm + SPRITE_POINTER_OFFSET + index];

        SpriteRenderInfo {
            x: (x_low | (x_msb << 8)) - SPRITE_X_OFFSET,
            y: self.registers[index * 2 + 1] as i32 - SPRITE_Y_OFFSET,
            data_base: pointer as usize * SPRITE_DATA_STRIDE,
            multicolor: self.registers[REG_SPRITE_MULTICOLOR_ENABLE] & (1 << index) != 0,
            x_expand: self.registers[REG_SPRITE_X_EXPAND] & (1 << index) != 0,
            y_expand: self.registers[REG_SPRITE_Y_EXPAND] & (1 << index) != 0,
            behind_background: self.registers[REG_SPRITE_PRIORITY] & (1 << index) != 0,
            own_color: PALETTE[(self.registers[REG_SPRITE_COLOR_BASE + index] & 0x0F) as usize],
        }
    }

    /// The color at one *source* (non-expanded) sprite pixel, or `None`
    /// if that pixel is transparent. `src_col` is `0..24` for a hi-res
    /// sprite or `0..12` (pixel *pairs*) for a multicolor one -- real
    /// hardware's own multicolor sprites are natively half horizontal
    /// resolution even before X-expansion (`render_sprites` is what
    /// doubles each source column into 1 or 2 real screen columns, and
    /// multicolor pairs into 2 real screen columns before that doubling
    /// too). Pixel-pair-to-color mapping (`00` transparent, `01`
    /// `$D025`, `10` the sprite's own color, `11` `$D026`) confirmed
    /// directly against Christian Bauer's VIC-II article (see
    /// `CREDITS.md`).
    fn sprite_pixel_color(
        &self,
        info: &SpriteRenderInfo,
        ram: &[u8],
        src_col: u32,
        src_row: u32,
    ) -> Option<(u8, u8, u8)> {
        let row_base = info.data_base + src_row as usize * 3;
        if info.multicolor {
            let pair = src_col;
            let byte = ram[row_base + (pair / 4) as usize];
            let shift = 6 - (pair % 4) * 2;
            match (byte >> shift) & 0x03 {
                0 => None,
                1 => Some(PALETTE[(self.registers[REG_SPRITE_MULTICOLOR0] & 0x0F) as usize]),
                2 => Some(info.own_color),
                _ => Some(PALETTE[(self.registers[REG_SPRITE_MULTICOLOR1] & 0x0F) as usize]),
            }
        } else {
            let byte = ram[row_base + (src_col / 8) as usize];
            let bit = 7 - (src_col % 8);
            if byte & (1 << bit) != 0 {
                Some(info.own_color)
            } else {
                None
            }
        }
    }

    /// Sprite-sprite and sprite-background collision detection for one
    /// raster line -- the per-line half that `render_sprites` (a once-per-
    /// frame composite) cannot be. The host calls it with each
    /// `TickResult::entered_line`, using the RAM slice the VIC currently
    /// sees, so the result reflects the registers and memory as they stand
    /// as that line begins, and the IRQ can reach the CPU at the right
    /// time. Returns `true` if a sprite collision interrupt should be
    /// requested (latched in `$D019` and enabled in `$D01A`).
    ///
    /// Rules, from Christian Bauer's VIC-II article and VICE's
    /// `vicii-sprites.c` (see `CREDITS.md`): any non-transparent pixel of
    /// two sprites at the same position collides (including multicolor
    /// pair `01`, and in the border, where the display window does not
    /// reach); a sprite pixel over a *foreground* background pixel
    /// (`background_pixel_is_foreground`) collides, but only inside the
    /// 320x200 window. The registers OR in new bits until read; the
    /// interrupt latch is set only when a register goes from zero to
    /// non-zero, so one unread collision never re-interrupts. `MxDP`
    /// does not matter -- a hidden-behind-background sprite still collides.
    ///
    /// Approximations: one sample at the start of each line (as for bad
    /// lines), and sprite data is read as
    /// registers/RAM stand at that moment rather than per-cycle DMA.
    pub fn scan_sprite_collisions(
        &mut self,
        line: u32,
        ram: &[u8],
        char_rom: &[u8],
        color_ram: &[u8],
    ) -> bool {
        let enabled = self.registers[REG_SPRITE_ENABLE];
        if enabled == 0 {
            return false;
        }
        let char_rom = self.chargen(ram, char_rom);
        let canvas_y = line as i32 - FIRST_DISPLAY_LINE;
        let canvas_w = (TEXT_COLS * CHAR_PX) as i32;
        let canvas_h = (TEXT_ROWS * CHAR_PX) as i32;
        // One slot per raster X (`0..504`): sprites collide wherever they
        // overlap, in the border too, and a sprite near the right end of the
        // line wraps round to the left.
        let mut owners = [0u8; RASTER_LINE_PIXELS as usize];
        let mut background_hits = 0u8;

        for index in 0..SPRITE_COUNT {
            if enabled & (1 << index) == 0 {
                continue;
            }
            let info = self.sprite_render_info(index, ram);
            let y_scale = if info.y_expand { 2 } else { 1 };
            let rel_y = canvas_y - info.y;
            if rel_y < 0 || rel_y >= (SPRITE_HEIGHT * y_scale) as i32 {
                continue;
            }
            let src_row = rel_y as u32 / y_scale;
            let x_scale = if info.x_expand { 2 } else { 1 };
            let col_width = if info.multicolor { 2 } else { 1 } * x_scale;
            let src_cols = if info.multicolor {
                SPRITE_WIDTH / 2
            } else {
                SPRITE_WIDTH
            };
            let in_window_row = (0..canvas_h).contains(&canvas_y);

            for src_col in 0..src_cols {
                if self
                    .sprite_pixel_color(&info, ram, src_col, src_row)
                    .is_none()
                {
                    continue;
                }
                for dx in 0..col_width {
                    let Some(x) = sprite_canvas_x(info.x, (src_col * col_width + dx) as i32) else {
                        continue;
                    };
                    owners[(x + SPRITE_X_OFFSET) as usize] |= 1 << index;
                    if in_window_row
                        && (0..canvas_w).contains(&x)
                        && self.background_pixel_is_foreground(
                            x as u32,
                            canvas_y as u32,
                            ram,
                            char_rom,
                            color_ram,
                        )
                    {
                        background_hits |= 1 << index;
                    }
                }
            }
        }

        let mut sprite_hits = 0u8;
        for &owned in owners.iter() {
            if owned.count_ones() >= 2 {
                sprite_hits |= owned;
            }
        }

        let enable = self.irq_enable();
        let mut request = false;
        if sprite_hits != 0 {
            if self.collision_sprite_sprite == 0 {
                self.irq_status |= IRQ_SPRITE_SPRITE;
                request |= enable & IRQ_SPRITE_SPRITE != 0;
            }
            self.collision_sprite_sprite |= sprite_hits;
        }
        if background_hits != 0 {
            if self.collision_sprite_background == 0 {
                self.irq_status |= IRQ_SPRITE_BACKGROUND;
                request |= enable & IRQ_SPRITE_BACKGROUND != 0;
            }
            self.collision_sprite_background |= background_hits;
        }
        request
    }

    /// Composites all 8 hardware sprites onto an already-rendered
    /// `320x200` background picture, in real hardware's own fixed
    /// priority order -- sprite 0 highest, sprite 7 lowest (drawn last/
    /// first respectively, so a later, opaque, higher-priority sprite
    /// pixel always overwrites whatever an earlier one or the background
    /// left there) -- confirmed directly against Christian Bauer's
    /// VIC-II article (see `CREDITS.md`), which also confirms this
    /// ordering is completely independent of each sprite's own `$D01B`
    /// (`MxDP`) bit: that bit only ever decides a sprite's visibility
    /// against *background* graphics, never against another sprite.
    /// `behind_background` sprites are hidden only where
    /// `background_pixel_is_foreground` says the background pixel
    /// underneath counts as foreground -- checked independently per
    /// sprite, so one sprite being hidden there never affects any other
    /// sprite drawn afterward.
    ///
    /// Real hardware fact, confirmed by the same article: the border
    /// has the *highest* display priority of all, overlaying sprites
    /// too -- this project's own architecture already keeps the border
    /// entirely separate from this `320x200` canvas (added afterward by
    /// the host via `machine::with_border`, see `CLAUDE.md`), so simply
    /// clipping any sprite pixel that would land outside this canvas
    /// reproduces that real behavior for free, not as an approximation.
    ///
    /// Two further real, documented scope boundaries, not oversights:
    /// VIC bank selection stays fixed at bank 0, same as every other
    /// address this crate computes (see `video_matrix_base`'s own doc
    /// comment -- real CIA2 wiring, a separate deferred roadmap item);
    /// and the real "sprite crunching" glitch (changing Y-expansion
    /// mid-render can corrupt a real chip's internal row counter) isn't
    /// modeled -- a deliberate, rarely-exploited timing quirk, not
    /// baseline sprite behavior any normal software depends on.
    pub fn render_sprites(&self, rgb: &mut [u8], ram: &[u8], char_rom: &[u8], color_ram: &[u8]) {
        let char_rom = self.chargen(ram, char_rom);
        let width = TEXT_COLS * CHAR_PX;
        let height = TEXT_ROWS * CHAR_PX;
        let enabled = self.registers[REG_SPRITE_ENABLE];

        for index in (0..SPRITE_COUNT).rev() {
            if enabled & (1 << index) == 0 {
                continue;
            }
            let info = self.sprite_render_info(index, ram);
            let x_scale = if info.x_expand { 2 } else { 1 };
            let y_scale = if info.y_expand { 2 } else { 1 };
            // Multicolor pixels are natively 2 real columns wide even
            // unexpanded (see `sprite_pixel_color`'s own doc comment).
            let col_width = if info.multicolor { 2 } else { 1 } * x_scale;
            let src_cols = if info.multicolor {
                SPRITE_WIDTH / 2
            } else {
                SPRITE_WIDTH
            };

            for src_row in 0..SPRITE_HEIGHT {
                for src_col in 0..src_cols {
                    let Some(color) = self.sprite_pixel_color(&info, ram, src_col, src_row) else {
                        continue;
                    };

                    for dy in 0..y_scale {
                        let y = info.y + (src_row * y_scale + dy) as i32;
                        if y < 0 || y as u32 >= height {
                            continue;
                        }
                        for dx in 0..col_width {
                            let offset = (src_col * col_width + dx) as i32;
                            let Some(x) = sprite_canvas_x(info.x, offset) else {
                                continue;
                            };
                            if x < 0 || x as u32 >= width {
                                continue;
                            }
                            if info.behind_background
                                && self.background_pixel_is_foreground(
                                    x as u32, y as u32, ram, char_rom, color_ram,
                                )
                            {
                                continue;
                            }

                            let idx = ((y as u32 * width + x as u32) * 3) as usize;
                            rgb[idx] = color.0;
                            rgb[idx + 1] = color.1;
                            rgb[idx + 2] = color.2;
                        }
                    }
                }
            }
        }
    }
}

/// One sprite's rendering-relevant state for the duration of one
/// `render_sprites` call -- gathered once per sprite (`sprite_render_info`)
/// rather than re-reading registers per pixel.
struct SpriteRenderInfo {
    /// Screen-relative X, already corrected by `SPRITE_X_OFFSET` -- can be
    /// negative or beyond the canvas width for a sprite parked off-screen.
    x: i32,
    y: i32,
    /// Absolute (bank-0-relative) address of this sprite's 63-byte data
    /// block.
    data_base: usize,
    multicolor: bool,
    x_expand: bool,
    y_expand: bool,
    /// `$D01B` (`MxDP`): `true` means this sprite draws behind qualifying
    /// background pixels, `false` means always in front.
    behind_background: bool,
    own_color: (u8, u8, u8),
}

/// One RGB triple (from `PALETTE`) repeated across the first 24 lanes of
/// each 32-lane pattern, indexed by the 4-bit color-RAM/register value --
/// computed once, ever, on first use.
static PALETTE24_TABLE: std::sync::LazyLock<[[u8; 32]; 16]> = std::sync::LazyLock::new(|| {
    let mut table = [[0u8; 32]; 16];
    for (i, color) in PALETTE.iter().enumerate() {
        for pixel in table[i].as_chunks_mut::<3>().0.iter_mut().take(8) {
            pixel[0] = color.0;
            pixel[1] = color.1;
            pixel[2] = color.2;
        }
    }
    table
});

/// Every possible character-ROM byte (8 pixel bits, MSB first) expanded
/// to 24 selector bytes -- 3 identical `0xFF` (bit set) / `0x00` (bit
/// clear) bytes per pixel, matching `PALETTE24_TABLE`'s own per-pixel
/// layout so the two blend lane-for-lane. `((bits >> shift) & 1).
/// wrapping_neg()` is the standard branchless "0/1 -> 0x00/0xFF" trick
/// (`0u8.wrapping_neg() == 0x00`, `1u8.wrapping_neg() == 0xFF`). All 256
/// entries computed once, ever, on first use.
static MASK24_TABLE: std::sync::LazyLock<[[u8; 32]; 256]> = std::sync::LazyLock::new(|| {
    let mut table = [[0u8; 32]; 256];
    for (bits, entry) in table.iter_mut().enumerate() {
        for (bit, pixel) in entry.as_chunks_mut::<3>().0.iter_mut().take(8).enumerate() {
            let set = ((bits as u8 >> (7 - bit)) & 1).wrapping_neg();
            pixel[0] = set;
            pixel[1] = set;
            pixel[2] = set;
        }
    }
    table
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn border_and_background_read_back_through_the_palette() {
        let mut vic = Vic::new();
        vic.write(0x20, 0x01); // white border
        vic.write(0x21, 0x06); // blue background
        assert_eq!(vic.border_color(), PALETTE[1]);
        assert_eq!(vic.background_color0(), PALETTE[6]);
    }

    #[test]
    fn unimplemented_registers_echo_back_what_was_written() {
        let mut vic = Vic::new();
        vic.write(0x11, 0x1B); // a real control register this milestone ignores
        assert_eq!(vic.read(0x11), 0x1B);
    }

    /// Regression guard for a real bug: without a live raster counter,
    /// real KERNAL boot code busy-waits forever on `$D012` (see
    /// `Vic::frame_cycle`'s own doc comment -- confirmed live against a
    /// real KERNAL ROM, not hypothetical).
    #[test]
    fn raster_register_advances_with_tick_and_wraps_at_frame_boundary() {
        let mut vic = Vic::new();
        assert_eq!(vic.read(0x12), 0);

        vic.tick(CYCLES_PER_LINE * 3); // 3 full lines in
        assert_eq!(vic.read(0x12), 3);

        vic.tick(CYCLES_PER_LINE * (TOTAL_RASTER_LINES - 3)); // wrap around
        assert_eq!(
            vic.read(0x12),
            0,
            "raster line should wrap back to 0 at the next frame"
        );
    }

    #[test]
    fn control1_bit7_reflects_the_raster_lines_9th_bit() {
        let mut vic = Vic::new();
        vic.write(0x11, 0b0101_0101); // real bits 0-6 the KERNAL might set
        assert_eq!(vic.read(0x11) & 0x80, 0, "raster line 0 -> bit 8 clear");

        vic.tick(CYCLES_PER_LINE * 300); // raster line 300 > 255, bit 8 set
        assert_eq!(vic.read(0x11) & 0x80, 0x80);
        assert_eq!(
            vic.read(0x11) & 0x7F,
            0b0101_0101,
            "the other bits must survive untouched"
        );
    }

    #[test]
    fn raster_irq_fires_once_when_the_raster_reaches_the_compare_line_and_is_enabled() {
        let mut vic = Vic::new();
        vic.write(0x12, 10); // compare line 10 (bit 8 clear -> $D011 untouched)
        vic.write(0x1A, 0x01); // enable the raster IRQ source

        // Ticks that don't reach line 10 yet must not fire.
        assert!(!vic.tick(CYCLES_PER_LINE * 9).raster_irq);
        // The tick that crosses onto line 10 fires exactly once.
        assert!(vic.tick(CYCLES_PER_LINE).raster_irq);
        // Staying on/advancing past line 10 afterward must not re-fire.
        assert!(!vic.tick(CYCLES_PER_LINE).raster_irq);
    }

    #[test]
    fn raster_irq_does_not_fire_when_disabled_in_d01a() {
        let mut vic = Vic::new();
        vic.write(0x12, 5);
        // $D01A left at 0 -- raster source not enabled.
        assert!(!vic.tick(CYCLES_PER_LINE * 5).raster_irq);
        // The latch still sets internally (observable via $D019's low
        // nibble), even though the return value (and thus a real CPU
        // IRQ) stays suppressed.
        assert_eq!(vic.read(0x19) & 0x0F, 0x01);
    }

    #[test]
    fn raster_compare_uses_both_d012_and_d011_bit7_as_the_9th_bit() {
        let mut vic = Vic::new();
        vic.write(0x12, 0x2C); // low 8 bits of line 300 (0x12C)
        vic.write(0x11, 0x80); // bit 7 set -> the 9th bit of the compare line
        vic.write(0x1A, 0x01);

        assert!(!vic.tick(CYCLES_PER_LINE * 299).raster_irq);
        assert!(
            vic.tick(CYCLES_PER_LINE).raster_irq,
            "compare line 300 needs $D011 bit 7 combined with $D012's low byte"
        );
    }

    #[test]
    fn writing_1_to_d019_clears_the_latched_bit_and_deasserts_irq_flag() {
        let mut vic = Vic::new();
        vic.write(0x12, 1);
        vic.write(0x1A, 0x01);
        assert!(vic.tick(CYCLES_PER_LINE).raster_irq);

        assert_eq!(vic.read(0x19), 0x81, "latch bit + asserted IRQ flag");
        vic.write(0x19, 0x01); // ack: write 1 to clear bit 0
        assert_eq!(
            vic.read(0x19),
            0x00,
            "clearing the only latched+enabled source also clears bit 7"
        );
    }

    #[test]
    fn bad_line_steals_40_cycles_when_den_set_at_first_dma_line_and_yscroll_matches() {
        let mut vic = Vic::new();
        // DEN set, YSCROLL=0 (default) -- line $30's own low 3 bits are
        // 0, so the very tick that reaches $30 is itself a bad line: DEN
        // gets latched and the bad-line check both happen on that same
        // transition.
        vic.write(0x11, DEN_BIT);

        let mut result = TickResult::default();
        for _ in 0..(FIRST_DMA_LINE) {
            result = vic.tick(CYCLES_PER_LINE);
        }
        assert_eq!(vic.raster_line(), FIRST_DMA_LINE);
        assert_eq!(
            result.stolen_cycles, BAD_LINE_STOLEN_CYCLES,
            "line $30 itself should be a bad line when DEN is set and YSCROLL matches"
        );

        // The very next line ($31) doesn't match YSCROLL (0) any more --
        // no steal.
        let result = vic.tick(CYCLES_PER_LINE);
        assert_eq!(result.stolen_cycles, 0);
    }

    #[test]
    fn bad_line_never_fires_when_den_is_clear_at_first_dma_line() {
        let mut vic = Vic::new();
        // DEN left clear -- bad lines must not be allowed this frame at
        // all, even though YSCROLL (0) matches line $30's own low bits.
        let mut any_stolen = false;
        for _ in 0..TOTAL_RASTER_LINES {
            if vic.tick(CYCLES_PER_LINE).stolen_cycles > 0 {
                any_stolen = true;
            }
        }
        assert!(
            !any_stolen,
            "no bad line should occur anywhere in the frame while DEN was clear at $30"
        );
    }

    #[test]
    fn bad_line_only_occurs_inside_the_real_dma_window() {
        let mut vic = Vic::new();
        vic.write(0x11, DEN_BIT); // DEN set
        // YSCROLL=0 matches line $30 (48) and every 8th line after it,
        // including 248 ($F8) which sits just past LAST_DMA_LINE ($F7) --
        // that line must not steal despite otherwise matching.
        let mut stolen_lines = Vec::new();
        for _ in 0..TOTAL_RASTER_LINES {
            let result = vic.tick(CYCLES_PER_LINE);
            if result.stolen_cycles > 0 {
                stolen_lines.push(vic.raster_line());
            }
        }
        assert!(
            stolen_lines.contains(&FIRST_DMA_LINE),
            "line $30 should steal"
        );
        assert!(
            !stolen_lines.contains(&248),
            "line $F8 (past LAST_DMA_LINE) must not steal even though YSCROLL matches"
        );
        assert!(
            stolen_lines
                .iter()
                .all(|&l| (FIRST_DMA_LINE..=LAST_DMA_LINE).contains(&l)),
            "every stolen line must fall inside the real DMA window"
        );
    }

    #[test]
    fn stolen_cycles_never_inflate_the_live_raster_position() {
        let mut vic = Vic::new();
        vic.write(0x11, DEN_BIT);

        let mut total_stolen = 0u32;
        for _ in 0..(FIRST_DMA_LINE + 1) {
            total_stolen += vic.tick(CYCLES_PER_LINE).stolen_cycles;
        }
        assert!(
            total_stolen > 0,
            "at least one bad line should have occurred by here"
        );
        // The raster position must reflect only the real cycles given to
        // `tick`, never the reported steal on top -- real hardware's
        // raster generator runs at a fixed pace independent of CPU bus
        // stalls.
        assert_eq!(vic.raster_line(), FIRST_DMA_LINE + 1);
    }

    #[test]
    fn render_text_draws_a_solid_glyph_over_the_background() {
        let mut vic = Vic::new();
        vic.write(0x21, 0x00); // black background

        let mut screen = vec![0u8; 1000];
        let mut color = vec![0u8; 1000];
        screen[0] = 1; // char code 1
        color[0] = 1; // white ink

        let mut chars = vec![0u8; 2048];
        // Char 1's glyph: solid 0xFF every scanline (a filled 8x8 block).
        for row in 0..8 {
            chars[8 + row] = 0xFF;
        }

        let (w, h, rgb) = vic.render_text(&screen, &color, &chars);
        assert_eq!((w, h), (320, 200));

        // Top-left pixel of cell (0,0) should be white (the glyph).
        assert_eq!(&rgb[0..3], &[0xFF, 0xFF, 0xFF]);
        // A pixel in the next cell over (col 1, still row 0) should be
        // background black, since screen[1] is char code 0 (blank glyph).
        let px = 8usize; // start of column 1
        let idx = px * 3;
        assert_eq!(&rgb[idx..idx + 3], &[0x00, 0x00, 0x00]);
    }

    #[test]
    fn video_mode_selects_correctly_from_ecm_bmm_mcm_bits() {
        let mut vic = Vic::new();
        assert_eq!(vic.video_mode(), VideoMode::Text);

        vic.write(0x16, MCM_BIT);
        assert_eq!(vic.video_mode(), VideoMode::MulticolorText);

        vic.write(0x16, 0x00);
        vic.write(0x11, BMM_BIT);
        assert_eq!(vic.video_mode(), VideoMode::Bitmap);

        vic.write(0x16, MCM_BIT);
        assert_eq!(vic.video_mode(), VideoMode::MulticolorBitmap);

        vic.write(0x16, 0x00);
        vic.write(0x11, ECM_BIT);
        assert_eq!(vic.video_mode(), VideoMode::ExtendedColorText);

        vic.write(0x11, ECM_BIT | BMM_BIT);
        assert_eq!(vic.video_mode(), VideoMode::Invalid);

        vic.write(0x16, MCM_BIT);
        assert_eq!(vic.video_mode(), VideoMode::Invalid);
    }

    #[test]
    fn memory_pointer_bases_are_derived_from_d018() {
        let mut vic = Vic::new();
        // VM13-10 = 0b0101 -> 5 * 0x400; CB13-11 = 0b011 -> 3 * 0x800;
        // CB13 alone (bit 3, part of that same 3-bit field) is clear.
        vic.write(0x18, 0b0101_0110);
        assert_eq!(vic.video_matrix_base(), 5 * 0x400);
        assert_eq!(vic.character_base(), 3 * 0x800);
        assert_eq!(vic.bitmap_base(), 0x0000);

        // CB13 (bit 3) set -> bitmap base $2000, independent of VM.
        vic.write(0x18, 0b0000_1000);
        assert_eq!(vic.bitmap_base(), 0x2000);
    }

    #[test]
    fn render_multicolor_text_handles_both_per_cell_rendering_rules() {
        let mut vic = Vic::new();
        vic.write(0x21, 0x00); // background 0: black
        vic.write(0x22, 0x06); // background 1: blue
        vic.write(0x23, 0x05); // background 2: green

        let mut screen = vec![0u8; 1000];
        let mut color = vec![0u8; 1000];
        let mut chars = vec![0u8; 2048];

        // Cell 0: multicolor (bit 3 set), own color = cyan (index 3).
        screen[0] = 1;
        color[0] = 0x0B; // bit3 set, low 3 bits = 3 (cyan)
        chars[8] = 0x6C; // 01 10 11 00 -> bg1, bg2, own, bg0

        // Cell 1: standard (bit 3 clear), own color = red (index 2).
        screen[1] = 2;
        color[1] = 0x02;
        chars[16] = 0xF0; // first 4 pixels set -> red, rest -> bg0 (black)

        let (_, _, rgb) = vic.render_multicolor_text(&screen, &color, &chars);

        let px = |n: usize| &rgb[n * 3..n * 3 + 3];
        assert_eq!(px(0), &[PALETTE[6].0, PALETTE[6].1, PALETTE[6].2]); // bg1 blue
        assert_eq!(px(1), &[PALETTE[6].0, PALETTE[6].1, PALETTE[6].2]);
        assert_eq!(px(2), &[PALETTE[5].0, PALETTE[5].1, PALETTE[5].2]); // bg2 green
        assert_eq!(px(4), &[PALETTE[3].0, PALETTE[3].1, PALETTE[3].2]); // own cyan
        assert_eq!(px(6), &[PALETTE[0].0, PALETTE[0].1, PALETTE[0].2]); // bg0 black

        // Cell 1 (columns 8-15): standard single-bit rendering.
        assert_eq!(px(8), &[PALETTE[2].0, PALETTE[2].1, PALETTE[2].2]); // red
        assert_eq!(px(12), &[PALETTE[0].0, PALETTE[0].1, PALETTE[0].2]); // bg0 black
    }

    #[test]
    fn render_bitmap_uses_screen_ram_nibbles_as_fg_and_bg() {
        let vic = Vic::new();
        let mut screen = vec![0u8; 1000];
        screen[0] = 0x1E; // high nibble 1 (white, fg), low nibble 14 (light blue, bg)
        let mut bitmap = vec![0u8; BITMAP_BYTES];
        bitmap[0] = 0xF0; // first 4 pixels set -> fg, rest -> bg

        let (w, h, rgb) = vic.render_bitmap(&bitmap, &screen);
        assert_eq!((w, h), (320, 200));

        let px = |n: usize| &rgb[n * 3..n * 3 + 3];
        assert_eq!(px(0), &[PALETTE[1].0, PALETTE[1].1, PALETTE[1].2]); // white
        assert_eq!(px(4), &[PALETTE[14].0, PALETTE[14].1, PALETTE[14].2]); // light blue
    }

    #[test]
    fn render_multicolor_bitmap_selects_among_four_color_sources() {
        let mut vic = Vic::new();
        vic.write(0x21, 0x00); // background 0: black

        let mut screen = vec![0u8; 1000];
        screen[0] = 0x37; // high nibble 3 (cyan), low nibble 7 (yellow)
        let mut color = vec![0u8; 1000];
        color[0] = 0x0A; // low nibble 10 (light red)
        let mut bitmap = vec![0u8; BITMAP_BYTES];
        bitmap[0] = 0x6C; // 01 10 11 00 -> hi-nibble, lo-nibble, color-ram, bg0

        let (_, _, rgb) = vic.render_multicolor_bitmap(&bitmap, &screen, &color);
        let px = |n: usize| &rgb[n * 3..n * 3 + 3];
        assert_eq!(px(0), &[PALETTE[3].0, PALETTE[3].1, PALETTE[3].2]); // hi nibble cyan
        assert_eq!(px(2), &[PALETTE[7].0, PALETTE[7].1, PALETTE[7].2]); // lo nibble yellow
        assert_eq!(px(4), &[PALETTE[10].0, PALETTE[10].1, PALETTE[10].2]); // color RAM light red
        assert_eq!(px(6), &[PALETTE[0].0, PALETTE[0].1, PALETTE[0].2]); // bg0 black
    }

    #[test]
    fn render_extended_color_text_masks_char_code_and_picks_background_per_cell() {
        let mut vic = Vic::new();
        vic.write(0x21, 0x00); // background 0: black
        vic.write(0x22, 0x06); // background 1: blue
        vic.write(0x23, 0x05); // background 2: green
        vic.write(0x24, 0x02); // background 3: red

        let mut screen = vec![0u8; 1000];
        // Top 2 bits = 01 (background 1) + low 6 bits = 1 (char index 1).
        screen[0] = 0b0100_0001;
        let mut color = vec![0u8; 1000];
        color[0] = 0x01; // white ink

        let mut chars = vec![0u8; 2048];
        chars[8] = 0xF0; // char 1's real (masked) glyph: first 4 pixels set
        chars[520] = 0x00; // char 65's glyph, if code were NOT masked: all clear

        let (_, _, rgb) = vic.render_extended_color_text(&screen, &color, &chars);
        let px = |n: usize| &rgb[n * 3..n * 3 + 3];
        // Bit 0 set in the masked glyph -> ink (white), proving the code
        // was masked to 0x01 rather than used unmasked (0x41, whose
        // glyph at chars[520] is all-clear and would show background).
        assert_eq!(px(0), &[PALETTE[1].0, PALETTE[1].1, PALETTE[1].2]);
        // Bit 7 clear -> background 1 (blue), from the code's top 2 bits.
        assert_eq!(px(7), &[PALETTE[6].0, PALETTE[6].1, PALETTE[6].2]);
    }

    #[test]
    fn render_invalid_mode_is_solid_black() {
        let vic = Vic::new();
        let (w, h, rgb) = vic.render_invalid_mode();
        assert_eq!((w, h), (320, 200));
        assert!(rgb.iter().all(|&b| b == 0));
    }

    #[test]
    fn render_dispatches_to_the_mode_selected_by_registers() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let color_ram = vec![0u8; 1000];
        let char_rom = vec![0u8; 2048];

        // Default text mode: video matrix base defaults to $0000 (no
        // $D018 write yet) -- confirm the dispatcher reaches render_text
        // by writing a distinctive border-independent background and
        // checking the canvas shape.
        let (w, h, _) = vic.render(&ram, &char_rom, &color_ram);
        assert_eq!((w, h), (320, 200));

        // Switch to standard bitmap mode with a real $D018-derived
        // bitmap base, and confirm the dispatcher picks render_bitmap's
        // output (a plain black canvas here, since bitmap/screen RAM are
        // still all zero -- the point is that it doesn't panic on a
        // real, non-trivial base offset and returns the right shape).
        vic.write(0x18, 0x08); // CB13 set -> bitmap base $2000
        vic.write(0x11, BMM_BIT);
        let bb = vic.bitmap_base();
        assert_eq!(bb, 0x2000);
        ram[bb] = 0xFF; // first bitmap byte: all pixels "set" (fg color)
        let (w, h, rgb) = vic.render(&ram, &char_rom, &color_ram);
        assert_eq!((w, h), (320, 200));
        // fg/bg both come from screen_ram[0] (all zero here -> index 0,
        // black) -- just confirm no panic and a real pixel was written.
        assert_eq!(&rgb[0..3], &[0x00, 0x00, 0x00]);
    }

    /// Builds a `Vic` with sprite `index` at screen position `(x, y)` (canvas
    /// coordinates), solid in its first row, and returns the raster line
    /// that row is drawn on.
    fn place_solid_sprite(vic: &mut Vic, ram: &mut [u8], index: u8, x: u8, y: u8) -> u32 {
        vic.write(index * 2, x + 24);
        vic.write(index * 2 + 1, y + 50);
        let enabled = vic.read(0x15) | (1 << index);
        vic.write(0x15, enabled);
        let pointer = 1 + index;
        ram[0x3F8 + index as usize] = pointer;
        for byte in 0..3 {
            ram[pointer as usize * 64 + byte] = 0xFF;
        }
        y as u32 + 51
    }

    #[test]
    fn overlapping_sprites_set_both_bits_in_d01e_and_clear_on_read() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let line = place_solid_sprite(&mut vic, &mut ram, 0, 100, 40);
        place_solid_sprite(&mut vic, &mut ram, 3, 110, 40);

        vic.scan_sprite_collisions(line, &ram, &[0; 2048], &[0; 1000]);
        assert_eq!(vic.read(0x1E), 0b0000_1001);
        // A peek leaves it; a CPU read clears it.
        assert_eq!(vic.read(0x1E), 0b0000_1001);
        assert_eq!(vic.read_clearing(0x1E), 0b0000_1001);
        assert_eq!(vic.read(0x1E), 0);
        // No sprite-background hit: empty background.
        assert_eq!(vic.read(0x1F), 0);
    }

    #[test]
    fn sprites_that_do_not_overlap_in_x_or_in_y_do_not_collide() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let line = place_solid_sprite(&mut vic, &mut ram, 0, 100, 40);
        place_solid_sprite(&mut vic, &mut ram, 1, 124, 40); // starts where 0 ends
        vic.scan_sprite_collisions(line, &ram, &[0; 2048], &[0; 1000]);
        assert_eq!(vic.read(0x1E), 0, "adjacent, not overlapping");

        // Same x, different row: sprite 2's solid row is on another line.
        place_solid_sprite(&mut vic, &mut ram, 2, 100, 60);
        vic.scan_sprite_collisions(line, &ram, &[0; 2048], &[0; 1000]);
        assert_eq!(vic.read(0x1E), 0);
    }

    #[test]
    fn a_transparent_pixel_never_collides() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let line = place_solid_sprite(&mut vic, &mut ram, 0, 100, 40);
        place_solid_sprite(&mut vic, &mut ram, 1, 100, 40);
        // Sprite 1's row has only its right-most pixel set; sprite 0 is
        // solid but they are 12 px apart, so the shared columns are clear.
        vic.write(0x02, 24 + 112);
        ram[2 * 64..2 * 64 + 3].copy_from_slice(&[0x00, 0x00, 0x01]);
        vic.scan_sprite_collisions(line, &ram, &[0; 2048], &[0; 1000]);
        // Sprite 1's lit pixel is at x = 112 + 23 = 135, past sprite 0's
        // 24-pixel span (100..124).
        assert_eq!(vic.read(0x1E), 0);
    }

    #[test]
    fn sprite_sprite_collision_works_in_the_border_outside_the_display_window() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        // Row 0 of both sprites is on raster line 51 - 20 = 31: top border.
        vic.write(0x00, 24 + 10);
        vic.write(0x01, 30);
        vic.write(0x02, 24 + 10);
        vic.write(0x03, 30);
        vic.write(0x15, 0b11);
        ram[0x3F8] = 1;
        ram[0x3F9] = 2;
        ram[64] = 0xFF;
        ram[128] = 0xFF;
        vic.scan_sprite_collisions(31, &ram, &[0; 2048], &[0; 1000]);
        assert_eq!(vic.read(0x1E), 0b11);
        assert_eq!(vic.read(0x1F), 0, "no background in the border");
    }

    /// A solid one-row sprite whose raw 9-bit `MxX` is `raw_x` (not offset by
    /// the usual 24), at register Y 30 (raster line 31, in the top border).
    fn place_wrapping_sprite(vic: &mut Vic, ram: &mut [u8], index: u8, raw_x: u16) {
        vic.write(index * 2, (raw_x & 0xFF) as u8);
        let msb = vic.read(0x10) & !(1 << index) | (((raw_x >> 8) as u8 & 1) << index);
        vic.write(0x10, msb);
        vic.write(index * 2 + 1, 30);
        vic.write(0x15, vic.read(0x15) | (1 << index));
        let pointer = 1 + index;
        ram[0x3F8 + index as usize] = pointer;
        ram[pointer as usize * 64..pointer as usize * 64 + 3].copy_from_slice(&[0xFF; 3]);
    }

    #[test]
    fn sprites_collide_across_the_wrap_of_the_504_pixel_line() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        // Sprite 0 covers raster x 500..503 and then 0..19; sprite 1 covers 5..28.
        place_wrapping_sprite(&mut vic, &mut ram, 0, 500);
        place_wrapping_sprite(&mut vic, &mut ram, 1, 5);
        vic.scan_sprite_collisions(31, &ram, &[0; 2048], &[0; 1000]);
        assert_eq!(vic.read(0x1E), 0b11, "the wrapped tail overlaps sprite 1");
    }

    #[test]
    fn an_mxx_of_504_or_more_never_starts_so_it_never_collides() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        place_wrapping_sprite(&mut vic, &mut ram, 0, 504);
        place_wrapping_sprite(&mut vic, &mut ram, 1, 5);
        vic.scan_sprite_collisions(31, &ram, &[0; 2048], &[0; 1000]);
        assert_eq!(vic.read(0x1E), 0);
        // One pixel below the limit still shows.
        place_wrapping_sprite(&mut vic, &mut ram, 0, 503);
        vic.scan_sprite_collisions(31, &ram, &[0; 2048], &[0; 1000]);
        assert_eq!(vic.read(0x1E), 0b11);
    }

    #[test]
    fn a_wide_sprite_at_the_right_end_of_the_line_reappears_at_the_left_of_the_window() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        // MxX 503, X-expanded: 48 pixels, 503 then 0..46 -- the window starts
        // at raster x 24, so canvas columns 0..22 show it.
        place_wrapping_sprite(&mut vic, &mut ram, 0, 503);
        vic.write(0x1D, 1);
        vic.write(0x27, 1); // white
        vic.write(0x01, 50); // canvas row 0
        let mut rgb = vec![0u8; 320 * 200 * 3];
        vic.render_sprites(&mut rgb, &ram, &[0; 2048], &[0; 1000]);
        let white = PALETTE[1];
        let pixel = |x: usize| (rgb[x * 3], rgb[x * 3 + 1], rgb[x * 3 + 2]);
        assert_eq!(pixel(0), white);
        assert_eq!(pixel(22), white);
        assert_eq!(pixel(23), (0, 0, 0));
        assert_eq!(pixel(319), (0, 0, 0));
    }

    /// Cycles the VIC-II steals over one frame of 63-cycle ticks with the
    /// display off (so no bad line adds to it).
    fn stolen_over_a_frame(vic: &mut Vic) -> u32 {
        vic.write(0x11, 0x0B);
        (0..312).map(|_| vic.tick(63).stolen_cycles).sum()
    }

    fn enable_sprite_at(vic: &mut Vic, n: u8, y: u8) {
        vic.write(0x15, vic.read(0x15) | (1 << n));
        vic.write(n * 2 + 1, y);
    }

    #[test]
    fn line_mode_charges_a_lone_sprite_five_cycles_for_each_of_its_21_lines() {
        let mut vic = Vic::new();
        enable_sprite_at(&mut vic, 0, 100);
        assert_eq!(stolen_over_a_frame(&mut vic), 21 * 5);
        // Same for a sprite that fetches at the start of the line.
        let mut vic = Vic::new();
        enable_sprite_at(&mut vic, 5, 100);
        assert_eq!(stolen_over_a_frame(&mut vic), 21 * 5);
    }

    #[test]
    fn line_mode_shares_the_ba_warning_between_adjacent_sprites() {
        // 3 + 3 x 2 per line, as the cycle engine measures.
        let mut vic = Vic::new();
        for n in 0..3 {
            enable_sprite_at(&mut vic, n, 100);
        }
        assert_eq!(stolen_over_a_frame(&mut vic), 21 * 9);
        // Sprites 2 and 3 run into each other across the line boundary:
        // 5 for the first line, 7 for the next 20, 2 for the last.
        let mut vic = Vic::new();
        enable_sprite_at(&mut vic, 2, 100);
        enable_sprite_at(&mut vic, 3, 100);
        assert_eq!(stolen_over_a_frame(&mut vic), 5 + 20 * 7 + 2);
        // Sprites 0 and 2 are not adjacent: two warnings.
        let mut vic = Vic::new();
        enable_sprite_at(&mut vic, 0, 100);
        enable_sprite_at(&mut vic, 2, 100);
        assert_eq!(stolen_over_a_frame(&mut vic), 21 * 10);
    }

    #[test]
    fn line_mode_charges_a_y_expanded_sprite_for_all_42_of_its_lines() {
        let mut vic = Vic::new();
        enable_sprite_at(&mut vic, 0, 100);
        vic.write(0x17, 1);
        assert_eq!(stolen_over_a_frame(&mut vic), 42 * 5);
    }

    #[test]
    fn line_mode_charges_nothing_without_sprites_and_a_bad_line_still_adds_its_own() {
        let mut vic = Vic::new();
        assert_eq!(stolen_over_a_frame(&mut vic), 0);
        let mut vic = Vic::new();
        enable_sprite_at(&mut vic, 0, 100);
        // Display on: 25 bad lines of 40 on top of the sprite.
        vic.write(0x11, 0x1B);
        let total: u32 = (0..312).map(|_| vic.tick(63).stolen_cycles).sum();
        assert_eq!(total, 25 * 40 + 21 * 5);
    }

    #[test]
    fn a_tick_spanning_several_lines_steps_each_of_them() {
        let mut vic = Vic::new();
        enable_sprite_at(&mut vic, 0, 100);
        vic.write(0x11, 0x0B);
        let mut total = 0;
        // 3 lines at a time (a long DMA chunk): the sprite's 21 lines are
        // still all counted.
        for _ in 0..104 {
            total += vic.tick(63 * 3).stolen_cycles;
        }
        assert_eq!(total, 21 * 5);
    }

    #[test]
    fn sprite_over_a_foreground_pixel_sets_d01f_even_when_drawn_behind() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        // Cell 0: char 1 with its first glyph row solid.
        vic.write(0x18, 0x08); // character generator in RAM at $2000
        ram[0] = 1;
        ram[0x2008] = 0xFF;
        let line = place_solid_sprite(&mut vic, &mut ram, 2, 0, 0);
        vic.write(0x1B, 0b100); // behind the background: still collides

        vic.scan_sprite_collisions(line, &ram, &[], &[0; 1000]);
        assert_eq!(vic.read(0x1F), 0b100);
        assert_eq!(vic.read(0x1E), 0);

        // The same sprite over *paper* (glyph row empty) does not collide.
        ram[0x2008] = 0x00;
        vic.read_clearing(0x1F);
        vic.scan_sprite_collisions(line, &ram, &[], &[0; 1000]);
        assert_eq!(vic.read(0x1F), 0);
    }

    #[test]
    fn collision_interrupts_latch_only_on_the_zero_to_nonzero_edge_and_need_d01a() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let line = place_solid_sprite(&mut vic, &mut ram, 0, 50, 10);
        place_solid_sprite(&mut vic, &mut ram, 1, 50, 10);

        // Not enabled: latched in $D019 bit 2, but no IRQ requested.
        assert!(!vic.scan_sprite_collisions(line, &ram, &[0; 2048], &[0; 1000]));
        assert_eq!(vic.read(0x19) & 0x04, 0x04);
        assert_eq!(vic.read(0x19) & 0x80, 0, "bit 7 follows $D01A");

        // Acknowledge the latch, enable it; the register still holds the old
        // collision, so a second hit is not a new edge.
        vic.write(0x19, 0x04);
        vic.write(0x1A, 0x04);
        assert!(!vic.scan_sprite_collisions(line, &ram, &[0; 2048], &[0; 1000]));
        // Read $D01E (clears it) and the next collision interrupts.
        vic.read_clearing(0x1E);
        assert!(vic.scan_sprite_collisions(line, &ram, &[0; 2048], &[0; 1000]));
        assert_eq!(vic.read(0x19) & 0x84, 0x84);
    }

    #[test]
    fn expanded_and_multicolor_sprites_use_their_real_width_and_height() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let line = place_solid_sprite(&mut vic, &mut ram, 0, 100, 10);
        vic.write(0x1D, 0b01); // sprite 0 doubled: 100..148
        place_solid_sprite(&mut vic, &mut ram, 1, 140, 10);
        vic.scan_sprite_collisions(line, &ram, &[0; 2048], &[0; 1000]);
        assert_eq!(
            vic.read(0x1E),
            0b11,
            "overlap exists only because of X expansion"
        );

        // Y expansion: sprite 0's solid row covers lines 10 and 11 of the canvas.
        vic.read_clearing(0x1E);
        vic.write(0x17, 0b01);
        vic.scan_sprite_collisions(line + 1, &ram, &[0; 2048], &[0; 1000]);
        assert_eq!(vic.read(0x1E), 0, "sprite 1 only has the one solid row");
        place_solid_sprite(&mut vic, &mut ram, 2, 100, 11);
        vic.scan_sprite_collisions(line + 1, &ram, &[0; 2048], &[0; 1000]);
        assert_eq!(vic.read(0x1E), 0b101);
    }

    #[test]
    fn ntsc_has_65_cycle_lines_and_263_lines_per_frame() {
        let mut vic = Vic::new();
        vic.set_ntsc(true);
        assert!(vic.is_ntsc());
        assert_eq!(vic.tick(NTSC_CYCLES_PER_LINE * 3).entered_line, Some(3));
        // A PAL-length 63 cycles is not yet a new line here.
        let mut vic = Vic::new();
        vic.set_ntsc(true);
        assert_eq!(vic.tick(CYCLES_PER_LINE).entered_line, None);
        // The frame wraps after 263 lines, not 312.
        let mut vic = Vic::new();
        vic.set_ntsc(true);
        vic.tick(NTSC_CYCLES_PER_LINE * 262);
        assert_eq!(vic.read(0x12), (262 & 0xFF) as u8);
        assert_eq!(vic.tick(NTSC_CYCLES_PER_LINE).entered_line, Some(0));
    }

    #[test]
    fn tick_reports_the_line_it_entered() {
        let mut vic = Vic::new();
        assert_eq!(vic.tick(10).entered_line, None);
        assert_eq!(vic.tick(CYCLES_PER_LINE).entered_line, Some(1));
    }

    #[test]
    fn cpu_writes_to_the_collision_registers_are_ignored() {
        let mut vic = Vic::new();
        vic.write(0x1E, 0xFF);
        vic.write(0x1F, 0xFF);
        assert_eq!((vic.read(0x1E), vic.read(0x1F)), (0, 0));
    }

    fn blank_canvas() -> Vec<u8> {
        vec![0u8; (320 * 200 * 3) as usize]
    }

    #[test]
    fn hires_sprite_renders_at_the_top_left_of_the_active_area() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let char_rom = vec![0u8; 2048];
        let color_ram = vec![0u8; 1000];

        vic.write(0x00, 24); // sprite 0 X low -> screen column 0
        vic.write(0x01, 50); // sprite 0 Y -> screen row 0
        vic.write(0x15, 0x01); // enable sprite 0
        vic.write(0x27, 0x02); // sprite 0 color: red
        ram[0x3F8] = 1; // sprite 0's data pointer (video matrix base $0000)
        ram[64] = 0xFF; // pointer*64 = 64: first data byte, all bits set

        let mut rgb = blank_canvas();
        vic.render_sprites(&mut rgb, &ram, &char_rom, &color_ram);

        let px = |n: usize| &rgb[n * 3..n * 3 + 3];
        assert_eq!(px(0), &[PALETTE[2].0, PALETTE[2].1, PALETTE[2].2]);
        // Outside the sprite's own 24-pixel-wide row: untouched (black).
        assert_eq!(px(30), &[0x00, 0x00, 0x00]);
    }

    #[test]
    fn disabled_sprite_is_never_drawn() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let char_rom = vec![0u8; 2048];
        let color_ram = vec![0u8; 1000];

        vic.write(0x00, 24);
        vic.write(0x01, 50);
        // $D015 left at 0 -- sprite 0 not enabled.
        vic.write(0x27, 0x02);
        ram[0x3F8] = 1;
        ram[64] = 0xFF;

        let mut rgb = blank_canvas();
        vic.render_sprites(&mut rgb, &ram, &char_rom, &color_ram);
        assert!(rgb.iter().all(|&b| b == 0));
    }

    #[test]
    fn sprite_x_msb_from_d010_extends_the_9_bit_x_range() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let char_rom = vec![0u8; 2048];
        let color_ram = vec![0u8; 1000];

        vic.write(0x00, 0); // X low = 0
        vic.write(0x10, 0x01); // sprite 0's X MSB set -> raw X = 256
        vic.write(0x01, 50);
        vic.write(0x15, 0x01);
        vic.write(0x27, 0x02);
        ram[0x3F8] = 1;
        ram[64] = 0xFF;

        let mut rgb = blank_canvas();
        vic.render_sprites(&mut rgb, &ram, &char_rom, &color_ram);

        // Screen column = 256 - SPRITE_X_OFFSET(24) = 232.
        let px = |n: usize| &rgb[n * 3..n * 3 + 3];
        assert_eq!(px(232), &[PALETTE[2].0, PALETTE[2].1, PALETTE[2].2]);
        assert_eq!(px(0), &[0x00, 0x00, 0x00]);
    }

    #[test]
    fn multicolor_sprite_maps_all_four_pixel_pair_values() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let char_rom = vec![0u8; 2048];
        let color_ram = vec![0u8; 1000];

        vic.write(0x00, 24);
        vic.write(0x01, 50);
        vic.write(0x15, 0x01);
        vic.write(0x1C, 0x01); // multicolor enabled for sprite 0
        vic.write(0x25, 0x06); // MM0: blue
        vic.write(0x26, 0x05); // MM1: green
        vic.write(0x27, 0x02); // sprite 0's own color: red
        ram[0x3F8] = 1;
        // First data byte's 4 pixel-pairs: 00 01 10 11 -> transparent,
        // MM0 (blue), own (red), MM1 (green).
        ram[64] = 0b00_01_10_11;

        let mut rgb = blank_canvas();
        vic.render_sprites(&mut rgb, &ram, &char_rom, &color_ram);

        let px = |n: usize| &rgb[n * 3..n * 3 + 3];
        assert_eq!(px(0), &[0x00, 0x00, 0x00]); // transparent: background shows
        assert_eq!(px(2), &[PALETTE[6].0, PALETTE[6].1, PALETTE[6].2]); // MM0 blue
        assert_eq!(px(4), &[PALETTE[2].0, PALETTE[2].1, PALETTE[2].2]); // own red
        assert_eq!(px(6), &[PALETTE[5].0, PALETTE[5].1, PALETTE[5].2]); // MM1 green
    }

    #[test]
    fn x_expansion_doubles_sprite_pixel_width() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let char_rom = vec![0u8; 2048];
        let color_ram = vec![0u8; 1000];

        vic.write(0x00, 24);
        vic.write(0x01, 50);
        vic.write(0x15, 0x01);
        vic.write(0x1D, 0x01); // X-expand sprite 0
        vic.write(0x27, 0x02);
        ram[0x3F8] = 1;
        ram[64] = 0x80; // only the leftmost source pixel set

        let mut rgb = blank_canvas();
        vic.render_sprites(&mut rgb, &ram, &char_rom, &color_ram);

        let px = |n: usize| &rgb[n * 3..n * 3 + 3];
        assert_eq!(px(0), &[PALETTE[2].0, PALETTE[2].1, PALETTE[2].2]);
        assert_eq!(px(1), &[PALETTE[2].0, PALETTE[2].1, PALETTE[2].2]);
        assert_eq!(px(2), &[0x00, 0x00, 0x00]); // back to background
    }

    #[test]
    fn y_expansion_doubles_sprite_pixel_height() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let char_rom = vec![0u8; 2048];
        let color_ram = vec![0u8; 1000];

        vic.write(0x00, 24);
        vic.write(0x01, 50);
        vic.write(0x15, 0x01);
        vic.write(0x17, 0x01); // Y-expand sprite 0
        vic.write(0x27, 0x02);
        ram[0x3F8] = 1;
        ram[64] = 0xFF; // row 0 only

        let mut rgb = blank_canvas();
        vic.render_sprites(&mut rgb, &ram, &char_rom, &color_ram);

        let width = 320usize;
        let px_at = |x: usize, y: usize| {
            let idx = (y * width + x) * 3;
            &rgb[idx..idx + 3]
        };
        assert_eq!(px_at(0, 0), &[PALETTE[2].0, PALETTE[2].1, PALETTE[2].2]);
        assert_eq!(px_at(0, 1), &[PALETTE[2].0, PALETTE[2].1, PALETTE[2].2]);
        assert_eq!(px_at(0, 2), &[0x00, 0x00, 0x00]); // row 1's data byte is 0
    }

    #[test]
    fn sprite_0_always_wins_over_sprite_7_regardless_of_write_order() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let char_rom = vec![0u8; 2048];
        let color_ram = vec![0u8; 1000];

        // Both sprites fully overlap at the same position.
        for sprite in [0usize, 7] {
            vic.write((sprite * 2) as u8, 24);
            vic.write((sprite * 2 + 1) as u8, 50);
        }
        vic.write(0x15, 0b1000_0001); // enable sprites 0 and 7
        vic.write(0x27, 0x02); // sprite 0: red
        vic.write(0x27 + 7, 0x06); // sprite 7: blue
        ram[0x3F8] = 1; // sprite 0 pointer
        ram[0x3F8 + 7] = 1; // sprite 7 pointer -- same data
        ram[64] = 0xFF;

        let mut rgb = blank_canvas();
        vic.render_sprites(&mut rgb, &ram, &char_rom, &color_ram);

        // Sprite 0 (drawn last) must win over sprite 7 (drawn first),
        // regardless of which one was configured first above.
        assert_eq!(&rgb[0..3], &[PALETTE[2].0, PALETTE[2].1, PALETTE[2].2]);
    }

    #[test]
    fn behind_background_sprite_hides_under_foreground_but_shows_over_background() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let color_ram = vec![0u8; 1000];

        // Text mode, cell 0: char code 1, glyph row 0 = 0xF0 (left half
        // ink/foreground, right half background/paper).
        ram[0] = 1; // screen RAM (video matrix base $0000) cell 0
        vic.write(0x18, 0x08); // character generator in RAM at $2000
        ram[0x2008] = 0xF0;

        // Sprite 0 spans columns 0-23 of row 0, MxDP=1 (behind background).
        vic.write(0x00, 24);
        vic.write(0x01, 50);
        vic.write(0x15, 0x01);
        vic.write(0x1B, 0x01); // behind background
        vic.write(0x27, 0x02); // red
        ram[0x3F8] = 1;
        ram[64] = 0xFF; // sprite fully opaque across its own row

        let (_, _, rgb) = vic.render(&ram, &[], &color_ram);
        let px = |n: usize| &rgb[n * 3..n * 3 + 3];
        // Column 0: background is foreground (ink) here -> background wins.
        assert_ne!(px(0), &[PALETTE[2].0, PALETTE[2].1, PALETTE[2].2]);
        // Column 4: background is paper (not foreground) -> sprite shows.
        assert_eq!(px(4), &[PALETTE[2].0, PALETTE[2].1, PALETTE[2].2]);
    }

    #[test]
    fn sprite_positioned_off_canvas_is_clipped_without_panicking() {
        let mut vic = Vic::new();
        let mut ram = vec![0u8; 0x10000];
        let char_rom = vec![0u8; 2048];
        let color_ram = vec![0u8; 1000];

        vic.write(0x00, 0); // raw X 0 -> screen x = 0 - 24 = -24 (fully off left edge)
        vic.write(0x01, 0); // raw Y 0 -> screen y = 0 - 50 = -50 (fully off top edge)
        vic.write(0x15, 0x01);
        vic.write(0x27, 0x02);
        ram[0x3F8] = 1;
        ram[64] = 0xFF;
        ram[65] = 0xFF;
        ram[66] = 0xFF;

        let mut rgb = blank_canvas();
        vic.render_sprites(&mut rgb, &ram, &char_rom, &color_ram); // must not panic
        assert!(rgb.iter().all(|&b| b == 0));
    }

    /// The original per-bit-branch algorithm `render_text` used before the
    /// `wide`-based rewrite above -- kept only here, as a reference to
    /// check the SIMD version against, and to time in
    /// `bench_render_text_simd_vs_naive_scalar` below.
    fn render_text_naive_reference(
        vic: &Vic,
        screen_ram: &[u8],
        color_ram: &[u8],
        char_rom: &[u8],
    ) -> Vec<u8> {
        let width = TEXT_COLS * CHAR_PX;
        let height = TEXT_ROWS * CHAR_PX;
        let mut rgb = vec![0u8; (width * height * 3) as usize];
        let bg = vic.background_color0();

        for row in 0..TEXT_ROWS {
            for col in 0..TEXT_COLS {
                let cell = (row * TEXT_COLS + col) as usize;
                let code = screen_ram[cell];
                let ink = PALETTE[(color_ram[cell] & 0x0F) as usize];
                let glyph_base = code as usize * 8;

                for line in 0..CHAR_PX {
                    let bits = char_rom[glyph_base + line as usize];
                    for bit in 0..CHAR_PX {
                        let set = bits & (0x80 >> bit) != 0;
                        let color = if set { ink } else { bg };
                        let px = col * CHAR_PX + bit;
                        let py = row * CHAR_PX + line;
                        let idx = ((py * width + px) * 3) as usize;
                        rgb[idx] = color.0;
                        rgb[idx + 1] = color.1;
                        rgb[idx + 2] = color.2;
                    }
                }
            }
        }
        rgb
    }

    /// Deterministic pseudo-random byte stream (no `rand` dependency
    /// needed for a test) -- xorshift32, seeded per call site so repeated
    /// test runs are reproducible.
    fn xorshift_bytes(seed: u32, len: usize) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xFF) as u8
            })
            .collect()
    }

    #[test]
    fn simd_render_matches_the_naive_reference_on_varied_data() {
        let mut vic = Vic::new();
        vic.write(0x20, 3);
        vic.write(0x21, 11);

        let screen = xorshift_bytes(1, 1000);
        let color = xorshift_bytes(2, 1000);
        let chars = xorshift_bytes(3, 2048);

        let (_, _, simd) = vic.render_text(&screen, &color, &chars);
        let naive = render_text_naive_reference(&vic, &screen, &color, &chars);

        assert_eq!(
            simd, naive,
            "SIMD and naive-reference renders must be pixel-identical"
        );
    }

    /// Real measured timing, this machine's own CPU, run manually (not
    /// part of the normal test suite -- `#[ignore]`d, and `cargo test`
    /// alone builds unoptimized, which wouldn't show a real SIMD win):
    ///
    /// ```text
    /// cargo test --release -p vic-ii -- --ignored --nocapture bench_render_text
    /// ```
    #[test]
    #[ignore]
    fn bench_render_text_simd_vs_naive_scalar() {
        let mut vic = Vic::new();
        vic.write(0x20, 3);
        vic.write(0x21, 11);
        let screen = xorshift_bytes(1, 1000);
        let color = xorshift_bytes(2, 1000);
        let chars = xorshift_bytes(3, 2048);

        const ITERS: u32 = 20_000;

        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            std::hint::black_box(vic.render_text(&screen, &color, &chars));
        }
        let simd_elapsed = start.elapsed();

        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            std::hint::black_box(render_text_naive_reference(&vic, &screen, &color, &chars));
        }
        let naive_elapsed = start.elapsed();

        println!(
            "SIMD:  {ITERS} frames in {simd_elapsed:?} ({:.2} us/frame)",
            simd_elapsed.as_secs_f64() * 1e6 / ITERS as f64
        );
        println!(
            "naive: {ITERS} frames in {naive_elapsed:?} ({:.2} us/frame)",
            naive_elapsed.as_secs_f64() * 1e6 / ITERS as f64
        );
        println!(
            "speedup: {:.2}x",
            naive_elapsed.as_secs_f64() / simd_elapsed.as_secs_f64()
        );
    }

    #[test]
    fn a_light_pen_latches_position_and_interrupt_once_per_frame() {
        let mut vic = Vic::default();
        vic.write(0x1A, 0x08); // light pen interrupt enabled
        assert!(vic.light_pen_trigger(100, 40));
        assert_eq!(vic.read(0x13), ((100 + 24) >> 1) as u8);
        assert_eq!(vic.read(0x14), (40 + 51) as u8);
        assert_eq!(vic.read(0x19) & 0x88, 0x88, "bit 3 set and /IRQ asserted");

        // A second touch in the same frame changes nothing.
        assert!(!vic.light_pen_trigger(10, 10));
        assert_eq!(vic.read(0x13), ((100 + 24) >> 1) as u8);

        // The CPU cannot overwrite the latches, and write-1-to-clear
        // acknowledges the interrupt.
        vic.write(0x13, 0);
        assert_eq!(vic.read(0x13), ((100 + 24) >> 1) as u8);
        vic.write(0x19, 0x08);
        assert_eq!(vic.read(0x19) & 0x0F, 0);

        // A new frame lets the pen latch again.
        let frame = vic.cycles_per_frame();
        vic.tick(frame - 1);
        vic.tick(2);
        assert!(vic.light_pen_trigger(0, 0));
        assert_eq!(vic.read(0x13), 12);
    }

    #[test]
    fn the_light_pen_interrupt_is_not_requested_while_masked() {
        let mut vic = Vic::default();
        assert!(!vic.light_pen_trigger(5, 5));
        assert_eq!(vic.read(0x19) & 0x08, 0x08, "latched even when masked");
    }
}
