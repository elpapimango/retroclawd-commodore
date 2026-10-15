//! Opt-in "true drive": a real 1541 (its own 6502, 2K RAM, user-supplied ROM,
//! two VIAs and the GCR disk mechanism, `c1541::C1541`) stepped alongside the
//! C64 and wired to it through the IEC serial bus, so the C64's own KERNAL
//! serial routines talk to the drive's DOS. This replaces the default
//! KERNAL-LOAD/SAVE fast traps (`C64::try_fast_kernal_load_trap`), which skip
//! the bus entirely; with a true drive the disk is attached to the drive and
//! the C64 side has none.
//!
//! The two machines run at nearly the same clock (985 248 Hz PAL vs the
//! drive's 1 MHz); the drive is caught up after every C64 instruction by a
//! fractional cycle account, and the bus levels are propagated after each
//! instruction on either side (the same loop `c64/tests/iec_1541.rs` drives by
//! hand). Accuracy tier: instruction granularity, no sub-instruction bus
//! timing -- the same level as the rest of this project's 6502 machines.

use c1541::{C1541, C1581, DriveModel, IecBus, Medium};
use machine::Machine as _;

/// The computer end of the serial bus: what it asserts and how it is told the
/// resulting line levels.
pub trait IecHost {
    /// `(ATN, CLOCK, DATA)` the computer asserts.
    fn iec_output(&self) -> (bool, bool, bool);
    /// The wired-AND `CLOCK` and `DATA` levels (true = high).
    fn set_iec_input(&mut self, clock: bool, data: bool);
    /// The computer's clock in Hz (the drive's cycles are scaled to it).
    fn clock_hz(&self) -> u64;
}

/// How long a drive stays empty between taking one disk out and putting the
/// next in (drive cycles, 0.3 s). A real swap always takes this long, and the
/// DOS only notices a changed disk -- re-reading the BAM instead of serving the
/// old one's free-block count -- when its write-protect light barrier sees the
/// gap (found with the real DOS ROM: an instant swap listed the new disk's
/// files but kept the old disk's `BLOCKS FREE`).
const DISK_SWAP_GAP_CYCLES: u64 = 300_000;

/// Which drive a true drive is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    C1541,
    C1570,
    C1571,
    C1581,
}

impl DriveKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "1541" => Some(Self::C1541),
            "1570" => Some(Self::C1570),
            "1571" => Some(Self::C1571),
            "1581" => Some(Self::C1581),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::C1541 => "1541",
            Self::C1570 => "1570",
            Self::C1571 => "1571",
            Self::C1581 => "1581",
        }
    }

    pub fn rom_size(self) -> usize {
        match self {
            Self::C1541 => c1541::ROM_SIZE,
            _ => c1541::ROM_SIZE_1571,
        }
    }
}

/// A disk for either kind of drive.
pub enum Disk {
    /// A GCR disk for the 1541/1570/1571.
    Gcr(Medium),
    /// A `.d81` for the 1581.
    D81(d81::D81),
}

enum Unit {
    Gcr(Box<C1541>),
    Mfm(Box<C1581>),
}

macro_rules! unit {
    ($self:expr, $d:ident => $body:expr) => {
        match $self {
            Unit::Gcr($d) => $body,
            Unit::Mfm($d) => $body,
        }
    };
}

impl Unit {
    fn iec_output(&self) -> (bool, bool) {
        unit!(self, d => d.iec_output())
    }

    fn set_iec_input(&mut self, atn: bool, clock: bool, data: bool) {
        unit!(self, d => d.set_iec_input(atn, clock, data));
    }

    fn step(&mut self) -> u32 {
        unit!(self, d => d.step())
    }

    fn clock_hz(&self) -> u64 {
        unit!(self, d => d.clock_hz())
    }

    fn has_disk(&self) -> bool {
        unit!(self, d => d.has_disk())
    }

    fn detach_disk(&mut self) {
        unit!(self, d => d.detach_disk());
    }

    fn disk_image(&self) -> Option<Vec<u8>> {
        unit!(self, d => d.disk_image())
    }

    fn take_modified_disk_image(&mut self) -> Option<Vec<u8>> {
        unit!(self, d => d.take_modified_disk_image())
    }

    fn mark_disk_saved(&mut self) {
        unit!(self, d => d.mark_disk_saved());
    }

    fn attach(&mut self, disk: Disk) -> Result<(), String> {
        match (self, disk) {
            (Unit::Gcr(d), Disk::Gcr(medium)) => d.attach_medium(medium),
            (Unit::Mfm(d), Disk::D81(image)) => {
                d.attach_d81(image);
                Ok(())
            }
            (Unit::Gcr(_), Disk::D81(_)) => {
                Err("a 1541/1570/1571 cannot read .d81 images (use --drive-type 1581)".to_string())
            }
            (Unit::Mfm(_), Disk::Gcr(_)) => Err("a 1581 reads .d81 images only".to_string()),
        }
    }

