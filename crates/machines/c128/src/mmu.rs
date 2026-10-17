//! The C128's MMU (8722): which RAM bank, ROM or I/O every 16K region of the
//! CPU's address space shows, which RAM is shared between the two banks, and
//! where the 8502's zero page and stack really live.
//!
//! Registers (`$D500-$D50B`, also `$FF00-$FF04` for the first five):
//!
//! | reg | name | |
//! |---|---|---|
//! | 0 | CR   | configuration: bit 0 I/O off, bit 1 `$4000-$7FFF` RAM, bits 3:2 `$8000-$BFFF` (BASIC hi / int. function / ext. function / RAM), bits 5:4 `$C000-$FFFF` (kernal / int. / ext. / RAM), bits 7:6 RAM bank |
//! | 1-4 | PCRA-D | pre-configurations; a write to `$FF01-$FF04` copies the register into CR |
//! | 5 | MCR  | bit 0 CPU (0 = Z80, 1 = 8502), bit 3 fast serial direction, bit 6 C64 mode, bit 7 reads the 40/80 key |
//! | 6 | RCR  | bits 7:6 VIC RAM bank, bits 1:0 shared size (1K/4K/8K/16K), bit 2 share bottom, bit 3 share top |
//! | 7/8 | P0L/P0H | where page 0 really is (bank in P0H bit 0); P0H is latched until P0L is written |
//! | 9/10 | P1L/P1H | the same for page 1 (the stack) |
//! | 11 | version | reads `$20` |
//!
//! Behaviour from the C128 Programmer's Reference Guide and VICE's
//! `c128mmu.c` (reference only, see `CREDITS.md`).

use serde::{Deserialize, Serialize};

/// What a CPU address resolves to for RAM-versus-ROM purposes (before I/O).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Ram,
    BasicLo,
    BasicHi,
    Editor,
    Kernal,
    InternalFunction,
    ExternalFunction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mmu {
    /// `[0]` CR, `[1..=4]` PCRA-D, `[5]` MCR, `[6]` RCR, `[7]` P0L,
    /// `[8]` P0H, `[9]` P1L, `[10]` P1H.
    regs: [u8; 11],
    p0h_latch: u8,
    p1h_latch: u8,
    /// The 40/80 column key as the MCR reads it (true = released = 40).
    pub key_40_released: bool,
}

impl Default for Mmu {
    fn default() -> Self {
        Self::new()
    }
}

impl Mmu {
    pub fn new() -> Self {
        let mut mmu = Self {
            regs: [0; 11],
            p0h_latch: 0,
            p1h_latch: 0,
            key_40_released: true,
        };
        mmu.reset();
        mmu
    }

    pub fn reset(&mut self) {
        self.regs = [0; 11];
        self.regs[9] = 1; // page 1 is page 1
        self.p0h_latch = 0;
        self.p1h_latch = 0;
    }

    pub fn cr(&self) -> u8 {
        self.regs[0]
    }

    pub fn mcr(&self) -> u8 {
        self.regs[5]
    }

    pub fn rcr(&self) -> u8 {
        self.regs[6]
    }

    /// `true` while the 8502 (not the Z80) is the CPU in charge.
    pub fn cpu_is_8502(&self) -> bool {
        self.regs[5] & 1 != 0
    }

    pub fn c64_mode(&self) -> bool {
        self.regs[5] & 0x40 != 0
    }

    pub fn fast_serial_out(&self) -> bool {
        self.regs[5] & 0x08 != 0
    }

    /// The 64K block the VIC sees (RCR bits 7:6; a stock C128 has two).
    pub fn vic_bank(&self) -> usize {
        usize::from((self.regs[6] >> 6) & 1)
    }

    /// The RAM bank the CPU sees outside the shared areas (CR bit 6).
    pub fn cpu_bank(&self) -> usize {
        usize::from((self.regs[0] >> 6) & 1)
    }

    pub fn p0(&self) -> (u8, usize) {
        (self.regs[7], usize::from(self.regs[8] & 1))
    }

    pub fn p1(&self) -> (u8, usize) {
        (self.regs[9], usize::from(self.regs[10] & 1))
    }

