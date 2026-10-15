//! Cycle-accurate VIC-II engine (PAL 6569, 63 cycles x 312 lines).
//!
//! The default model (`Vic::tick`) is line accurate: it advances a raster
//! counter by whole instructions and charges a flat 40-cycle steal per bad
//! line. This module is the alternative: [`Vic::cycle`] runs *one clock
//! cycle* of the chip's real schedule, so a host that calls it once per CPU
//! bus cycle sees the chip as the 6510 does -- bad lines, sprite DMA, the
//! BA/AEC bus arbitration that stalls the CPU, and the internal VC/VCBASE/
//! RC/VMLI counters, all evolving cycle by cycle (which is what makes FLD,
//! mid-line register changes and sprite multiplexing behave).
//!
//! Everything here follows Christian Bauer's "The MOS 6567/6569 video
//! controller (VIC-II) and its application in the Commodore 64" (sections
//! 3.6 cycle table, 3.7 display state/bad lines, 3.8 sprites; see
//! `CREDITS.md`): the per-cycle schedule below is that article's PAL table.
//!
//! Two halves: the *bus side* -- every c-, g-, p- and s-access is performed
//! on its real cycle against a [`VicMemory`], the counters and display/idle
//! state follow the article's rules, and BA is reported per cycle -- and the
//! *pixel side*: each cycle also emits its eight pixels (graphics sequencer
//! for all five modes with XSCROLL, the eight sprite shifters with priority
//! and expansion, and the border unit's two flip flops with CSEL/RSEL
//! comparisons) into a 384x264 frame ([`Vic::cycle_frame`]), so a register
//! write in the middle of a line changes the picture from the next
//! character cell on -- border opening, split screens, FLD and sprite
//! multiplexing show up as they do on a monitor.
//!
//! Known simplifications, stated so they are not mistaken for hardware
//! facts: the c-accesses of a bad line that starts after cycle 15 read the
//! right data here (real silicon reads whatever the CPU left on the bus for
//! the first few); a register write takes effect at the next 8-pixel cell
//! (real colour/mode switches land a pixel or two into the cell, which some
//! demo effects exploit); sprite pixels wrap at the 504-pixel line but the
//! sprite-versus-sprite/background collision flags stay the per-line scan
//! (`Vic::scan_sprite_collisions`); NTSC (65-cycle) schedules are not
//! implemented. Sprite crunch falls out of the real flip flop rules: MCBASE is a
//! 6-bit counter, the Y-expansion flip flop is held set while MxYE is clear and
//! a write that clears MxYE sets it for good (`Vic::write`), so clearing the
//! bit around cycles 15/16 advances a row that should have repeated. The vertical border flip flop is also set
//! when the frame wraps (the comparisons alone would leave it clear after an
//! opened bottom border and so open the next frame's top border as well); that
//! is the one rule here that comes from how demos behave, not from the
//! article.

use super::{
    BMM_BIT, DEN_BIT, ECM_BIT, FIRST_DMA_LINE, IRQ_RASTER, LAST_DMA_LINE, MCM_BIT, PALETTE,
    REG_BACKGROUND_COLOR0, REG_BORDER_COLOR, REG_CONTROL1, REG_CONTROL2, REG_SPRITE_COLOR_BASE,
    REG_SPRITE_ENABLE, REG_SPRITE_MULTICOLOR_ENABLE, REG_SPRITE_MULTICOLOR0,
    REG_SPRITE_MULTICOLOR1, REG_SPRITE_PRIORITY, REG_SPRITE_X_EXPAND, REG_SPRITE_X_MSB,
    REG_SPRITE_Y_EXPAND, SPRITE_COUNT, SPRITE_POINTER_OFFSET, Vic,
};

/// Cycles per PAL raster line, and the schedule's first/last cycles of the
/// phases below (Bauer's numbering, 1-based).
const LINE_CYCLES: u32 = 63;
/// BA goes low for a bad line no earlier than this cycle...
const BA_CHAR_FIRST: u32 = 12;
/// ...the first c-access can be done in this cycle (BA + 3, never earlier)...
const C_ACCESS_FIRST: u32 = 15;
/// ...and the last c-access is done in this cycle.
const C_ACCESS_LAST: u32 = 54;
/// g-accesses run in the first phase of cycles 16-55.
const G_ACCESS_FIRST: u32 = 16;
const G_ACCESS_LAST: u32 = 55;
/// VC <- VCBASE, VMLI <- 0 (and RC <- 0 on a bad line).
const VC_LOAD_CYCLE: u32 = 14;
/// The two cycles that check whether a sprite's DMA starts.
const SPRITE_DMA_CHECK_FIRST: u32 = 55;
const SPRITE_DMA_CHECK_LAST: u32 = 56;
/// Sprite MC <- MCBASE and display-on check.
const SPRITE_DISPLAY_CHECK: u32 = 58;
/// RC == 7 check / VCBASE <- VC.
const RC_CHECK_CYCLE: u32 = 58;
/// MCBASE += 2 / += 1 (Y-expansion flip flop set).
const MCBASE_ADVANCE_A: u32 = 15;
const MCBASE_ADVANCE_B: u32 = 16;
/// The cycle in which each sprite's pointer is fetched (p-access); the
/// three data bytes follow in phi2 of that cycle and both phases of the next.
const SPRITE_P_CYCLE: [u32; SPRITE_COUNT] = [58, 60, 62, 1, 3, 5, 7, 9];
/// Cycles of BA warning before a sprite's first s-access.
const BA_WARNING: i32 = 3;
const CHAR_COLUMNS: usize = 40;
/// The picture the engine produces: the 320x200 window with a 32-pixel
/// border on every side, in the same shape the line-accurate path hands out.
pub const FRAME_WIDTH: usize = 384;
pub const FRAME_HEIGHT: usize = 264;
/// Raster line shown in the frame's first row (display starts at line 51).
const FRAME_FIRST_LINE: u32 = 19;
/// Pixels in a PAL raster line.
const LINE_PIXELS: u32 = 504;
/// Cycles that cover the frame's columns (x = 496..503 then 0..375).
const VISIBLE_FIRST_CYCLE: u32 = 13;
const VISIBLE_LAST_CYCLE: u32 = 60;
/// Idle-state g-access address (and its ECM variant).
const IDLE_ADDRESS: u16 = 0x3FFF;
const IDLE_ADDRESS_ECM: u16 = 0x39FF;

/// What the chip can see: the 16K it addresses (RAM, the character ROM
/// shadow, a cartridge's Ultimax window -- all resolved by the host) and the
/// 4-bit colour RAM. Implemented by the machine around the chip.
pub trait VicMemory {
    /// The byte at a 14-bit VIC address (`addr & 0x3FFF`).
    fn vic_read(&self, addr: u16) -> u8;
    /// Colour RAM nibble `index` (`0..1000`).
    fn color_ram(&self, index: u16) -> u8;
}

/// One character cell as the sequencer sees it: the graphics byte a g-access
/// fetched plus the c-access data (matrix byte, colour nibble) that goes with
/// it. The idle state's cell has zero c-data.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct Cell {
    g: u8,
    m: u8,
    c: u8,
}

