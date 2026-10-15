//! MOS 8563 / 8568 VDC: the C128's 80-column video chip.
//!
//! A 6845-style CRTC with its own video RAM (16K, or 64K on the 128DCR),
//! reached through two ports: the index/status port (write the register
//! number; read the status byte -- bit 7 ready, bit 6 light pen, bit 5
//! "vertical blank" i.e. outside the active display, low bits the chip
//! revision) and the data port (read/write the selected register). Register
//! 31 is the window into video RAM: it reads and writes the byte at the
//! update address (registers 18/19) and advances it. Writing register 30
//! starts a block fill or copy of that many bytes.
//!
//! The picture is rendered from the registers and video RAM on demand
//! ([`Vdc::render`]): text mode with per-character attributes (alternate
//! set, reverse, underline, blink, 16 RGBI colours), the hardware cursor,
//! reverse-screen, and bitmap mode. Behaviour follows the 8563 data sheet and
//! VICE's `vdc-mem.c` / `vdc-draw.c` (reference only, see `CREDITS.md`).

use serde::{Deserialize, Serialize};

pub const REGISTER_COUNT: usize = 38;

/// Bits that read back as 1 in each register (VICE's `regmask`).
const UNUSED_BITS: [u8; REGISTER_COUNT] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0xE0, 0x00, 0x00, 0xFC, 0xE0, 0x80, 0xE0, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xE0, 0x00, 0x00, 0x00, 0x00, 0x0F, 0xE0, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xF0, 0x3F,
];

/// The 16 RGBI colours.
pub const PALETTE: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0x55, 0x55, 0x55],
    [0x00, 0x00, 0xAA],
    [0x55, 0x55, 0xFF],
    [0x00, 0xAA, 0x00],
    [0x55, 0xFF, 0x55],
    [0x00, 0xAA, 0xAA],
    [0x55, 0xFF, 0xFF],
    [0xAA, 0x00, 0x00],
    [0xFF, 0x55, 0x55],
    [0xAA, 0x00, 0xAA],
    [0xFF, 0x55, 0xFF],
    [0xAA, 0x55, 0x00],
    [0xFF, 0xFF, 0x55],
    [0xAA, 0xAA, 0xAA],
    [0xFF, 0xFF, 0xFF],
];

/// Chip revision reported in the status byte (8568 as in the 128DCR).
const REVISION: u8 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vdc {
    /// `REGISTER_COUNT` registers (a `Vec`: serde has no 38-element arrays).
    regs: Vec<u8>,
    index: u8,
    ram: Vec<u8>,
    /// 64K of video RAM fitted (else 16K).
    ram64: bool,
    /// Video frames since power-on (drives blinking).
    frame: u64,
    /// Bus cycles into the current frame.
    frame_cycles: u64,
    cycles_per_frame: u64,
}

impl Vdc {
    /// `ram64`: 64K of video RAM (128DCR) rather than 16K.
    pub fn new(ram64: bool) -> Self {
        Self {
            regs: vec![0; REGISTER_COUNT],
            index: 0,
            ram: vec![0; if ram64 { 0x1_0000 } else { 0x4000 }],
            ram64,
            frame: 0,
            frame_cycles: 0,
            cycles_per_frame: 17_000,
        }
    }

    /// Sets the machine clock so a video frame lasts 1/60 s.
    pub fn set_clock(&mut self, clock_hz: u64) {
        self.cycles_per_frame = clock_hz / 60;
    }

    pub fn reg(&self, n: usize) -> u8 {
        self.regs[n]
    }

    pub fn ram(&self) -> &[u8] {
        &self.ram
    }

    /// Advances the frame counter (blink phase, vertical-blank flag).
    pub fn tick(&mut self, cycles: u32) {
        self.frame_cycles += u64::from(cycles);
        while self.frame_cycles >= self.cycles_per_frame {
            self.frame_cycles -= self.cycles_per_frame;
            self.frame += 1;
        }
    }

    /// Outside the active display area (the top and bottom border).
    fn in_blank(&self) -> bool {
        self.frame_cycles * 100 >= self.cycles_per_frame * 76
    }

    // -- video RAM addressing ---------------------------------------------

