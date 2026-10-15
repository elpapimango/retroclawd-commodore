//! Commodore REU (1700 = 128K, 1764 = 256K, 1750 = 512K): a RAM expansion
//! with a DMA engine on the expansion port, registers at `$DF00-$DF0A`.
//!
//! Behaviour follows VICE's `src/c64/cart/reu.c` (read as a reference, see
//! `CREDITS.md`). A transfer runs to completion the instant it is started
//! and reports how many CPU cycles it took, which the machine adds to the
//! step that started it (the real CPU is held off the bus meanwhile).

/// The C64 side of a transfer: reads and writes exactly as the CPU sees the
/// memory map (so a transfer into `$D800` reaches colour RAM).
pub trait DmaBus {
    fn dma_read(&mut self, addr: u16) -> u8;
    fn dma_write(&mut self, addr: u16, value: u8);
}

const CMD_EXECUTE: u8 = 0x80;
const CMD_AUTOLOAD: u8 = 0x20;
const CMD_FF00_DISABLED: u8 = 0x10;
const CMD_TYPE: u8 = 0x03;

const ST_VERIFY_ERROR: u8 = 0x20;
const ST_END_OF_BLOCK: u8 = 0x40;
const ST_INTERRUPT: u8 = 0x80;
const ST_256K_CHIPS: u8 = 0x10;

const MASK_VERIFY_IRQ: u8 = 0x20;
const MASK_EOB_IRQ: u8 = 0x40;
const MASK_ENABLE: u8 = 0x80;

const CTRL_FIX_REU: u8 = 0x40;
const CTRL_FIX_C64: u8 = 0x80;

/// The address counter of the 1700/1764/1750 is 19 bits wide.
const ADDRESS_MASK: u32 = 0x7_FFFF;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReuState {
    pub ram: Vec<u8>,
    pub status: u8,
    pub command: u8,
    pub c64_base: u16,
    pub reu_base: u16,
    pub bank: u8,
    pub length: u16,
    pub mask: u8,
    pub address_control: u8,
    pub shadow_c64: u16,
    pub shadow_reu: u16,
    pub shadow_bank: u8,
    pub shadow_length: u16,
    pub armed: bool,
}

#[derive(Debug, Clone)]
pub struct Reu {
    s: ReuState,
    irq_edge: bool,
}

impl Reu {
    /// `kilobytes` must be 128, 256 or 512 -- the three Commodore models.
    pub fn new(kilobytes: usize) -> Result<Self, String> {
        if !matches!(kilobytes, 128 | 256 | 512) {
            return Err(format!(
                "REU size must be 128, 256 or 512 (the 1700/1764/1750), not {kilobytes}"
            ));
        }
        let mut reu = Reu {
            s: ReuState {
                ram: vec![0; kilobytes * 1024],
                status: 0,
                command: 0,
                c64_base: 0,
                reu_base: 0,
                bank: 0,
                length: 0,
                mask: 0,
                address_control: 0,
                shadow_c64: 0,
                shadow_reu: 0,
                shadow_bank: 0,
                shadow_length: 0,
                armed: false,
            },
            irq_edge: false,
        };
        reu.reset();
        Ok(reu)
    }

    pub fn kilobytes(&self) -> usize {
        self.s.ram.len() / 1024
    }

    /// Power-on register state (the DRAM keeps its contents).
    pub fn reset(&mut self) {
        let s = &mut self.s;
        s.status = if s.ram.len() >= 256 * 1024 {
            ST_256K_CHIPS
        } else {
            0
        };
        s.command = CMD_FF00_DISABLED;
        s.c64_base = 0;
        s.reu_base = 0;
        s.bank = 0;
        s.length = 0xFFFF;
        s.mask = 0x1F;
        s.address_control = 0x3F;
        s.shadow_c64 = 0;
        s.shadow_reu = 0;
        s.shadow_bank = 0;
        s.shadow_length = 0xFFFF;
        s.armed = false;
        self.irq_edge = false;
    }

    pub fn state(&self) -> ReuState {
        self.s.clone()
    }

    pub fn restore_state(&mut self, state: ReuState) {
        if state.ram.len() == self.s.ram.len() {
            self.s = state;
        }
    }

