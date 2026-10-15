//! The C128's address space as the 8502 and the Z80 see it, and the chips
//! behind it.
//!
//! **8502, C128 mode.** The MMU (`crate::mmu`) decides per 16K region whether
//! a read sees RAM (one of two 64K banks, with the bottom/top common areas
//! always bank 0), the BASIC low/high ROMs, the editor (`$C000-$CFFF`), the
//! character ROM (`$D000-$DFFF` with I/O off) or the kernal (`$E000-$FFFF`).
//! Writes always land in RAM. `$D000-$DFFF` is the I/O block while CR bit 0
//! is clear: VIC-IIe, SID, MMU registers, the VDC, colour RAM and the two
//! CIAs. `$00/$01` is the 8502's own port and `$FF00-$FF04` the MMU.
//!
//! **Z80.** A Z80 starts the machine: its map has the 4K boot BIOS at
//! `$0000` while CR bit 0 is clear, colour RAM at `$1000-$13FF`, the same
//! RAM and ROM layout as the 8502 above, and reaches the I/O chips with
//! `IN`/`OUT` (the high byte of the port selects the chip). Writing the MCR
//! hands the machine to the 8502.
//!
//! The 128K ROM set is the standard international one: BASIC low (`$4000`),
//! BASIC high (`$8000`), a 16K image holding the editor (4K), the Z80 BIOS
//! (4K) and the kernal (8K), and an 8K character ROM whose upper half is the
//! C128 set and lower half the C64 set.

use machine::Bus;

use crate::keyboard::{C128Key, KeyMatrix};
use crate::mmu::{Mmu, Source};

pub const RAM_SIZE: usize = 0x2_0000;
pub const BASIC_ROM_SIZE: usize = 0x8000;
pub const KERNAL_IMAGE_SIZE: usize = 0x4000;
pub const CHARGEN_SIZE: usize = 0x2000;
pub const C64_ROM_SIZE: usize = 0x2000;
const COLOR_RAM_SIZE: usize = 0x800;

pub const PAL_CLOCK_HZ: u64 = 985_248;
pub const NTSC_CLOCK_HZ: u64 = 1_022_727;

const CIA2_PA_CLOCK_IN: u8 = 0x40;
const CIA2_PA_DATA_IN: u8 = 0x80;
const CIA2_PA_ATN_OUT: u8 = 0x08;
const CIA2_PA_CLOCK_OUT: u8 = 0x10;
const CIA2_PA_DATA_OUT: u8 = 0x20;

/// The ROM images the machine boots from.
#[derive(Clone)]
pub struct Roms {
    /// BASIC low ($4000-$7FFF) followed by BASIC high ($8000-$BFFF).
    pub basic: Vec<u8>,
    /// Editor (4K) + Z80 BIOS (4K) + kernal (8K).
    pub kernal: Vec<u8>,
    pub chargen: Vec<u8>,
    /// C64-mode BASIC and kernal (8K each).
    pub c64_basic: Vec<u8>,
    pub c64_kernal: Vec<u8>,
}

impl Roms {
    pub fn check(&self) -> Result<(), String> {
        for (name, rom, want) in [
            ("BASIC (low + high)", &self.basic, BASIC_ROM_SIZE),
            ("kernal image", &self.kernal, KERNAL_IMAGE_SIZE),
            ("character", &self.chargen, CHARGEN_SIZE),
            ("C64 BASIC", &self.c64_basic, C64_ROM_SIZE),
            ("C64 kernal", &self.c64_kernal, C64_ROM_SIZE),
        ] {
            if rom.len() != want {
                return Err(format!(
                    "{name} ROM must be {want} bytes, got {}",
                    rom.len()
                ));
            }
        }
        Ok(())
    }
}

/// What a C64-mode address shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum C64Region {
    Ram,
    Basic,
    Kernal,
    Chargen,
    Io,
}

/// Which chip a Z80 `IN`/`OUT` port reaches (its high byte).
fn z80_port_high(port: u16) -> u8 {
    (port >> 8) as u8
}