    /// Register read (`reg` `0..=0xB`; anything else is open).
    pub fn read(&self, reg: u8, game_exrom: (bool, bool)) -> u8 {
        match reg {
            5 => {
                // Bit 7: 40/80 key released; bits 5:4: the GAME and EXROM
                // lines as inputs (1 = high = no cartridge pulling it low).
                let (game, exrom) = game_exrom;
                (self.regs[5] & 0x0F)
                    | if self.key_40_released { 0x80 } else { 0 }
                    | if game { 0x10 } else { 0 }
                    | if exrom { 0x20 } else { 0 }
                    | (self.regs[5] & 0x40)
            }
            8 | 10 => self.regs[usize::from(reg)] | 0xF0,
            11 => 0x20,
            0..=10 => self.regs[usize::from(reg)],
            _ => 0xFF,
        }
    }

    /// Register write.
    pub fn write(&mut self, reg: u8, value: u8) {
        match reg {
            0..=4 | 6 => self.regs[usize::from(reg)] = value,
            5 => self.regs[5] = (value & 0x7F) | 0x30,
            // The high bytes are latched; the low byte commits both.
            8 => self.p0h_latch = value,
            10 => self.p1h_latch = value,
            7 => {
                self.regs[7] = value;
                self.regs[8] = self.p0h_latch;
            }
            9 => {
                self.regs[9] = value;
                self.regs[10] = self.p1h_latch;
            }
            _ => {}
        }
    }

    /// A write to `$FF01-$FF04`: load the matching pre-configuration into CR.
    pub fn load_config(&mut self, n: u8) {
        self.regs[0] = self.regs[usize::from(n)];
    }

    /// Size of the shared (common) RAM areas in bytes.
    pub fn shared_size(&self) -> usize {
        match self.regs[6] & 3 {
            0 => 1024,
            n => 2048 << n,
        }
    }

    /// Whether `address` is in shared RAM (always bank 0).
    pub fn is_shared(&self, address: u16) -> bool {
        let size = self.shared_size();
        let a = usize::from(address);
        (self.regs[6] & 4 != 0 && a < size) || (self.regs[6] & 8 != 0 && a >= 0x1_0000 - size)
    }

    /// What the CR makes of a (non-I/O) address in C128 mode.
    pub fn source(&self, address: u16) -> Source {
        let cr = self.regs[0];
        match address {
            0x0000..=0x3FFF => Source::Ram,
            0x4000..=0x7FFF => {
                if cr & 0x02 == 0 {
                    Source::BasicLo
                } else {
                    Source::Ram
                }
            }
            0x8000..=0xBFFF => match (cr >> 2) & 3 {
                0 => Source::BasicHi,
                1 => Source::InternalFunction,
                2 => Source::ExternalFunction,
                _ => Source::Ram,
            },
            _ => match (cr >> 4) & 3 {
                0 => {
                    if address < 0xD000 {
                        Source::Editor
                    } else {
                        Source::Kernal
                    }
                }
                1 => Source::InternalFunction,
                2 => Source::ExternalFunction,
                _ => Source::Ram,
            },
        }
    }

    /// The I/O block is visible at `$D000-$DFFF` (CR bit 0 clear).
    pub fn io_visible(&self) -> bool {
        self.regs[0] & 1 == 0
    }

    /// Whether page `page` of `bank` is RAM under the current CR -- the
    /// condition for the page-0/page-1 swap to apply to it.
    fn page_is_ram(&self, page: u8, bank: usize) -> bool {
        if bank != self.cpu_bank() {
            return false;
        }
        let cr = self.regs[0];
        match page {
            0x00..=0x3F => true,
            0x40..=0x7F => cr & 0x02 != 0,
            0x80..=0xBF => (cr >> 2) & 3 == 3,
            0xD0..=0xDF => cr & 1 != 0 && (cr >> 4) & 3 == 3,
            _ => (cr >> 4) & 3 == 3,
        }
    }

