//! The Commodore 1581: a 3.5" MFM drive that is, like the 1541, a complete
//! second computer -- a 6502 at 2 MHz, 8K RAM, a 32K DOS ROM, a 6526 CIA
//! that talks to the IEC bus and the drive's control lines, and a WD1770
//! floppy controller reading and writing the raw MFM track of a
//! [`crate::floppy81::Floppy81`].
//!
//! Memory map (VICE `drive/iec/memiec.c`, the 1581 branch):
//! - `$0000-$1FFF`: 8K RAM
//! - `$4000-$5FFF`: the 6526 (16 registers, mirrored)
//! - `$6000-$7FFF`: the WD1770 (4 registers, mirrored)
//! - `$8000-$FFFF`: DOS ROM
//!
//! CIA wiring (VICE `cia1581d.c`): Port A bit 0 head select (also which half
//! of the image the head reads, see `floppy81`), bit 2 motor (0 = on), bit 6
//! LED, bits 4:3 device number (input), bit 7 disk-change (input, 1 = no
//! change since the last step). Port B is the serial bus exactly as the
//! 1541's VIA1 Port B plus bit 6 (write-protect sense, 1 = writable) and bit
//! 5 (fast-serial direction); the receivers are inverting, so bits 7/2/0 read
//! 1 while ATN/CLOCK/DATA are asserted. Unlike the 1541 the ATN-acknowledge
//! gate pulls DATA low only while ATN is asserted *and* the ACK bit is set.

use machine::{Bus, Machine, RunStats};

use crate::floppy81::{Floppy81, Floppy81State};

/// 32K, `$8000-$FFFF`.
pub const ROM_SIZE_1581: usize = 0x8000;
const RAM_SIZE: usize = 0x2000;
const CLOCK_HZ_1581: u64 = 2_000_000;
const PACING_HZ: u64 = 50;

const PB_DATA_IN: u8 = 0x01;
const PB_DATA_OUT: u8 = 0x02;
const PB_CLOCK_IN: u8 = 0x04;
const PB_CLOCK_OUT: u8 = 0x08;
const PB_ATN_ACK: u8 = 0x10;
const PB_WRITABLE: u8 = 0x40;
const PB_ATN_IN: u8 = 0x80;
const PA_SIDE: u8 = 0x01;
const PA_MOTOR_OFF: u8 = 0x04;
const PA_NO_DISK_CHANGE: u8 = 0x80;

#[derive(Clone)]
struct Bus1581 {
    ram: Vec<u8>,
    rom: Vec<u8>,
    cia: cia6526::Cia,
    wd: wd1770::Wd1770<Floppy81>,
    /// The device number minus 8 (CIA Port A bits 4:3).
    device_bits: u8,
    iec_atn_in: bool,
    iec_clock_in: bool,
    iec_data_in: bool,
    /// CIA Port B bit 5 as driven: the fast serial direction.
    fast_serial_out: bool,
}

impl Bus1581 {
    fn new(rom: Vec<u8>, device_number: u8) -> Self {
        Self {
            ram: vec![0; RAM_SIZE],
            rom,
            cia: cia6526::Cia::new(),
            wd: wd1770::Wd1770::new(Floppy81::default(), 2, false),
            device_bits: ((device_number - 8) & 3) << 3,
            iec_atn_in: true,
            iec_clock_in: true,
            iec_data_in: true,
            fast_serial_out: false,
        }
    }

    /// Port A as the CPU reads it: outputs show what was written, inputs the
    /// device number and the disk-change line.
    fn port_a(&self) -> u8 {
        let mut input = self.device_bits;
        if !self.wd.floppy.disk_change() {
            input |= PA_NO_DISK_CHANGE;
        }
        (input & !self.cia.ddr_a) | (self.cia.port_a & self.cia.ddr_a)
    }

    fn port_b(&self) -> u8 {
        let mut input = self.cia.port_b & 0x1A | PB_WRITABLE;
        if !self.iec_data_in {
            input |= PB_DATA_IN;
        }
        if !self.iec_clock_in {
            input |= PB_CLOCK_IN;
        }
        if !self.iec_atn_in {
            input |= PB_ATN_IN;
        }
        (input & !self.cia.ddr_b) | (self.cia.port_b & self.cia.ddr_b)
    }

