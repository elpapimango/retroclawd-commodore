//! Commodore 1541 floppy disk drive: a real, complete second computer in
//! its own right (`mos6502::Cpu` + 2 real `via6522::Via` chips + its own
//! ROM/RAM), not a peripheral bolted onto `c64::C64` -- exactly how real
//! hardware works (the drive has its own CPU, runs its own DOS, and only
//! ever talks to the C64 over the real IEC serial bus, see `crate::iec`).
//! Same "own machine, wired to another over a real bus" shape this
//! project's `Machine` trait already generalizes across every other
//! computer here.
//!
//! **Real memory map** (VIA base addresses and Port B bit layout
//! confirmed directly against VICE's own real source this session --
//! `drive/drivemem.c`'s `"VIA1", 0x1800, 0x180f` / `"VIA2", 0x1c00,
//! 0x1c0f` registrations and `drive/iec/via1d1541.c`'s own documented bit
//! layout, see `CREDITS.md`; RAM/ROM ranges are the commonly-cited real
//! 1541 memory map, not independently re-verified against source this
//! session, same "commonly documented" epistemic tier this project
//! already uses for e.g. the ZX Spectrum 128K `0x7ffd` decode):
//! - `$0000`-`$07FF`: 2K static RAM (the only range real hardware's own
//!   RAM chip decodes -- this crate does not attempt to reproduce any
//!   real incomplete-address-decode mirroring in the unmapped gap above
//!   it, same "no source found pinning down the exact real decode
//!   precision, so this deliberately doesn't guess a wider mirrored
//!   decode" stance `zx_spectrum`'s own Fuller joystick port already
//!   takes).
//! - `$1800`-`$1BFF`: VIA1 (the serial-bus/IEC + device-address-jumper
//!   chip, see `crate::iec` and `set_iec_input`/`iec_output` below),
//!   16-byte register block mirrored across the whole window.
//! - `$1C00`-`$1FFF`: VIA2 (the disk-mechanism chip -- head stepper
//!   motor, spindle motor, density select, and the real bit-level
//!   SYNC-detection/byte-assembly GCR hardware). Wired to a real
//!   `crate::disk::DiskMechanism` (see that module's own doc comment for
//!   the full real pin assignments and the GCR read pipeline) -- `tick_disk`
//!   below drives it every `Machine::step`, and its completed bytes reach
//!   VIA2's Port A plus a `CA1`/6502-SO byte-ready pulse, gated by `CA2`
//!   (`SOE`), exactly the real hardware protocol DOS firmware bit-bangs
//!   through.
//! - `$C000`-`$FFFF`: 16K DOS ROM.
//!
//! No real 1541 ROM image is committed under `roms/` the way the C64's
//! own KERNAL/BASIC/character ROMs are (see `roms/README.md`) -- this
//! project doesn't have a legally-suppliable copy to add, and this pass's
//! own tests accordingly use synthetic ROM content throughout, same
//! "boots at t=0" testing shape every other machine here uses before a
//! real ROM is available (this crate simply never got one). A real 1541
//! ROM plugged in via a future `--rom-1541`-shaped flag should boot
//! through this bus unmodified once one is supplied, same as C64's own
//! KERNAL did.

use machine::{Bus, Machine, RunStats};

/// 16K, `$C000`-`$FFFF` -- the real 1541 DOS ROM's own size.
pub const ROM_SIZE: usize = 0x4000;
/// 32K, `$8000`-`$FFFF` -- the 1570/1571 DOS ROM's size.
pub const ROM_SIZE_1571: usize = 0x8000;
const RAM_SIZE: usize = 0x0800;
const CIA_BASE: u16 = 0x4000;
const CIA_END: u16 = 0x7FFF;

/// Which Commodore drive this is. They share the IEC protocol and (for the
/// 1570/1571) the 1541's whole VIA/GCR design: the 1570 and 1571 add a 6526
/// CIA (the C128's fast serial shift register), a WD1770 for MFM, a second
/// head (1571) and a 2 MHz CPU clock, and double the ROM to 32K at `$8000`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DriveModel {
    C1541,
    C1570,
    C1571,
}

impl DriveModel {
    /// The DOS ROM image size this model needs.
    pub const fn rom_size(self) -> usize {
        match self {
            DriveModel::C1541 => ROM_SIZE,
            DriveModel::C1570 | DriveModel::C1571 => ROM_SIZE_1571,
        }
    }

    /// Whether this is a 1570/1571 (the 32K-ROM, CIA-bearing family).
    pub const fn is_1571_family(self) -> bool {
        !matches!(self, DriveModel::C1541)
    }

    /// Only the 1571 has a second head; the 1570 reads one side.
    pub const fn double_sided(self) -> bool {
        matches!(self, DriveModel::C1571)
    }

    pub const fn name(self) -> &'static str {
        match self {
            DriveModel::C1541 => "1541",
            DriveModel::C1570 => "1570",
            DriveModel::C1571 => "1571",
        }
    }

    const fn rom_base(self) -> u16 {
        match self {
            DriveModel::C1541 => 0xC000,
            _ => 0x8000,
        }
    }
}
const VIA1_BASE: u16 = 0x1800;
const VIA1_END: u16 = 0x1BFF;
const VIA2_BASE: u16 = 0x1C00;
const VIA2_END: u16 = 0x1FFF;