    fn save_state(&self) -> Vec<u8> {
        unit!(self, d => d.save_state())
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        unit!(self, d => d.load_state(bytes))
    }

    fn reset(&mut self) {
        unit!(self, d => d.reset());
    }
}

pub struct DriveLink {
    drive: Unit,
    bus: IecBus,
    /// Drive cycles still owed for the C64 cycles that have passed.
    owed: f64,
    /// A disk waiting out the swap gap, with the drive cycles left until it
    /// is inserted.
    pending_disk: Option<(Disk, u64)>,
}

impl DriveLink {
    pub fn new(kind: DriveKind, rom: Vec<u8>, device: u8) -> Result<Self, String> {
        let drive = match kind {
            DriveKind::C1541 => {
                Unit::Gcr(Box::new(C1541::with_model(DriveModel::C1541, rom, device)?))
            }
            DriveKind::C1570 => {
                Unit::Gcr(Box::new(C1541::with_model(DriveModel::C1570, rom, device)?))
            }
            DriveKind::C1571 => {
                Unit::Gcr(Box::new(C1541::with_model(DriveModel::C1571, rom, device)?))
            }
            DriveKind::C1581 => Unit::Mfm(Box::new(C1581::new(rom, device)?)),
        };
        let mut link = DriveLink {
            drive,
            bus: IecBus::new(),
            owed: 0.0,
            pending_disk: None,
        };
        link.settle();
        Ok(link)
    }

    fn settle(&mut self) {
        // Seed both sides' view of an idle bus.
        let (drive_clock, drive_data) = self.drive.iec_output();
        self.bus.set_drive_outputs(drive_clock, drive_data);
        self.drive
            .set_iec_input(self.bus.atn(), self.bus.clock(), self.bus.data());
    }

    /// Copies each side's output onto the shared bus and feeds the combined
    /// levels back into both.
    fn propagate(&mut self, host: &mut impl IecHost) {
        let (atn, clock, data) = host.iec_output();
        self.bus.set_computer_outputs(atn, clock, data);
        let (drive_clock, drive_data) = self.drive.iec_output();
        self.bus.set_drive_outputs(drive_clock, drive_data);
        host.set_iec_input(self.bus.clock(), self.bus.data());
        self.drive
            .set_iec_input(self.bus.atn(), self.bus.clock(), self.bus.data());
    }

    /// Called after the computer ran for `host_cycles`: publishes its bus outputs
    /// and runs the drive for the same span of real time.
    pub fn after_host_step(&mut self, host: &mut impl IecHost, host_cycles: u32) {
        self.propagate(host);
        self.owed += f64::from(host_cycles) * self.drive.clock_hz() as f64 / host.clock_hz() as f64;
        while self.owed >= 1.0 {
            let used = self.drive.step();
            self.owed -= f64::from(used);
            if let Some((_, left)) = &mut self.pending_disk {
                *left = left.saturating_sub(u64::from(used));
                if *left == 0
                    && let Some((disk, _)) = self.pending_disk.take()
                {
                    // A disk the drive cannot read was refused at attach.
                    let _ = self.drive.attach(disk);
                }
            }
            self.propagate(host);
        }
    }

    /// Inserts `disk`. When one is already in (or still waiting), it comes out
    /// first and the new one only goes in after `DISK_SWAP_GAP_CYCLES`; into an
    /// empty drive it goes in at once.
    pub fn attach_disk(&mut self, disk: Disk) -> Result<(), String> {
        // Refuse a disk the drive cannot read before touching the one inside.
        match (&self.drive, &disk) {
            (Unit::Gcr(d), Disk::Gcr(Medium::D71(_))) if !d.model().is_1571_family() => {
                return Err(
                    "a 1541 cannot read .d71 images (use --drive-type 1570 or 1571)".to_string(),
                );
            }
            (Unit::Gcr(_), Disk::D81(_)) => {
                return Err(
                    "a 1541/1570/1571 cannot read .d81 images (use --drive-type 1581)".to_string(),
                );
            }
            (Unit::Mfm(_), Disk::Gcr(_)) => {
                return Err("a 1581 reads .d81 images only".to_string());
            }
            _ => {}
        }
        if self.drive.has_disk() || self.pending_disk.is_some() {
            self.drive.detach_disk();
            self.pending_disk = Some((disk, DISK_SWAP_GAP_CYCLES));
            Ok(())
        } else {
            self.drive.attach(disk)
        }
    }

    /// The disk image as the drive has it (a disk still waiting out a swap gap
    /// is not in the drive yet).
    pub fn disk_image(&self) -> Option<Vec<u8>> {
        self.drive.disk_image()
    }

