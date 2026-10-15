//! The AMD AM29F040B flash command state machine EasyFlash uses (one chip
//! for ROML, one for ROMH, 512 KB each). Follows the command set in the
//! datasheet as VICE's `flash040core.c` models it (read as a reference, see
//! `CREDITS.md`): unlock cycles `$AA`/`$55` at `$555`/`$2AA`, autoselect,
//! byte program (bits only go 1 -> 0), sector and chip erase with their
//! timeouts, and the DQ6 toggle / DQ7 status bits while busy.
//!
//! The chip holds no data of its own: the caller passes the 512 KB array so
//! it stays in the cartridge's ROM vectors.

use serde::{Deserialize, Serialize};

pub const FLASH_SIZE: usize = 0x80000;
const SECTOR_SIZE: usize = 0x10000;
const MAGIC_1: usize = 0x555;
const MAGIC_2: usize = 0x2AA;
const MAGIC_MASK: usize = 0x7FF;
const TOGGLE_BITS: u8 = 0x40;
/// Cycles the erase command waits for another sector before it starts.
const ERASE_TIMEOUT_CYCLES: u32 = 50;
const ERASE_SECTOR_CYCLES: u32 = 1_000_000;
const ERASE_CHIP_CYCLES: u32 = 8_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum State {
    Read,
    Magic1,
    Magic2,
    Autoselect,
    ByteProgram,
    ByteProgramError,
    EraseMagic1,
    EraseMagic2,
    EraseSelect,
    ChipErase,
    SectorEraseTimeout,
    SectorErase,
    SectorEraseSuspend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flash040 {
    state: State,
    base_state: State,
    program_byte: u8,
    last_read: u8,
    /// One bit per 64 KB sector queued for erase.
    erase_mask: u8,
    /// Cycles until the pending erase step happens.
    countdown: Option<u32>,
    /// Flips on every status read (DQ6 while programming).
    toggle: bool,
}

impl Default for Flash040 {
    fn default() -> Self {
        Self::new()
    }
}

impl Flash040 {
    pub fn new() -> Self {
        Self {
            state: State::Read,
            base_state: State::Read,
            program_byte: 0,
            last_read: 0,
            erase_mask: 0,
            countdown: None,
            toggle: false,
        }
    }

    /// The reset line / `$F0` command: back to array-read mode.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// True while a command sequence or erase is in progress.
    pub fn busy(&self) -> bool {
        self.state != State::Read && self.state != State::Autoselect
    }

    fn magic_1(addr: usize) -> bool {
        addr & MAGIC_MASK == MAGIC_1
    }

    fn magic_2(addr: usize) -> bool {
        addr & MAGIC_MASK == MAGIC_2
    }

    /// A bus read. Reads during a command show chip status instead of data.
    pub fn read(&mut self, data: &[u8], addr: usize) -> u8 {
        let addr = addr % FLASH_SIZE;
        let value = match self.state {
            State::Autoselect => match addr & 0xFF {
                0 => 0x01, // manufacturer: AMD
                1 => 0xA4, // device: AM29F040
                2 => 0,    // sector protection
                _ => data[addr],
            },
            State::ByteProgramError => {
                self.toggle = !self.toggle;
                // DQ7 = inverse of the data, DQ6 toggles, DQ5 = timeout.
                ((self.program_byte ^ 0x80) & 0x80) | (u8::from(self.toggle) << 6) | (1 << 5)
            }
            State::SectorEraseSuspend
            | State::ChipErase
            | State::SectorErase
            | State::SectorEraseTimeout => {
                let mut v = self.program_byte;
                self.program_byte ^= TOGGLE_BITS;
                if self.state != State::SectorEraseTimeout {
                    v |= 0x08; // DQ3: the erase timer has expired
                }
                v
            }
            _ => data[addr],
        };
        self.last_read = value;
        value
    }

    /// A bus write. Returns `true` when the array changed.
    pub fn store(&mut self, data: &mut [u8], addr: usize, byte: u8) -> bool {
        let addr = addr % FLASH_SIZE;
        let mut changed = false;
        match self.state {
            State::Read => {
                if Self::magic_1(addr) && byte == 0xAA {
                    self.state = State::Magic1;
                }
            }
            State::Magic1 => {
                self.state = if Self::magic_2(addr) && byte == 0x55 {
                    State::Magic2
                } else {
                    self.base_state
                };
            }
            State::Magic2 => {
                self.state = if Self::magic_1(addr) {
                    match byte {
                        0x90 => {
                            self.base_state = State::Autoselect;
                            State::Autoselect
                        }
                        0xF0 => {
                            self.base_state = State::Read;
                            State::Read
                        }
                        0xA0 => State::ByteProgram,
                        0x80 => State::EraseMagic1,
                        _ => self.base_state,
                    }
                } else {
                    self.base_state
                };
            }
            State::ByteProgram => {
                let old = data[addr];
                let new = old & byte;
                self.program_byte = byte;
                data[addr] = new;
                changed = true;
                self.state = if new == byte {
                    self.base_state
                } else {
                    State::ByteProgramError
                };
            }
            State::EraseMagic1 => {
                self.state = if Self::magic_1(addr) && byte == 0xAA {
                    State::EraseMagic2
                } else {
                    self.base_state
                };
            }
            State::EraseMagic2 => {
                self.state = if Self::magic_2(addr) && byte == 0x55 {
                    State::EraseSelect
                } else {
                    self.base_state
                };
            }
            State::EraseSelect => {
                if Self::magic_1(addr) && byte == 0x10 {
                    self.state = State::ChipErase;
                    self.program_byte = 0;
                    self.countdown = Some(ERASE_CHIP_CYCLES);
                } else if byte == 0x30 {
                    self.erase_mask |= 1 << (addr / SECTOR_SIZE);
                    self.program_byte = 0;
                    self.state = State::SectorEraseTimeout;
                    self.countdown = Some(ERASE_TIMEOUT_CYCLES);
                } else {
                    self.state = self.base_state;
                }
            }
            State::SectorEraseTimeout => {
                if byte == 0x30 {
                    self.erase_mask |= 1 << (addr / SECTOR_SIZE);
                } else {
                    self.state = self.base_state;
                    self.erase_mask = 0;
                    self.countdown = None;
                }
            }
            State::SectorErase => {
                if byte == 0xB0 {
                    self.state = State::SectorEraseSuspend;
                    self.countdown = None;
                }
            }
            State::SectorEraseSuspend => {
                if byte == 0x30 {
                    self.state = State::SectorErase;
                    self.countdown = Some(ERASE_SECTOR_CYCLES);
                }
            }
            State::ByteProgramError | State::Autoselect => {
                if Self::magic_1(addr) && byte == 0xAA {
                    self.state = State::Magic1;
                }
                if byte == 0xF0 {
                    self.state = State::Read;
                    self.base_state = State::Read;
                }
            }
            State::ChipErase => {}
        }
        changed
    }

    /// Advances an erase in progress. Returns `true` when the array changed.
    pub fn tick(&mut self, data: &mut [u8], cycles: u32) -> bool {
        let Some(left) = self.countdown else {
            return false;
        };
        if cycles < left {
            self.countdown = Some(left - cycles);
            return false;
        }
        self.countdown = None;
        match self.state {
            State::SectorEraseTimeout => {
                self.state = State::SectorErase;
                self.countdown = Some(ERASE_SECTOR_CYCLES);
                false
            }
            State::SectorErase => {
                let sector = (0..8).find(|s| self.erase_mask & (1 << s) != 0);
                let mut changed = false;
                if let Some(s) = sector {
                    data[s * SECTOR_SIZE..(s + 1) * SECTOR_SIZE].fill(0xFF);
                    self.erase_mask &= !(1 << s);
                    changed = true;
                }
                if self.erase_mask != 0 {
                    self.countdown = Some(ERASE_SECTOR_CYCLES);
                } else {
                    self.state = self.base_state;
                }
                changed
            }
            State::ChipErase => {
                data[..FLASH_SIZE].fill(0xFF);
                self.state = self.base_state;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chip() -> (Flash040, Vec<u8>) {
        (Flash040::new(), vec![0xFF; FLASH_SIZE])
    }

    fn unlock(f: &mut Flash040, d: &mut [u8]) {
        f.store(d, 0x555, 0xAA);
        f.store(d, 0x2AA, 0x55);
    }

    fn program(f: &mut Flash040, d: &mut [u8], addr: usize, byte: u8) {
        unlock(f, d);
        f.store(d, 0x555, 0xA0);
        f.store(d, addr, byte);
    }

    #[test]
    fn programming_clears_bits_and_returns_to_read_mode() {
        let (mut f, mut d) = chip();
        program(&mut f, &mut d, 0x1234, 0x5A);
        assert_eq!(f.read(&d, 0x1234), 0x5A);
        // Programming can only clear bits: 0x5A & 0xF0 = 0x50, not 0xF0 -> error state.
        program(&mut f, &mut d, 0x1234, 0xF0);
        assert_eq!(d[0x1234], 0x50);
        assert!(f.busy(), "a failed program leaves the error status up");
        let status = f.read(&d, 0x1234);
        assert_eq!(status & 0x20, 0x20, "DQ5 timeout flag");
        f.store(&mut d, 0, 0xF0); // reset command
        assert_eq!(f.read(&d, 0x1234), 0x50);
    }

    #[test]
    fn autoselect_reports_amd_and_the_device_code_until_reset() {
        let (mut f, mut d) = chip();
        unlock(&mut f, &mut d);
        f.store(&mut d, 0x555, 0x90);
        assert_eq!((f.read(&d, 0), f.read(&d, 1)), (0x01, 0xA4));
        f.store(&mut d, 0, 0xF0);
        assert_eq!(f.read(&d, 0), 0xFF);
    }

    #[test]
    fn a_wrong_unlock_sequence_does_nothing() {
        let (mut f, mut d) = chip();
        f.store(&mut d, 0x555, 0xAA);
        f.store(&mut d, 0x2AA, 0x54);
        f.store(&mut d, 0x555, 0xA0);
        f.store(&mut d, 0x10, 0x00);
        assert_eq!(d[0x10], 0xFF);
    }

    #[test]
    fn sector_erase_waits_then_clears_only_that_sector() {
        let (mut f, mut d) = chip();
        d[0x0005] = 0x00;
        d[0x10005] = 0x00;
        unlock(&mut f, &mut d);
        f.store(&mut d, 0x555, 0x80);
        unlock(&mut f, &mut d);
        f.store(&mut d, 0x10000, 0x30); // sector 1
        assert!(f.busy());
        assert_eq!(d[0x10005], 0x00, "nothing is erased immediately");
        // DQ3 clear while the timeout window is open.
        assert_eq!(f.read(&d, 0x10000) & 0x08, 0);
        assert!(!f.tick(&mut d, ERASE_TIMEOUT_CYCLES));
        assert_eq!(f.read(&d, 0x10000) & 0x08, 0x08);
        assert!(f.tick(&mut d, ERASE_SECTOR_CYCLES));
        assert_eq!(d[0x10005], 0xFF);
        assert_eq!(d[0x0005], 0x00, "other sectors are untouched");
        assert!(!f.busy());
    }

    #[test]
    fn chip_erase_clears_everything_after_its_delay() {
        let (mut f, mut d) = chip();
        d[0x7FFFF] = 0;
        unlock(&mut f, &mut d);
        f.store(&mut d, 0x555, 0x80);
        unlock(&mut f, &mut d);
        f.store(&mut d, 0x555, 0x10);
        assert!(f.tick(&mut d, ERASE_CHIP_CYCLES));
        assert_eq!(d[0x7FFFF], 0xFF);
    }
}