/// The real ~1MHz rate a 1541's own separate crystal drives its CPU at --
/// a genuinely different, independent clock from the C64's own PAL-locked
/// 985248Hz (the two aren't clock-synchronized on real hardware at all;
/// only the bit-banged IEC protocol's own handshake keeps them
/// coordinated). Commonly cited as "1MHz" in essentially every 1541
/// reference; not independently re-derived from a crystal value this
/// session. `pub(crate)` since `crate::disk::DiskMechanism::tick` also
/// needs it (the real bit clock is derived from this same CPU clock, not
/// a separate crystal).
pub(crate) const CLOCK_HZ: u64 = 1_000_000;

/// The 1541 has no video output at all -- this is purely `Machine::run`'s
/// own `Clock` pacing granularity (how often its real-time throttling loop
/// wakes up), picked to match every other machine here for consistency,
/// not a hardware fact this machine actually has.
const PACING_HZ: u64 = 50;

/// Real VIA1 Port B bit layout (confirmed against VICE's own
/// `via1d1541.c` this session -- see `CREDITS.md`): bit 7 = ATN IN, bits
/// 6:5 = device-address jumpers (`00`=#8,`01`=#9,`10`=#10,`11`=#11), bit 4
/// = ATN ACK (a real output bit DOS software toggles as part of its own
/// bit-banged handshake -- this bus applies no hardwired side effect to
/// it itself, same "once the timing is right, the real ROM's own code
/// handles the rest" stance this project already takes for e.g. ZX81's
/// NMI-driven video), bit 3 = CLOCK OUT, bit 2 = CLOCK IN, bit 1 = DATA
/// OUT, bit 0 = DATA IN.
const VIA1_PB_DATA_IN: u8 = 0x01;
const VIA1_PB_DATA_OUT: u8 = 0x02;
const VIA1_PB_CLOCK_IN: u8 = 0x04;
const VIA1_PB_CLOCK_OUT: u8 = 0x08;
const VIA1_PB_ATN_IN: u8 = 0x80;
const VIA1_PB_ATN_ACK: u8 = 0x10;

/// 1570/1571 VIA1 Port A bits (VICE `via1d1541.c`, the 1571 branches): bit 7
/// reads 0 while a disk byte is waiting (`BIT $180F / BMI` in the DOS read
/// loops), bit 0 reads 0 on track 1 (the track-0 sensor), bit 1 is the fast
/// serial direction, bit 2 the head select and bit 5 the 2 MHz CPU clock.
const VIA1_PA_TRACK0_N: u8 = 0x01;
const VIA1_PA_FAST_DIR: u8 = 0x02;
const VIA1_PA_SIDE: u8 = 0x04;
const VIA1_PA_MHZ2: u8 = 0x20;
const VIA1_PA_BYTE_NOT_READY: u8 = 0x80;

/// Real VIA2 (disk-mechanism) Port B bit layout -- confirmed against both
/// VICE's own `rotation.c`/drive source and MAME's own `c1541.cpp` this
/// session, cross-checked against each other (see `CREDITS.md`): bits
/// 1:0 = `STP1:STP0` (head stepper motor phase), bit 2 = `MTR` (spindle
/// motor on/off), bit 3 = `ACT` (drive activity LED, not modeled -- no
/// GUI indicator exists for it), bits 6:5 = `DS1:DS0` (density/speed-zone
/// select). Bit 7 reads 0 while the head is over a SYNC mark (DOS polls it
/// with `BIT $1C00 / BMI`, found by running the real 1541 DOS ROM -- it
/// never gets past its first header read without it) and bit 4 is the
/// write-protect sense (1 = a writable disk is in).
const VIA2_PB_MTR: u8 = 0x04;
const VIA2_PB_WPS: u8 = 0x10;
const VIA2_PB_SYNC_N: u8 = 0x80;

#[derive(Clone)]
pub(crate) struct C1541Bus {
    pub(crate) model: DriveModel,
    pub(crate) ram: Vec<u8>,
    pub(crate) rom: Vec<u8>,
    pub(crate) via1: via6522::Via,
    pub(crate) via2: via6522::Via,
    /// VIA1 Port B bits 6:5, precomputed once from the device number this
    /// drive was constructed with -- a real hardware fact (physical
    /// jumpers, not live/rewritable session state), so this is never part
    /// of save-state restore, see `crate::save_state`.
    pub(crate) device_jumper_bits: u8,
    /// The IEC bus's own current level for each of the 3 lines, as last
    /// reported via `set_iec_input` -- kept here (rather than recomputed
    /// on every VIA1 Port B access) since `ATN`'s value also needs to
    /// reach VIA1's `CA1` edge-detector, a `&mut self` operation `Via::
    /// peek`'s `&self` signature can't perform lazily on read.
    pub(crate) iec_atn_in: bool,
    pub(crate) iec_clock_in: bool,
    pub(crate) iec_data_in: bool,
    /// The real disk-mechanism hardware VIA2 is wired to -- see
    /// `crate::disk`'s own doc comment.
    pub(crate) disk: crate::disk::DiskMechanism,
    /// The 1570/1571's 6526 at `$4000`.
    pub(crate) cia: cia6526::Cia,
    /// A disk byte has been assembled and VIA2 Port A has not been read
    /// since (what VIA1 Port A bit 7 reports on the 1570/1571).
    pub(crate) byte_ready_latch: bool,
    /// VIA1 Port A bit 1 as the DOS last drove it: the fast serial line
    /// direction (1 = drive drives the line). Stored for the C128 burst
    /// path.
    pub(crate) fast_serial_out: bool,
}