    /// True if an interrupt was raised since the last call (consumed).
    pub fn take_irq(&mut self) -> bool {
        std::mem::take(&mut self.irq_edge)
    }

    /// True while a transfer is waiting for the `$FF00` write.
    pub fn armed(&self) -> bool {
        self.s.armed
    }

    pub fn peek(&self, offset: u16) -> u8 {
        let s = &self.s;
        match offset & 0x1F {
            0x00 => s.status,
            0x01 => s.command,
            0x02 => s.c64_base as u8,
            0x03 => (s.c64_base >> 8) as u8,
            0x04 => s.reu_base as u8,
            0x05 => (s.reu_base >> 8) as u8,
            0x06 => s.bank | 0xF8,
            0x07 => s.length as u8,
            0x08 => (s.length >> 8) as u8,
            0x09 => s.mask | 0x1F,
            0x0A => s.address_control | 0x3F,
            _ => 0xFF,
        }
    }

    /// A CPU read. The status register clears its event bits when read.
    pub fn read(&mut self, offset: u16) -> u8 {
        let value = self.peek(offset);
        if offset & 0x1F == 0 {
            self.s.status &= !(ST_VERIFY_ERROR | ST_END_OF_BLOCK | ST_INTERRUPT);
        }
        value
    }

    /// A CPU write. Returns `true` if a transfer must run now.
    #[must_use]
    pub fn write(&mut self, offset: u16, value: u8) -> bool {
        let s = &mut self.s;
        match offset & 0x1F {
            0x01 => {
                s.command = value;
                if value & CMD_EXECUTE != 0 {
                    if value & CMD_FF00_DISABLED != 0 {
                        return true;
                    }
                    s.armed = true; // runs on the next write to $FF00
                }
            }
            0x02 => {
                s.c64_base = (s.c64_base & 0xFF00) | value as u16;
                s.shadow_c64 = s.c64_base;
            }
            0x03 => {
                s.c64_base = (s.c64_base & 0x00FF) | (value as u16) << 8;
                s.shadow_c64 = s.c64_base;
            }
            0x04 => {
                s.reu_base = (s.reu_base & 0xFF00) | value as u16;
                s.shadow_reu = s.reu_base;
            }
            0x05 => {
                s.reu_base = (s.reu_base & 0x00FF) | (value as u16) << 8;
                s.shadow_reu = s.reu_base;
            }
            0x06 => {
                s.bank = value;
                s.shadow_bank = value;
            }
            0x07 => {
                s.length = (s.length & 0xFF00) | value as u16;
                s.shadow_length = s.length;
            }
            0x08 => {
                s.length = (s.length & 0x00FF) | (value as u16) << 8;
                s.shadow_length = s.length;
            }
            0x09 => {
                s.mask = value;
                self.reevaluate_interrupt();
            }
            0x0A => s.address_control = value,
            _ => {}
        }
        false
    }

    fn reevaluate_interrupt(&mut self) {
        let s = &mut self.s;
        let raised = s.mask & MASK_ENABLE != 0
            && ((s.status & ST_END_OF_BLOCK != 0 && s.mask & MASK_EOB_IRQ != 0)
                || (s.status & ST_VERIFY_ERROR != 0 && s.mask & MASK_VERIFY_IRQ != 0));
        if raised && s.status & ST_INTERRUPT == 0 {
            s.status |= ST_INTERRUPT;
            self.irq_edge = true;
        }
    }

    fn read_reu(&self, address: u32) -> u8 {
        self.s.ram.get(address as usize).copied().unwrap_or(0xFF)
    }

    fn write_reu(&mut self, address: u32, value: u8) {
        if let Some(cell) = self.s.ram.get_mut(address as usize) {
            *cell = value;
        }
    }