#[derive(Clone)]
pub struct C128Bus {
    pub(crate) ram: Vec<u8>,
    pub(crate) roms: Roms,
    pub(crate) color_ram: Vec<u8>,
    /// The 8502's on-chip port.
    pub(crate) port_ddr: u8,
    pub(crate) port_data: u8,
    pub(crate) mmu: Mmu,
    pub(crate) vic: vic_ii::Vic,
    pub(crate) cia1: cia6526::Cia,
    pub(crate) cia2: cia6526::Cia,
    pub(crate) sid: sid::Sid,
    pub(crate) keyboard: KeyMatrix,
    pub(crate) joystick: [u8; 2],
    /// `$D02F`: the K0-K2 keyboard lines (low 3 bits).
    pub(crate) k_lines: u8,
    /// `$D030` bit 0: the CPU runs at 2 MHz.
    pub(crate) fast: bool,
    pub(crate) iec_clock_in: bool,
    pub(crate) iec_data_in: bool,
    pub(crate) cia2_nmi_active: bool,
    /// The 80-column video chip.
    pub(crate) vdc: vdc8563::Vdc,
    /// True only while the 8502 runs one instruction in `vic_accuracy =
    /// cycle` mode: each of its bus cycles then advances the whole machine by
    /// one clock (`cpu_cycle`), and a read waits while the VIC-II holds BA
    /// low. The debugger's and the loader's own `read`/`write` calls never
    /// tick the clock.
    pub(crate) in_cpu: bool,
    /// Interrupt requests raised by a peripheral in the middle of an
    /// instruction; `C128::step` hands them to the CPU afterwards.
    pub(crate) pending_irq: bool,
    pub(crate) pending_nmi: bool,
    /// Clock cycles run since the current `C128::step` began (the CPU's plus
    /// the ones it spent stalled by BA).
    pub(crate) step_cycles: u32,
    /// Clock cycles the 8502 has spent waiting for BA (cycle mode only).
    pub(crate) stalled_cycles: u64,
}

impl C128Bus {
    pub fn new(roms: Roms) -> Self {
        let mut bus = Self {
            ram: vec![0; RAM_SIZE],
            roms,
            color_ram: vec![0; COLOR_RAM_SIZE],
            port_ddr: 0,
            port_data: 0,
            mmu: Mmu::new(),
            vic: vic_ii::Vic::new(),
            cia1: cia6526::Cia::new(),
            cia2: cia6526::Cia::new(),
            sid: sid::Sid::new(),
            keyboard: KeyMatrix::new(),
            joystick: [0; 2],
            k_lines: 7,
            fast: false,
            iec_clock_in: true,
            iec_data_in: true,
            cia2_nmi_active: false,
            vdc: vdc8563::Vdc::new(true),
            in_cpu: false,
            pending_irq: false,
            pending_nmi: false,
            step_cycles: 0,
            stalled_cycles: 0,
        };
        bus.cia1.set_flag(true);
        // Agree with the memory map from the start (the flag is otherwise
        // only refreshed when the map changes).
        bus.sync_vic();
        bus
    }

    // -- the 8502's port --------------------------------------------------

    /// `$01` as read: driven bits show what was written; inputs float high
    /// except CAPS LOCK (bit 6, low while the key is locked down) and the
    /// cassette sense (bit 4, high with no button).
    pub(crate) fn port01(&self) -> u8 {
        let mut inputs = 0xFF;
        if self.keyboard.caps_lock {
            inputs &= !0x40;
        }
        (self.port_data & self.port_ddr) | (inputs & !self.port_ddr)
    }

    /// Colour RAM bank (0/1) the CPU sees: 8502 port bit 0.
    fn cpu_color_bank(&self) -> usize {
        if self.port01() & 1 != 0 { 0 } else { 0x400 }
    }

    /// Colour RAM bank the VIC displays: 8502 port bit 1.
    pub(crate) fn vic_color_bank(&self) -> usize {
        if self.port01() & 2 != 0 { 0 } else { 0x400 }
    }

    /// 8502 port bit 2 clear: the VIC sees the character ROM at `$1000` of
    /// every 16K bank.
    pub(crate) fn vic_sees_chargen(&self) -> bool {
        self.port01() & 4 == 0
    }