impl C1541Bus {
    fn new(model: DriveModel, rom: Vec<u8>, device_number: u8) -> Self {
        let device_jumper_bits = ((device_number - 8) & 0x03) << 5;
        let mut bus = Self {
            model,
            ram: vec![0; RAM_SIZE],
            rom,
            via1: via6522::Via::new(),
            via2: via6522::Via::new(),
            device_jumper_bits,
            iec_atn_in: true,
            iec_clock_in: true,
            iec_data_in: true,
            disk: crate::disk::DiskMechanism::default(),
            cia: cia6526::Cia::new(),
            byte_ready_latch: false,
            fast_serial_out: false,
        };
        // Port A (the fast-serial/parallel connector on real hardware) is
        // unused this pass -- floats high, matching every other unwired
        // input convention this project already uses.
        bus.via1.set_external_port_a(0xFF);
        bus.refresh_via1_port_a();
        bus.apply_external_port_b();
        // Seeds VIA1's own `CA1` edge-tracking to the idle baseline (the
        // inverted ATN, so low while ATN is released) -- `Via::new()`'s bare
        // `Default` derive has no way to know a fresh chip's `CA1` pin
        // starts out released (real hardware behavior is host-wiring-
        // dependent, not something a generic chip crate can assume) --
        // without this, the very first real `ATN` assertion after
        // construction would be invisible to edge detection (it would
        // look like "already low, no edge"), not a real hardware quirk.
        bus.via1.set_ca1(false);
        // Same idle-high seed, for VIA2's own `CA1` -- the real
        // byte-ready line also idles high on real hardware (a pulse
        // means "byte ready", not a held level), same reasoning as
        // VIA1's own `ATN` seed above.
        bus.via2.set_ca1(true);
        bus.refresh_via2_inputs();
        bus
    }

    /// Attaches a parsed `.d64` disk image to the drive's real disk
    /// mechanism -- the media itself stays external, same `--rom`/
    /// `--tape`/`--disk` convention every other machine here already
    /// uses (never part of a save state, see `crate::save_state`).
    pub(crate) fn attach_disk(&mut self, disk: crate::disk::Medium) {
        self.disk.attach(disk);
        self.refresh_via2_inputs();
    }

    pub(crate) fn detach_disk(&mut self) {
        self.disk.detach();
        self.refresh_via2_inputs();
    }

    /// Reflects VIA2's own currently *effective* Port B state (driven
    /// output bits, real hardware fact regardless of which register the
    /// CPU just touched -- DDRB or ORB) into the disk mechanism's own
    /// stepper/motor/density inputs. Called after every real VIA2 write,
    /// see `Bus::write` below.
    fn sync_disk_control(&mut self) {
        let port_b = self.via2.effective_port_b();
        self.disk.update_stepper(port_b & 0x03);
        self.disk.set_motor_on(port_b & VIA2_PB_MTR != 0);
        self.disk.set_density((port_b >> 5) & 0x03);
    }

    /// Advances the disk mechanism by `cycles` real clock cycles and
    /// routes any newly assembled byte onto VIA2's Port A -- returns
    /// `true` only when that byte's `CA1` pulse should also propagate as
    /// a real 6502 SO-pin assertion, i.e. only while VIA2's own `CA2`
    /// (`SOE`) output is currently asserted (real hardware: DOS firmware
    /// gates the byte-ready signal behind this line, see `crate::disk`'s
    /// own doc comment). The disk mechanism itself keeps finding SYNC and
    /// assembling bytes regardless of `SOE` -- only whether that arrival
    /// is actually *signaled* to the CPU is gated here.
    /// Feeds the mechanism's SYNC and write-protect sensors into VIA2's
    /// Port B input pins.
    pub(crate) fn refresh_via2_inputs(&mut self) {
        let mut ext = 0;
        if !self.disk.sync_detected() {
            ext |= VIA2_PB_SYNC_N;
        }
        if self.disk.has_disk() {
            ext |= VIA2_PB_WPS;
        }
        self.via2.set_external_port_b(ext);
    }

    pub(crate) fn tick_disk(&mut self, cycles: u32) -> bool {
        // Write mode is CB2 low with Port A driven as an output.
        let writing = self.via2.cb2_output_level() == Some(false) && self.via2.ddr_a == 0xFF;
        self.disk.set_write(writing, self.via2.effective_port_a());
        let byte_ready = self.disk.tick(cycles);
        self.refresh_via2_inputs();
        if !byte_ready {
            return false;
        }
        self.byte_ready_latch = true;
        if !writing {
            self.via2
                .set_external_port_a(self.disk.last_assembled_byte());
        }
        let soe = self.via2.ca2_output_level().unwrap_or(false);
        if soe {
            // A real byte-ready pulse: the line briefly leaves its
            // idle-high state and returns, giving both a falling and a
            // rising edge every time regardless of which one VIA2's own
            // `PCR` currently treats as "active" -- see `crate::disk`'s
            // own doc comment.
            self.via2.set_ca1(false);
            self.via2.set_ca1(true);
        }
        soe
    }