/// The engine's internal state (serialised with the chip).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct CycleEngine {
    pub(crate) enabled: bool,
    /// Video counter (10 bits), its line-start copy, row counter (3 bits)
    /// and the video-matrix line index (0-39).
    vc: u16,
    vcbase: u16,
    rc: u8,
    vmli: u8,
    /// Display state (true) or idle state (false).
    display: bool,
    /// c-accesses of this line are under way / when the first one is done.
    char_dma: bool,
    char_dma_from: u32,
    /// The video matrix line the c-accesses fill (character codes) and its
    /// colour nibbles, and what the g-accesses read for each column.
    matrix: Vec<u8>,
    colors: Vec<u8>,
    graphics: Vec<u8>,
    /// Sprite state, bit n = sprite n.
    sprite_dma: u8,
    sprite_display: u8,
    expansion_ff: u8,
    mcbase: [u8; SPRITE_COUNT],
    mc: [u8; SPRITE_COUNT],
    pointer: [u8; SPRITE_COUNT],
    /// The three bytes each sprite's s-accesses fetched this line.
    sprite_data: Vec<u8>,
    /// BA as of the cycle just run.
    ba: bool,

    /// Graphics sequencer: the cell fetched in the cycle just run (displayed
    /// from the next cycle), the one being displayed and the one before it
    /// (its tail shows through when XSCROLL shifts the picture right).
    cell_pending: Cell,
    cell_cur: Cell,
    cell_prev: Cell,
    /// Border unit flip flops.
    border_main: bool,
    border_vertical: bool,
    /// Sprites displaying in this raster line and their three data bytes,
    /// latched in cycle 1 (sprites 0-2) or as their s-accesses finish.
    line_sprite_display: u8,
    sprite_row: Vec<u8>,
    /// The picture being drawn and the last complete one (palette indices);
    /// transient, so not part of a save state.
    #[serde(skip)]
    frame_building: Vec<u8>,
    #[serde(skip)]
    frame_complete: Vec<u8>,
}

impl CycleEngine {
    /// Forces the Y-expansion flip flop of the sprites in `mask` to set (a
    /// write that clears their MxYE bit does this on the chip).
    pub(crate) fn set_expansion_ff(&mut self, mask: u8) {
        self.expansion_ff |= mask;
    }

    fn reset_buffers(&mut self) {
        self.matrix = vec![0; CHAR_COLUMNS];
        self.colors = vec![0; CHAR_COLUMNS];
        self.graphics = vec![0; CHAR_COLUMNS];
        self.sprite_data = vec![0; SPRITE_COUNT * 3];
        self.sprite_row = vec![0; SPRITE_COUNT * 3];
        // The unit starts with both flip flops set: a closed border.
        self.border_main = true;
        self.border_vertical = true;
    }
}

/// What one [`Vic::cycle`] call reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CycleResult {
    /// A raster-compare match was latched in this cycle and the raster
    /// source is enabled in `$D01A` (edge, as `TickResult::raster_irq`).
    pub raster_irq: bool,
    /// The raster line this cycle began (cycle 1 of a line).
    pub entered_line: Option<u32>,
    /// BA is low in this cycle: the CPU stalls on a read until it rises.
    pub ba_low: bool,
}

impl Vic {
    /// Switches the chip to the cycle engine. PAL only (the 65-cycle NTSC
    /// schedule is not implemented). The next [`Vic::cycle`] runs the cycle
    /// the raster position currently points at.
    pub fn enable_cycle_engine(&mut self) -> Result<(), String> {
        if self.ntsc {
            return Err(
                "the cycle-accurate VIC-II implements the PAL 6569 only; use --region pal"
                    .to_string(),
            );
        }
        if !self.engine.enabled {
            self.engine.enabled = true;
            self.engine.reset_buffers();
            // `cycle` advances before it runs, so step back one to make the
            // first call execute the cycle we are standing on.
            let frame = self.cycles_per_frame();
            self.frame_cycle = (self.frame_cycle + frame - 1) % frame;
        }
        Ok(())
    }

    /// Back to the per-instruction model.
    pub fn disable_cycle_engine(&mut self) {
        if self.engine.enabled {
            self.engine.enabled = false;
            let frame = self.cycles_per_frame();
            self.frame_cycle = (self.frame_cycle + 1) % frame;
        }
    }

    /// The last complete picture as `(width, height, RGB)`, `None` until the
    /// raster has passed the bottom of the first frame after enabling.
    pub fn cycle_frame(&self) -> Option<(u32, u32, Vec<u8>)> {
        let frame = &self.engine.frame_complete;
        if frame.len() != FRAME_WIDTH * FRAME_HEIGHT {
            return None;
        }
        let mut rgb = Vec::with_capacity(frame.len() * 3);
        for &index in frame {
            let (r, g, b) = PALETTE[usize::from(index & 0x0F)];
            rgb.extend_from_slice(&[r, g, b]);
        }
        Some((FRAME_WIDTH as u32, FRAME_HEIGHT as u32, rgb))
    }

    pub fn cycle_engine_enabled(&self) -> bool {
        self.engine.enabled
    }

    /// BA as of the cycle last run: low while the chip needs the bus.
    pub fn ba_low(&self) -> bool {
        self.engine.ba
    }

    /// The cycle (1-63) of the line the chip is in.
    pub fn line_cycle(&self) -> u32 {
        self.frame_cycle % LINE_CYCLES + 1
    }

    /// The 40 character codes and colour nibbles of the current video matrix
    /// line, as filled by the c-accesses.
    pub fn line_matrix(&self) -> (&[u8], &[u8]) {
        (&self.engine.matrix, &self.engine.colors)
    }

    /// The byte each of the 40 columns' g-access fetched in the current line.
    pub fn line_graphics(&self) -> &[u8] {
        &self.engine.graphics
    }

    /// The three bytes sprite `n`'s s-accesses fetched in the current line.
    pub fn sprite_bytes(&self, n: usize) -> [u8; 3] {
        let e = &self.engine;
        [
            e.sprite_data[n * 3],
            e.sprite_data[n * 3 + 1],
            e.sprite_data[n * 3 + 2],
        ]
    }

    /// `(VC, VCBASE, RC, VMLI, display state)`.
    pub fn video_counters(&self) -> (u16, u16, u8, u8, bool) {
        let e = &self.engine;
        (e.vc, e.vcbase, e.rc, e.vmli, e.display)
    }

    /// Bit n set = sprite n's DMA is on / its display is on.
    pub fn sprite_dma_mask(&self) -> u8 {
        self.engine.sprite_dma
    }

    pub fn sprite_display_mask(&self) -> u8 {
        self.engine.sprite_display
    }

    /// MCBASE and MC of sprite `n`.
    pub fn sprite_counters(&self, n: usize) -> (u8, u8) {
        (self.engine.mcbase[n], self.engine.mc[n])
    }

    /// The bad-line condition for the cycle being run (Bauer 3.5): the line
    /// is in `$30-$F7`, its low three bits equal YSCROLL, and DEN was set
    /// somewhere in line `$30` of this frame.
    fn bad_line_now(&self, line: u32) -> bool {
        self.allow_bad_lines
            && (FIRST_DMA_LINE..=LAST_DMA_LINE).contains(&line)
            && (line & 7) == u32::from(self.registers[REG_CONTROL1] & 0x07)
    }

    /// Runs one clock cycle. See the module documentation; the host calls
    /// this once per CPU bus cycle (plus once per stalled cycle) and then
    /// reads [`Vic::ba_low`].
    pub fn cycle<M: VicMemory>(&mut self, mem: &M) -> CycleResult {
        debug_assert!(self.engine.enabled, "enable_cycle_engine first");
        let frame = self.cycles_per_frame();
        self.frame_cycle = (self.frame_cycle + 1) % frame;
        let line = self.frame_cycle / LINE_CYCLES;
        let cyc = self.frame_cycle % LINE_CYCLES + 1;

        let mut result = CycleResult::default();

        if self.frame_cycle == 0 {
            self.light_pen_latched = false;
        }
        if cyc == 1 {
            result.entered_line = Some(line);
            // Every line starts with the c-accesses not yet begun.
            self.engine.char_dma = false;
            self.engine.char_dma_from = 0;
            if line == 0 {
                // VCBASE is cleared somewhere in the blanked top lines.
                self.engine.vcbase = 0;
                self.allow_bad_lines = false;
            }
        }
        // The raster interrupt: cycle 1 of the line (cycle 2 of line 0).
        if (line != 0 && cyc == 1) || (line == 0 && cyc == 2) {
            // Compare against the programmed line, as `tick` does.
            let compare = self.raster_compare_line();
            if line == compare {
                self.irq_status |= IRQ_RASTER;
                result.raster_irq = self.irq_enable() & IRQ_RASTER != 0;
            }
        }
        // The DEN latch: seen set anywhere in line $30 enables bad lines.
        if line == FIRST_DMA_LINE && self.registers[REG_CONTROL1] & DEN_BIT != 0 {
            self.allow_bad_lines = true;
        }

        // Pixels first: they show what earlier cycles fetched, and a CPU
        // write in this cycle takes effect from the next one.
        self.run_sequencer(cyc, line);

        let bad = self.bad_line_now(line);
        self.run_video_logic(mem, cyc, bad);
        self.run_sprite_logic(mem, cyc, line);
        self.finish_cycle(mem, cyc, line);

        self.engine.ba = self.compute_ba(cyc);
        result.ba_low = self.engine.ba;
        result
    }