    /// Executes the programmed transfer and returns the CPU cycles it took.
    pub fn run(&mut self, bus: &mut dyn DmaBus) -> u32 {
        self.s.armed = false;
        let kind = self.s.command & CMD_TYPE;
        let mut host = self.s.c64_base;
        let mut reu = (self.s.reu_base as u32 | (self.s.bank as u32) << 16) & ADDRESS_MASK;
        let reu_bank_high = (self.s.bank as u32) << 16 & !ADDRESS_MASK;
        let total: u32 = if self.s.length == 0 {
            0x1_0000
        } else {
            self.s.length as u32
        };
        let host_step: u16 = if self.s.address_control & CTRL_FIX_C64 != 0 {
            0
        } else {
            1
        };
        let reu_step: u32 = if self.s.address_control & CTRL_FIX_REU != 0 {
            0
        } else {
            1
        };

        let mut done = 0u32;
        let mut cycles = 0u32;
        let mut mismatch = false;
        let mut mismatch_on_last = false;
        while done < total {
            let remaining_after = total - done - 1;
            match kind {
                0 => {
                    let value = bus.dma_read(host);
                    self.write_reu(reu, value);
                    cycles += 1;
                }
                1 => {
                    let value = self.read_reu(reu);
                    bus.dma_write(host, value);
                    cycles += 1;
                }
                2 => {
                    let from_c64 = bus.dma_read(host);
                    let from_reu = self.read_reu(reu);
                    bus.dma_write(host, from_reu);
                    self.write_reu(reu, from_c64);
                    cycles += 2;
                }
                _ => {
                    let a = bus.dma_read(host);
                    let b = self.read_reu(reu);
                    cycles += 1;
                    if a != b {
                        mismatch = true;
                        mismatch_on_last = remaining_after == 0;
                    }
                }
            }
            done += 1;
            host = host.wrapping_add(host_step);
            reu = (reu + reu_step) & ADDRESS_MASK;
            if mismatch {
                break;
            }
        }

        // End-of-transfer register update.
        let s = &mut self.s;
        if s.command & CMD_AUTOLOAD != 0 {
            s.c64_base = s.shadow_c64;
            s.reu_base = s.shadow_reu;
            s.bank = s.shadow_bank;
            s.length = s.shadow_length;
        } else {
            if host_step != 0 {
                s.c64_base = host;
            }
            if reu_step != 0 {
                s.reu_base = reu as u16;
                s.bank = ((reu | reu_bank_high) >> 16) as u8;
            }
            // A finished transfer leaves 1 in the length register.
            s.length = (total - done + 1) as u16;
        }
        s.command = (s.command & !CMD_EXECUTE) | CMD_FF00_DISABLED;

        let finished = !mismatch || mismatch_on_last;
        if finished {
            s.status |= ST_END_OF_BLOCK;
        }
        if mismatch {
            s.status |= ST_VERIFY_ERROR;
        }
        self.reevaluate_interrupt();
        cycles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ram(Vec<u8>);
    impl DmaBus for Ram {
        fn dma_read(&mut self, addr: u16) -> u8 {
            self.0[addr as usize]
        }
        fn dma_write(&mut self, addr: u16, value: u8) {
            self.0[addr as usize] = value;
        }
    }

    fn ram() -> Ram {
        Ram((0..=0xFFFFu32).map(|i| (i * 7 + 3) as u8).collect())
    }

    /// Programs the registers and starts the command; returns the cycles.
    fn start(reu: &mut Reu, bus: &mut Ram, c64: u16, reu_addr: u32, len: u16, command: u8) -> u32 {
        let _ = reu.write(2, c64 as u8);
        let _ = reu.write(3, (c64 >> 8) as u8);
        let _ = reu.write(4, reu_addr as u8);
        let _ = reu.write(5, (reu_addr >> 8) as u8);
        let _ = reu.write(6, (reu_addr >> 16) as u8);
        let _ = reu.write(7, len as u8);
        let _ = reu.write(8, (len >> 8) as u8);
        if reu.write(1, command | CMD_EXECUTE | CMD_FF00_DISABLED) {
            reu.run(bus)
        } else {
            0
        }
    }

    #[test]
    fn only_the_three_commodore_sizes_exist() {
        assert!(Reu::new(128).is_ok() && Reu::new(256).is_ok() && Reu::new(512).is_ok());
        assert!(Reu::new(64).unwrap_err().contains("1750"));
        assert!(Reu::new(2048).is_err());
    }

    #[test]
    fn power_on_registers_match_the_documented_values() {
        let mut reu = Reu::new(512).unwrap();
        assert_eq!(reu.peek(0x00), 0x10, "256K+ chips bit");
        assert_eq!(Reu::new(128).unwrap().peek(0x00), 0x00);
        assert_eq!(reu.peek(0x01), 0x10);
        assert_eq!((reu.peek(0x07), reu.peek(0x08)), (0xFF, 0xFF));
        assert_eq!(reu.peek(0x06), 0xF8, "unused bank bits read as 1");
        assert_eq!(reu.peek(0x09), 0x1F);
        assert_eq!(reu.peek(0x0A), 0x3F);
        assert_eq!(reu.read(0x1F), 0xFF, "unused registers");
    }

    #[test]
    fn stash_then_fetch_round_trips_a_block_and_takes_a_cycle_per_byte() {
        let mut reu = Reu::new(512).unwrap();
        let mut bus = ram();
        let original: Vec<u8> = bus.0[0x2000..0x2100].to_vec();
        assert_eq!(start(&mut reu, &mut bus, 0x2000, 0x1_0000, 0x100, 0), 0x100);
        for byte in &mut bus.0[0x2000..0x2100] {
            *byte = 0;
        }
        assert_eq!(start(&mut reu, &mut bus, 0x2000, 0x1_0000, 0x100, 1), 0x100);
        assert_eq!(&bus.0[0x2000..0x2100], &original[..]);
        assert_eq!(reu.peek(0x00) & 0x40, 0x40, "end of block");
        assert_eq!(reu.read(0x00) & 0x40, 0x40);
        assert_eq!(
            reu.peek(0x00) & 0xE0,
            0,
            "reading status clears the event bits"
        );
    }

    #[test]
    fn registers_advance_unless_autoload_restores_them() {
        let mut reu = Reu::new(256).unwrap();
        let mut bus = ram();
        start(&mut reu, &mut bus, 0x3000, 0x0_1234, 0x10, 0);
        assert_eq!(
            (reu.peek(0x02), reu.peek(0x03)),
            (0x10, 0x30),
            "C64 base moved on"
        );
        assert_eq!((reu.peek(0x04), reu.peek(0x05)), (0x44, 0x12));
        assert_eq!((reu.peek(0x07), reu.peek(0x08)), (1, 0), "length ends at 1");
        start(&mut reu, &mut bus, 0x3000, 0x0_1234, 0x10, CMD_AUTOLOAD);
        assert_eq!(
            (reu.peek(0x02), reu.peek(0x03)),
            (0x00, 0x30),
            "autoload reloads"
        );
        assert_eq!((reu.peek(0x07), reu.peek(0x08)), (0x10, 0));
    }

    #[test]
    fn length_zero_means_64k_and_fixed_addresses_do_not_move() {
        let mut reu = Reu::new(512).unwrap();
        let mut bus = ram();
        bus.0[0x4000] = 0xAB;
        let cycles = start(&mut reu, &mut bus, 0x4000, 0, 0, 0);
        assert_eq!(cycles, 0x1_0000);
        // Fill by fixed C64 address: stash the same byte 16 times.
        let _ = reu.write(0x0A, CTRL_FIX_C64 | 0x3F);
        start(&mut reu, &mut bus, 0x4000, 0x100, 16, 0);
        assert!((0..16).all(|i| reu.s.ram[0x100 + i] == 0xAB));
        assert_eq!(reu.peek(0x02), 0x00, "fixed: C64 base unchanged");
    }

    #[test]
    fn swap_exchanges_both_sides_and_costs_two_cycles_a_byte() {
        let mut reu = Reu::new(512).unwrap();
        let mut bus = ram();
        bus.0[0x5000..0x5004].copy_from_slice(&[1, 2, 3, 4]);
        start(&mut reu, &mut bus, 0x5000, 0x200, 4, 0); // stash 1,2,3,4
        bus.0[0x5000..0x5004].copy_from_slice(&[9, 8, 7, 6]);
        assert_eq!(start(&mut reu, &mut bus, 0x5000, 0x200, 4, 2), 8);
        assert_eq!(&bus.0[0x5000..0x5004], &[1, 2, 3, 4]);
        assert_eq!(&reu.s.ram[0x200..0x204], &[9, 8, 7, 6]);
    }

    #[test]
    fn verify_flags_a_mismatch_and_stops_there() {
        let mut reu = Reu::new(512).unwrap();
        let mut bus = ram();
        start(&mut reu, &mut bus, 0x6000, 0x300, 8, 0);
        assert_eq!(start(&mut reu, &mut bus, 0x6000, 0x300, 8, 3), 8);
        assert_eq!(reu.read(0x00) & 0x60, 0x40, "all equal: end of block only");

        bus.0[0x6003] ^= 0xFF;
        let cycles = start(&mut reu, &mut bus, 0x6000, 0x300, 8, 3);
        assert_eq!(cycles, 4, "stops after the mismatching byte");
        assert_eq!(reu.read(0x00) & 0x60, 0x20, "verify error, no end of block");

        bus.0[0x6000..0x6008].copy_from_slice(&reu.s.ram[0x300..0x308]);
        bus.0[0x6007] ^= 0xFF;
        start(&mut reu, &mut bus, 0x6000, 0x300, 8, 3);
        assert_eq!(
            reu.read(0x00) & 0x60,
            0x60,
            "a mismatch on the last byte sets both"
        );
    }

    #[test]
    fn reu_addresses_wrap_at_512k_and_unbacked_memory_reads_as_open_bus() {
        let mut reu = Reu::new(128).unwrap();
        let mut bus = ram();
        // 128K: address $20000 is past the DRAM -- stash is dropped, fetch
        // reads $FF.
        start(&mut reu, &mut bus, 0x7000, 0x2_0000, 4, 0);
        start(&mut reu, &mut bus, 0x7100, 0x2_0000, 4, 1);
        assert_eq!(&bus.0[0x7100..0x7104], &[0xFF; 4]);
        // And the 19-bit counter wraps from $7FFFF to 0.
        let mut reu = Reu::new(512).unwrap();
        bus.0[0x7200..0x7202].copy_from_slice(&[0x11, 0x22]);
        start(&mut reu, &mut bus, 0x7200, 0x7_FFFF, 2, 0);
        assert_eq!((reu.s.ram[0x7_FFFF], reu.s.ram[0]), (0x11, 0x22));
    }

    #[test]
    fn end_of_block_interrupt_needs_both_mask_bits_and_fires_once() {
        let mut reu = Reu::new(512).unwrap();
        let mut bus = ram();
        let _ = reu.write(0x09, 0x1F | MASK_EOB_IRQ); // source bit only
        start(&mut reu, &mut bus, 0x2000, 0, 4, 0);
        assert!(!reu.take_irq(), "interrupts not enabled");
        let _ = reu.write(0x09, MASK_ENABLE | MASK_EOB_IRQ | 0x1F);
        assert!(reu.take_irq(), "enabling with the event latched raises it");
        assert!(!reu.take_irq());
        assert_eq!(reu.read(0x00) & 0xC0, 0xC0, "status shows pending + EOB");
        assert_eq!(reu.peek(0x00) & 0x80, 0, "and reading cleared it");
    }

    #[test]
    fn a_command_with_ff00_enabled_waits_for_the_trigger() {
        let mut reu = Reu::new(512).unwrap();
        // Bit 4 clear: armed, not started.
        assert!(!reu.write(0x01, CMD_EXECUTE));
        assert!(reu.armed());
        let mut bus = ram();
        reu.run(&mut bus);
        assert!(!reu.armed());
        assert_eq!(reu.peek(0x01) & CMD_EXECUTE, 0, "execute bit clears");
    }

    #[test]
    fn state_carries_the_dram_and_registers() {
        let mut reu = Reu::new(128).unwrap();
        let mut bus = ram();
        start(&mut reu, &mut bus, 0x2000, 0x40, 8, 0);
        let saved = reu.state();
        let mut other = Reu::new(128).unwrap();
        other.restore_state(saved);
        assert_eq!(other.s.ram[0x40..0x48], reu.s.ram[0x40..0x48]);
        assert_eq!(other.peek(0x02), reu.peek(0x02));
        // A state from another size is ignored rather than corrupting.
        let mut big = Reu::new(512).unwrap();
        big.restore_state(reu.state());
        assert_eq!(big.kilobytes(), 512);
    }
}
