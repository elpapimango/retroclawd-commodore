//! Western Digital WD1770/WD1772 floppy disk controller (MFM only).
//!
//! The chip is modelled the way it works: a command state machine that reads
//! and writes the *byte stream under the head* of a rotating track, one byte
//! per 32 us (250 kbit/s MFM). Everything the controller does -- stepping,
//! waiting for index pulses, hunting for an ID field, checking CRCs, handing
//! the CPU one byte at a time through DRQ, laying down a formatted track from
//! the bytes the CPU feeds it -- happens against a [`Floppy`], which owns the
//! rotating track. Nothing in here knows what a disk image is.
//!
//! Registers (`A1:A0`): 0 status/command, 1 track, 2 sector, 3 data. DRQ and
//! the lost-data flag are the status register's bits 1 and 2; INTRQ is
//! exposed as [`Wd1770::intrq`] (a 1581 does not wire it to the CPU, the DOS
//! polls the status register).
//!
//! Timing constants follow the datasheet: 24 us to start a command, 30 ms
//! head settling when the `E` flag is set, step rates of 6/12/20/30 ms
//! (WD1770) or 6/12/2/3 ms (WD1772), one byte per 32 us. They are scaled by
//! the CPU clock in MHz the host passes to [`Wd1770::new`].
//!
//! Behaviour reference: the datasheet, cross-checked against VICE's
//! `drive/iec/wd1770.c` (reference only, see `CREDITS.md`).

use serde::{Deserialize, Serialize};

/// What the controller needs from the drive it is attached to.
pub trait Floppy {
    /// The index hole is under the sensor.
    fn index(&self) -> bool;
    /// Index pulses seen since the last reset of the counter.
    fn index_count(&self) -> u32;
    fn index_count_reset(&mut self);
    /// The disk turns by `bytes` byte times.
    fn rotate(&mut self, bytes: u64);
    fn track0(&self) -> bool;
    fn write_protect(&self) -> bool;
    /// One step pulse: `inward` moves the head toward higher tracks.
    fn seek_pulse(&mut self, inward: bool);
    /// The byte under the head; bit 8 marks a sync byte (`A1`/`C2` with a
    /// missing clock). The disk advances by one byte.
    fn read(&mut self) -> u16;
    /// Writes a byte at the head (bit 8 = sync mark) and advances.
    fn write(&mut self, byte: u16);
}

const BYTE_US: u64 = 32;
const PREPARE_US: u64 = 24;
const SETTLING_US: u64 = 30_000;
const STEP_RATES_US: [[u64; 4]; 2] = [[6000, 12000, 20000, 30000], [6000, 12000, 2000, 3000]];

// Status bits.
const MO: u8 = 0x80;
const WP: u8 = 0x40;
const SU_RT: u8 = 0x20;
const SE_RNF: u8 = 0x10;
const CRC_ERR: u8 = 0x08;
const T0_LD: u8 = 0x04;
const IP_DRQ: u8 = 0x02;
const BSY: u8 = 0x01;

// Command flag bits.
const FLAG_A: u8 = 0x01; // write: data address mark
const FLAG_V_E: u8 = 0x04; // type I verify / type II-III settle delay
const FLAG_H: u8 = 0x08;
const FLAG_U_M: u8 = 0x10; // type I update track / type II multiple
const FLAG_I2: u8 = 0x04;
const FLAG_I3: u8 = 0x08;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Command {
    Restore,
    Seek,
    Step,
    StepIn,
    StepOut,
    ReadSector,
    WriteSector,
    ReadAddress,
    ForceInterrupt,
    ReadTrack,
    WriteTrack,
}