    pub(crate) fn apply_external_port_b(&mut self) {
        let mut ext = self.device_jumper_bits;
        // The drive's receivers are inverting buffers (7406): a bit reads 1
        // while its bus line is pulled low/asserted. The real DOS depends on
        // it (ATN service at $E85B falls through only on bit 7 = 1).
        if !self.iec_data_in {
            ext |= VIA1_PB_DATA_IN;
        }
        if !self.iec_clock_in {
            ext |= VIA1_PB_CLOCK_IN;
        }
        if !self.iec_atn_in {
            ext |= VIA1_PB_ATN_IN;
        }
        self.via1.set_external_port_b(ext);
    }

    /// Reflects the IEC bus's current computed line levels into VIA1 --
    /// both Port B's plain input bits and (for `ATN` specifically) the
    /// dedicated `CA1` edge-interrupt pin real hardware wires the same
    /// physical `ATN` signal to as well. Called by `C1541::set_iec_input`
    /// (the public, `Machine`-external half of this crate's own IEC
    /// wiring) whenever the bus's computed levels change.
    fn set_iec_input(&mut self, atn: bool, clock: bool, data: bool) {
        self.iec_atn_in = atn;
        self.iec_clock_in = clock;
        self.iec_data_in = data;
        self.apply_external_port_b();
        // CA1 sees the same inverted ATN, so asserting ATN is a rising edge
        // (the real DOS programs PCR = $01, positive edge).
        self.via1.set_ca1(!atn);
    }

    /// What this drive is currently asserting onto `CLOCK`/`DATA` --
    /// real hardware never lets the drive drive `ATN` at all, so there's
    /// no third return value here. "Asserting" means driven-as-output
    /// (`ddr_b` bit set) *and* the corresponding `ORB` bit set to `1`
    /// (real hardware's own inverted-driver convention: a `1` written to
    /// an `*_OUT` bit pulls the physical line low/active).
    fn iec_output(&self) -> (bool, bool) {
        let driven = self.via1.port_b & self.via1.ddr_b;
        // The hardware ATN-acknowledge gate: DATA is pulled low whenever the
        // ATN ACK bit (an output) equals the ATN line level, i.e. the
        // moment ATN is asserted with ACK still clear, until DOS sets ACK
        // to match -- this is what lets the computer see a device answer
        // within a millisecond of ATN, before any software ran (VICE's
        // `via1d1541.c` `store_prb`: `(~data ^ cpu_bus) << 3`). Only once
        // the firmware has configured the bit as an output.
        let atn_ack_gate = self.via1.ddr_b & VIA1_PB_ATN_ACK != 0
            && (self.via1.port_b & VIA1_PB_ATN_ACK != 0) == self.iec_atn_in;
        (
            driven & VIA1_PB_CLOCK_OUT != 0,
            driven & VIA1_PB_DATA_OUT != 0 || atn_ack_gate,
        )
    }

    /// 1570/1571 VIA1 Port A inputs: the byte-ready level and the track-0
    /// sensor (the other pins read 0, as VICE's `read_pra`).
    pub(crate) fn refresh_via1_port_a(&mut self) {
        if !self.model.is_1571_family() {
            self.via1.set_external_port_a(0xFF);
            return;
        }
        let mut ext = 0;
        if !self.byte_ready_latch {
            ext |= VIA1_PA_BYTE_NOT_READY;
        }
        if !self.disk.at_track_one() {
            ext |= VIA1_PA_TRACK0_N;
        }
        self.via1.set_external_port_a(ext);
    }

    /// Applies VIA1 Port A's output bits to the 1570/1571 hardware: head
    /// select, 2 MHz clock, fast serial direction.
    fn sync_via1_port_a_outputs(&mut self) {
        if !self.model.is_1571_family() {
            return;
        }
        let pa = self.via1.effective_port_a();
        if self.model.double_sided() {
            self.disk.set_side(u8::from(pa & VIA1_PA_SIDE != 0));
        }
        self.disk
            .set_cpu_mhz(if pa & VIA1_PA_MHZ2 != 0 { 2 } else { 1 });
        self.fast_serial_out = pa & VIA1_PA_FAST_DIR != 0;
    }

    /// The drive CPU's current clock in Hz (a 1571 can run at 2 MHz).
    pub(crate) fn clock_hz(&self) -> u64 {
        CLOCK_HZ * u64::from(self.disk.cpu_mhz())
    }

    fn peek(&self, addr: u16) -> u8 {
        let family = self.model.is_1571_family();
        match addr {
            0x0000..=0x07FF => self.ram[addr as usize],
            0x0800..=0x0FFF if family => self.ram[(addr & 0x07FF) as usize],
            VIA1_BASE..=VIA1_END => self.via1.peek((addr & 0x0F) as u8),
            VIA2_BASE..=VIA2_END => self.via2.peek((addr & 0x0F) as u8),
            CIA_BASE..=CIA_END if family => self.cia.peek((addr & 0x0F) as u8),
            _ if addr >= self.model.rom_base() => self.rom[(addr - self.model.rom_base()) as usize],
            // Real hardware's own address decode in this unmapped gap
            // isn't modeled (see this module's own doc comment) --
            // `0xFF` matches every other machine's own "unmapped read"
            // convention here.
            _ => 0xFF,
        }
    }
}