    /// Display/idle state, the counters, and the c- and g-accesses.
    fn run_video_logic<M: VicMemory>(&mut self, mem: &M, cyc: u32, bad: bool) {
        // A bad line condition in cycles 12-54 puts BA low, starts the
        // c-accesses (no earlier than three cycles later, never before 15)
        // and turns the display state on.
        if bad && (BA_CHAR_FIRST..=C_ACCESS_LAST).contains(&cyc) && !self.engine.char_dma {
            self.engine.char_dma = true;
            self.engine.char_dma_from = (cyc + BA_WARNING as u32).max(C_ACCESS_FIRST);
        }
        if bad && (BA_CHAR_FIRST..=C_ACCESS_LAST).contains(&cyc) {
            self.engine.display = true;
        }

        if cyc == VC_LOAD_CYCLE {
            self.engine.vc = self.engine.vcbase;
            self.engine.vmli = 0;
            if bad {
                self.engine.rc = 0;
            }
        }

        let vm = self.video_matrix_base() as u16;

        // g-access (first phase of 16-55).
        if (G_ACCESS_FIRST..=G_ACCESS_LAST).contains(&cyc) {
            let ecm = self.registers[REG_CONTROL1] & ECM_BIT != 0;
            if self.engine.display {
                let column = usize::from(self.engine.vmli).min(CHAR_COLUMNS - 1);
                let addr = if self.registers[REG_CONTROL1] & BMM_BIT != 0 {
                    let a = self.bitmap_base() as u16
                        | ((self.engine.vc & 0x3FF) << 3)
                        | u16::from(self.engine.rc);
                    if ecm { a & !0x0600 } else { a }
                } else {
                    let code = u16::from(self.engine.matrix[column]);
                    let code = if ecm { code & 0x3F } else { code };
                    self.character_base() as u16 | (code << 3) | u16::from(self.engine.rc)
                };
                let g = mem.vic_read(addr & 0x3FFF);
                self.engine.graphics[column] = g;
                self.engine.cell_pending = Cell {
                    g,
                    m: self.engine.matrix[column],
                    c: self.engine.colors[column],
                };
                self.engine.vc = (self.engine.vc + 1) & 0x3FF;
                self.engine.vmli = self.engine.vmli.saturating_add(1);
            } else {
                let column = usize::from(self.engine.vmli).min(CHAR_COLUMNS - 1);
                let addr = if ecm { IDLE_ADDRESS_ECM } else { IDLE_ADDRESS };
                let g = mem.vic_read(addr);
                self.engine.graphics[column] = g;
                self.engine.cell_pending = Cell { g, m: 0, c: 0 };
            }
        }

        // c-access (second phase of 15-54 once started).
        if self.engine.char_dma
            && (C_ACCESS_FIRST..=C_ACCESS_LAST).contains(&cyc)
            && cyc >= self.engine.char_dma_from
        {
            let column = usize::from(self.engine.vmli).min(CHAR_COLUMNS - 1);
            self.engine.matrix[column] = mem.vic_read((vm | (self.engine.vc & 0x3FF)) & 0x3FFF);
            self.engine.colors[column] = mem.color_ram(self.engine.vc & 0x3FF) & 0x0F;
        }

        if cyc == RC_CHECK_CYCLE {
            if self.engine.rc == 7 {
                self.engine.display = false;
                self.engine.vcbase = self.engine.vc;
            }
            if bad {
                self.engine.display = true;
            }
            if self.engine.display {
                self.engine.rc = (self.engine.rc + 1) & 7;
            }
        }
    }

    /// Sprite DMA state machine and the p-/s-accesses (Bauer 3.8.1).
    fn run_sprite_logic<M: VicMemory>(&mut self, mem: &M, cyc: u32, line: u32) {
        let enabled = self.registers[REG_SPRITE_ENABLE];
        let y_expand = self.registers[REG_SPRITE_Y_EXPAND];
        let line_low = (line & 0xFF) as u8;

        // The flip flop reads as set for any sprite that is not Y-expanded.
        let ff = self.engine.expansion_ff | !y_expand;
        if cyc == MCBASE_ADVANCE_A {
            for n in 0..SPRITE_COUNT {
                if ff & (1 << n) != 0 {
                    self.engine.mcbase[n] = self.engine.mcbase[n].wrapping_add(2) & 0x3F;
                }
            }
        }
        if cyc == MCBASE_ADVANCE_B {
            for n in 0..SPRITE_COUNT {
                if ff & (1 << n) != 0 {
                    self.engine.mcbase[n] = self.engine.mcbase[n].wrapping_add(1) & 0x3F;
                    if self.engine.mcbase[n] == 63 {
                        self.engine.sprite_dma &= !(1 << n);
                        self.engine.sprite_display &= !(1 << n);
                    }
                }
            }
        }
        if cyc == SPRITE_DMA_CHECK_FIRST {
            // The flip flop is held set for sprites that are not Y-expanded
            // and toggles for those that are.
            self.engine.expansion_ff = (self.engine.expansion_ff ^ y_expand) | !y_expand;
        }
        if cyc == SPRITE_DMA_CHECK_FIRST || cyc == SPRITE_DMA_CHECK_LAST {
            for n in 0..SPRITE_COUNT {
                let bit = 1u8 << n;
                if enabled & bit != 0
                    && self.registers[n * 2 + 1] == line_low
                    && self.engine.sprite_dma & bit == 0
                {
                    self.engine.sprite_dma |= bit;
                    self.engine.mcbase[n] = 0;
                    if y_expand & bit != 0 {
                        self.engine.expansion_ff &= !bit;
                    }
                }
            }
        }
        if cyc == SPRITE_DISPLAY_CHECK {
            for n in 0..SPRITE_COUNT {
                let bit = 1u8 << n;
                self.engine.mc[n] = self.engine.mcbase[n];
                if self.engine.sprite_dma & bit != 0
                    && enabled & bit != 0
                    && self.registers[n * 2 + 1] == line_low
                {
                    self.engine.sprite_display |= bit;
                }
            }
        }

        let vm = self.video_matrix_base() as u16;
        for (n, &p) in SPRITE_P_CYCLE.iter().enumerate() {
            let next = p % LINE_CYCLES + 1;
            if cyc == p {
                // p-access: always done, DMA or not.
                self.engine.pointer[n] =
                    mem.vic_read((vm + SPRITE_POINTER_OFFSET as u16 + n as u16) & 0x3FFF);
                if self.engine.sprite_dma & (1 << n) != 0 {
                    self.sprite_fetch(mem, n, 0);
                }
            } else if cyc == next && self.engine.sprite_dma & (1 << n) != 0 {
                self.sprite_fetch(mem, n, 1);
                self.sprite_fetch(mem, n, 2);
                if n >= 3 {
                    // Sprites 3-7 fetch in the line they are displayed in.
                    let base = n * 3;
                    let row: [u8; 3] = [
                        self.engine.sprite_data[base],
                        self.engine.sprite_data[base + 1],
                        self.engine.sprite_data[base + 2],
                    ];
                    self.engine.sprite_row[base..base + 3].copy_from_slice(&row);
                }
            }
        }
    }

    /// One s-access: byte `slot` of sprite `n`'s row, then MC++.
    fn sprite_fetch<M: VicMemory>(&mut self, mem: &M, n: usize, slot: usize) {
        let addr = (u16::from(self.engine.pointer[n]) << 6) | u16::from(self.engine.mc[n] & 0x3F);
        self.engine.sprite_data[n * 3 + slot] = mem.vic_read(addr & 0x3FFF);
        self.engine.mc[n] = self.engine.mc[n].wrapping_add(1);
    }