impl Command {
    fn decode(byte: u8) -> (Command, i8) {
        match byte & 0xF0 {
            0x00 => (Command::Restore, 1),
            0x10 => (Command::Seek, 1),
            0xC0 => (Command::ReadAddress, 3),
            0xD0 => (Command::ForceInterrupt, 4),
            0xE0 => (Command::ReadTrack, 3),
            0xF0 => (Command::WriteTrack, 3),
            _ => match byte & 0xE0 {
                0x20 => (Command::Step, 1),
                0x40 => (Command::StepIn, 1),
                0x60 => (Command::StepOut, 1),
                0x80 => (Command::ReadSector, 2),
                _ => (Command::WriteSector, 2),
            },
        }
    }
}

/// CRC-CCITT (polynomial `0x1021`), one byte.
pub fn crc_byte(crc: u16, byte: u8) -> u16 {
    let mut crc = crc ^ (u16::from(byte) << 8);
    for _ in 0..8 {
        crc = if crc & 0x8000 != 0 {
            (crc << 1) ^ 0x1021
        } else {
            crc << 1
        };
    }
    crc
}

/// CRC preset after the `A1 A1 A1 FE` of an ID field.
pub const CRC_AFTER_IDAM: u16 = 0xB230;
/// CRC preset after the `A1 A1 A1 FB` of a data field.
pub const CRC_AFTER_DAM: u16 = 0xE295;

/// The controller's own state (not the floppy's).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wd1770State {
    data: u8,
    track: u8,
    sector: u8,
    status: u8,
    cmd: u8,
    crc: u16,
    command: Command,
    ty: i8,
    step: u32,
    byte_count: i32,
    tmp: u32,
    direction: bool,
    sync: bool,
    irq: bool,
    clk: u64,
    now: u64,
}

#[derive(Clone)]
pub struct Wd1770<F: Floppy> {
    pub floppy: F,
    st: Wd1770State,
    /// CPU clock in MHz (scales every delay).
    mhz: u64,
    is1772: bool,
}

impl<F: Floppy> Wd1770<F> {
    pub fn new(floppy: F, cpu_mhz: u64, is1772: bool) -> Self {
        Self {
            floppy,
            st: Wd1770State {
                data: 0,
                track: 0,
                sector: 0,
                status: 0,
                cmd: 0,
                crc: 0,
                command: Command::Restore,
                ty: 0,
                step: 0,
                byte_count: 0,
                tmp: 0,
                direction: false,
                sync: false,
                irq: false,
                clk: 0,
                now: 0,
            },
            mhz: cpu_mhz.max(1),
            is1772,
        }
    }

    pub fn dump_state(&self) -> Wd1770State {
        self.st.clone()
    }

    pub fn restore_state(&mut self, state: Wd1770State) {
        self.st = state;
    }

    pub fn reset(&mut self) {
        let now = self.st.now;
        self.st.ty = 0;
        self.st.status = 0;
        self.st.track = 0;
        self.st.sector = 0;
        self.st.data = 0;
        self.st.cmd = 0;
        self.st.step = 0;
        self.st.clk = now;
    }

    /// The interrupt request line.
    pub fn intrq(&self) -> bool {
        self.st.irq
    }

    /// The data request line (status bit 1 while a transfer is running).
    pub fn drq(&self) -> bool {
        self.st.status & IP_DRQ != 0
    }

    /// Advances the controller by `cycles` CPU cycles.
    pub fn tick(&mut self, cycles: u32) {
        self.st.now += u64::from(cycles);
        self.execute();
    }

    pub fn read(&mut self, reg: u8) -> u8 {
        self.execute();
        match reg & 3 {
            0 => {
                self.st.irq = false;
                self.st.status
            }
            1 => self.st.track,
            2 => self.st.sector,
            _ => {
                self.st.status &= !IP_DRQ;
                self.st.data
            }
        }
    }

    pub fn peek(&self, reg: u8) -> u8 {
        match reg & 3 {
            0 => self.st.status,
            1 => self.st.track,
            2 => self.st.sector,
            _ => self.st.data,
        }
    }