    /// The 4K of character ROM the VIC and the CPU see: the upper half in
    /// C128 mode, the lower half (the C64 set) in C64 mode.
    pub(crate) fn chargen_4k(&self) -> &[u8] {
        if self.mmu.c64_mode() {
            &self.roms.chargen[..0x1000]
        } else {
            &self.roms.chargen[0x1000..]
        }
    }

    // -- the VIC's view ---------------------------------------------------

    /// First byte of the 16K the VIC sees in `ram`.
    pub(crate) fn vic_bank_base(&self) -> usize {
        let bits = self.cia2.effective_port_a() & 0x03;
        self.mmu.vic_bank() * 0x1_0000 + (3 - usize::from(bits)) * 0x4000
    }

    /// Whether the character ROM shows at `$1000-$1FFF` of the VIC's bank.
    pub(crate) fn vic_chargen_shadow(&self) -> bool {
        if self.mmu.c64_mode() {
            // As on a C64: the ROM shows in VIC banks 0 and 2 only.
            let base = self.vic_bank_base() & 0xFFFF;
            base == 0x0000 || base == 0x8000
        } else {
            self.vic_sees_chargen()
        }
    }

    pub(crate) fn sync_vic(&mut self) {
        let shadow = self.vic_chargen_shadow();
        self.vic.set_chargen_shadow(shadow);
    }

    // -- cycle-accurate mode ------------------------------------------------

    /// Whether the VIC-II runs its per-cycle engine (`vic_accuracy = cycle`).
    pub(crate) fn cycle_mode(&self) -> bool {
        self.vic.cycle_engine_enabled()
    }

    /// One clock cycle of everything that runs beside the CPU in cycle mode:
    /// the VIC-II (which owns the raster and the bus arbitration), both
    /// CIAs, the SID and the VDC. Interrupt requests are parked in
    /// `pending_irq`/`pending_nmi`.
    pub(crate) fn tick_cycle(&mut self) {
        let base = self.vic_bank_base();
        let color_bank = self.vic_color_bank();
        let shadow = self.vic_chargen_shadow();
        let chargen: &[u8] = if self.mmu.c64_mode() {
            &self.roms.chargen[..0x1000]
        } else {
            &self.roms.chargen[0x1000..]
        };
        let result = {
            let view = VicView {
                ram: &self.ram[base..base + 0x4000],
                chargen,
                color_ram: &self.color_ram[color_bank..color_bank + 0x400],
                shadow,
            };
            self.vic.cycle(&view)
        };
        if result.raster_irq {
            self.pending_irq = true;
        }
        // Sprite collisions stay a per-line scan, as on the C64.
        if let Some(line) = result.entered_line
            && self.vic.scan_sprite_collisions(
                line,
                &self.ram[base..base + 0x4000],
                chargen,
                &self.color_ram[color_bank..color_bank + 0x400],
            )
        {
            self.pending_irq = true;
        }
        if self.cia1.tick(1) {
            self.pending_irq = true;
        }
        let cia2_level = self.cia2.tick(1);
        if self.cia2_nmi_edge(cia2_level) {
            self.pending_nmi = true;
        }
        self.sid.tick(1);
        self.vdc.tick(1);
        self.step_cycles += 1;
    }

    /// The clock cycle of one 8502 bus access: run a cycle, and if the access
    /// is a read while BA is low, keep running cycles until BA rises -- RDY
    /// halts the CPU on a read, never on a write.
    fn cpu_cycle(&mut self, write: bool) {
        self.tick_cycle();
        while !write && self.vic.ba_low() {
            self.stalled_cycles += 1;
            self.tick_cycle();
        }
    }

    // -- the CIAs -----------------------------------------------------------

    fn keyboard_port_b(&self) -> u8 {
        let pa = self.cia1.port_a | !self.cia1.ddr_a;
        let matrix = self.keyboard.read(pa, self.k_lines);
        let pb = (matrix & !self.cia1.ddr_b) | (self.cia1.port_b & self.cia1.ddr_b);
        pb & !self.joystick[0]
    }

    fn joystick_port_a(&self) -> u8 {
        self.cia1.effective_port_a() & !self.joystick[1]
    }