impl Bus for C1541Bus {
    // `read_io`/`write_io` stay at `machine::Bus`'s defaults -- the 6502
    // has no separate IO-port space, everything above is memory-mapped,
    // same real hardware fact `c64::bus::C64Bus` already established.

    fn read(&mut self, addr: u16) -> u8 {
        let family = self.model.is_1571_family();
        match addr {
            0x0000..=0x07FF => self.ram[addr as usize],
            0x0800..=0x0FFF if family => self.ram[(addr & 0x07FF) as usize],
            VIA1_BASE..=VIA1_END => {
                self.refresh_via1_port_a();
                self.via1.read((addr & 0x0F) as u8)
            }
            VIA2_BASE..=VIA2_END => {
                let reg = (addr & 0x0F) as u8;
                let value = self.via2.read(reg);
                if reg == 0x01 || reg == 0x0F {
                    // Reading the disk data port takes the waiting byte.
                    self.byte_ready_latch = false;
                }
                value
            }
            CIA_BASE..=CIA_END if family => self.cia.read((addr & 0x0F) as u8),
            _ if addr >= self.model.rom_base() => self.rom[(addr - self.model.rom_base()) as usize],
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        let family = self.model.is_1571_family();
        match addr {
            0x0000..=0x07FF => self.ram[addr as usize] = val,
            0x0800..=0x0FFF if family => self.ram[(addr & 0x07FF) as usize] = val,
            VIA1_BASE..=VIA1_END => {
                self.via1.write((addr & 0x0F) as u8, val);
                if matches!(addr & 0x0F, 0x01 | 0x03 | 0x0F) {
                    self.sync_via1_port_a_outputs();
                }
            }
            VIA2_BASE..=VIA2_END => {
                self.via2.write((addr & 0x0F) as u8, val);
                if matches!(addr & 0x0F, 0x01 | 0x0F) {
                    // Writing the data port hands the waiting cell its byte.
                    self.byte_ready_latch = false;
                }
                self.sync_disk_control();
            }
            CIA_BASE..=CIA_END if family => self.cia.write((addr & 0x0F) as u8, val),
            // ROM writes and the unmapped gap both silently drop, same
            // convention every other machine's own ROM-window write
            // already follows.
            _ => {}
        }
    }
}

#[derive(Clone)]
pub struct C1541 {
    pub(crate) cpu: mos6502::Cpu,
    pub(crate) bus: C1541Bus,
}

impl C1541 {
    /// `device_number` selects which of the 4 real jumper-settable IEC
    /// addresses (`8`-`11`) this drive answers to -- `8` is the
    /// overwhelmingly common real default (an unmodified drive straight
    /// from the factory). A plain 1541 with its 16K ROM.
    pub fn new(rom: Vec<u8>, device_number: u8) -> Result<Self, String> {
        Self::with_model(DriveModel::C1541, rom, device_number)
    }

    /// A drive of the given model; `rom` must be that model's DOS image
    /// (`DriveModel::rom_size`).
    pub fn with_model(model: DriveModel, rom: Vec<u8>, device_number: u8) -> Result<Self, String> {
        if rom.len() != model.rom_size() {
            return Err(format!(
                "{} ROM must be {} bytes, got {}",
                model.name(),
                model.rom_size(),
                rom.len()
            ));
        }
        if !(8..=11).contains(&device_number) {
            return Err(format!(
                "{} device number must be 8-11, got {device_number}",
                model.name()
            ));
        }

        let mut bus = C1541Bus::new(model, rom, device_number);
        let mut cpu = mos6502::Cpu::new();
        cpu.reset(&mut bus);
        Ok(Self { cpu, bus })
    }

    pub fn model(&self) -> DriveModel {
        self.bus.model
    }

    /// Whether the disk head is on track 1 (the DOS's track-0 sensor).
    pub fn at_track_one(&self) -> bool {
        self.bus.disk.at_track_one()
    }

    /// The disk in the drive as image bytes, with anything the drive has
    /// written since it was inserted.
    pub fn disk_image(&self) -> Option<Vec<u8>> {
        self.bus.disk.disk_bytes()
    }

    /// The same, only if the drive has changed it since the last
    /// `mark_disk_saved`.
    pub fn take_modified_disk_image(&mut self) -> Option<Vec<u8>> {
        self.bus
            .disk
            .disk_modified()
            .then(|| self.bus.disk.disk_bytes())
            .flatten()
    }

    pub fn disk_modified(&self) -> bool {
        self.bus.disk.disk_modified()
    }

    pub fn mark_disk_saved(&mut self) {
        self.bus.disk.mark_disk_saved();
    }

    /// What this drive is currently asserting onto `CLOCK`/`DATA` -- see
    /// `C1541Bus::iec_output`'s own doc comment for the real hardware
    /// convention this reflects.
    pub fn iec_output(&self) -> (bool, bool) {
        self.bus.iec_output()
    }

    /// Feeds this drive the IEC bus's own currently computed line levels
    /// (from `crate::iec::IecBus`, combining this drive's own output above
    /// with the computer's) -- the other half of the real bus's round
    /// trip. Nothing in this codebase calls this continuously yet (no
    /// dual-machine stepping loop exists), see `c64`'s own `tests/
    /// iec_1541.rs` for how the full round trip is meant to be driven.
    pub fn set_iec_input(&mut self, atn: bool, clock: bool, data: bool) {
        self.bus.set_iec_input(atn, clock, data);
    }