    /// Where an access to `address` lands in the 128K of RAM once the
    /// page 0/1 relocation and the shared areas are applied, or `None` when
    /// the plain bank mapping applies.
    ///
    /// Page 0 (the 8502's zero page) reads page P0 of bank P0H; a page equal
    /// to P0 (when it is RAM in the current bank) reads page 0 of that bank
    /// -- the two swap places. The stack page does the same with P1. The
    /// shared areas override the bank (they are always bank 0).
    pub fn relocated(&self, address: u16) -> Option<usize> {
        let page = (address >> 8) as u8;
        let pos = usize::from(address & 0xFF);
        let (p0, p0b) = self.p0();
        let (p1, p1b) = self.p1();
        let shared_zero = self.is_shared(0);
        let shared_stack = self.is_shared(0x100);
        let pick = |target_page: u8, bank: usize, shared: bool| -> usize {
            let bank = if shared { 0 } else { bank };
            (bank << 16) | (usize::from(target_page) << 8) | pos
        };
        if page == 0 && (p0 != 0 || p0b != 0) {
            return Some(pick(p0, p0b, self.is_shared(u16::from(p0) << 8)));
        }
        if page == 1 && (p1 != 1 || p1b != 0) {
            return Some(pick(p1, p1b, self.is_shared(u16::from(p1) << 8)));
        }
        if page == p0 && page != 0 && self.page_is_ram(p0, p0b) && (p0 != 0 || p0b != 0) {
            return Some(pick(0, p0b, shared_zero));
        }
        if page == p1 && page != 1 && self.page_is_ram(p1, p1b) && (p1 != 1 || p1b != 0) {
            return Some(pick(1, p1b, shared_stack));
        }
        None
    }