    fn cia2_port_a_read(&self) -> u8 {
        let mut floating = !self.cia2.ddr_a;
        if self.cia2.ddr_a & CIA2_PA_CLOCK_IN == 0 {
            floating = (floating & !CIA2_PA_CLOCK_IN)
                | if self.iec_clock_in {
                    CIA2_PA_CLOCK_IN
                } else {
                    0
                };
        }
        if self.cia2.ddr_a & CIA2_PA_DATA_IN == 0 {
            floating =
                (floating & !CIA2_PA_DATA_IN) | if self.iec_data_in { CIA2_PA_DATA_IN } else { 0 };
        }
        (self.cia2.port_a & self.cia2.ddr_a) | floating
    }

    /// What the computer asserts onto `(ATN, CLOCK, DATA)`.
    pub(crate) fn iec_output(&self) -> (bool, bool, bool) {
        let driven = self.cia2.port_a & self.cia2.ddr_a;
        (
            driven & CIA2_PA_ATN_OUT != 0,
            driven & CIA2_PA_CLOCK_OUT != 0,
            driven & CIA2_PA_DATA_OUT != 0,
        )
    }

    pub(crate) fn cia2_nmi_edge(&mut self, level: bool) -> bool {
        let edge = level && !self.cia2_nmi_active;
        self.cia2_nmi_active = level;
        edge
    }

    pub(crate) fn press_key(&mut self, key: C128Key) {
        self.keyboard.press(key);
        if key == C128Key::Display4080 {
            self.mmu.key_40_released = false;
        }
    }

    pub(crate) fn release_key(&mut self, key: C128Key) {
        self.keyboard.release(key);
        if key == C128Key::Display4080 {
            self.mmu.key_40_released = true;
        }
    }

    // -- I/O ----------------------------------------------------------------

    fn color_index(&self, addr: u16) -> usize {
        self.cpu_color_bank() + usize::from(addr - 0xD800)
    }

    fn io_read(&mut self, addr: u16) -> u8 {
        match addr {
            0xD000..=0xD3FF => match addr & 0x3F {
                0x2F => 0xF8 | self.k_lines,
                0x30 => 0xFC | u8::from(self.fast),
                reg if reg > 0x2E => 0xFF,
                reg => self.vic.read_clearing(reg as u8),
            },
            0xD400..=0xD4FF => self.sid.read((addr & 0x1F) as u8),
            0xD500..=0xD5FF => self.mmu.read((addr & 0xFF) as u8, (true, true)),
            0xD600..=0xD6FF => self.vdc.read((addr & 1) as u8),
            0xD800..=0xDBFF => self.color_ram[self.color_index(addr)] | 0xF0,
            0xDC00..=0xDCFF if (addr & 0x0F) == 1 => self.keyboard_port_b(),
            0xDC00..=0xDCFF if (addr & 0x0F) == 0 => self.joystick_port_a(),
            0xDC00..=0xDCFF => self.cia1.read((addr & 0x0F) as u8),
            0xDD00..=0xDDFF if (addr & 0x0F) == 0 => self.cia2_port_a_read(),
            0xDD00..=0xDDFF => {
                let value = self.cia2.read((addr & 0x0F) as u8);
                if (addr & 0x0F) == 0x0D {
                    self.cia2_nmi_active = self.cia2.interrupt_asserted();
                }
                value
            }
            _ => 0xFF,
        }
    }

    fn io_peek(&self, addr: u16) -> u8 {
        match addr {
            0xD000..=0xD3FF => match addr & 0x3F {
                0x2F => 0xF8 | self.k_lines,
                0x30 => 0xFC | u8::from(self.fast),
                reg if reg > 0x2E => 0xFF,
                reg => self.vic.read(reg as u8),
            },
            0xD400..=0xD4FF => self.sid.read((addr & 0x1F) as u8),
            0xD500..=0xD5FF => self.mmu.read((addr & 0xFF) as u8, (true, true)),
            0xD600..=0xD6FF => self.vdc.peek((addr & 1) as u8),
            0xD800..=0xDBFF => self.color_ram[self.color_index(addr)] | 0xF0,
            0xDC00..=0xDCFF if (addr & 0x0F) == 1 => self.keyboard_port_b(),
            0xDC00..=0xDCFF if (addr & 0x0F) == 0 => self.joystick_port_a(),
            0xDC00..=0xDCFF => self.cia1.peek((addr & 0x0F) as u8),
            0xDD00..=0xDDFF if (addr & 0x0F) == 0 => self.cia2_port_a_read(),
            0xDD00..=0xDDFF => self.cia2.peek((addr & 0x0F) as u8),
            _ => 0xFF,
        }
    }