    /// Mounts a parsed `.d64` disk image onto this drive's real disk
    /// mechanism -- see `crate::disk`'s own doc comment for the full real
    /// GCR read pipeline this makes readable through VIA2.
    pub fn attach_disk(&mut self, disk: d64::D64) {
        self.bus.attach_disk(crate::disk::Medium::D64(disk));
    }

    /// Mounts any supported medium; a `.d71` needs a 1570/1571.
    pub fn attach_medium(&mut self, medium: crate::disk::Medium) -> Result<(), String> {
        match medium {
            crate::disk::Medium::D64(d) => {
                self.attach_disk(d);
                Ok(())
            }
            crate::disk::Medium::D71(d) => self.attach_d71(d),
        }
    }

    /// Mounts a double-sided `.d71` (a 1571 reads both sides; a 1570 only
    /// the first, and a 1541 cannot read the format at all).
    pub fn attach_d71(&mut self, disk: d71::D71) -> Result<(), String> {
        if !self.bus.model.is_1571_family() {
            return Err("a 1541 cannot read .d71 images (use a 1570 or 1571)".to_string());
        }
        self.bus.attach_disk(crate::disk::Medium::D71(disk));
        Ok(())
    }

    pub fn detach_disk(&mut self) {
        self.bus.detach_disk();
    }

    pub fn has_disk(&self) -> bool {
        self.bus.disk.has_disk()
    }

    /// The real track number (1-based) the disk mechanism's head is
    /// currently positioned over.
    pub fn current_track(&self) -> u8 {
        self.bus.disk.track_number()
    }
}

impl Machine for C1541 {
    fn clock_hz(&self) -> u64 {
        self.bus.clock_hz()
    }

    fn frame_hz(&self) -> u64 {
        PACING_HZ
    }

    fn run(&mut self, running: &mut dyn FnMut(&dyn Machine) -> bool) -> RunStats {
        machine::Clock::new(self.frame_hz()).run(self, running)
    }

    fn step(&mut self) -> u32 {
        let cycles = self.cpu.step(&mut self.bus);
        let via1_irq = self.bus.via1.tick(cycles as u8);
        // Ticked (and its real byte-ready CA1 pulse applied, if any)
        // before VIA2's own timer tick below, so a same-step-enabled CA1
        // interrupt is visible to that same call's own IFR/IER check --
        // see `C1541Bus::tick_disk`'s own doc comment.
        if self.bus.model.is_1571_family() && self.bus.cia.tick(cycles as u8) {
            self.cpu.request_irq();
        }
        if self.bus.tick_disk(cycles) {
            // Real hardware fact: byte-ready is wired directly to the
            // 6502's own SO pin, not just VIA2's CA1 -- see `crate::disk`'s
            // own doc comment.
            self.cpu.set_overflow();
        }
        let via2_irq = self.bus.via2.tick(cycles as u8);
        if via1_irq || via2_irq {
            self.cpu.request_irq();
        }
        cycles
    }

    fn halted(&self) -> bool {
        self.cpu.halted
    }

    fn pc(&self) -> u16 {
        self.cpu.pc
    }

    fn registers(&self) -> Vec<(&'static str, u64)> {
        self.cpu.registers()
    }

    fn read_memory(&self, addr: u16) -> u8 {
        self.bus.peek(addr)
    }

    fn load(&mut self, addr: u16, bytes: &[u8]) -> usize {
        let mut a = addr;
        for &b in bytes {
            self.bus.write(a, b);
            a = a.wrapping_add(1);
        }
        bytes.len()
    }

    fn set_pc(&mut self, addr: u16) {
        self.cpu.pc = addr;
    }

    fn reset(&mut self) {
        self.cpu.reset(&mut self.bus);
    }