    /// End-of-cycle housekeeping on the pixel side: outside the g-access
    /// window the chip does idle accesses, which is what shows when the side
    /// border is opened; at the end of a line the vertical border flip flop
    /// is checked against the line that is about to start; and when the last
    /// visible line is done the finished picture is published.
    fn finish_cycle<M: VicMemory>(&mut self, mem: &M, cyc: u32, line: u32) {
        if !(G_ACCESS_FIRST..=G_ACCESS_LAST).contains(&cyc) {
            let ecm = self.registers[REG_CONTROL1] & ECM_BIT != 0;
            let g = mem.vic_read(if ecm { IDLE_ADDRESS_ECM } else { IDLE_ADDRESS });
            self.engine.cell_pending = Cell { g, m: 0, c: 0 };
        }
        if cyc == LINE_CYCLES {
            let next = line + 1;
            let rsel = self.registers[REG_CONTROL1] & 0x08 != 0;
            let (top, bottom) = if rsel { (51, 251) } else { (55, 247) };
            if next == bottom || next >= 312 {
                // `next >= 312`: the frame wraps with the border closed, so a
                // bottom border that was opened does not leave the next
                // frame's top open too (an assumption, see the module docs:
                // the compare logic alone would leave the flip flop clear).
                self.engine.border_vertical = true;
            }
            if next == top && self.registers[REG_CONTROL1] & DEN_BIT != 0 {
                self.engine.border_vertical = false;
            }
            if line == FRAME_FIRST_LINE + FRAME_HEIGHT as u32 - 1 {
                let e = &mut self.engine;
                if e.frame_building.len() == FRAME_WIDTH * FRAME_HEIGHT {
                    std::mem::swap(&mut e.frame_building, &mut e.frame_complete);
                }
            }
        }
    }

    /// The eight pixels of this cycle: border flip flops, graphics, sprites.
    fn run_sequencer(&mut self, cyc: u32, line: u32) {
        {
            let e = &mut self.engine;
            e.cell_prev = e.cell_cur;
            e.cell_cur = e.cell_pending;
            if cyc == 1 {
                // Sprites 0-2 fetched their row at the end of the previous
                // line; the rest fetch in cycles 1-10 below.
                e.line_sprite_display = e.sprite_display;
                let head: Vec<u8> = e.sprite_data[..9].to_vec();
                e.sprite_row[..9].copy_from_slice(&head);
            }
            if e.frame_building.len() != FRAME_WIDTH * FRAME_HEIGHT {
                e.frame_building = vec![0; FRAME_WIDTH * FRAME_HEIGHT];
            }
        }
        if !(VISIBLE_FIRST_CYCLE..=VISIBLE_LAST_CYCLE).contains(&cyc) {
            return;
        }

        let ctl1 = self.registers[REG_CONTROL1];
        let ctl2 = self.registers[REG_CONTROL2];
        let csel = ctl2 & 0x08 != 0;
        let (left, right) = if csel { (24, 344) } else { (31, 335) };
        let rsel = ctl1 & 0x08 != 0;
        let (top, bottom) = if rsel { (51, 251) } else { (55, 247) };
        let den = ctl1 & DEN_BIT != 0;
        let xscroll = i32::from(ctl2 & 7);
        let border_color = self.registers[REG_BORDER_COLOR] & 0x0F;
        let row = line.checked_sub(FRAME_FIRST_LINE).map(|r| r as usize);
        let row = row.filter(|&r| r < FRAME_HEIGHT);

        let x_start = ((cyc + 49) % LINE_CYCLES) * 8;
        let (cur, prev) = (self.engine.cell_cur, self.engine.cell_prev);
        let sprites = self.engine.line_sprite_display;

        for i in 0..8u32 {
            let x = x_start + i;
            // Border unit (Bauer 3.9, items 1, 4, 5, 6).
            if x == right {
                self.engine.border_main = true;
            }
            if x == left {
                if line == bottom {
                    self.engine.border_vertical = true;
                } else if line == top && den {
                    self.engine.border_vertical = false;
                }
                if !self.engine.border_vertical {
                    self.engine.border_main = false;
                }
            }
            let Some(row) = row else { continue };
            let column = if x >= 496 {
                (x - 496) as usize
            } else if x <= 375 {
                (x + 8) as usize
            } else {
                continue;
            };

            let color = if self.engine.border_main {
                border_color
            } else {
                let j = i as i32 - xscroll;
                let (cell, bit) = if j >= 0 {
                    (cur, j as usize)
                } else {
                    (prev, (j + 8) as usize)
                };
                let (mut color, foreground) = self.graphics_pixel(cell, bit);
                if sprites != 0
                    && let Some((sprite_color, behind)) = self.sprite_pixel(sprites, x)
                    && !(behind && foreground)
                {
                    color = sprite_color;
                }
                color
            };
            self.engine.frame_building[row * FRAME_WIDTH + column] = color;
        }
    }

    /// The colour index and the foreground flag (does a sprite behind the
    /// background hide here?) of pixel `bit` (0-7) of `cell` in the current
    /// display mode (Bauer 3.7.3).
    fn graphics_pixel(&self, cell: Cell, bit: usize) -> (u8, bool) {
        let ctl1 = self.registers[REG_CONTROL1];
        let multicolor = self.registers[REG_CONTROL2] & MCM_BIT != 0;
        let bg = |n: usize| self.registers[REG_BACKGROUND_COLOR0 + n] & 0x0F;
        let one = (cell.g >> (7 - bit)) & 1 != 0;
        let pair = (cell.g >> (6 - (bit & !1))) & 3;
        match (ctl1 & ECM_BIT != 0, ctl1 & BMM_BIT != 0, multicolor) {
            (false, false, false) => {
                if one {
                    (cell.c, true)
                } else {
                    (bg(0), false)
                }
            }
            (false, false, true) => {
                if cell.c & 8 == 0 {
                    if one { (cell.c, true) } else { (bg(0), false) }
                } else {
                    match pair {
                        0 => (bg(0), false),
                        1 => (bg(1), false),
                        2 => (bg(2), true),
                        _ => (cell.c & 7, true),
                    }
                }
            }
            (true, false, false) => {
                if one {
                    (cell.c, true)
                } else {
                    (bg(usize::from(cell.m >> 6)), false)
                }
            }
            (false, true, false) => {
                if one {
                    (cell.m >> 4, true)
                } else {
                    (cell.m & 0x0F, false)
                }
            }
            (false, true, true) => match pair {
                0 => (bg(0), false),
                1 => (cell.m >> 4, false),
                2 => (cell.m & 0x0F, true),
                _ => (cell.c, true),
            },
            // ECM together with BMM: the chip shows black.
            _ => (0, one),
        }
    }

    /// The colour of the highest-priority opaque sprite pixel at raster
    /// position `x` of this line, and whether that sprite sits behind the
    /// background (`$D01B`).
    fn sprite_pixel(&self, active: u8, x: u32) -> Option<(u8, bool)> {
        let multicolor_all = self.registers[REG_SPRITE_MULTICOLOR_ENABLE];
        let x_expand_all = self.registers[REG_SPRITE_X_EXPAND];
        for n in 0..SPRITE_COUNT {
            let bit = 1u8 << n;
            if active & bit == 0 {
                continue;
            }
            let msb = u32::from((self.registers[REG_SPRITE_X_MSB] >> n) & 1);
            let sprite_x = u32::from(self.registers[n * 2]) | (msb << 8);
            if sprite_x >= LINE_PIXELS {
                continue;
            }
            let scale = if x_expand_all & bit != 0 { 2 } else { 1 };
            let offset = (x + LINE_PIXELS - sprite_x) % LINE_PIXELS;
            if offset >= 24 * scale {
                continue;
            }
            let col = (offset / scale) as usize;
            let base = n * 3;
            let data = (u32::from(self.engine.sprite_row[base]) << 16)
                | (u32::from(self.engine.sprite_row[base + 1]) << 8)
                | u32::from(self.engine.sprite_row[base + 2]);
            let own = self.registers[REG_SPRITE_COLOR_BASE + n] & 0x0F;
            let color = if multicolor_all & bit != 0 {
                match (data >> (22 - (col & !1))) & 3 {
                    0 => continue,
                    1 => self.registers[REG_SPRITE_MULTICOLOR0] & 0x0F,
                    2 => own,
                    _ => self.registers[REG_SPRITE_MULTICOLOR1] & 0x0F,
                }
            } else if (data >> (23 - col)) & 1 != 0 {
                own
            } else {
                continue;
            };
            let behind = self.registers[REG_SPRITE_PRIORITY] & bit != 0;
            return Some((color, behind));
        }
        None
    }