    pub fn write(&mut self, reg: u8, value: u8) {
        self.execute();
        match reg & 3 {
            0 => self.start_command(value),
            1 => self.st.track = value,
            2 => self.st.sector = value,
            _ => {
                self.st.status &= !IP_DRQ;
                self.st.data = value;
            }
        }
    }

    fn start_command(&mut self, value: u8) {
        self.st.cmd = value;
        let (command, ty) = Command::decode(value);
        self.st.command = command;
        self.st.ty = ty;
        self.catch_up_rotation();
        self.st.step = 0;
        self.execute();
    }

    // -- timing helpers -------------------------------------------------

    fn byte_time(&self) -> u64 {
        BYTE_US * self.mhz
    }

    fn prepare(&self) -> u64 {
        PREPARE_US * self.mhz
    }

    fn settling(&self) -> u64 {
        SETTLING_US * self.mhz
    }

    fn step_rate(&self) -> u64 {
        STEP_RATES_US[usize::from(self.is1772)][usize::from(self.st.cmd & 3)] * self.mhz
    }

    /// Turns the disk for the whole byte times elapsed since `clk`.
    fn catch_up_rotation(&mut self) {
        let bytes = (self.st.now - self.st.clk) / self.byte_time();
        self.floppy.rotate(bytes);
        self.st.clk += bytes * self.byte_time();
    }

    /// `true` once `us`-scaled `delay` cycles have passed since `clk`.
    fn due(&self, delay: u64) -> bool {
        self.st.now >= self.st.clk + delay
    }

    fn set_drq_or_lost(&mut self) {
        self.st.status |= if self.st.status & IP_DRQ != 0 {
            T0_LD
        } else {
            IP_DRQ
        };
    }

    // -- the state machine ---------------------------------------------

    fn execute(&mut self) {
        loop {
            let finished = match self.st.ty {
                -1 | 0 => {
                    if self.st.ty == -1 {
                        self.st.status &= !(WP | IP_DRQ | T0_LD);
                        if self.floppy.index() {
                            self.st.status |= IP_DRQ;
                        }
                        if self.floppy.track0() {
                            self.st.status |= T0_LD;
                        }
                        if self.floppy.write_protect() {
                            self.st.status |= WP;
                        }
                    }
                    if !self.due(self.prepare()) {
                        return;
                    }
                    self.st.status &= !BSY;
                    self.catch_up_rotation();
                    if self.floppy.index_count() >= 10 {
                        self.st.status &= !MO;
                    }
                    if self.st.cmd & FLAG_I2 != 0 && self.floppy.index_count() != self.st.tmp {
                        self.st.irq = true;
                        self.st.tmp = self.floppy.index_count();
                    }
                    return;
                }
                1 => match self.type1() {
                    Some(done) => done,
                    None => return,
                },
                2 => match self.type2() {
                    Some(done) => done,
                    None => return,
                },
                3 => match self.type3() {
                    Some(done) => done,
                    None => return,
                },
                _ => {
                    // Force interrupt.
                    if !self.due(self.prepare()) {
                        return;
                    }
                    self.st.clk += self.prepare();
                    self.st.status &= BSY;
                    if self.st.cmd & FLAG_I3 != 0 {
                        self.st.irq = true;
                    }
                    self.floppy.index_count_reset();
                    self.st.tmp = self.floppy.index_count();
                    self.st.ty = if self.st.status & BSY != 0 { 0 } else { -1 };
                    continue;
                }
            };
            debug_assert!(finished);
            self.st.cmd = 0;
            self.st.irq = true;
            self.floppy.index_count_reset();
        }
    }