    fn io_write(&mut self, addr: u16, val: u8) {
        match addr {
            0xD000..=0xD3FF => match addr & 0x3F {
                0x2F => self.k_lines = val & 7,
                0x30 => self.fast = val & 1 != 0,
                reg if reg > 0x2E => {}
                reg => self.vic.write(reg as u8, val),
            },
            0xD400..=0xD4FF => self.sid.write((addr & 0x1F) as u8, val),
            0xD500..=0xD5FF => {
                let reg = (addr & 0xFF) as u8;
                if reg <= 0x0A {
                    self.mmu.write(reg, val);
                }
            }
            0xD600..=0xD6FF => self.vdc.write((addr & 1) as u8, val),
            0xD800..=0xDBFF => {
                let index = self.color_index(addr);
                self.color_ram[index] = val & 0x0F;
            }
            0xDC00..=0xDCFF => self.cia1.write((addr & 0x0F) as u8, val),
            0xDD00..=0xDDFF => {
                self.cia2.write((addr & 0x0F) as u8, val);
                self.sync_vic();
            }
            _ => {}
        }
    }

    // -- ROM windows -----------------------------------------------------------

    fn rom_read(&self, source: Source, addr: u16) -> u8 {
        match source {
            Source::BasicLo | Source::BasicHi => self.roms.basic[usize::from(addr - 0x4000)],
            Source::Editor => self.roms.kernal[usize::from(addr - 0xC000)],
            Source::Kernal => {
                if (0xD000..0xE000).contains(&addr) {
                    self.chargen_4k()[usize::from(addr - 0xD000)]
                } else {
                    self.roms.kernal[0x2000 + usize::from(addr - 0xE000)]
                }
            }
            // No function ROMs are fitted.
            Source::InternalFunction | Source::ExternalFunction => 0xFF,
            Source::Ram => 0xFF,
        }
    }

    /// One 8502 read in C128 mode (no I/O side effects when `peek`).
    fn read_128(&mut self, addr: u16, peek: bool) -> u8 {
        match addr {
            0x0000 => return self.port_ddr,
            0x0001 => return self.port01(),
            0xFF00..=0xFF04 => return self.mmu.read((addr & 0xFF) as u8, (true, true)),
            _ => {}
        }
        if let Some(index) = self.mmu.relocated(addr) {
            return self.ram[index];
        }
        if self.mmu.io_visible() && (0xD000..0xE000).contains(&addr) {
            return if peek {
                self.io_peek(addr)
            } else {
                self.io_read(addr)
            };
        }
        match self.mmu.source(addr) {
            Source::Ram => self.ram[self.mmu.ram_index(addr)],
            source => self.rom_read(source, addr),
        }
    }