    /// Register 28 bit 4 chooses 64K addressing; the chips fitted may be
    /// 16K, in which case the address lines are folded.
    fn ram_index(&self, addr: u16) -> usize {
        let wide = self.regs[28] & 0x10 != 0;
        match (wide, self.ram64) {
            (true, true) => usize::from(addr),
            (true, false) => {
                // 64K addressing onto 16K chips.
                usize::from((addr & 0x00FF) | ((addr & 0x7E00) >> 1))
            }
            (false, true) => {
                // 16K addressing onto 64K chips.
                let tmp = (addr & 0x3F00) << 1 | (addr & 0x0100);
                usize::from((addr & 0x80FF) | tmp)
            }
            (false, false) => usize::from(addr & 0x3FFF),
        }
    }

    fn ram_read(&self, addr: u16) -> u8 {
        self.ram[self.ram_index(addr)]
    }

    fn ram_write(&mut self, addr: u16, value: u8) {
        let i = self.ram_index(addr);
        self.ram[i] = value;
    }

    fn update_address(&self) -> u16 {
        u16::from(self.regs[18]) << 8 | u16::from(self.regs[19])
    }

    fn set_update_address(&mut self, addr: u16) {
        self.regs[18] = (addr >> 8) as u8;
        self.regs[19] = addr as u8;
    }

    // -- ports ------------------------------------------------------------

    /// Port 0 (index/status) or 1 (data).
    pub fn read(&mut self, port: u8) -> u8 {
        if port & 1 == 0 {
            // Always ready: block operations complete instantly.
            let mut status = REVISION | 0x80;
            if self.in_blank() {
                status |= 0x20;
            }
            return status;
        }
        let reg = usize::from(self.index);
        if reg == 31 {
            let addr = self.update_address();
            let value = self.ram_read(addr);
            self.set_update_address(addr.wrapping_add(1));
            return value;
        }
        if reg < REGISTER_COUNT {
            self.regs[reg] | UNUSED_BITS[reg]
        } else {
            0xFF
        }
    }

    /// Side-effect-free read.
    pub fn peek(&self, port: u8) -> u8 {
        if port & 1 == 0 {
            let mut status = REVISION | 0x80;
            if self.in_blank() {
                status |= 0x20;
            }
            return status;
        }
        let reg = usize::from(self.index);
        if reg == 31 {
            return self.ram_read(self.update_address());
        }
        if reg < REGISTER_COUNT {
            self.regs[reg] | UNUSED_BITS[reg]
        } else {
            0xFF
        }
    }

    pub fn write(&mut self, port: u8, value: u8) {
        if port & 1 == 0 {
            self.index = value & 0x3F;
            return;
        }
        let reg = usize::from(self.index);
        if reg >= REGISTER_COUNT {
            return;
        }
        self.regs[reg] = value;
        match reg {
            30 => self.fill_or_copy(),
            31 => {
                let addr = self.update_address();
                self.ram_write(addr, value);
                self.set_update_address(addr.wrapping_add(1));
            }
            _ => {}
        }
    }

    /// Register 30: copy (register 24 bit 7) from the block start address
    /// (registers 32/33) or fill with register 31, to the update address.
    fn fill_or_copy(&mut self) {
        let count = match self.regs[30] {
            0 => 256usize,
            n => usize::from(n),
        };
        let dest = self.update_address();
        if self.regs[24] & 0x80 != 0 {
            let src = u16::from(self.regs[32]) << 8 | u16::from(self.regs[33]);
            for i in 0..count {
                let byte = self.ram_read(src.wrapping_add(i as u16));
                self.ram_write(dest.wrapping_add(i as u16), byte);
            }
            let end = src.wrapping_add(count as u16);
            self.regs[32] = (end >> 8) as u8;
            self.regs[33] = end as u8;
            self.regs[31] = self.ram_read(end.wrapping_sub(1));
        } else {
            let value = self.regs[31];
            for i in 0..count {
                self.ram_write(dest.wrapping_add(i as u16), value);
            }
        }
        self.set_update_address(dest.wrapping_add(count as u16));
    }

    // -- the picture -----------------------------------------------------------

    fn blink_phase(&self, slow: bool) -> bool {
        // 1/16 or 1/32 of the frame rate.
        let bit = if slow { 16 } else { 8 };
        self.frame & bit != 0
    }