    fn save_state(&self) -> Vec<u8> {
        crate::save_state::encode(self)
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        crate::save_state::decode(self, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_drive() -> C1541 {
        C1541::new(vec![0xEA; ROM_SIZE], 8).unwrap()
    }

    #[test]
    fn boots_from_the_real_rom_reset_vector() {
        let mut rom = vec![0xEAu8; ROM_SIZE];
        rom[0x3FFC] = 0x34; // $FFFC/$FFFD -> ROM offset $3FFC/$3FFD
        rom[0x3FFD] = 0x12;
        let drive = C1541::new(rom, 8).unwrap();
        assert_eq!(drive.pc(), 0x1234);
    }

    #[test]
    fn rejects_a_wrong_sized_rom_or_an_out_of_range_device_number() {
        assert!(C1541::new(vec![0; ROM_SIZE - 1], 8).is_err());
        assert!(C1541::new(vec![0xEA; ROM_SIZE], 12).is_err());
        assert!(C1541::new(vec![0xEA; ROM_SIZE], 7).is_err());
    }

    #[test]
    fn ram_reads_and_writes_land_in_the_real_2k_window() {
        let mut drive = test_drive();
        drive.load(0x0400, &[0x42]);
        assert_eq!(drive.read_memory(0x0400), 0x42);
    }

    #[test]
    fn via1_registers_mirror_across_the_whole_1800_1bff_window() {
        let mut drive = test_drive();
        drive.load(0x1802, &[0x1A]); // DDRB via the primary address
        assert_eq!(
            drive.read_memory(0x1BF2),
            0x1A,
            "same register, a mirrored address"
        );
    }

    #[test]
    fn via2_registers_mirror_across_the_whole_1c00_1fff_window() {
        let mut drive = test_drive();
        drive.load(0x1C02, &[0x55]);
        assert_eq!(drive.read_memory(0x1FF2), 0x55);
    }

    #[test]
    fn device_number_jumper_bits_show_up_on_via1_port_b() {
        let drive9 = C1541::new(vec![0xEA; ROM_SIZE], 9).unwrap();
        // DDRB defaults to all-input (0), so Port B reads straight from
        // the external state -- bits 6:5 should show device 9's `01`.
        assert_eq!(drive9.read_memory(0x1800) & 0x60, 0x20);

        let drive11 = C1541::new(vec![0xEA; ROM_SIZE], 11).unwrap();
        assert_eq!(drive11.read_memory(0x1800) & 0x60, 0x60);
    }

    #[test]
    fn idle_iec_bus_reads_every_input_bit_high() {
        let drive = test_drive();
        assert_eq!(
            drive.read_memory(0x1800) & 0x85,
            0,
            "DATA IN/CLOCK IN/ATN IN read 0 while their lines are idle-high (inverted)"
        );
    }

    #[test]
    fn the_atn_acknowledge_gate_pulls_data_low_until_dos_sets_the_ack_bit() {
        let mut drive = C1541::new(vec![0xEA; ROM_SIZE], 8).unwrap();
        drive.load(0x1802, &[0x1A]); // real DDRB: ATN ACK, CLOCK OUT, DATA OUT drive
        drive.set_iec_input(true, true, true);
        assert_eq!(drive.iec_output(), (false, false), "idle: released");
        drive.set_iec_input(false, true, true); // ATN asserted, ACK still clear
        assert!(drive.iec_output().1, "DATA acknowledged in hardware");
        drive.load(0x1800, &[0x10]); // DOS sets ATN ACK to match
        assert!(!drive.iec_output().1, "released again");
        drive.set_iec_input(true, true, true); // ATN released, ACK still set
        assert!(drive.iec_output().1, "gate closes the other way round");
    }

    #[test]
    fn atn_input_reaches_both_port_b_and_the_real_ca1_edge_interrupt() {
        let mut drive = test_drive();
        drive.load(0x180E, &[0x80 | 0x02]); // enable VIA1's CA1 interrupt (IER)
        drive.load(0x180C, &[0x01]); // PCR as the real DOS sets it: CA1 positive edge
        drive.set_iec_input(false, true, true); // ATN asserted (active-low)

        assert_eq!(
            drive.read_memory(0x1800) & 0x80,
            0x80,
            "Port B bit 7 (ATN IN) reads 1 while ATN is asserted"
        );
        assert_ne!(
            drive.read_memory(0x180D) & 0x02,
            0,
            "the ATN edge should have set VIA1's own CA1 interrupt flag"
        );
    }

    #[test]
    fn drive_output_getter_reflects_clock_and_data_out_bits() {
        let mut drive = test_drive();
        drive.load(0x1802, &[0x1A]); // DDRB: bits 1/3/4 output
        assert_eq!(drive.iec_output(), (false, false));

        drive.load(0x1800, &[VIA1_PB_CLOCK_OUT | VIA1_PB_DATA_OUT]);
        assert_eq!(drive.iec_output(), (true, true));
    }

    fn blank_test_disk() -> d64::D64 {
        d64::D64::parse(&vec![0u8; 683 * d64::SECTOR_SIZE]).unwrap()
    }

    #[test]
    fn attach_and_detach_disk_round_trip_through_has_disk() {
        let mut drive = test_drive();
        assert!(!drive.has_disk());
        drive.attach_disk(blank_test_disk());
        assert!(drive.has_disk());
        drive.detach_disk();
        assert!(!drive.has_disk());
    }

    #[test]
    fn writing_via2_port_b_moves_the_disk_mechanisms_stepper() {
        let mut drive = test_drive();
        drive.load(0x1C02, &[0xFF]); // VIA2 DDRB: all output
        assert_eq!(drive.current_track(), 1);

        drive.load(0x1C00, &[0b0000_0001]); // STP1:STP0 00 -> 01: one real step
        drive.load(0x1C00, &[0b0000_0010]); // 01 -> 10: another real step
        assert_eq!(
            drive.current_track(),
            2,
            "two real +1 phase steps must move exactly one full track"
        );
    }

    fn so_flag_set(drive: &C1541) -> bool {
        drive
            .registers()
            .into_iter()
            .find(|(name, _)| *name == "P")
            .map(|(_, val)| val & 0x40 != 0)
            .unwrap_or(false)
    }

    #[test]
    fn byte_ready_only_asserts_the_6502_so_pin_while_soe_is_asserted() {
        let mut drive = test_drive();
        drive.attach_disk(blank_test_disk());
        drive.load(0x1C02, &[0xFF]); // VIA2 DDRB: all output
        drive.load(0x1C00, &[VIA2_PB_MTR]); // spindle motor on, STP/DS all 0

        // VIA2's CA2 (SOE) is left at its power-on default (an input mode,
        // `ca2_output_level()` reports `None`) -- real hardware fact this
        // crate's own `tick_disk` treats the same as "not asserted",
        // exactly like DOS firmware hasn't configured/asserted SOE yet.
        // Running well past a full byte-time must never set SO.
        for _ in 0..64 {
            drive.step();
        }
        assert!(
            !so_flag_set(&drive),
            "SO must stay clear while SOE is deasserted"
        );

        // CA2 (PCR bits 3:1) = manual output, driven high -- asserts SOE.
        drive.load(0x1C0C, &[0b111 << 1]);
        for _ in 0..64 {
            drive.step();
        }
        assert!(
            so_flag_set(&drive),
            "SO must be set once SOE is asserted and a real byte completes"
        );
    }

    fn drive_1571() -> C1541 {
        C1541::with_model(DriveModel::C1571, vec![0xEA; ROM_SIZE_1571], 8).unwrap()
    }

    #[test]
    fn a_1571_needs_a_32k_rom_and_a_1541_a_16k_one() {
        assert!(C1541::with_model(DriveModel::C1571, vec![0; ROM_SIZE], 8).is_err());
        assert!(C1541::with_model(DriveModel::C1541, vec![0; ROM_SIZE_1571], 8).is_err());
        assert!(C1541::with_model(DriveModel::C1570, vec![0; ROM_SIZE_1571], 8).is_ok());
    }

    #[test]
    fn the_1571_resets_through_its_vector_at_the_top_of_a_32k_rom() {
        let mut rom = vec![0xEAu8; ROM_SIZE_1571];
        rom[0x7FFC] = 0x78;
        rom[0x7FFD] = 0x56;
        let drive = C1541::with_model(DriveModel::C1571, rom, 8).unwrap();
        assert_eq!(drive.pc(), 0x5678);
    }

    #[test]
    fn the_1571_mirrors_its_2k_ram_and_maps_a_6526_at_4000() {
        let mut drive = drive_1571();
        drive.load(0x0010, &[0x77]);
        assert_eq!(
            drive.read_memory(0x0810),
            0x77,
            "RAM mirrors across $0000-$0FFF"
        );
        drive.load(0x4002, &[0xFF]); // CIA DDRA
        assert_eq!(drive.read_memory(0x4002), 0xFF);
        assert_eq!(
            drive.read_memory(0x4012),
            0xFF,
            "the CIA mirrors across $4000-$7FFF"
        );
        // The 1541 has neither.
        let mut plain = test_drive();
        plain.load(0x4002, &[0x12]);
        assert_eq!(plain.read_memory(0x4002), 0xFF);
    }

    #[test]
    fn via1_port_a_reports_byte_ready_inverted_and_the_track_zero_sensor() {
        let mut drive = drive_1571();
        // Idle: no byte waiting (bit 7 = 1), on track 1 (bit 0 = 0).
        assert_eq!(drive.bus.read(0x180F) & 0x81, 0x80);
        drive.bus.byte_ready_latch = true;
        assert_eq!(drive.bus.read(0x180F) & 0x80, 0, "a waiting byte reads 0");
        // Reading or writing the disk data port takes it.
        drive.bus.read(0x1C01);
        assert!(!drive.bus.byte_ready_latch);
        drive.bus.byte_ready_latch = true;
        drive.bus.write(0x1C01, 0);
        assert!(!drive.bus.byte_ready_latch);
    }

    #[test]
    fn via1_port_a_bit_5_switches_the_drive_to_2_mhz_and_bit_2_selects_the_head() {
        let mut drive = drive_1571();
        assert_eq!(drive.clock_hz(), 1_000_000);
        drive.bus.write(0x1803, 0x26); // DDRA: bits 1, 2, 5 are outputs
        drive.bus.write(0x180F, 0x20);
        assert_eq!(drive.clock_hz(), 2_000_000);
        drive.bus.write(0x180F, 0x00);
        assert_eq!(drive.clock_hz(), 1_000_000);
        drive.bus.write(0x180F, 0x04);
        assert_eq!(drive.bus.disk.side(), 1);
        drive.bus.write(0x180F, 0x00);
        assert_eq!(drive.bus.disk.side(), 0);
    }

    #[test]
    fn a_1570_has_one_head() {
        let mut drive = C1541::with_model(DriveModel::C1570, vec![0xEA; ROM_SIZE_1571], 8).unwrap();
        drive.bus.write(0x1803, 0x26);
        drive.bus.write(0x180F, 0x04);
        assert_eq!(drive.bus.disk.side(), 0);
    }

    #[test]
    fn a_1541_refuses_a_d71() {
        let mut drive = test_drive();
        assert!(drive.attach_d71(d71::D71::blank_formatted(*b"71")).is_err());
        let mut drive = drive_1571();
        assert!(drive.attach_d71(d71::D71::blank_formatted(*b"71")).is_ok());
    }

    #[test]
    fn the_1571_save_state_refuses_a_different_model() {
        let saved = drive_1571().save_state();
        let mut other = C1541::with_model(DriveModel::C1570, vec![0xEA; ROM_SIZE_1571], 8).unwrap();
        assert!(other.load_state(&saved).is_err());
        let mut same = drive_1571();
        assert!(same.load_state(&saved).is_ok());
    }
}