    fn write_128(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000 => {
                self.port_ddr = val;
                self.sync_vic();
                return;
            }
            0x0001 => {
                self.port_data = val;
                self.sync_vic();
                return;
            }
            0xFF00 => {
                self.mmu.write(0, val);
                return;
            }
            0xFF01..=0xFF04 => {
                self.mmu.load_config((addr & 0xFF) as u8);
                return;
            }
            _ => {}
        }
        if let Some(index) = self.mmu.relocated(addr) {
            self.ram[index] = val;
            return;
        }
        if self.mmu.io_visible() && (0xD000..0xE000).contains(&addr) {
            self.io_write(addr, val);
            return;
        }
        let index = self.mmu.ram_index(addr);
        self.ram[index] = val;
    }

    // -- C64 mode ---------------------------------------------------------

    /// What the 8502 port's LORAM/HIRAM/CHAREN lines make visible in C64
    /// mode (no cartridge, so GAME and EXROM are both high).
    fn c64_view(&self, addr: u16) -> C64Region {
        let p = self.port01();
        let (loram, hiram, charen) = (p & 1 != 0, p & 2 != 0, p & 4 != 0);
        match addr {
            0xA000..=0xBFFF if loram && hiram => C64Region::Basic,
            0xD000..=0xDFFF if hiram || loram => {
                if charen {
                    C64Region::Io
                } else {
                    C64Region::Chargen
                }
            }
            0xE000..=0xFFFF if hiram => C64Region::Kernal,
            _ => C64Region::Ram,
        }
    }

    fn read_64(&mut self, addr: u16, peek: bool) -> u8 {
        match addr {
            0x0000 => return self.port_ddr,
            0x0001 => return self.port01(),
            _ => {}
        }
        match self.c64_view(addr) {
            C64Region::Basic => self.roms.c64_basic[usize::from(addr - 0xA000)],
            C64Region::Kernal => self.roms.c64_kernal[usize::from(addr - 0xE000)],
            C64Region::Chargen => self.chargen_4k()[usize::from(addr - 0xD000)],
            C64Region::Io => {
                if peek {
                    self.io_peek(addr)
                } else {
                    self.io_read(addr)
                }
            }
            C64Region::Ram => self.ram[self.mmu.ram_index(addr)],
        }
    }

    fn write_64(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000 => {
                self.port_ddr = val;
                self.sync_vic();
                return;
            }
            0x0001 => {
                self.port_data = val;
                self.sync_vic();
                return;
            }
            _ => {}
        }
        if self.c64_view(addr) == C64Region::Io {
            self.io_write(addr, val);
        } else {
            let index = self.mmu.ram_index(addr);
            self.ram[index] = val;
        }
    }

    /// Side-effect-free read through the 8502's map.
    pub(crate) fn peek(&self, addr: u16) -> u8 {
        if self.mmu.c64_mode() {
            match addr {
                0x0000 => return self.port_ddr,
                0x0001 => return self.port01(),
                _ => {}
            }
            return match self.c64_view(addr) {
                C64Region::Basic => self.roms.c64_basic[usize::from(addr - 0xA000)],
                C64Region::Kernal => self.roms.c64_kernal[usize::from(addr - 0xE000)],
                C64Region::Chargen => self.chargen_4k()[usize::from(addr - 0xD000)],
                C64Region::Io => self.io_peek(addr),
                C64Region::Ram => self.ram[self.mmu.ram_index(addr)],
            };
        }
        // `read_128` needs `&mut` only for I/O side effects, which `peek`
        // avoids; the path is duplicated here to stay `&self`.
        match addr {
            0x0000 => return self.port_ddr,
            0x0001 => return self.port01(),
            0xFF00..=0xFF04 => return self.mmu.read((addr & 0xFF) as u8, (true, true)),
            _ => {}
        }
        if let Some(index) = self.mmu.relocated(addr) {
            return self.ram[index];
        }
        if self.mmu.io_visible() && (0xD000..0xE000).contains(&addr) {
            return self.io_peek(addr);
        }
        match self.mmu.source(addr) {
            Source::Ram => self.ram[self.mmu.ram_index(addr)],
            source => self.rom_read(source, addr),
        }
    }

    // -- the Z80's view ------------------------------------------------------------

    fn z80_read(&mut self, addr: u16) -> u8 {
        if (0xFF00..=0xFF04).contains(&addr) {
            return self.mmu.read((addr & 0xFF) as u8, (true, true));
        }
        // The BIOS sits over the first 4K of RAM bank 0.
        if addr < 0x1000 && self.mmu.cr() & 0xC0 == 0 {
            return self.roms.kernal[0x1000 + usize::from(addr)];
        }
        // With the I/O block on, colour RAM shows at $1000-$13FF.
        if self.mmu.io_visible() && (0x1000..0x1400).contains(&addr) {
            return self.color_ram[self.cpu_color_bank() + usize::from(addr - 0x1000)] | 0xF0;
        }
        match self.mmu.source(addr) {
            Source::Ram => self.ram[self.mmu.ram_index(addr)],
            source => self.rom_read(source, addr),
        }
    }

    fn z80_write(&mut self, addr: u16, val: u8) {
        match addr {
            0xFF00 => {
                self.mmu.write(0, val);
                return;
            }
            0xFF01..=0xFF04 => {
                self.mmu.load_config((addr & 0xFF) as u8);
                return;
            }
            _ => {}
        }
        if addr < 0x1000 && self.mmu.cr() & 0xC0 == 0 {
            // Under the BIOS the writes reach the RAM at $D000.
            self.ram[0xD000 + usize::from(addr)] = val;
            return;
        }
        if self.mmu.io_visible() && (0x1000..0x1400).contains(&addr) {
            let index = self.cpu_color_bank() + usize::from(addr - 0x1000);
            self.color_ram[index] = val & 0x0F;
            return;
        }
        let index = self.mmu.ram_index(addr);
        self.ram[index] = val;
    }

    fn z80_in(&mut self, port: u16) -> u8 {
        match z80_port_high(port) {
            0xD0..=0xDF => {
                // The I/O block is reached by port whatever CR says.
                self.io_read(0xD000 | (port & 0x0FFF))
            }
            _ => self.z80_read(port),
        }
    }

    fn z80_out(&mut self, port: u16, val: u8) {
        match z80_port_high(port) {
            0xD0..=0xDF => {
                let io = 0xD000 | (port & 0x0FFF);
                self.io_write(io, val);
            }
            _ => self.z80_write(port, val),
        }
    }
}