    /// Whether the cursor is drawn in this frame.
    fn cursor_visible(&self) -> bool {
        match (self.regs[10] >> 5) & 3 {
            0 => true,
            1 => false,
            2 => self.blink_phase(false),
            _ => self.blink_phase(true),
        }
    }

    /// Horizontal pixels per character cell as displayed.
    fn char_width(&self) -> usize {
        8
    }

    /// Size of the picture `render` produces.
    pub fn dimensions(&self) -> (usize, usize) {
        let cols = usize::from(self.regs[1]).clamp(1, 255);
        let rows = usize::from(self.regs[6]).clamp(1, 80);
        let height = usize::from(self.regs[9] & 0x1F) + 1;
        (cols * self.char_width(), rows * height)
    }

    /// Renders the active display area as RGB.
    pub fn render(&self) -> (u32, u32, Vec<u8>) {
        let (width, height) = self.dimensions();
        let mut rgb = vec![0u8; width * height * 3];
        let cols = usize::from(self.regs[1]).clamp(1, 255);
        let rows = usize::from(self.regs[6]).clamp(1, 80);
        let line_height = usize::from(self.regs[9] & 0x1F) + 1;
        let stride = cols + usize::from(self.regs[27]);
        let screen = u16::from(self.regs[12]) << 8 | u16::from(self.regs[13]);
        let attr_base = u16::from(self.regs[20]) << 8 | u16::from(self.regs[21]);
        let charset = u16::from(self.regs[28] & 0xE0) << 8;
        let bitmap = self.regs[25] & 0x80 != 0;
        let attributes = self.regs[25] & 0x40 != 0;
        let reverse_screen = self.regs[24] & 0x40 != 0;
        let blink_slow = self.regs[24] & 0x20 != 0;
        let blink_on = self.blink_phase(blink_slow);
        let underline_line = usize::from(self.regs[29] & 0x1F);
        let cursor_pos = u16::from(self.regs[14]) << 8 | u16::from(self.regs[15]);
        let cursor_from = usize::from(self.regs[10] & 0x1F);
        let cursor_to = usize::from(self.regs[11] & 0x1F);
        let show_cursor = self.cursor_visible();
        let default_fg = self.regs[26] >> 4;
        let default_bg = self.regs[26] & 0x0F;

        for row in 0..rows {
            for col in 0..cols {
                let cell = row * stride + col;
                let attr = if attributes {
                    self.ram_read(attr_base.wrapping_add(cell as u16))
                } else {
                    0
                };
                let (mut fg, mut bg) = if attributes && !bitmap {
                    (attr & 0x0F, default_bg)
                } else if attributes {
                    (attr >> 4, attr & 0x0F)
                } else {
                    (default_fg, default_bg)
                };
                let code = self.ram_read(screen.wrapping_add(cell as u16));
                let reverse = attributes && !bitmap && attr & 0x40 != 0;
                let blink_hidden = attributes && !bitmap && attr & 0x10 != 0 && !blink_on;
                if reverse != reverse_screen {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let is_cursor =
                    !bitmap && show_cursor && cursor_pos == screen.wrapping_add(cell as u16);
                for line in 0..line_height {
                    let bits = if bitmap {
                        // Linear bitmap: `cols` bytes per scan line, the
                        // row-increment register skipping after each row.
                        let at = (row * line_height + line) * cols
                            + row * usize::from(self.regs[27])
                            + col;
                        self.ram_read(screen.wrapping_add(at as u16))
                    } else {
                        let alt = if attributes && attr & 0x80 != 0 {
                            0x1000u16
                        } else {
                            0
                        };
                        let at = charset
                            .wrapping_add(alt)
                            .wrapping_add(u16::from(code) * 16)
                            .wrapping_add(line.min(15) as u16);
                        let mut b = self.ram_read(at);
                        if attributes && attr & 0x20 != 0 && line == underline_line {
                            b = 0xFF;
                        }
                        if blink_hidden {
                            b = 0;
                        }
                        b
                    };
                    let inverted_line =
                        is_cursor && line >= cursor_from && line <= cursor_to.max(cursor_from);
                    for px in 0..8usize {
                        let on = (bits >> (7 - px)) & 1 != 0;
                        let on = on != inverted_line;
                        let color = if on { fg } else { bg };
                        let [r, g, b] = PALETTE[usize::from(color & 0x0F)];
                        let at = ((row * line_height + line) * width + col * 8 + px) * 3;
                        rgb[at] = r;
                        rgb[at + 1] = g;
                        rgb[at + 2] = b;
                    }
                }
            }
        }
        (width as u32, height as u32, rgb)
    }

    /// The border colour: register 26's background nibble.
    pub fn border_color(&self) -> [u8; 3] {
        PALETTE[usize::from(self.regs[26] & 0x0F)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &mut Vdc, reg: u8, value: u8) {
        v.write(0, reg);
        v.write(1, value);
    }

    fn get(v: &mut Vdc, reg: u8) -> u8 {
        v.write(0, reg);
        v.read(1)
    }

    #[test]
    fn register_protocol_and_unused_bits() {
        let mut v = Vdc::new(true);
        set(&mut v, 1, 80);
        assert_eq!(get(&mut v, 1), 80);
        set(&mut v, 5, 0x03);
        assert_eq!(get(&mut v, 5), 0xE3, "unused bits read as 1");
        v.write(0, 0xFF);
        assert_eq!(v.index, 0x3F, "the top two bits of the index are ignored");
    }

    #[test]
    fn the_status_port_reports_ready_and_the_revision() {
        let mut v = Vdc::new(true);
        assert_eq!(v.read(0) & 0x9F, 0x80 | REVISION);
    }

    #[test]
    fn register_31_writes_and_reads_video_ram_with_auto_increment() {
        let mut v = Vdc::new(true);
        set(&mut v, 28, 0x10);
        set(&mut v, 18, 0x12);
        set(&mut v, 19, 0x34);
        v.write(0, 31);
        v.write(1, 0xAA);
        v.write(1, 0xBB);
        assert_eq!(v.ram()[0x1234], 0xAA);
        assert_eq!(v.ram()[0x1235], 0xBB);
        assert_eq!((v.reg(18), v.reg(19)), (0x12, 0x36));
        set(&mut v, 18, 0x12);
        set(&mut v, 19, 0x34);
        v.write(0, 31);
        assert_eq!(v.read(1), 0xAA);
        assert_eq!(v.read(1), 0xBB);
    }

    #[test]
    fn writing_register_30_fills_or_copies_a_block() {
        let mut v = Vdc::new(true);
        set(&mut v, 28, 0x10);
        set(&mut v, 31, 0x5A); // (this write itself stores at, and advances, the update address)
        set(&mut v, 18, 0x20);
        set(&mut v, 19, 0x00);
        set(&mut v, 30, 4); // fill 4 bytes
        assert_eq!(&v.ram()[0x2000..0x2004], &[0x5A; 4]);
        assert_eq!((v.reg(18), v.reg(19)), (0x20, 0x04));
        // Copy them to $3000.
        set(&mut v, 24, 0x80);
        set(&mut v, 18, 0x30);
        set(&mut v, 19, 0x00);
        set(&mut v, 32, 0x20);
        set(&mut v, 33, 0x00);
        set(&mut v, 30, 4);
        assert_eq!(&v.ram()[0x3000..0x3004], &[0x5A; 4]);
        assert_eq!((v.reg(32), v.reg(33)), (0x20, 0x04));
    }

    #[test]
    fn a_zero_count_means_256_bytes() {
        let mut v = Vdc::new(true);
        set(&mut v, 28, 0x10);
        set(&mut v, 31, 0x77);
        set(&mut v, 18, 0x00);
        set(&mut v, 19, 0x00);
        set(&mut v, 30, 0);
        assert!(v.ram()[..256].iter().all(|&b| b == 0x77));
        assert_eq!(v.ram()[256], 0, "only 256 bytes");
    }

    #[test]
    fn sixteen_k_chips_fold_the_address_lines() {
        let mut v = Vdc::new(false);
        assert_eq!(v.ram().len(), 0x4000);
        set(&mut v, 28, 0x10); // 64K addressing
        set(&mut v, 18, 0x00);
        set(&mut v, 19, 0x00);
        v.write(0, 31);
        v.write(1, 1);
        assert_eq!(v.ram()[0], 1);
        // Addresses that differ only in A15 land on the same cell (the
        // mapping drops it).
        assert_eq!(v.ram_index(0x8000), v.ram_index(0x0000));
    }

    fn text_vdc() -> Vdc {
        let mut v = Vdc::new(true);
        set(&mut v, 28, 0x10 | 0x20); // charset at $2000
        set(&mut v, 1, 80);
        set(&mut v, 6, 25);
        set(&mut v, 9, 7);
        set(&mut v, 12, 0x00);
        set(&mut v, 13, 0x00);
        set(&mut v, 20, 0x08);
        set(&mut v, 21, 0x00);
        set(&mut v, 25, 0x40); // attributes on
        set(&mut v, 26, 0xF0); // white on black
        set(&mut v, 10, 0x20); // cursor off
        v
    }

    #[test]
    fn text_mode_draws_the_glyph_bits_in_the_attribute_colour() {
        let mut v = text_vdc();
        // Glyph 1: top row 10000001, rest blank, at $2000 + 16.
        v.ram[0x2000 + 16] = 0b1000_0001;
        v.ram[0] = 1; // screen cell 0
        v.ram[0x0800] = 0x02; // attribute: dark blue
        let (w, h, rgb) = v.render();
        assert_eq!((w, h), (640, 200));
        assert_eq!(&rgb[0..3], &PALETTE[2], "leftmost lit pixel");
        assert_eq!(&rgb[3..6], &PALETTE[0], "background");
        assert_eq!(&rgb[7 * 3..7 * 3 + 3], &PALETTE[2], "rightmost lit pixel");
    }

    #[test]
    fn reverse_underline_and_the_alternate_set_attributes_apply() {
        let mut v = text_vdc();
        v.ram[0x2000 + 16] = 0xFF;
        v.ram[0] = 1;
        v.ram[0x0800] = 0x40 | 0x0F; // reverse, white
        let (_, _, rgb) = v.render();
        assert_eq!(
            &rgb[0..3],
            &PALETTE[0],
            "reverse: the lit bit shows the background"
        );
        // Underline on scan line R29 draws a full row.
        set(&mut v, 29, 7);
        v.ram[0] = 0;
        v.ram[0x0800] = 0x20 | 0x0F;
        let (_, _, rgb) = v.render();
        let at = (7 * 640) * 3;
        assert_eq!(&rgb[at..at + 3], &PALETTE[15]);
        // Alternate set reads the glyph $1000 higher.
        v.ram[0x3000 + 16] = 0xFF;
        v.ram[0] = 1;
        v.ram[0x0800] = 0x80 | 0x0F;
        let (_, _, rgb) = v.render();
        assert_eq!(&rgb[0..3], &PALETTE[15]);
    }

    #[test]
    fn the_cursor_inverts_its_scan_lines_at_its_position() {
        let mut v = text_vdc();
        set(&mut v, 10, 0x00); // solid cursor from line 0
        set(&mut v, 11, 0x07);
        set(&mut v, 14, 0x00);
        set(&mut v, 15, 0x01); // cell 1
        v.ram[0x0801] = 0x0F;
        let (_, _, rgb) = v.render();
        assert_eq!(&rgb[8 * 3..8 * 3 + 3], &PALETTE[15], "blank cell inverted");
        assert_eq!(&rgb[0..3], &PALETTE[0], "other cells untouched");
    }

    #[test]
    fn blinking_attribute_follows_the_frame_counter() {
        let mut v = text_vdc();
        v.ram[0x2000 + 16] = 0xFF;
        v.ram[0] = 1;
        v.ram[0x0800] = 0x10 | 0x0F; // blink
        v.set_clock(60_000); // 1000 cycles per frame
        let mut seen_lit = false;
        let mut seen_dark = false;
        for _ in 0..40 {
            v.tick(1000);
            let (_, _, rgb) = v.render();
            if rgb[0..3] == PALETTE[15] {
                seen_lit = true;
            } else {
                seen_dark = true;
            }
        }
        assert!(seen_lit && seen_dark);
    }

    #[test]
    fn the_blank_flag_is_set_below_the_active_area() {
        let mut v = Vdc::new(true);
        v.set_clock(60_000);
        assert_eq!(v.read(0) & 0x20, 0);
        v.tick(900);
        assert_ne!(v.read(0) & 0x20, 0);
    }
}