    /// Spin-up and head-load preamble shared by types I-III: after it,
    /// `step` is 3 (type II/III continue with the settle delay).
    fn motor_on_wait(&mut self) -> bool {
        // step 1: motor already on (or `h` flag) skips the wait.
        if self.st.step == 1 {
            if self.st.cmd & FLAG_H != 0 || self.st.status & MO != 0 {
                self.st.status |= MO;
                self.st.step = 3;
                return true;
            }
            self.st.status |= MO;
            self.floppy.index_count_reset();
            self.st.step = 2;
        }
        // step 2: six index pulses of spin-up.
        if self.st.step == 2 {
            self.catch_up_rotation();
            if self.floppy.index_count() < 6 {
                return false;
            }
            self.st.step = 3;
        }
        true
    }

    /// Type I (restore, seek, step). `None` = still waiting, `Some(true)` =
    /// the command finished.
    fn type1(&mut self) -> Option<bool> {
        loop {
            match self.st.step {
                0 => {
                    if !self.due(self.prepare()) {
                        return None;
                    }
                    self.st.clk += self.prepare();
                    self.st.status |= BSY;
                    self.st.status &= !(CRC_ERR | SE_RNF | IP_DRQ);
                    self.st.irq = false;
                    self.st.step = 1;
                }
                1 | 2 => {
                    if !self.motor_on_wait() {
                        return None;
                    }
                }
                3 => {
                    match self.st.command {
                        Command::StepIn => self.st.direction = true,
                        Command::StepOut => self.st.direction = false,
                        Command::Restore => {
                            self.st.track = 0xFF;
                            self.st.data = 0;
                            self.st.step = 4;
                            continue;
                        }
                        Command::Seek => {
                            self.st.step = 4;
                            continue;
                        }
                        _ => {}
                    }
                    self.st.step = if self.st.cmd & FLAG_U_M != 0 { 5 } else { 6 };
                }
                4 => {
                    if self.st.data == self.st.track {
                        self.st.step = 8;
                        continue;
                    }
                    self.st.direction = self.st.data > self.st.track;
                    self.st.step = 5;
                }
                5 => {
                    self.st.track = if self.st.direction {
                        self.st.track.wrapping_add(1)
                    } else {
                        self.st.track.wrapping_sub(1)
                    };
                    self.st.step = 6;
                }
                6 => {
                    if self.floppy.track0() && !self.st.direction {
                        self.st.track = 0;
                        self.st.step = 8;
                        continue;
                    }
                    self.floppy.seek_pulse(self.st.direction);
                    self.st.step = 7;
                }
                7 => {
                    if !self.due(self.step_rate()) {
                        return None;
                    }
                    self.st.clk += self.step_rate();
                    if self.st.cmd < 0x20 {
                        self.st.step = 4;
                        continue;
                    }
                    self.st.step = 8;
                }
                8 => {
                    if self.st.cmd & FLAG_V_E == 0 {
                        self.st.ty = -1;
                        return Some(true);
                    }
                    self.st.step = 9;
                }
                9 => {
                    if !self.due(self.settling()) {
                        return None;
                    }
                    self.st.clk += self.settling();
                    self.floppy.index_count_reset();
                    self.st.sync = false;
                    self.st.step = 10;
                }
                10 => {
                    if self.floppy.index_count() >= 6 {
                        self.st.status |= SE_RNF;
                        self.st.ty = -1;
                        return Some(true);
                    }
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.st.clk += self.byte_time();
                    let res = self.floppy.read();
                    if !self.st.sync || res != 0xFE {
                        self.st.sync = res == 0x1A1;
                        continue;
                    }
                    self.st.sync = false;
                    self.st.crc = CRC_AFTER_IDAM;
                    self.st.byte_count = 6;
                    self.st.step = 11;
                }
                _ => {
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.st.clk += self.byte_time();
                    let res = self.floppy.read();
                    if self.st.byte_count == 6 && res != u16::from(self.st.track) {
                        self.st.step = 10;
                        continue;
                    }
                    self.st.crc = crc_byte(self.st.crc, res as u8);
                    self.st.byte_count -= 1;
                    if self.st.byte_count != 0 {
                        continue;
                    }
                    if self.st.crc != 0 {
                        self.st.status |= CRC_ERR;
                        self.st.step = 10;
                        continue;
                    }
                    self.st.status &= !CRC_ERR;
                    self.st.ty = -1;
                    return Some(true);
                }
            }
        }
    }