    /// Applies Port A's output bits to the mechanism.
    fn sync_port_a(&mut self) {
        let pa = self.cia.effective_port_a();
        // The WD1770's head line is the inverse of Port A bit 0.
        self.wd
            .floppy
            .set_head(if pa & PA_SIDE != 0 { 0 } else { 1 });
        self.wd.floppy.set_motor(pa & PA_MOTOR_OFF == 0);
    }

    /// What this drive asserts onto CLOCK and DATA.
    fn iec_output(&self) -> (bool, bool) {
        let driven = self.cia.port_b & self.cia.ddr_b;
        let atn_asserted = !self.iec_atn_in;
        let ack_gate = driven & PB_ATN_ACK != 0 && atn_asserted;
        (
            driven & PB_CLOCK_OUT != 0,
            driven & PB_DATA_OUT != 0 || ack_gate,
        )
    }

    fn peek(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[addr as usize],
            0x4000..=0x5FFF => match addr & 0x0F {
                0 => self.port_a(),
                1 => self.port_b(),
                reg => self.cia.peek(reg as u8),
            },
            0x6000..=0x7FFF => self.wd.peek((addr & 3) as u8),
            0x8000..=0xFFFF => self.rom[(addr - 0x8000) as usize],
            _ => 0xFF,
        }
    }
}

impl Bus for Bus1581 {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[addr as usize],
            0x4000..=0x5FFF => match addr & 0x0F {
                0 => self.port_a(),
                1 => self.port_b(),
                reg => self.cia.read(reg as u8),
            },
            0x6000..=0x7FFF => self.wd.read((addr & 3) as u8),
            0x8000..=0xFFFF => self.rom[(addr - 0x8000) as usize],
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram[addr as usize] = val,
            0x4000..=0x5FFF => {
                self.cia.write((addr & 0x0F) as u8, val);
                if matches!(addr & 0x0F, 0 | 2) {
                    self.sync_port_a();
                }
                if matches!(addr & 0x0F, 1 | 3) {
                    self.fast_serial_out = self.cia.port_b & self.cia.ddr_b & 0x20 != 0;
                }
            }
            0x6000..=0x7FFF => self.wd.write((addr & 3) as u8, val),
            _ => {}
        }
    }
}

#[derive(Clone)]
pub struct C1581 {
    cpu: mos6502::Cpu,
    bus: Bus1581,
}

impl C1581 {
    pub fn new(rom: Vec<u8>, device_number: u8) -> Result<Self, String> {
        if rom.len() != ROM_SIZE_1581 {
            return Err(format!(
                "1581 ROM must be {ROM_SIZE_1581} bytes, got {}",
                rom.len()
            ));
        }
        if !(8..=11).contains(&device_number) {
            return Err(format!(
                "1581 device number must be 8-11, got {device_number}"
            ));
        }
        let mut bus = Bus1581::new(rom, device_number);
        bus.cia.set_flag(true);
        let mut cpu = mos6502::Cpu::new();
        cpu.reset(&mut bus);
        Ok(Self { cpu, bus })
    }

    pub fn iec_output(&self) -> (bool, bool) {
        self.bus.iec_output()
    }

    pub fn set_iec_input(&mut self, atn: bool, clock: bool, data: bool) {
        // ATN is also wired to the CIA's FLAG pin: asserting it (the line
        // going low) raises the interrupt the DOS answers.
        self.bus.cia.set_flag(atn);
        self.bus.iec_atn_in = atn;
        self.bus.iec_clock_in = clock;
        self.bus.iec_data_in = data;
    }

    pub fn attach_d81(&mut self, disk: d81::D81) {
        self.bus.wd.floppy.attach(disk);
    }

    pub fn detach_disk(&mut self) {
        self.bus.wd.floppy.detach();
    }