    pub fn take_modified_disk_image(&mut self) -> Option<Vec<u8>> {
        self.drive.take_modified_disk_image()
    }

    pub fn mark_disk_saved(&mut self) {
        self.drive.mark_disk_saved();
    }

    pub fn detach_disk(&mut self) {
        self.pending_disk = None;
        self.drive.detach_disk();
    }

    /// The drive's own save state.
    pub fn save_state(&self) -> Vec<u8> {
        self.drive.save_state()
    }

    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.drive.load_state(bytes)?;
        self.owed = 0.0;
        Ok(())
    }

    /// Resets the drive's CPU along with the computer.
    pub fn reset(&mut self) {
        self.drive.reset();
        self.owed = 0.0;
    }
}

/// Save-state container when a true drive is running: magic, computer-state length, the
/// computer state, then the drive's. A plain state has no magic, so old states
/// still load.
const DRIVE_STATE_MAGIC: &[u8; 4] = b"TDRV";

pub fn pack_drive_state(computer: &[u8], drive: &[u8]) -> Vec<u8> {
    let mut out = DRIVE_STATE_MAGIC.to_vec();
    out.extend_from_slice(&(computer.len() as u32).to_le_bytes());
    out.extend_from_slice(computer);
    out.extend_from_slice(drive);
    out
}

/// (computer state, drive state) of a packed true-drive state.
pub type PackedState<'a> = (&'a [u8], &'a [u8]);