    /// The preamble shared by type II and III once `step` reaches 3:
    /// optional settle delay (`E` flag). Returns `Some(next_step)` when
    /// finished, `None` while waiting.
    fn settle(&mut self) -> Option<()> {
        if self.st.step == 3 {
            if self.st.cmd & FLAG_V_E == 0 {
                self.st.step = 5;
                return Some(());
            }
            self.st.step = 4;
        }
        if self.st.step == 4 {
            if !self.due(self.settling()) {
                return None;
            }
            self.st.clk += self.settling();
            self.st.step = 5;
        }
        Some(())
    }

    /// Type II (read/write sector).
    fn type2(&mut self) -> Option<bool> {
        loop {
            match self.st.step {
                0 => {
                    if !self.due(self.prepare()) {
                        return None;
                    }
                    self.st.clk += self.prepare();
                    self.st.status |= BSY;
                    self.st.status &= !(IP_DRQ | T0_LD | SE_RNF | SU_RT | WP);
                    self.st.step = 1;
                }
                1..=4 => {
                    if self.st.step <= 2 && !self.motor_on_wait() {
                        return None;
                    }
                    self.settle()?;
                }
                5 => {
                    if self.st.command == Command::WriteSector && self.floppy.write_protect() {
                        self.st.status |= WP;
                        self.st.ty = 0;
                        return Some(true);
                    }
                    self.floppy.index_count_reset();
                    self.st.sync = false;
                    self.st.step = 6;
                }
                6 => {
                    if self.floppy.index_count() >= 5 {
                        self.st.status |= SE_RNF;
                        self.st.ty = 0;
                        return Some(true);
                    }
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.st.clk += self.byte_time();
                    let res = self.floppy.read();
                    if !self.st.sync || res != 0xFE {
                        self.st.sync = res == 0x1A1;
                        continue;
                    }
                    self.st.sync = false;
                    self.st.crc = CRC_AFTER_IDAM;
                    self.st.byte_count = 6;
                    self.st.step = 7;
                }
                7 => {
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.st.clk += self.byte_time();
                    let res = self.floppy.read();
                    if (self.st.byte_count == 6 && res != u16::from(self.st.track))
                        || (self.st.byte_count == 4 && res != u16::from(self.st.sector))
                    {
                        self.st.step = 6;
                        continue;
                    }
                    if self.st.byte_count == 3 {
                        self.st.tmp = u32::from(res as u8);
                    }
                    self.st.crc = crc_byte(self.st.crc, res as u8);
                    self.st.byte_count -= 1;
                    if self.st.byte_count != 0 {
                        continue;
                    }
                    if self.st.crc != 0 {
                        self.st.status |= CRC_ERR;
                        self.st.step = 6;
                        continue;
                    }
                    self.st.status &= !CRC_ERR;
                    self.st.crc = 0xFFFF;
                    if self.st.command == Command::WriteSector {
                        self.st.byte_count = 0;
                        self.st.step = 10;
                    } else {
                        self.st.byte_count = 43;
                        self.st.step = 8;
                    }
                }
                8 => {
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    let left = self.st.byte_count;
                    self.st.byte_count -= 1;
                    if left == 0 {
                        // No data mark within reach of the ID: look for the
                        // next ID.
                        self.st.step = 6;
                        continue;
                    }
                    self.st.clk += self.byte_time();
                    let res = self.floppy.read();
                    if !self.st.sync || (res != 0xFB && res != 0xF8) {
                        if !self.st.sync {
                            self.st.crc = 0xFFFF;
                        }
                        self.st.crc = crc_byte(self.st.crc, res as u8);
                        self.st.sync = res == 0x1A1;
                        continue;
                    }
                    self.st.crc = crc_byte(self.st.crc, res as u8);
                    if res & 0xFF == 0xF8 {
                        self.st.status |= SU_RT;
                    }
                    self.st.byte_count = (128 << self.st.tmp) + 2;
                    self.st.step = 9;
                }
                9 => {
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.st.clk += self.byte_time();
                    let res = self.floppy.read();
                    if self.st.byte_count > 2 {
                        self.set_drq_or_lost();
                        self.st.data = res as u8;
                    }
                    self.st.crc = crc_byte(self.st.crc, res as u8);
                    self.st.byte_count -= 1;
                    if self.st.byte_count != 0 {
                        continue;
                    }
                    if self.st.crc != 0 {
                        self.st.status |= CRC_ERR;
                        self.st.ty = 0;
                        return Some(true);
                    }
                    if self.st.cmd & FLAG_U_M != 0 {
                        self.st.sector = self.st.sector.wrapping_add(1);
                        self.st.step = 5;
                        continue;
                    }
                    self.st.ty = 0;
                    return Some(true);
                }
                10 => {
                    // Write sector: gap, sync, data mark.
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.st.clk += self.byte_time();
                    self.st.byte_count += 1;
                    if self.st.byte_count == 2 {
                        self.st.status |= IP_DRQ;
                    }
                    if self.st.byte_count == 2 + 9 && self.st.status & IP_DRQ != 0 {
                        // The CPU never supplied the first byte.
                        self.st.status ^= IP_DRQ | T0_LD;
                        self.st.ty = 0;
                        return Some(true);
                    }
                    if self.st.byte_count <= 11 + 2 + 9 {
                        self.floppy.read();
                        continue;
                    }
                    if self.st.byte_count <= 11 + 12 + 2 + 9 {
                        self.floppy.write(0);
                        continue;
                    }
                    if self.st.byte_count <= 11 + 12 + 2 + 9 + 3 {
                        self.floppy.write(0x1A1);
                        self.st.crc = crc_byte(self.st.crc, 0xA1);
                        continue;
                    }
                    let mark = if self.st.cmd & FLAG_A != 0 {
                        0xF8
                    } else {
                        0xFB
                    };
                    self.floppy.write(mark);
                    self.st.crc = crc_byte(self.st.crc, mark as u8);
                    self.st.byte_count = (128 << self.st.tmp) + 3;
                    self.st.step = 11;
                }
                _ => {
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.st.clk += self.byte_time();
                    self.st.byte_count -= 1;
                    match self.st.byte_count {
                        0 => self.floppy.write(0xFF),
                        1 => {
                            self.floppy.write(self.st.crc & 0xFF);
                            continue;
                        }
                        2 => {
                            self.floppy.write(self.st.crc >> 8);
                            continue;
                        }
                        _ => {
                            self.set_drq_or_lost();
                            self.st.crc = crc_byte(self.st.crc, self.st.data);
                            self.floppy.write(u16::from(self.st.data));
                            self.st.data = 0;
                            continue;
                        }
                    }
                    if self.st.cmd & FLAG_U_M != 0 {
                        self.st.sector = self.st.sector.wrapping_add(1);
                        self.st.step = 5;
                        continue;
                    }
                    self.st.ty = 0;
                    return Some(true);
                }
            }
        }
    }