    /// BA for the cycle just run: low from the cycle a bad line is seen
    /// (12 earliest) through cycle 54, and for every DMA-active sprite from
    /// three cycles before its first s-access through the cycle after its p
    /// cycle.
    fn compute_ba(&self, cyc: u32) -> bool {
        if self.engine.char_dma && (BA_CHAR_FIRST..=C_ACCESS_LAST).contains(&cyc) {
            return true;
        }
        let here = cyc as i32;
        (0..SPRITE_COUNT).any(|n| {
            if self.engine.sprite_dma & (1 << n) == 0 {
                return false;
            }
            let p = SPRITE_P_CYCLE[n] as i32;
            let (from, to) = (p - BA_WARNING, p + 1);
            (from..=to).contains(&here) || (from..=to).contains(&(here - LINE_CYCLES as i32))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16K of RAM plus 1000 colour nibbles.
    struct Mem {
        ram: Vec<u8>,
        color: Vec<u8>,
    }

    impl Mem {
        fn new() -> Self {
            Self {
                ram: vec![0; 0x4000],
                color: vec![0; 1000],
            }
        }
    }

    impl VicMemory for Mem {
        fn vic_read(&self, addr: u16) -> u8 {
            self.ram[addr as usize & 0x3FFF]
        }
        fn color_ram(&self, index: u16) -> u8 {
            self.color[index as usize]
        }
    }

    fn started() -> Vic {
        let mut vic = Vic::new();
        vic.enable_cycle_engine().unwrap();
        vic
    }

    /// Runs cycles until the chip stands at `cycle` of `line` (the cycle
    /// has just been executed).
    fn run_to(vic: &mut Vic, mem: &Mem, line: u32, cycle: u32) {
        let target = line * LINE_CYCLES + cycle - 1;
        let frame = vic.cycles_per_frame();
        let mut guard = 0;
        loop {
            vic.cycle(mem);
            if vic.frame_cycle == target {
                return;
            }
            guard += 1;
            assert!(guard < 2 * frame, "never reached line {line} cycle {cycle}");
        }
    }

    /// The cycles (1-63) of the current line, from the one just run, in
    /// which BA is low, collected over one whole line.
    fn ba_profile(vic: &mut Vic, mem: &Mem) -> Vec<u32> {
        let mut low = Vec::new();
        for _ in 0..LINE_CYCLES {
            vic.cycle(mem);
            if vic.ba_low() {
                low.push(vic.line_cycle());
            }
        }
        low
    }

    #[test]
    fn the_engine_walks_the_raster_cycle_by_cycle_and_wraps_the_frame() {
        let mem = Mem::new();
        let mut vic = started();
        vic.cycle(&mem);
        assert_eq!((vic.raster_line(), vic.line_cycle()), (0, 1));
        run_to(&mut vic, &mem, 5, 1);
        assert_eq!((vic.raster_line(), vic.line_cycle()), (5, 1));
        assert_eq!(vic.read(0x12), 5);
        run_to(&mut vic, &mem, 311, 63);
        vic.cycle(&mem);
        assert_eq!((vic.raster_line(), vic.line_cycle()), (0, 1));
    }

    #[test]
    fn ntsc_is_refused() {
        let mut vic = Vic::new();
        vic.set_ntsc(true);
        assert!(vic.enable_cycle_engine().is_err());
    }

    #[test]
    fn a_raster_irq_latches_in_cycle_one_of_the_compare_line() {
        let mem = Mem::new();
        let mut vic = started();
        vic.write(0x12, 100);
        vic.write(0x1A, 1);
        run_to(&mut vic, &mem, 99, 63);
        let r = vic.cycle(&mem);
        assert!(r.raster_irq);
        assert_eq!(r.entered_line, Some(100));
        assert_eq!(vic.read(0x19) & 1, 1);
        // Not again on the following cycles.
        assert!(!vic.cycle(&mem).raster_irq);
    }

    #[test]
    fn without_den_no_line_is_a_bad_line_and_ba_stays_high() {
        let mem = Mem::new();
        let mut vic = started();
        vic.write(0x11, 0x0B); // DEN off, YSCROLL 3
        run_to(&mut vic, &mem, 0x32, 63); // the next line, $33, would be bad
        assert!(ba_profile(&mut vic, &mem).is_empty());
    }

    #[test]
    fn a_bad_line_holds_ba_low_for_cycles_12_to_54_and_fetches_the_matrix() {
        let mut mem = Mem::new();
        // Video matrix at $0400 (default $D018 = 0 -> base 0; set it).
        let mut vic = started();
        vic.write(0x18, 0x10); // matrix $0400, chargen base 0
        for i in 0..40 {
            mem.ram[0x0400 + i] = 0x41 + i as u8;
            mem.color[i] = 0x10 | (i as u8 & 0x0F); // high nibble ignored
        }
        vic.write(0x11, 0x1B); // DEN on, YSCROLL 3
        // Line $33 is the first bad line (low three bits 3).
        run_to(&mut vic, &mem, 0x32, 63);
        let low = ba_profile(&mut vic, &mem);
        assert_eq!(low, (12..=54).collect::<Vec<u32>>());
        let (codes, colors) = vic.line_matrix();
        for i in 0..40 {
            assert_eq!(codes[i], 0x41 + i as u8, "column {i}");
            assert_eq!(colors[i], i as u8 & 0x0F, "colour {i}");
        }
    }

    #[test]
    fn the_g_accesses_fetch_glyph_rows_from_the_character_generator() {
        let mut mem = Mem::new();
        let mut vic = started();
        vic.write(0x18, 0x1A); // matrix $0400, chargen $2800
        mem.ram[0x0400] = 2;
        mem.ram[0x0401] = 3;
        // glyph 2 row 0, glyph 3 row 0 (bad line has RC = 0)
        mem.ram[0x2800 + 2 * 8] = 0xA5;
        mem.ram[0x2800 + 3 * 8] = 0x5A;
        vic.write(0x11, 0x1B);
        run_to(&mut vic, &mem, 0x32, 63);
        ba_profile(&mut vic, &mem);
        let g = vic.line_graphics();
        assert_eq!((g[0], g[1]), (0xA5, 0x5A));
    }

    #[test]
    fn counters_follow_the_display_state_rules_over_eight_lines() {
        let mem = Mem::new();
        let mut vic = started();
        vic.write(0x11, 0x1B);
        run_to(&mut vic, &mem, 0x33, 14);
        // Cycle 14 of the bad line: VC loaded, RC cleared.
        assert_eq!(vic.video_counters(), (0, 0, 0, 0, true));
        run_to(&mut vic, &mem, 0x33, 58);
        // After 40 g-accesses VC = 40; RC was incremented at cycle 58.
        let (vc, vcbase, rc, vmli, display) = vic.video_counters();
        assert_eq!((vc, vcbase, rc, vmli, display), (40, 0, 1, 40, true));
        // Eight lines in, RC is 7 at cycle 58: idle state, VCBASE takes VC
        // (and RC, no longer counting, stays at 7 until the next bad line).
        run_to(&mut vic, &mem, 0x3A, 58);
        let (vc, vcbase, rc, _, display) = vic.video_counters();
        assert_eq!((vc, vcbase, rc, display), (40, 40, 7, false));
        // The next bad line fetches the *next* row of the matrix.
        run_to(&mut vic, &mem, 0x3B, 15);
        let (vc, vcbase, _, _, display) = vic.video_counters();
        assert_eq!((vcbase, display), (40, true));
        assert_eq!(vc, 40, "VC restarts at VCBASE");
    }

    #[test]
    fn changing_yscroll_before_cycle_12_cancels_a_bad_line() {
        let mem = Mem::new();
        let mut vic = started();
        vic.write(0x11, 0x1B);
        // Line $33 would be bad; move YSCROLL away in cycle 11.
        run_to(&mut vic, &mem, 0x33, 11);
        vic.write(0x11, 0x1C);
        let mut low = Vec::new();
        for _ in 0..(LINE_CYCLES - 11) {
            vic.cycle(&mem);
            if vic.ba_low() {
                low.push(vic.line_cycle());
            }
        }
        assert!(low.is_empty(), "no bad line, no BA: {low:?}");
        assert!(!vic.video_counters().4, "the video logic stays idle");
    }

    #[test]
    fn making_the_next_line_bad_again_is_fld_the_row_does_not_advance() {
        let mut mem = Mem::new();
        let mut vic = started();
        vic.write(0x18, 0x10);
        mem.ram[0x0400] = 0x11;
        vic.write(0x11, 0x1B);
        run_to(&mut vic, &mem, 0x32, 63);
        ba_profile(&mut vic, &mem); // line $33: the real bad line
        assert_eq!(vic.video_counters().2, 1, "RC counted on to 1");

        // Line $34 is not bad (YSCROLL 3) -- but set YSCROLL to 4 in cycle
        // 11 and it is: the matrix is fetched again and RC restarts.
        run_to(&mut vic, &mem, 0x34, 11);
        vic.write(0x11, 0x1C);
        let mut low = Vec::new();
        for _ in 0..(LINE_CYCLES - 11) {
            vic.cycle(&mem);
            if vic.ba_low() {
                low.push(vic.line_cycle());
            }
        }
        assert_eq!(low, (12..=54).collect::<Vec<u32>>());
        let (vc, vcbase, rc, _, display) = vic.video_counters();
        assert_eq!((vc, vcbase, rc, display), (40, 0, 1, true));
    }

    #[test]
    fn a_bad_line_seen_late_starts_the_c_accesses_three_cycles_after_ba_low() {
        let mut mem = Mem::new();
        let mut vic = started();
        vic.write(0x18, 0x10);
        for i in 0..40 {
            mem.ram[0x0400 + i] = 0x80 + i as u8;
        }
        // DEN on in line $30, but YSCROLL is wrong until cycle 29 of line $33.
        vic.write(0x11, 0x18);
        run_to(&mut vic, &mem, 0x33, 28);
        vic.write(0x11, 0x1B);
        let mut low = Vec::new();
        for _ in 0..(LINE_CYCLES - 28) {
            vic.cycle(&mem);
            if vic.ba_low() {
                low.push(vic.line_cycle());
            }
        }
        assert_eq!(low, (29..=54).collect::<Vec<u32>>());
    }

    // ---- sprites ----

    fn sprite_setup(mem: &mut Mem, vic: &mut Vic, n: usize, y: u8, pointer: u8) {
        vic.write(0x15, vic.read(0x15) | (1 << n));
        vic.write(n as u8 * 2 + 1, y);
        mem.ram[0x03F8 + n] = pointer;
        let base = pointer as usize * 64;
        for i in 0..63 {
            mem.ram[base + i] = (n as u8) << 5 | (i as u8 & 0x1F);
        }
    }

    #[test]
    fn a_lone_sprite_holds_ba_low_for_five_cycles_and_fetches_its_row() {
        let mut mem = Mem::new();
        let mut vic = started();
        vic.write(0x18, 0x00); // matrix at 0 so pointers sit at $03F8
        sprite_setup(&mut mem, &mut vic, 0, 100, 0x10);
        // DMA starts in cycle 55 of line 100 ...
        run_to(&mut vic, &mem, 100, 54);
        assert!(!vic.ba_low());
        let mut low = Vec::new();
        for _ in 0..(LINE_CYCLES - 54) {
            vic.cycle(&mem);
            if vic.ba_low() {
                low.push(vic.line_cycle());
            }
        }
        assert_eq!(low, vec![55, 56, 57, 58, 59]);
        assert_eq!(vic.sprite_dma_mask(), 1);
        assert_eq!(vic.sprite_bytes(0), [0x00, 0x01, 0x02]);
        // ... and display is switched on in cycle 58.
        assert_eq!(vic.sprite_display_mask(), 1);
        assert_eq!(vic.sprite_counters(0).1, 3);
    }

    #[test]
    fn consecutive_sprites_share_one_continuous_ba_window() {
        let mut mem = Mem::new();
        let mut vic = started();
        for n in 0..3 {
            sprite_setup(&mut mem, &mut vic, n, 80, 0x10 + n as u8);
        }
        run_to(&mut vic, &mem, 80, 54);
        let mut low = Vec::new();
        for _ in 0..(LINE_CYCLES - 54) {
            vic.cycle(&mem);
            if vic.ba_low() {
                low.push(vic.line_cycle());
            }
        }
        // Three sprites: 3 warning cycles + 3 x 2 = 9 cycles, 55..=63.
        assert_eq!(low, (55..=63).collect::<Vec<u32>>());
    }

    #[test]
    fn sprite_three_spans_the_end_and_the_start_of_the_line() {
        let mut mem = Mem::new();
        let mut vic = started();
        sprite_setup(&mut mem, &mut vic, 3, 60, 0x20);
        run_to(&mut vic, &mem, 60, 60);
        let mut low = Vec::new();
        for _ in 0..5 {
            vic.cycle(&mem);
            if vic.ba_low() {
                low.push((vic.raster_line(), vic.line_cycle()));
            }
        }
        // BA low in 61-63 of line 60 and 1-2 of line 61.
        assert_eq!(low, vec![(60, 61), (60, 62), (60, 63), (61, 1), (61, 2)]);
        assert_eq!(vic.sprite_bytes(3)[0], 3 << 5);
    }

    #[test]
    fn a_sprite_advances_three_bytes_per_line_and_stops_after_21_rows() {
        let mut mem = Mem::new();
        let mut vic = started();
        sprite_setup(&mut mem, &mut vic, 0, 40, 0x10);
        for row in 0..21u32 {
            // DMA starts in line 40 and fetches row 0 there.
            run_to(&mut vic, &mem, 40 + row, 63);
            let bytes = vic.sprite_bytes(0);
            // Row r holds data bytes 3r..3r+2 of the 63-byte block.
            let expect = |i: u32| (i & 0x1F) as u8;
            assert_eq!(
                bytes,
                [expect(row * 3), expect(row * 3 + 1), expect(row * 3 + 2)],
                "row {row}"
            );
        }
        // Row 20 was fetched in line 60; MCBASE reaches 63 in cycle 16 of
        // line 61 and DMA and display go off.
        run_to(&mut vic, &mem, 61, 15);
        assert_eq!(vic.sprite_dma_mask(), 1);
        run_to(&mut vic, &mem, 61, 16);
        assert_eq!(vic.sprite_dma_mask(), 0);
        assert_eq!(vic.sprite_display_mask(), 0);
    }

    #[test]
    fn y_expansion_repeats_each_sprite_row_for_two_lines() {
        let mut mem = Mem::new();
        let mut vic = started();
        sprite_setup(&mut mem, &mut vic, 0, 40, 0x10);
        vic.write(0x17, 1);
        // Row 0 is fetched in line 40 (DMA start resets the flip flop), is
        // not advanced past in line 41, and row 1 follows in line 42.
        let mut rows = Vec::new();
        for line in 40..44 {
            run_to(&mut vic, &mem, line, 63);
            rows.push(vic.sprite_bytes(0));
        }
        assert_eq!(rows[0], rows[1], "doubled row");
        assert_ne!(rows[1], rows[2], "then the next row");
        assert_eq!(rows[2], rows[3], "doubled again");
        assert_eq!(rows[2], [3, 4, 5]);
    }

    #[test]
    fn clearing_mxye_before_the_mcbase_update_crunches_the_sprite() {
        // Without the write: rows repeat, as in the test above.
        let mut mem = Mem::new();
        let mut vic = started();
        sprite_setup(&mut mem, &mut vic, 0, 40, 0x10);
        vic.write(0x17, 1);
        run_to(&mut vic, &mem, 40, 63);
        let first = vic.sprite_bytes(0);
        run_to(&mut vic, &mem, 41, 63);
        assert_eq!(vic.sprite_bytes(0), first, "line 41 repeats row 0");

        // With MxYE cleared before cycle 15 of line 41 and set again before
        // cycle 55, the flip flop is forced set: MCBASE moves on a row early,
        // so line 41 already shows row 1 -- one row of the sprite is lost.
        let mut vic = started();
        sprite_setup(&mut mem, &mut vic, 0, 40, 0x10);
        vic.write(0x17, 1);
        run_to(&mut vic, &mem, 41, 10);
        vic.write(0x17, 0);
        run_to(&mut vic, &mem, 41, 30);
        vic.write(0x17, 1);
        run_to(&mut vic, &mem, 41, 63);
        assert_eq!(vic.sprite_bytes(0), [3, 4, 5], "crunched: row 1 in line 41");
        // The flip flop toggles back at cycle 55, so row 1 is then repeated.
        run_to(&mut vic, &mem, 42, 63);
        assert_eq!(vic.sprite_bytes(0), [3, 4, 5]);
    }

    #[test]
    fn mcbase_is_a_six_bit_counter_so_a_misaligned_sprite_does_not_stop_at_63() {
        let mut mem = Mem::new();
        let mut vic = started();
        sprite_setup(&mut mem, &mut vic, 0, 40, 0x10);
        // Clear MxYE between cycle 15 and 16 of line 41: only the "+1" half
        // of the update happens, MCBASE is 1 and no longer a multiple of 3.
        vic.write(0x17, 1);
        run_to(&mut vic, &mem, 40, 63);
        run_to(&mut vic, &mem, 41, 15);
        vic.write(0x17, 0);
        run_to(&mut vic, &mem, 41, 17);
        vic.write(0x17, 1);
        // MCBASE never hits 63 exactly now; DMA keeps running past the
        // normal 21 lines instead of stopping.
        run_to(&mut vic, &mem, 70, 63);
        assert_eq!(
            vic.sprite_dma_mask(),
            1,
            "still fetching long after line 61"
        );
    }

    #[test]
    fn the_pointer_is_fetched_even_for_a_sprite_that_is_off() {
        let mut mem = Mem::new();
        let mut vic = started();
        mem.ram[0x03F8 + 5] = 0x77;
        run_to(&mut vic, &mem, 20, 20);
        assert_eq!(vic.engine.pointer[5], 0x77);
        assert!(!vic.ba_low());
    }

    // ---- the pixel side ----

    /// A tiny deterministic generator for test data.
    fn noise(seed: u32, len: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761) | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 11) as u8
            })
            .collect()
    }

    /// Runs the engine until `frames` complete pictures exist, calling `hook`
    /// after every cycle with the line and cycle that were just run.
    fn run_frames(vic: &mut Vic, mem: &Mem, frames: u32, hook: &mut dyn FnMut(&mut Vic, u32, u32)) {
        for _ in 0..frames * 312 * LINE_CYCLES {
            vic.cycle(mem);
            let (line, cyc) = (vic.raster_line(), vic.line_cycle());
            hook(vic, line, cyc);
        }
    }

    fn no_hook(_: &mut Vic, _: u32, _: u32) {}

    /// The 320x200 window of the engine's picture.
    fn window(vic: &Vic) -> Vec<u8> {
        let (w, _, rgb) = vic.cycle_frame().expect("a complete frame");
        let mut out = Vec::new();
        for row in 32..232 {
            let start = (row * w as usize + 32) * 3;
            out.extend_from_slice(&rgb[start..start + 320 * 3]);
        }
        out
    }

    /// RGB of the frame pixel at raster `line` and frame column `col`.
    fn pixel(vic: &Vic, line: u32, col: usize) -> (u8, u8, u8) {
        let (w, _, rgb) = vic.cycle_frame().unwrap();
        let at = ((line - FRAME_FIRST_LINE) as usize * w as usize + col) * 3;
        (rgb[at], rgb[at + 1], rgb[at + 2])
    }

    /// A display with random screen, colour and glyph/bitmap data.
    fn random_screen(ctl1: u8, ctl2: u8, d018: u8) -> (Vic, Mem) {
        let mut mem = Mem::new();
        let mut vic = started();
        vic.set_chargen_shadow(false);
        mem.ram[0x0400..0x0800].copy_from_slice(&noise(1, 0x400));
        mem.ram[0x2000..0x4000].copy_from_slice(&noise(2, 0x2000));
        // The byte idle accesses read: zero shows the background colour.
        mem.ram[0x3FFF] = 0;
        mem.color
            .copy_from_slice(&noise(3, 1000).iter().map(|b| b & 0x0F).collect::<Vec<_>>());
        vic.write(0x18, d018);
        vic.write(0x16, ctl2);
        vic.write(0x11, ctl1);
        vic.write(0x20, 14);
        vic.write(0x21, 6);
        vic.write(0x22, 1);
        vic.write(0x23, 5);
        vic.write(0x24, 8);
        (vic, mem)
    }

    fn static_window(vic: &Vic, mem: &Mem) -> Vec<u8> {
        let (w, h, rgb) = vic.render(&mem.ram, &[], &mem.color);
        assert_eq!((w, h), (320, 200));
        rgb
    }

    #[test]
    fn every_display_mode_matches_the_static_renderer_pixel_for_pixel() {
        // (name, $D011, $D016, $D018)
        let modes = [
            ("text", 0x1B, 0x08, 0x1A),
            ("multicolor text", 0x1B, 0x18, 0x1A),
            ("extended colour", 0x5B, 0x08, 0x1A),
            ("bitmap", 0x3B, 0x08, 0x18),
            ("multicolor bitmap", 0x3B, 0x18, 0x18),
        ];
        for (name, ctl1, ctl2, d018) in modes {
            let (mut vic, mem) = random_screen(ctl1, ctl2, d018);
            run_frames(&mut vic, &mem, 2, &mut no_hook);
            let expected = static_window(&vic, &mem);
            let got = window(&vic);
            let bad = expected
                .chunks(3)
                .zip(got.chunks(3))
                .position(|(a, b)| a != b);
            assert_eq!(bad, None, "{name}: first differing pixel (row-major index)");
        }
    }

    #[test]
    fn moving_a_sprite_after_it_is_drawn_shows_it_a_second_time_multiplexing() {
        let (mut vic, mut mem) = random_screen(0x1B, 0x08, 0x1A);
        // Solid sprite 0: pointer in the matrix's last bytes, 63 bytes of $FF.
        mem.ram[0x07F8] = 0x20;
        mem.ram[0x0800..0x0800 + 63].fill(0xFF);
        // Blank the screen so the sprite stands out against the background.
        mem.ram[0x0400..0x0800 - 0x18].fill(0x20);
        mem.ram[0x2800 + 0x20 * 8..0x2800 + 0x21 * 8].fill(0);
        vic.write(0x15, 1);
        vic.write(0x00, 100);
        vic.write(0x01, 80);
        vic.write(0x27, 7);
        let mut hook = |v: &mut Vic, line: u32, cyc: u32| {
            // After the first appearance (lines 81-101) has been drawn, point
            // the same sprite further down.
            if line == 110 && cyc == 20 {
                v.write(0x01, 140);
            }
            // Restore for the next frame.
            if line == 200 && cyc == 20 {
                v.write(0x01, 80);
            }
        };
        run_frames(&mut vic, &mem, 2, &mut hook);
        let yellow = PALETTE[7];
        let lit = |line: u32| pixel(&vic, line, 100 + 8) == yellow;
        assert!(
            !lit(80) && lit(81) && lit(101) && !lit(102),
            "first appearance 81-101"
        );
        assert!(!lit(120), "gone in between");
        assert!(
            !lit(140) && lit(141) && lit(161) && !lit(162),
            "second appearance 141-161"
        );
    }

    #[test]
    fn xscroll_shifts_the_picture_right_and_the_idle_cell_fills_the_gap() {
        let (mut plain, mem) = random_screen(0x1B, 0x08, 0x1A);
        run_frames(&mut plain, &mem, 2, &mut no_hook);
        let (mut scrolled, mem2) = random_screen(0x1B, 0x08 | 3, 0x1A);
        run_frames(&mut scrolled, &mem2, 2, &mut no_hook);
        let (a, b) = (window(&plain), window(&scrolled));
        let px = |w: &[u8], row: usize, x: usize| {
            let i = (row * 320 + x) * 3;
            [w[i], w[i + 1], w[i + 2]]
        };
        let bg = [PALETTE[6].0, PALETTE[6].1, PALETTE[6].2];
        for row in [0usize, 7, 90, 199] {
            // Three pixels in, the scrolled picture is the plain one shifted.
            for x in 3..320 {
                assert_eq!(px(&b, row, x), px(&a, row, x - 3), "row {row} x {x}");
            }
            // The first three show the idle cell ($3FFF reads 0: background).
            for x in 0..3 {
                assert_eq!(px(&b, row, x), bg, "row {row} x {x}");
            }
        }
    }

    #[test]
    fn the_border_surrounds_the_window_in_the_border_colour() {
        let (mut vic, mem) = random_screen(0x1B, 0x08, 0x1A);
        run_frames(&mut vic, &mem, 2, &mut no_hook);
        let (w, h, rgb) = vic.cycle_frame().unwrap();
        assert_eq!((w, h), (384, 264));
        let border = PALETTE[14];
        let at = |row: usize, col: usize| {
            let i = (row * w as usize + col) * 3;
            (rgb[i], rgb[i + 1], rgb[i + 2])
        };
        for (row, col) in [
            (0, 0),
            (31, 200),
            (232, 5),
            (263, 383),
            (100, 31),
            (100, 352),
        ] {
            assert_eq!(at(row, col), border, "row {row} col {col}");
        }
        assert_ne!(at(100, 32), border, "the first window column is not border");
    }

    #[test]
    fn sprites_match_the_static_renderer_including_priority_and_expansion() {
        let (mut vic, mut mem) = random_screen(0x1B, 0x08, 0x1A);
        // Pointers sit in the video matrix's last eight bytes.
        for n in 0..4usize {
            mem.ram[0x07F8 + n] = 0x20 + n as u8;
            let at = (0x20 + n) * 64;
            mem.ram[at..at + 64].copy_from_slice(&noise(10 + n as u32, 64));
        }
        vic.write(0x15, 0x0F);
        for (n, (x, y)) in [(100u8, 80u8), (110, 84), (140, 100), (200, 120)]
            .into_iter()
            .enumerate()
        {
            vic.write(n as u8 * 2, x);
            vic.write(n as u8 * 2 + 1, y);
            vic.write(0x27 + n as u8, 2 + n as u8);
        }
        vic.write(0x25, 3);
        vic.write(0x26, 7);
        vic.write(0x1C, 0b0100); // sprite 2 multicolor
        vic.write(0x1D, 0b1000); // sprite 3 wide
        vic.write(0x17, 0b1000); // and tall
        vic.write(0x1B, 0b0010); // sprite 1 behind the background
        run_frames(&mut vic, &mem, 2, &mut no_hook);
        let expected = static_window(&vic, &mem);
        let got = window(&vic);
        let bad = expected
            .chunks(3)
            .zip(got.chunks(3))
            .position(|(a, b)| a != b);
        assert_eq!(
            bad,
            None,
            "first differing pixel (row-major index) -> row {:?}",
            bad.map(|i| i / 320)
        );
    }

    #[test]
    fn opening_the_side_border_shows_the_idle_pattern_on_that_line_only() {
        let (mut vic, mem) = random_screen(0x1B, 0x08, 0x1A);
        // Idle accesses read $3FFF; 0 shows the background colour (6).
        let mut hook = |v: &mut Vic, line: u32, cyc: u32| {
            if line == 100 && cyc == 56 {
                v.write(0x16, 0x00); // 38 columns: the right compare is x = 335
            }
            if line == 100 && cyc == 59 {
                v.write(0x16, 0x08);
            }
        };
        run_frames(&mut vic, &mem, 2, &mut hook);
        let border = PALETTE[14];
        // x = 344..375 are columns 352..383.
        assert_ne!(pixel(&vic, 100, 360), border, "line 100: border opened");
        assert_eq!(pixel(&vic, 99, 360), border, "line 99: closed");
        assert_eq!(pixel(&vic, 101, 360), border, "line 101: closed");
        // The opened area shows the idle byte at $3FFF: all zero -> background.
        assert_eq!(pixel(&vic, 100, 360), PALETTE[6]);
    }

    #[test]
    fn switching_to_24_rows_after_line_247_opens_the_bottom_border() {
        let (mut vic, mem) = random_screen(0x1B, 0x08, 0x1A);
        let mut hook = |v: &mut Vic, line: u32, cyc: u32| {
            // Line 247 itself would still catch the bottom compare at the left
            // edge (cycle 17), so the write goes in the line after.
            if line == 248 && cyc == 10 {
                v.write(0x11, 0x13); // RSEL off: bottom compare becomes 247
            }
            if line == 255 && cyc == 10 {
                v.write(0x11, 0x1B); // RSEL back on before the next frame
            }
        };
        run_frames(&mut vic, &mem, 2, &mut hook);
        let border = PALETTE[14];
        // Line 247 is the last text line before the trick; 251-254 lie in the
        // lower border normally but now show the idle pattern (background).
        for line in 251..=254 {
            assert_eq!(pixel(&vic, line, 200), PALETTE[6], "line {line} open");
            assert_eq!(pixel(&vic, line, 10), border, "side border stays");
        }
    }

    #[test]
    fn without_the_trick_the_lower_border_is_closed() {
        let (mut vic, mem) = random_screen(0x1B, 0x08, 0x1A);
        run_frames(&mut vic, &mem, 2, &mut no_hook);
        for line in 251..=254 {
            assert_eq!(pixel(&vic, line, 200), PALETTE[14], "line {line}");
        }
    }

    #[test]
    fn fld_repeats_the_first_scanline_of_the_character_row() {
        // Eight distinct glyph scanlines for character 1; every cell shows it.
        let mut mem = Mem::new();
        let mut vic = started();
        vic.set_chargen_shadow(false);
        vic.write(0x18, 0x1A);
        for i in 0..1000 {
            mem.ram[0x0400 + i] = 1;
        }
        for r in 0..8u8 {
            mem.ram[0x2800 + 8 + r as usize] = 1 << r; // scanline r: one bit set
        }
        vic.write(0x11, 0x1B);
        vic.write(0x16, 0x08);
        vic.write(0x20, 14);
        vic.write(0x21, 6);
        // Colour RAM 1 (white ink); the bit position tells the scanline.
        mem.color.iter_mut().for_each(|c| *c = 1);
        // Lines $34-$36: make the line bad by pointing YSCROLL at it before
        // cycle 12 -- the row is fetched again and RC restarts from 0.
        let mut hook = |v: &mut Vic, line: u32, cyc: u32| {
            if cyc == 10 && (0x34..=0x36).contains(&line) {
                v.write(0x11, 0x18 | (line as u8 & 7));
            }
            if cyc == 10 && line == 0x37 {
                v.write(0x11, 0x18 | 3); // back to normal: next bad line is $3B
            }
        };
        run_frames(&mut vic, &mem, 2, &mut hook);
        let white = PALETTE[1];
        // Which glyph bit is lit in the first cell of a line: pixel (7-r).
        let scanline = |line: u32| {
            (0..8)
                .find(|&bit| pixel(&vic, line, 32 + bit) == white)
                .unwrap()
        };
        // Lines $33-$36 all show scanline 0 (bit 0 -> pixel 7); then 1, 2, ...
        for line in 0x33..=0x36 {
            assert_eq!(scanline(line), 7, "line {line:#x}");
        }
        assert_eq!(scanline(0x37), 6);
        assert_eq!(scanline(0x38), 5);
    }
}