pub fn unpack_drive_state(bytes: &[u8]) -> Result<Option<PackedState<'_>>, String> {
    if bytes.len() < 8 || &bytes[..4] != DRIVE_STATE_MAGIC {
        return Ok(None);
    }
    let len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let rest = &bytes[8..];
    if len > rest.len() {
        return Err("the true-drive state is truncated".to_string());
    }
    Ok(Some((&rest[..len], &rest[len..])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use c64::C64;
    use machine::Machine;

    /// A hand-made 1541 "firmware": configure VIA1 like the real DOS (DDRB
    /// $1A), wait for ATN to be asserted, then answer by setting DATA OUT
    /// (and the ATN-acknowledge bit so the hardware gate lets go).
    fn drive_rom() -> Vec<u8> {
        let mut rom = vec![0xEA; 0x4000];
        let code = [
            0xA9, 0x1A, 0x8D, 0x02, 0x18, // LDA #$1A / STA $1802
            0xA9, 0x00, 0x8D, 0x00, 0x18, // LDA #0 / STA $1800
            0xAD, 0x00, 0x18, 0x30, 0xFB, // wait: LDA $1800 / BMI wait (ATN released)
            0xA9, 0x12, 0x8D, 0x00, 0x18, // DATA OUT + ATNA
            0x4C, 0x14, 0xC0, // spin
        ];
        rom[..code.len()].copy_from_slice(&code);
        rom[0x3FFC] = 0x00;
        rom[0x3FFD] = 0xC0;
        rom
    }

    struct Host<'a>(&'a mut C64);

    impl IecHost for Host<'_> {
        fn iec_output(&self) -> (bool, bool, bool) {
            self.0.iec_output()
        }
        fn set_iec_input(&mut self, clock: bool, data: bool) {
            self.0.set_iec_input(clock, data);
        }
        fn clock_hz(&self) -> u64 {
            self.0.clock_hz()
        }
    }

    fn c64() -> C64 {
        C64::new(vec![0xEA; 8192], vec![0xEA; 8192], vec![0; 4096]).unwrap()
    }

    #[test]
    fn the_drive_answers_atn_on_the_bus_to_the_c64_program() {
        let mut c64 = c64();
        let mut link = DriveLink::new(DriveKind::C1541, drive_rom(), 8).unwrap();
        // C64: DDRA like the KERNAL ($3F), assert ATN, wait for DATA IN to go
        // low, note it at $0400 and spin.
        let code = [
            0xA9, 0x3F, 0x8D, 0x02, 0xDD, // DDRA
            0xA9, 0x08, 0x8D, 0x00, 0xDD, // ATN OUT asserted
            0xAD, 0x00, 0xDD, 0x30, 0xFB, // wait: LDA $DD00 / BMI wait (DATA released)
            0xA9, 0x01, 0x8D, 0x00, 0x04, // $0400 = 1
            0x4C, 0x14, 0xC0, // spin
        ];
        c64.load(0xC000, &code);
        c64.set_pc(0xC000);
        let mut cycles = 0u64;
        while cycles < 30_000 {
            let used = c64.step();
            link.after_host_step(&mut Host(&mut c64), used);
            cycles += u64::from(used);
        }
        assert_eq!(c64.read_memory(0x0400), 1, "DATA came back low after ATN");
    }

    #[test]
    fn a_drive_that_does_not_exist_leaves_the_bus_idle_and_roms_are_size_checked() {
        assert!(DriveLink::new(DriveKind::C1541, vec![0; 100], 8).is_err());
        let mut c64 = c64();
        let mut link = DriveLink::new(DriveKind::C1541, vec![0xEA; 0x4000], 8).unwrap();
        for _ in 0..1000 {
            let used = c64.step();
            link.after_host_step(&mut Host(&mut c64), used);
        }
        assert_eq!(c64.read_memory(0xDD00) & 0x80, 0x80, "DATA stays released");
    }

    fn blank_disk() -> d64::D64 {
        d64::D64::parse(&vec![0u8; 683 * d64::SECTOR_SIZE]).unwrap()
    }

    #[test]
    fn a_disk_goes_into_an_empty_drive_at_once_but_a_swap_waits_out_the_gap() {
        let mut c64 = c64();
        let mut link = DriveLink::new(DriveKind::C1541, drive_rom(), 8).unwrap();
        link.attach_disk(Disk::Gcr(Medium::D64(blank_disk())))
            .unwrap();
        assert!(link.drive.has_disk(), "empty drive: inserted immediately");

        link.attach_disk(Disk::Gcr(Medium::D64(blank_disk())))
            .unwrap();
        assert!(
            !link.drive.has_disk(),
            "the old disk is out during the swap"
        );
        assert!(link.pending_disk.is_some());
        // Run less than the gap: still empty; run past it: the new disk is in.
        let mut elapsed = 0u64;
        while elapsed < DISK_SWAP_GAP_CYCLES / 2 {
            elapsed += u64::from(c64.step());
            link.after_host_step(&mut Host(&mut c64), 1);
        }
        let _ = elapsed;
        assert!(!link.drive.has_disk());
        for _ in 0..DISK_SWAP_GAP_CYCLES * 2 {
            link.after_host_step(&mut Host(&mut c64), 1);
        }
        assert!(link.drive.has_disk(), "inserted after the gap");
        assert!(link.pending_disk.is_none());

        // Ejecting cancels a disk that is still waiting.
        link.attach_disk(Disk::Gcr(Medium::D64(blank_disk())))
            .unwrap();
        link.detach_disk();
        assert!(link.pending_disk.is_none() && !link.drive.has_disk());
    }

    #[test]
    fn drive_state_packs_and_unpacks_round_trip() {
        let packed = pack_drive_state(&[1, 2, 3], &[9, 9]);
        let (a, b) = unpack_drive_state(&packed).unwrap().unwrap();
        assert_eq!((a, b), (&[1u8, 2, 3][..], &[9u8, 9][..]));
        assert!(unpack_drive_state(&[0, 1, 2]).unwrap().is_none());
        assert!(unpack_drive_state(b"TDRV\xFF\xFF\xFF\xFF").is_err());
    }

    #[test]
    fn drive_kinds_parse_and_know_their_rom_sizes() {
        for (text, size) in [
            ("1541", 0x4000),
            ("1570", 0x8000),
            ("1571", 0x8000),
            ("1581", 0x8000),
        ] {
            let kind = DriveKind::parse(text).unwrap();
            assert_eq!(kind.name(), text);
            assert_eq!(kind.rom_size(), size);
        }
        assert!(DriveKind::parse("1551").is_none());
    }

    #[test]
    fn a_1581_link_takes_only_d81_and_a_gcr_drive_refuses_one() {
        let rom = vec![0xEA; 0x8000];
        let mut link = DriveLink::new(DriveKind::C1581, rom.clone(), 8).unwrap();
        assert!(
            link.attach_disk(Disk::D81(d81::D81::blank_formatted(*b"81")))
                .is_ok()
        );
        assert!(
            link.attach_disk(Disk::Gcr(Medium::D64(blank_disk())))
                .is_err()
        );
        let mut gcr = DriveLink::new(DriveKind::C1571, rom, 8).unwrap();
        assert!(
            gcr.attach_disk(Disk::D81(d81::D81::blank_formatted(*b"81")))
                .is_err()
        );
        assert!(
            gcr.attach_disk(Disk::Gcr(Medium::D71(d71::D71::blank_formatted(*b"71"))))
                .is_ok()
        );
        let mut plain = DriveLink::new(DriveKind::C1541, vec![0xEA; 0x4000], 8).unwrap();
        assert!(
            plain
                .attach_disk(Disk::Gcr(Medium::D71(d71::D71::blank_formatted(*b"71"))))
                .is_err()
        );
        assert!(DriveLink::new(DriveKind::C1581, vec![0xEA; 0x4000], 8).is_err());
    }
}