    /// The physical RAM index of a plain (non-relocated) RAM access.
    pub fn ram_index(&self, address: u16) -> usize {
        let bank = if self.is_shared(address) {
            0
        } else {
            self.cpu_bank()
        };
        (bank << 16) | usize::from(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_state_is_z80_with_everything_rom_and_page_1_in_place() {
        let mmu = Mmu::new();
        assert!(!mmu.cpu_is_8502());
        assert_eq!(mmu.cr(), 0);
        assert!(mmu.io_visible());
        assert_eq!(mmu.p0(), (0, 0));
        assert_eq!(mmu.p1(), (1, 0));
        assert_eq!(mmu.read(11, (true, true)), 0x20);
    }

    fn cr_selects_rom_ram_and_function_roms_per_region() {
        let mut mmu = Mmu::new();
        assert_eq!(mmu.source(0x1000), Source::Ram);
        assert_eq!(mmu.source(0x5000), Source::BasicLo);
        assert_eq!(mmu.source(0x9000), Source::BasicHi);
        assert_eq!(mmu.source(0xC800), Source::Editor);
        assert_eq!(mmu.source(0xE000), Source::Kernal);
        mmu.write(0, 0b0011_1111);
        assert_eq!(mmu.source(0x5000), Source::Ram);
        assert_eq!(mmu.source(0x9000), Source::Ram);
        assert_eq!(mmu.source(0xE000), Source::Ram);
        assert!(!mmu.io_visible());
        mmu.write(0, 0b0001_0100);
        assert_eq!(mmu.source(0x9000), Source::InternalFunction);
        assert_eq!(mmu.source(0xE000), Source::InternalFunction);
        mmu.write(0, 0b0010_1000);
        assert_eq!(mmu.source(0x9000), Source::ExternalFunction);
        assert_eq!(mmu.source(0xE000), Source::ExternalFunction);
    }

    fn writing_ff01_to_ff04_loads_the_matching_preconfiguration_into_cr() {
        let mut mmu = Mmu::new();
        mmu.write(1, 0x3E);
        mmu.write(2, 0x7F);
        mmu.load_config(1);
        assert_eq!(mmu.cr(), 0x3E);
        mmu.load_config(2);
        assert_eq!(mmu.cr(), 0x7F);
        assert_eq!(mmu.cpu_bank(), 1);
    }
    /// Merged (every check kept): `reset_state_is_z80_with_everything_rom_and_page_1_in_place`, `cr_selects_rom_ram_and_function_roms_per_region`, `writing_ff01_to_ff04_loads_the_matching_preconfiguration_into_cr`.
    #[test]
    fn cr_and_preconfig() {
        reset_state_is_z80_with_everything_rom_and_page_1_in_place();
        cr_selects_rom_ram_and_function_roms_per_region();
        writing_ff01_to_ff04_loads_the_matching_preconfiguration_into_cr();
    }

    fn shared_ram_is_always_bank_zero_and_sized_by_rcr() {
        let mut mmu = Mmu::new();
        mmu.write(0, 0x40); // bank 1
        assert_eq!(mmu.ram_index(0x2000), 0x1_2000);
        mmu.write(6, 0b0000_0110); // share bottom, 8K
        assert!(mmu.is_shared(0x1FFF));
        assert!(!mmu.is_shared(0x2000));
        assert_eq!(
            mmu.ram_index(0x1FFF),
            0x1FFF,
            "bank 0 although bank 1 selected"
        );
        assert_eq!(mmu.ram_index(0x2000), 0x1_2000);
        mmu.write(6, 0b0000_1000); // share top, 1K
        assert!(mmu.is_shared(0xFC00));
        assert!(!mmu.is_shared(0xFBFF));
        assert_eq!(mmu.ram_index(0xFFFF), 0xFFFF);
    }

    fn the_high_page_pointer_bytes_are_latched_until_the_low_byte_is_written() {
        let mut mmu = Mmu::new();
        mmu.write(8, 1);
        assert_eq!(mmu.p0(), (0, 0), "P0H not committed yet");
        mmu.write(7, 0x20);
        assert_eq!(mmu.p0(), (0x20, 1));
        assert_eq!(
            mmu.read(8, (true, true)),
            0xF1,
            "unused high nibble reads 1"
        );
    }

    fn page_zero_relocation_swaps_page_0_and_the_target_page() {
        let mut mmu = Mmu::new();
        mmu.write(7, 0x30); // page 0 now lives at page $30 of bank 0
        assert_eq!(mmu.relocated(0x0010), Some(0x3010));
        assert_eq!(
            mmu.relocated(0x3010),
            Some(0x0010),
            "and page $30 shows page 0"
        );
        assert_eq!(mmu.relocated(0x2010), None);
        // The stack follows P1 (page $40 is RAM once $4000-$7FFF is).
        mmu.write(0, 0x02);
        mmu.write(9, 0x40);
        assert_eq!(mmu.relocated(0x01FF), Some(0x40FF));
        assert_eq!(mmu.relocated(0x40FF), Some(0x01FF));
    }

    fn a_relocation_target_that_is_rom_is_not_swapped_back() {
        let mut mmu = Mmu::new();
        mmu.write(7, 0x50); // page $50 is BASIC ROM in the reset configuration
        assert_eq!(mmu.relocated(0x0010), Some(0x5010));
        assert_eq!(mmu.relocated(0x5010), None);
    }
    /// Merged (every check kept): `shared_ram_is_always_bank_zero_and_sized_by_rcr`, `the_high_page_pointer_bytes_are_latched_until_the_low_byte_is_written`, `page_zero_relocation_swaps_page_0_and_the_target_page`, `a_relocation_target_that_is_rom_is_not_swapped_back`.
    #[test]
    fn ram_sharing_and_relocation() {
        shared_ram_is_always_bank_zero_and_sized_by_rcr();
        the_high_page_pointer_bytes_are_latched_until_the_low_byte_is_written();
        page_zero_relocation_swaps_page_0_and_the_target_page();
        a_relocation_target_that_is_rom_is_not_swapped_back();
    }

    #[test]
    fn mcr_reports_the_40_80_key_and_forces_unused_bits() {
        let mut mmu = Mmu::new();
        mmu.write(5, 0x01);
        assert!(mmu.cpu_is_8502());
        assert_eq!(mmu.read(5, (true, true)) & 0x80, 0x80);
        mmu.key_40_released = false;
        assert_eq!(mmu.read(5, (true, true)) & 0x80, 0);
        mmu.write(5, 0x41);
        assert!(mmu.c64_mode());
    }
}