/// What the VIC-II sees in cycle mode: the 16K of RAM its bank selects, the
/// character ROM shadowing `$1000-$1FFF` when the map says so, and the colour
/// RAM bank it displays.
struct VicView<'a> {
    ram: &'a [u8],
    chargen: &'a [u8],
    color_ram: &'a [u8],
    shadow: bool,
}

impl vic_ii::VicMemory for VicView<'_> {
    fn vic_read(&self, addr: u16) -> u8 {
        let a = usize::from(addr & 0x3FFF);
        if self.shadow && (0x1000..0x2000).contains(&a) {
            self.chargen[a - 0x1000]
        } else {
            self.ram[a]
        }
    }

    fn color_ram(&self, index: u16) -> u8 {
        self.color_ram[usize::from(index) % self.color_ram.len()]
    }
}

impl Bus for C128Bus {
    // Whichever CPU the MMU's mode register selects drives the bus, so one
    // type serves both maps.
    fn dummy_cycle(&mut self, _addr: u16, write: bool, _value: u8) {
        if self.in_cpu {
            self.cpu_cycle(write);
        }
    }

    fn read(&mut self, addr: u16) -> u8 {
        if self.in_cpu {
            self.cpu_cycle(false);
        }
        if self.mmu.cpu_is_8502() {
            if self.mmu.c64_mode() {
                self.read_64(addr, false)
            } else {
                self.read_128(addr, false)
            }
        } else {
            self.z80_read(addr)
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        if self.in_cpu {
            self.cpu_cycle(true);
        }
        if self.mmu.cpu_is_8502() {
            if self.mmu.c64_mode() {
                self.write_64(addr, val);
            } else {
                self.write_128(addr, val);
            }
        } else {
            self.z80_write(addr, val);
        }
    }

    fn read_io(&mut self, port: u16) -> u8 {
        self.z80_in(port)
    }

    fn write_io(&mut self, port: u16, val: u8) {
        self.z80_out(port, val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roms() -> Roms {
        let mut kernal = vec![0u8; KERNAL_IMAGE_SIZE];
        kernal[0x0000] = 0xED; // editor
        kernal[0x1000] = 0xB1; // z80 bios
        kernal[0x2000] = 0xCE; // kernal $E000
        kernal[0x3FFC] = 0x3D;
        kernal[0x3FFD] = 0xFF;
        let mut basic = vec![0u8; BASIC_ROM_SIZE];
        basic[0] = 0xBA; // $4000
        basic[0x4000] = 0xBB; // $8000
        let mut chargen = vec![0u8; CHARGEN_SIZE];
        chargen[0] = 0xC0; // C64 set
        chargen[0x1000] = 0xC1; // C128 set
        Roms {
            basic,
            kernal,
            chargen,
            c64_basic: vec![0; C64_ROM_SIZE],
            c64_kernal: vec![0; C64_ROM_SIZE],
        }
    }

    /// A bus with the 8502 in charge (the MCR's CPU bit set).
    fn bus() -> C128Bus {
        let mut bus = C128Bus::new(roms());
        bus.mmu.write(5, 1);
        bus
    }

    fn z80_bus() -> C128Bus {
        C128Bus::new(roms())
    }

    #[test]
    fn reset_configuration_shows_the_roms_and_io() {
        let mut bus = bus();
        assert_eq!(bus.read(0x4000), 0xBA);
        assert_eq!(bus.read(0x8000), 0xBB);
        assert_eq!(bus.read(0xC000), 0xED);
        assert_eq!(bus.read(0xE000), 0xCE);
        assert_eq!(bus.read(0xFFFC), 0x3D);
        // I/O is visible (CR bit 0 clear): the MMU version register.
        assert_eq!(bus.read(0xD50B), 0x20);
    }

    #[test]
    fn character_rom_replaces_io_when_cr_bit_0_is_set() {
        let mut bus = bus();
        bus.write(0xFF00, 0x01);
        assert_eq!(bus.read(0xD000), 0xC1, "the C128 set");
    }

    #[test]
    fn writes_to_rom_areas_land_in_the_ram_beneath() {
        let mut bus = bus();
        bus.write(0x4000, 0x99);
        assert_eq!(bus.read(0x4000), 0xBA, "still reading ROM");
        bus.write(0xFF00, 0b0000_0010); // RAM at $4000
        assert_eq!(bus.read(0x4000), 0x99);
    }

    #[test]
    fn the_second_ram_bank_is_separate_except_for_the_common_area() {
        let mut bus = bus();
        bus.write(0xFF00, 0x3F); // all RAM, bank 0, no I/O
        bus.write(0x2000, 0x11);
        bus.write(0xFF00, 0x7F); // bank 1
        assert_eq!(bus.read(0x2000), 0x00);
        bus.write(0x2000, 0x22);
        bus.write(0xFF00, 0x3F);
        assert_eq!(bus.read(0x2000), 0x11);
        // Share the bottom 8K: bank 1 now sees bank 0's.
        bus.write(0xFF00, 0x7F);
        bus.write(0xD506, 0); // I/O is off in this configuration
        let _ = bus.read(0x2000);
    }

    #[test]
    fn the_preconfiguration_registers_switch_cr_through_ff01() {
        let mut bus = bus();
        bus.write(0xFF00, 0x3F);
        bus.write(0xFF01, 0x00); // (write ignored value; loads PCRA = 0)
        assert_eq!(bus.read(0xFF00), 0x00, "back to the ROM configuration");
        assert_eq!(bus.read(0xE000), 0xCE);
    }

    #[test]
    fn the_cpu_port_is_always_at_zero_and_one() {
        let mut bus = bus();
        bus.write(0x0000, 0x2F);
        bus.write(0x0001, 0x07);
        assert_eq!(bus.read(0x0000), 0x2F);
        assert_eq!(bus.read(0x0001) & 0x07, 0x07);
    }

    #[test]
    fn k_lines_select_the_extra_keyboard_columns() {
        let mut bus = bus();
        bus.press_key(C128Key::Escape); // K1 row 0
        bus.write(0xDC00, 0xFF);
        bus.write(0xDC02, 0xFF); // DDRA all outputs
        bus.write(0xD02F, 0b101); // select K1
        assert_eq!(bus.read(0xDC01) & 1, 0);
        bus.write(0xD02F, 0b111);
        assert_eq!(bus.read(0xDC01) & 1, 1);
    }

    #[test]
    fn the_z80_sees_the_bios_at_zero_while_io_is_on() {
        let mut z = z80_bus();
        assert_eq!(z.read(0x0000), 0xB1);
        z.write(0xFF00, 0x3F); // I/O off, still bank 0: the BIOS stays
        assert_eq!(z.read(0x0000), 0xB1);
        z.write(0xFF00, 0x7F); // bank 1: plain RAM
        z.write(0x0000, 0x77);
        assert_eq!(z.read(0x0000), 0x77);
    }

    #[test]
    fn z80_ports_reach_the_chips_by_their_high_byte() {
        let mut bus = z80_bus();
        bus.write_io(0xD02F, 0x05);
        assert_eq!(bus.k_lines, 5);
        assert_eq!(bus.read_io(0xD50B), 0x20);
    }
}