    pub fn has_disk(&self) -> bool {
        self.bus.wd.floppy.has_disk()
    }

    pub fn disk_image(&self) -> Option<Vec<u8>> {
        self.bus.wd.floppy.disk_bytes()
    }

    pub fn disk_modified(&self) -> bool {
        self.bus.wd.floppy.modified()
    }

    pub fn take_modified_disk_image(&mut self) -> Option<Vec<u8>> {
        // Anything still in the track buffer is written back first.
        self.bus.wd.floppy.commit();
        self.disk_modified().then(|| self.disk_image()).flatten()
    }

    pub fn mark_disk_saved(&mut self) {
        self.bus.wd.floppy.mark_saved();
    }

    /// The cylinder the head is on.
    pub fn current_cylinder(&self) -> u8 {
        self.bus.wd.floppy.cylinder()
    }
}

impl Machine for C1581 {
    fn clock_hz(&self) -> u64 {
        CLOCK_HZ_1581
    }

    fn frame_hz(&self) -> u64 {
        PACING_HZ
    }

    fn run(&mut self, running: &mut dyn FnMut(&dyn Machine) -> bool) -> RunStats {
        machine::Clock::new(self.frame_hz()).run(self, running)
    }

    fn step(&mut self) -> u32 {
        let cycles = self.cpu.step(&mut self.bus);
        if self.bus.cia.tick(cycles as u8) {
            self.cpu.request_irq();
        }
        self.bus.wd.tick(cycles);
        cycles
    }

    fn halted(&self) -> bool {
        self.cpu.halted
    }

    fn pc(&self) -> machine::Addr {
        u64::from(self.cpu.pc)
    }

    fn registers(&self) -> Vec<(&'static str, u64)> {
        self.cpu.registers()
    }

    fn read_memory(&self, addr: machine::Addr) -> u8 {
        let addr = addr as u16;
        self.bus.peek(addr)
    }

    fn load(&mut self, addr: machine::Addr, bytes: &[u8]) -> usize {
        let addr = addr as u16;
        let mut a = addr;
        for &b in bytes {
            self.bus.write(a, b);
            a = a.wrapping_add(1);
        }
        bytes.len()
    }

    fn set_pc(&mut self, addr: machine::Addr) {
        let addr = addr as u16;
        self.cpu.pc = addr;
    }

    fn reset(&mut self) {
        self.cpu.reset(&mut self.bus);
    }

    fn save_state(&self) -> Vec<u8> {
        let state = State1581 {
            cpu: self.cpu,
            ram: self.bus.ram.clone(),
            cia: self.bus.cia.clone(),
            wd: self.bus.wd.dump_state(),
            floppy: self.bus.wd.floppy.dump_state(),
            iec: (
                self.bus.iec_atn_in,
                self.bus.iec_clock_in,
                self.bus.iec_data_in,
            ),
            fast_serial_out: self.bus.fast_serial_out,
        };
        bincode::serde::encode_to_vec(&state, bincode::config::standard())
            .expect("1581 save state should always be encodable")
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        let (state, _): (State1581, usize) =
            bincode::serde::decode_from_slice(bytes, bincode::config::standard())
                .map_err(|e| format!("failed to decode 1581 save state: {e}"))?;
        if state.ram.len() != self.bus.ram.len() {
            return Err("save state RAM size mismatch".to_string());
        }
        self.cpu = state.cpu;
        self.bus.ram = state.ram;
        self.bus.cia = state.cia;
        self.bus.wd.restore_state(state.wd);
        self.bus.wd.floppy.restore_state(state.floppy);
        (
            self.bus.iec_atn_in,
            self.bus.iec_clock_in,
            self.bus.iec_data_in,
        ) = state.iec;
        self.bus.fast_serial_out = state.fast_serial_out;
        self.bus.sync_port_a();
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct State1581 {
    cpu: mos6502::Cpu,
    ram: Vec<u8>,
    cia: cia6526::Cia,
    wd: wd1770::Wd1770State,
    floppy: Floppy81State,
    iec: (bool, bool, bool),
    fast_serial_out: bool,
}