    /// Type III (read address, read track, write track).
    fn type3(&mut self) -> Option<bool> {
        loop {
            match self.st.step {
                0 => {
                    if !self.due(self.prepare()) {
                        return None;
                    }
                    self.st.clk += self.prepare();
                    self.st.status |= BSY;
                    self.st.status &= !(IP_DRQ | T0_LD | SE_RNF | CRC_ERR);
                    self.st.step = 1;
                }
                1..=4 => {
                    if self.st.step <= 2 && !self.motor_on_wait() {
                        return None;
                    }
                    self.settle()?;
                }
                5 => {
                    self.floppy.index_count_reset();
                    self.st.sync = false;
                    match self.st.command {
                        Command::WriteTrack => {
                            if self.floppy.write_protect() {
                                self.st.status |= WP;
                                self.st.ty = 0;
                                return Some(true);
                            }
                            self.st.status |= IP_DRQ;
                            self.st.byte_count = 3;
                            self.st.step = 9;
                        }
                        Command::ReadTrack => self.st.step = 6,
                        _ => self.st.step = 7,
                    }
                }
                6 => {
                    // Read track: wait for the index hole, then stream bytes.
                    if self.floppy.index_count() < 1 {
                        self.catch_up_rotation();
                        return None;
                    }
                    if self.floppy.index_count() > 1 {
                        self.st.ty = 0;
                        return Some(true);
                    }
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.st.clk += self.byte_time();
                    self.st.data = self.floppy.read() as u8;
                    self.set_drq_or_lost();
                }
                7 => {
                    // Read address: hunt for the next ID mark.
                    if self.floppy.index_count() >= 6 {
                        self.st.status |= SE_RNF;
                        self.st.ty = 0;
                        return Some(true);
                    }
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.st.clk += self.byte_time();
                    let res = self.floppy.read();
                    if !self.st.sync || res != 0xFE {
                        self.st.sync = res == 0x1A1;
                        continue;
                    }
                    self.st.crc = CRC_AFTER_IDAM;
                    self.st.byte_count = 6;
                    self.st.step = 8;
                }
                8 => {
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.set_drq_or_lost();
                    self.st.clk += self.byte_time();
                    self.st.data = self.floppy.read() as u8;
                    if self.st.byte_count == 6 {
                        self.st.sector = self.st.data;
                    }
                    self.st.crc = crc_byte(self.st.crc, self.st.data);
                    self.st.byte_count -= 1;
                    if self.st.byte_count != 0 {
                        continue;
                    }
                    if self.st.crc != 0 {
                        self.st.status |= CRC_ERR;
                    }
                    self.st.ty = 0;
                    return Some(true);
                }
                9 => {
                    // Write track: the first three byte times give the CPU
                    // time to supply the first byte.
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.st.clk += self.byte_time();
                    self.floppy.read();
                    self.st.byte_count -= 1;
                    if self.st.byte_count != 0 {
                        continue;
                    }
                    if self.st.status & IP_DRQ != 0 {
                        self.st.status ^= IP_DRQ | T0_LD;
                        self.st.ty = 0;
                        return Some(true);
                    }
                    self.st.byte_count = 0;
                    self.st.tmp = 0;
                    self.st.step = 10;
                }
                _ => {
                    if self.floppy.index_count() < 1 {
                        self.catch_up_rotation();
                        return None;
                    }
                    if self.floppy.index_count() > 1 {
                        self.st.status &= !IP_DRQ;
                        self.st.ty = 0;
                        return Some(true);
                    }
                    if !self.due(self.byte_time()) {
                        return None;
                    }
                    self.st.clk += self.byte_time();
                    let mut res = u16::from(self.st.data);
                    if self.st.byte_count != 0 {
                        self.floppy.write(self.st.crc & 0xFF);
                        self.st.byte_count -= 1;
                    } else {
                        self.set_drq_or_lost();
                        match res {
                            0xF5 => {
                                res = 0x1A1;
                                if self.st.tmp == 0 {
                                    self.st.crc = 0xFFFF;
                                    self.st.tmp = 1;
                                }
                            }
                            0xF6 => res = 0x1C2,
                            0xF7 => {
                                self.st.byte_count = 1;
                                res = self.st.crc >> 8;
                                self.st.tmp = 0;
                            }
                            _ => {}
                        }
                        if self.st.tmp != 0 {
                            self.st.crc = crc_byte(self.st.crc, res as u8);
                        }
                        self.floppy.write(res);
                        self.st.data = 0;
                    }
                }
            }
        }
    }
}
