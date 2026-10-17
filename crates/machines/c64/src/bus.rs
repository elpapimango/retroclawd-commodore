//! Commodore 64 machine implementation: real PLA/6510-port memory
//! banking, a full CIA1 + CIA2 (Timer A/B, TOD clock, serial register,
//! see `crate::cia`) with the real keyboard matrix wired into CIA1's Port
//! A/B (see `crate::keyboard`) and real VIC bank selection wired into
//! CIA2's Port A (see `vic_bank_base` below), VIC-II with raster IRQ,
//! bad-line cycle stealing, all 5 display modes, and all 8 hardware
//! sprites (see `vic_ii`), and a real SID (see `sid`) wired into
//! `$D400`-`$D7FF` and the audio pipeline below. VIC-II sprite-collision
//! detection remains explicitly deferred -- see `TODO.md`'s "Commodore 64
//! support" section for the full scope cut and its backlog.

use machine::{Bus, Machine, RunStats};

pub const KERNAL_ROM_SIZE: usize = 0x2000;
pub const BASIC_ROM_SIZE: usize = 0x2000;
pub const CHAR_ROM_SIZE: usize = 0x1000;

const RAM_SIZE: usize = 0x10000;
const COLOR_RAM_SIZE: usize = 0x0400;

const PAL_CLOCK_HZ: u64 = 985_248;
const PAL_FRAME_HZ: u64 = 50;
/// 6567R8 machine: 1 022 727 Hz, 59.8 Hz frames (paced at 60 Hz here, the
/// same integer rounding the other machines use for their frame rate).
const NTSC_CLOCK_HZ: u64 = 1_022_727;
const NTSC_FRAME_HZ: u64 = 60;
/// Longest stretch of REU DMA fed to the peripherals in one go -- under a
/// raster line, so the VIC and CIAs never skip an event.
const DMA_CHUNK_CYCLES: u32 = 32;

/// 32px, matching every other machine's own border convention here
/// (`zx_spectrum::video::BORDER_PX`) -- a common emulator choice, not a
/// broadcast-accurate PAL measurement.
const BORDER_PX: u32 = 32;

/// Real CIA2 Port A (`$DD00`) bit layout for the IEC serial bus --
/// confirmed directly against VICE's own real source this session
/// (`c64cia2.c`'s own comment block: "Bit 7 Serial Bus Data Input / Bit 6
/// Serial Bus Clock Pulse Input / Bit 5 Serial Bus Data Output / Bit 4
/// Serial Bus Clock Pulse Output / Bit 3 Serial Bus ATN Signal Output",
/// see `CREDITS.md`) -- bits 0-1 are the VIC bank select bits
/// `vic_bank_base` already decodes, bit 2 is RS-232/userport TXD (not
/// modeled). Same real, inverted-driver hardware convention `crate::iec`
/// documents for the drive side too: a written `1` on an `*_OUT` bit
/// pulls the physical bus line low (asserts it); an `*_IN` bit reads the
/// real, current wired-AND bus level directly (`1` = released/high).
const CIA2_PA_ATN_OUT: u8 = 0x08;
const CIA2_PA_CLOCK_OUT: u8 = 0x10;
const CIA2_PA_DATA_OUT: u8 = 0x20;
const CIA2_PA_CLOCK_IN: u8 = 0x40;
const CIA2_PA_DATA_IN: u8 = 0x80;

/// Real KERNAL LOAD entry point (`try_fast_kernal_load_trap`'s own trap
/// address) and the zero-page variables a caller's already-completed
/// `SETNAM`/`SETLFS` leave set before `JSR $FFD5` -- every one of these
/// was confirmed by disassembling this project's own committed
/// `roms/c64/kernal-*.bin` files directly this session (a throwaway
/// Python 6502 disassembler, not recalled), not just cited from a web
/// reference: `$FFD5` is a literal `JMP $F49E` byte-for-byte identical
/// across all 5 committed KERNAL revisions; `$F49E`'s own first two
/// instructions (`STX $C3`/`STY $C4`) save the caller's given load
/// address before jumping through the real `$0330` LOAD vector; `$F3D5`
/// (the shared serial-bus "open" routine `$F49E`'s own default vector
/// target calls) was disassembled too, confirming `LDA ($BB),Y` reads
/// the filename through the zero-page-indirect pointer at `$BB`/`$BC`
/// (FNADR) bounded by `$B7` (FNLEN), and `LDA $B9`/`STA $90` confirm SA/
/// STATUS. See `CREDITS.md` for the full disassembly transcript and the
/// LOAD calling-convention citations (carry/A/X/Y on exit) this couldn't
/// derive from raw disassembly alone.
const KERNAL_LOAD_ENTRY: u16 = 0xF49E;
/// The exact two bytes real KERNAL LOAD's `$F49E` entry point begins
/// with on every committed ROM (`STX $C3`) -- checked before trapping,
/// same "opcode still there" guard `zx_spectrum::try_fast_tape_load_trap`
/// already established, so a custom ROM reusing this address for
/// something else isn't silently hijacked.
const KERNAL_LOAD_ENTRY_FIRST_BYTES: [u8; 2] = [0x86, 0xC3];
const ZP_STATUS: u16 = 0x90;
const ZP_VERCK: u16 = 0x93;
const ZP_FNLEN: u16 = 0xB7;
const ZP_SA: u16 = 0xB9;
const ZP_FA: u16 = 0xBA;
const ZP_FNADR_LO: u16 = 0xBB;
const ZP_FNADR_HI: u16 = 0xBC;
const ZP_EAL: u16 = 0xAE;
const ZP_EAH: u16 = 0xAF;
/// Real CBM DOS convention (commonly documented, confirmed via search
/// this session): any device number 8 or above is a disk drive on the
/// serial/IEC bus. Devices 4-7 (printers, etc.) and below aren't serviced
/// by this trap -- they fall through to real, unmodified KERNAL execution,
/// same as before this feature existed.
const KERNAL_LOAD_MIN_DISK_DEVICE: u8 = 8;
/// Device 1 is the Datasette.
const KERNAL_TAPE_DEVICE: u8 = 1;
/// Real KERNAL LOAD error code for "FILE NOT FOUND" (confirmed via
/// search this session, not guessed) -- returned in the A register with
/// carry set.
const KERNAL_ERROR_FILE_NOT_FOUND: u8 = 4;
/// Real KERNAL error code for "MISSING FILE NAME" (SAVE with an empty name).
const KERNAL_ERROR_MISSING_FILE_NAME: u8 = 8;
/// Real KERNAL SAVE entry point (`$FFD8` is a `JMP $F5DD`): `STX $AE / STY
/// $AF / TAX / LDA $00,X / STA $C1 / LDA $01,X / STA $C2 / JMP ($0332)`.
/// A holds the zero-page address of the start-address pointer, X/Y the end
/// address (exclusive). Byte-identical in the committed `-01`/`-02`/`-03`
/// KERNALs (checked by hand against the ROM images).
const KERNAL_SAVE_ENTRY: u16 = 0xF5DD;
const KERNAL_SAVE_ENTRY_FIRST_BYTES: [u8; 2] = [0x86, 0xAE];

/// Real C64 PLA/6510-port memory banking. `$A000`-`$BFFF` (BASIC),
/// `$D000`-`$DFFF` (I/O vs. character ROM vs. RAM), and `$E000`-`$FFFF`
/// (KERNAL) are the only banked windows -- `$0002`-`$9FFF` and
/// `$C000`-`$CFFF` are always RAM on real hardware, no banking logic
/// needed there at all. Table per the commonly-cited C64 Programmer's
/// Reference Guide banking table (see `CREDITS.md`).
///
/// Real hardware fact this bus's `read`/`write` split on: a ROM window
/// being visible for *reads* never stops a *write* from landing in the
/// real RAM underneath -- the 64K RAM array always exists and is always
/// written, regardless of what's currently banked in for reads. The one
/// exception is the I/O window (`$D000`-`$DFFF` when `CHAREN` selects
/// I/O): those really are chip registers, not RAM-shadowed, so writes
/// there go to the chips, not to the RAM underneath.
#[derive(Clone)]
pub struct C64Bus {
    pub(crate) ram: Vec<u8>,
    kernal_rom: Vec<u8>,
    basic_rom: Vec<u8>,
    char_rom: Vec<u8>,
    pub(crate) color_ram: Vec<u8>,

    /// `$0000` -- the 6510's on-chip I/O port direction register (1 =
    /// that bit is driven as output).
    pub(crate) port_ddr: u8,
    /// `$0001` -- the 6510's on-chip I/O port data register. Bits 0-2
    /// (LORAM/HIRAM/CHAREN) are what actually drive memory banking.
    pub(crate) port_data: u8,

    pub(crate) vic: vic_ii::Vic,
    pub(crate) cia1: crate::cia::Cia,
    pub(crate) cia2: crate::cia::Cia,
    pub(crate) keyboard: crate::keyboard::KeyMatrix,
    /// Joystick directions/fire held on control port 1 (`[0]`, wired to CIA1
    /// Port B alongside the keyboard rows) and port 2 (`[1]`, CIA1 Port A),
    /// as `JoystickButton` bits, active high here (the CIA pins read low when
    /// pressed). Transient host input, never saved in a save state.
    pub(crate) joystick: [u8; 2],
    /// Paddles and the 1351 mouse (the analogue side of the control ports).
    pub(crate) controllers: crate::controllers::Controllers,
    pub(crate) sid: sid::Sid,

    /// The IEC serial bus's own current `CLOCK`/`DATA` line levels, as
    /// last reported via `set_iec_input` -- `ATN` has no `*_IN` bit on the
    /// C64 side at all (see `CIA2_PA_ATN_OUT`'s own doc comment: the C64
    /// is always the bus's `ATN` master on real hardware, it never needs
    /// to read someone else driving it). Defaults to `true` (idle-high,
    /// released) for both, matching every real IEC line's own idle state
    /// before anything asserts it.
    pub(crate) iec_clock_in: bool,
    pub(crate) iec_data_in: bool,

    /// The currently loaded cassette tape, if any -- `None` until
    /// `attach_tap` is called. Real hardware fact this crate models
    /// faithfully (see `crate::bus`'s own top-level doc comment on the
    /// real 6510-port/CIA1-FLAG cassette wiring): motor control (6510
    /// port bit 5), SENSE (bit 4), and the READ line (CIA1's own FLAG
    /// pin) are genuinely different mechanisms from every other
    /// machine's own tape wiring here (`zx_spectrum`'s port-bit poll,
    /// `cpc`'s own PPI-mediated motor/EAR bits).
    pub(crate) tape: Option<c64_tape::TapePlayer>,
    /// Records the cassette WRITE line (always listening; the motor gates it)
    /// and collects what the fast SAVE trap writes.
    /// Programs in the standard cassette format found on the attached tape
    /// (for the fast LOAD trap) and the "tape position" after the last one
    /// loaded.
    pub(crate) tape_files: Vec<c64_tape::rom_format::RomFile>,
    pub(crate) tape_cursor: usize,
    /// Whether KERNAL LOAD/SAVE from/to the cassette is served instantly
    /// (default) instead of through the real-time pulse path.
    pub(crate) fast_tape: bool,
    pub(crate) tape_recorder: Option<crate::tape_recorder::TapeRecorder>,
    /// The Datasette's RECORD button: pressing it (`arm_tape_recorder`)
    /// makes SENSE read low so the KERNAL's real-time SAVE starts at once.
    pub(crate) record_pressed: bool,
    /// CPU cycles elapsed at the start of the instruction being executed,
    /// the time base for the recorder.
    pub(crate) stamp: u64,

    /// The cartridge in the expansion port, if any (see `crate::cartridge`).
    /// It drives the EXROM/GAME lines the PLA table below reads, answers
    /// `$DE00`-`$DFFF`, and supplies the ROML/ROMH windows.
    pub(crate) cartridge: Option<crate::cartridge::Cartridge>,

    /// A RAM expansion unit in the expansion port (exclusive with a
    /// cartridge -- they share the port), and the CPU cycles its DMA has
    /// stolen that `C64::step` has not yet charged.
    pub(crate) reu: Option<crate::reu::Reu>,
    /// A GeoRAM in the expansion port (excludes an REU or a cartridge).
    pub(crate) georam: Option<crate::georam::GeoRam>,
    /// A SwiftLink / Turbo232 serial cartridge and the I/O page it answers on.
    pub(crate) acia: Option<(acia6551::Acia, u8)>,
    /// An RR-Net / TFE ethernet cartridge.
    pub(crate) rrnet: Option<crate::rrnet::RrNet>,
    /// A text printer on the serial bus (device 4/5), when the trap is enabled.
    pub(crate) printer: Option<crate::printer::Printer>,
    dma_cycles: u32,

    /// `vic_accuracy = cycle`: every CPU bus cycle advances the whole machine
    /// by one clock (see `tick_cycle`) and the VIC-II's BA stalls CPU reads.
    /// Off (line accuracy), the machine is ticked once per instruction.
    pub(crate) cycle_mode: bool,
    /// True only while `mos6502::Cpu::step` runs, so the debugger's, the
    /// traps' and the REU's own `read`/`write` calls never tick the clock.
    in_cpu: bool,
    /// Interrupt requests raised by a peripheral in the middle of an
    /// instruction; `C64::step` hands them to the CPU afterwards (the CPU
    /// samples its lines at instruction boundaries anyway).
    pending_irq: bool,
    pending_nmi: bool,
    /// CIA2's interrupt output as last seen: the 6510's NMI input is edge
    /// triggered, so a level held while the ICR is unacknowledged is one NMI,
    /// not one per instruction (a game that programs a fast CIA2 timer and
    /// never reads the ICR used to livelock in the NMI entry).
    pub(crate) cia2_nmi_active: bool,
    /// Clock cycles run since the current `C64::step` began (CPU cycles plus
    /// the ones the CPU spent stalled by BA).
    step_cycles: u32,
    /// Of those, the cycles a read spent waiting for BA (diagnostics/tests).
    pub(crate) stalled_cycles: u64,

    /// The currently attached `.d64` disk image, if any -- `None` until
    /// `attach_disk` is called. Unlike `tape`, this is a direct, real-media
    /// attachment with no simulated drive hardware behind it at all (no
    /// `c1541`, no IEC bus, no GCR bitstream) -- `try_fast_kernal_load_trap`
    /// (see `crate::C64`'s own doc comment) is the only thing that ever
    /// reads it, servicing a real KERNAL LOAD request directly from the
    /// image's own logical directory/sector contents. This is deliberately
    /// simpler than real hardware (which always goes through a real,
    /// separately-clocked 1541/1571/1581), the same "IEC-level fast
    /// loader" shape real fastloader carts/DOS wedges and VICE's own "no
    /// true drive emulation" mode both use: replace the KERNAL's own slow
    /// serial-bus transfer with a direct block read, since nothing here
    /// needs the real bit-level protocol simulated to get the same
    /// *logical* result. `.d71`/`.d81` reuse this exact same trap --
    /// they're just a different real disk geometry (double-sided 1571,
    /// uniform-sector 1581) behind the identical CBM DOS directory/file
    /// chain format, see `AttachedDisk`'s own doc comment.
    pub(crate) disk: Option<AttachedDisk>,
    /// Set when the SAVE trap changed the attached image; cleared by
    /// `C64::mark_disk_saved`/`take_modified_disk_image` and by attaching a
    /// new image. Not part of save states (the image itself is external).
    pub(crate) disk_modified: bool,
}

/// Whichever real CBM DOS disk image is currently attached -- `.d64`
/// (1541), `.d71` (1571, double-sided), or `.d81` (1581, uniform-sector,
/// its own genuinely different header/BAM shape -- see `d81`'s own module
/// doc comment). `try_fast_kernal_load_trap` only ever needs a filename
/// lookup + file read, so this only exposes that one operation rather than
/// re-exposing each format crate's full API -- format-specific geometry
/// stays entirely inside each crate.
#[derive(Clone)]
pub(crate) enum AttachedDisk {
    D64(d64::D64),
    D71(d71::D71),
    D81(d81::D81),
}

/// Finds a program by real CBM DOS filename-matching rules and returns its
/// `(embedded load address, payload bytes after the 2-byte header)` --
/// `None` for either "no matching entry" or "entry found but too short to
/// carry a real load-address header", exactly mirroring
/// `try_fast_kernal_load_trap`'s own single FILE-NOT-FOUND branch for both
/// cases. Implemented once per format crate (not generalized behind a
/// shared trait object) since each crate's own `DirectoryEntry`/`Error`
/// types are deliberately not shared -- see `d71`/`d81`'s own "own shape
/// per format" doc comments.
trait CbmDisk {
    fn find_program_bytes(&self, name: &[u8]) -> Option<(u16, Vec<u8>)>;
}

impl CbmDisk for d64::D64 {
    fn find_program_bytes(&self, name: &[u8]) -> Option<(u16, Vec<u8>)> {
        let entries = self.directory_entries();
        let entry = d64::D64::find_program(&entries, name)?;
        let bytes = self.read_file(entry).ok()?;
        (bytes.len() >= 2).then(|| {
            (
                u16::from_le_bytes([bytes[0], bytes[1]]),
                bytes[2..].to_vec(),
            )
        })
    }
}

impl CbmDisk for d71::D71 {
    fn find_program_bytes(&self, name: &[u8]) -> Option<(u16, Vec<u8>)> {
        let entries = self.directory_entries();
        let entry = d71::D71::find_program(&entries, name)?;
        let bytes = self.read_file(entry).ok()?;
        (bytes.len() >= 2).then(|| {
            (
                u16::from_le_bytes([bytes[0], bytes[1]]),
                bytes[2..].to_vec(),
            )
        })
    }
}

impl CbmDisk for d81::D81 {
    fn find_program_bytes(&self, name: &[u8]) -> Option<(u16, Vec<u8>)> {
        let entries = self.directory_entries();
        let entry = d81::D81::find_program(&entries, name)?;
        let bytes = self.read_file(entry).ok()?;
        (bytes.len() >= 2).then(|| {
            (
                u16::from_le_bytes([bytes[0], bytes[1]]),
                bytes[2..].to_vec(),
            )
        })
    }
}

impl AttachedDisk {
    /// Serialised image, in whichever format is attached.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        match self {
            AttachedDisk::D64(disk) => disk.to_bytes(),
            AttachedDisk::D71(disk) => disk.to_bytes(),
            AttachedDisk::D81(disk) => disk.to_bytes(),
        }
    }

    /// Stores a PRG in whichever image is attached.
    fn save_program(
        &mut self,
        name: &[u8],
        data: &[u8],
        replace: bool,
    ) -> Option<Result<(), cbm_dos::SaveError>> {
        Some(match self {
            AttachedDisk::D64(disk) => disk.save_program(name, data, replace),
            AttachedDisk::D71(disk) => disk.save_program(name, data, replace),
            AttachedDisk::D81(disk) => disk.save_program(name, data, replace),
        })
    }

    fn find_program_bytes(&self, name: &[u8]) -> Option<(u16, Vec<u8>)> {
        match self {
            AttachedDisk::D64(disk) => disk.find_program_bytes(name),
            AttachedDisk::D71(disk) => disk.find_program_bytes(name),
            AttachedDisk::D81(disk) => disk.find_program_bytes(name),
        }
    }
}

/// Bitmask of the PLA configurations (VICE's index: bit 4 = GAME active,
/// bit 3 = EXROM active, bit 2 = CHAREN, bit 1 = HIRAM, bit 0 = LORAM) in
/// which a region is visible. Transcribed from VICE's `c64meminit.c` arrays
/// (`c64meminit_roml_config`/`_romh_config`/`_io_config`/`_romh_mapping`)
/// and its comment table; see `CREDITS.md`.
const fn cfg_mask(list: &[u8]) -> u32 {
    let mut mask = 0u32;
    let mut i = 0;
    while i < list.len() {
        mask |= 1 << list[i];
        i += 1;
    }
    mask
}

/// 8K and 16K cartridges, and every Ultimax configuration, show ROML.
const PLA_ROML: u32 = cfg_mask(&[11, 15, 16, 17, 18, 19, 20, 21, 22, 23, 27, 31]);
/// ROMH at `$A000` (16K cartridges with HIRAM set).
const PLA_ROMH_A000: u32 = cfg_mask(&[26, 27, 30, 31]);
/// ROMH at `$E000` (Ultimax).
const PLA_ROMH_E000: u32 = cfg_mask(&[16, 17, 18, 19, 20, 21, 22, 23]);
const PLA_BASIC: u32 = cfg_mask(&[3, 7, 11, 15]);
const PLA_KERNAL: u32 = cfg_mask(&[2, 3, 6, 7, 10, 11, 14, 15, 26, 27, 30, 31]);
const PLA_CHARGEN: u32 = cfg_mask(&[1, 2, 3, 9, 10, 11, 26, 27]);
const PLA_IO: u32 = cfg_mask(&[
    5, 6, 7, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 29, 30, 31,
]);
/// Ultimax (GAME active, EXROM not): `$1000-$7FFF`, `$A000-$CFFF` are open and
/// RAM exists only below `$1000`.
const PLA_ULTIMAX: u32 = cfg_mask(&[16, 17, 18, 19, 20, 21, 22, 23]);

impl C64Bus {
    fn new(kernal_rom: Vec<u8>, basic_rom: Vec<u8>, char_rom: Vec<u8>) -> Self {
        let mut bus = Self {
            ram: vec![0; RAM_SIZE],
            kernal_rom,
            basic_rom,
            char_rom,
            color_ram: vec![0; COLOR_RAM_SIZE],
            port_ddr: 0,
            port_data: 0,
            vic: vic_ii::Vic::new(),
            cia1: crate::cia::Cia::new(),
            cia2: crate::cia::Cia::new(),
            keyboard: crate::keyboard::KeyMatrix::new(),
            joystick: [0; 2],
            controllers: crate::controllers::Controllers::default(),
            sid: sid::Sid::new(),
            iec_clock_in: true,
            iec_data_in: true,
            tape: None,
            tape_files: Vec::new(),
            tape_cursor: 0,
            fast_tape: true,
            tape_recorder: Some(crate::tape_recorder::TapeRecorder::new()),
            record_pressed: false,
            stamp: 0,
            cartridge: None,
            reu: None,
            georam: None,
            acia: None,
            rrnet: None,
            printer: None,
            dma_cycles: 0,
            cycle_mode: false,
            in_cpu: false,
            pending_irq: false,
            pending_nmi: false,
            cia2_nmi_active: false,
            step_cycles: 0,
            stalled_cycles: 0,
            disk: None,
            disk_modified: false,
        };
        // Seeds CIA1's own FLAG-pin edge-tracking to the same idle-high
        // baseline the real cassette READ line (and `c64_tape::TapePlayer`
        // itself) starts at -- `Cia::new()`'s bare `Default` derive has no
        // way to know a fresh chip's FLAG pin starts out released, same
        // "generic chip crate can't assume a host's own initial wiring"
        // reasoning `c1541::bus::C1541Bus::new`'s own `via1.set_ca1(true)`
        // seed already established for VIA1's `ATN` line. Without this,
        // the tape's very first real negative edge would be invisible to
        // edge detection (it would look like "already low, no edge").
        bus.cia1.set_flag(true);
        bus
    }

    /// CIA1 Port B ($DC01) read, with the real keyboard matrix wired in:
    /// bits configured as input (`ddr_b` clear, the KERNAL's own default)
    /// reflect the matrix scan result for whatever columns Port A
    /// currently drives low; bits configured as output (`ddr_b` set --
    /// not the common case, but real hardware allows it) reflect
    /// whatever was last written instead, since those pins aren't
    /// electrically connected to the row lines' pull-ups at all once
    /// driven. Pure/non-mutating -- safe for both `io_read` and
    /// `io_peek` to share.
    fn keyboard_port_b(&self) -> u8 {
        // Bits not driven as output on Port A float high (not selected),
        // same "undriven pin reads 1" convention as `port01` above.
        let pa_effective = self.cia1.port_a | !self.cia1.ddr_a;
        let matrix_pb = self.keyboard.read(pa_effective);
        let pb = (matrix_pb & !self.cia1.ddr_b) | (self.cia1.port_b & self.cia1.ddr_b);
        // A joystick in control port 1 pulls its pins low, whatever else is
        // driving them -- which is also why joystick 1 makes ghost keypresses
        // on a real C64.
        pb & !(self.joystick[0] | self.controllers.lines(1))
    }

    /// CIA1 Port A ($DC00) read: the pin levels -- driven outputs show what
    /// was written, undriven inputs float high -- with a control port 2
    /// joystick's pressed directions/fire pulled low.
    /// A SID register read: POTX/POTY (`$19`/`$1A`) come from whatever sits
    /// in the control port(s) CIA1 Port A bits 7:6 select, everything else
    /// from the chip.
    fn sid_read(&self, reg: u8) -> u8 {
        match reg {
            0x19 | 0x1A => {
                let select = self.cia1.effective_port_a() >> 6;
                self.controllers.pot(select, usize::from(reg - 0x19))
            }
            _ => self.sid.read(reg),
        }
    }

    fn joystick_port_a(&self) -> u8 {
        self.cia1.effective_port_a() & !(self.joystick[1] | self.controllers.lines(2))
    }

    /// The port's real *effective* readable value: a bit driven as output
    /// (`port_ddr` bit set) reflects `port_data`; a bit left as input
    /// (`port_ddr` bit clear) floats to `1` (the 6510's real internal
    /// pull-ups) -- this is what makes the machine boot into
    /// BASIC+KERNAL+I/O-visible banking *before* the KERNAL's own reset
    /// code has written anything to `$01` at all: with both registers
    /// starting at `0`, every bit floats high, giving
    /// `LORAM=HIRAM=CHAREN=1`, exactly the banking the CPU needs in order
    /// to fetch its own reset vector from KERNAL ROM in the first place.
    fn port01(&self) -> u8 {
        // The 6510 in the C64 has six port pins: bits 6 and 7 do not exist and read back 0, so `PEEK(1)` is 55 ($37) in the
        // standard configuration (c64-wiki "Memory Map"), not 247.
        let base = ((self.port_data & self.port_ddr) | !self.port_ddr) & 0x3F;
        // Real hardware: cassette SENSE (bit 4) reads low (active) exactly
        // when the Datasette senses a button pressed, if left configured
        // as input -- confirmed against VICE's own real `c64pla.c`
        // `c64pla_config_changed` this session (see `CREDITS.md`). No real
        // physical button exists here, so this asserts SENSE the instant a
        // tape is attached (same "no separate press-play step needed"
        // convention `zx_spectrum::ZxSpectrum`'s own tape auto-play
        // already established), sparing the KERNAL's own real
        // "PRESS PLAY ON TAPE" wait loop entirely.
        if (self.tape.is_some() || self.record_pressed) && self.port_ddr & 0x10 == 0 {
            base & !0x10
        } else {
            base
        }
    }

    /// Real hardware: MOTOR (6510 port bit 5) is active-low -- the
    /// Datasette motor spins exactly when bit 5 is driven low as an
    /// output; left as input (a real edge case no real software
    /// exercises) also reads as "on" by this same formula, matching
    /// VICE's own confirmed `(dir & data) & 0x20` check exactly (see
    /// `CREDITS.md`) rather than this crate's own simplified guess.
    /// Called after every write to `$0000`/`$0001`, either of which can
    /// change the real effective output value.
    fn apply_tape_motor(&mut self) {
        let motor_on = self.port_ddr & self.port_data & 0x20 == 0;
        if self.tape_recorder.is_some() {
            // WRITE (bit 3) floats high when not driven as an output.
            let level = (self.port_data & self.port_ddr) | !self.port_ddr;
            let level = level & 0x08 != 0;
            let stamp = self.stamp;
            if let Some(recorder) = &mut self.tape_recorder {
                recorder.note(stamp, motor_on, level);
            }
        }
        if let Some(tape) = &mut self.tape {
            if motor_on {
                tape.play();
            } else {
                tape.pause();
            }
        }
    }

    /// Installs a tape for loading, replacing any tape already installed
    /// -- see `crate::C64::load_tape`.
    pub(crate) fn attach_tape(&mut self, tape: c64_tape::Tape) {
        self.tape_files = c64_tape::rom_format::decode_files(&tape);
        self.tape_cursor = 0;
        self.tape = Some(c64_tape::TapePlayer::new(tape));
        self.apply_tape_motor();
    }

    /// Takes the tape out: the READ line goes back to idle-high and SENSE
    /// to "no button pressed".
    pub(crate) fn detach_tape(&mut self) {
        self.tape = None;
        self.tape_files.clear();
        self.tape_cursor = 0;
        self.cia1.set_flag(true);
    }

    /// The program a tape LOAD would find: the first one at or after the
    /// tape position whose name starts with `name` (the KERNAL compares only
    /// as many characters as the requested name has; an empty name takes
    /// whatever comes next). The tape is treated as rewinding on its own
    /// when nothing matches after the position. Moves the position past the
    /// program found.
    pub(crate) fn next_tape_program(&mut self, name: &[u8]) -> Option<(u16, Vec<u8>)> {
        let count = self.tape_files.len();
        let matches = |file: &c64_tape::rom_format::RomFile| {
            name.len() <= 16 && file.name[..name.len()] == *name
        };
        let found = (self.tape_cursor..count)
            .chain(0..self.tape_cursor.min(count))
            .find(|&i| matches(&self.tape_files[i]))?;
        self.tape_cursor = found + 1;
        let file = &self.tape_files[found];
        Some((file.start, file.data.clone()))
    }

    pub(crate) fn set_tape_speed(&mut self, multiplier: u32) {
        if let Some(tape) = &mut self.tape {
            tape.set_speed_multiplier(multiplier);
        }
    }

    /// Installs a disk image, replacing any already attached -- see
    /// `crate::C64::load_disk`/`load_disk_d71`/`load_disk_d81`.
    pub(crate) fn attach_disk(&mut self, disk: AttachedDisk) {
        self.disk = Some(disk);
        self.disk_modified = false;
    }

    /// Advances the currently loaded tape's playback by `cycles` real CPU
    /// cycles (a no-op if none installed or motor off, both handled
    /// inside `c64_tape::TapePlayer::tick` itself) and reflects every real
    /// READ-line transition onto CIA1's own FLAG pin -- see
    /// `c64_tape::TapePlayer::tick`'s own doc comment for why this loops
    /// over the transition count rather than a single before/after level
    /// comparison.
    pub(crate) fn tick_tape(&mut self, cycles: u32) {
        let Some(tape) = &mut self.tape else { return };
        let transitions = tape.tick(cycles);
        // Every `.tap` pulse ends in one falling edge on the READ line (the
        // time between falling edges is exactly what a pulse length is), so
        // each completed pulse is a low blip on FLAG. Toggling the level per
        // pulse instead would present only every second pulse as an edge to
        // the KERNAL, which then reads garbage.
        for _ in 0..transitions {
            self.cia1.set_flag(false);
            self.cia1.set_flag(true);
        }
    }

    /// One clock cycle of everything that runs beside the CPU, in cycle
    /// mode: the VIC-II (which owns the raster and the bus arbitration),
    /// tape, cartridge, both CIAs, the SID and the controllers. Interrupt
    /// requests are parked in `pending_irq`/`pending_nmi`.
    /// Feeds CIA2's interrupt level to the NMI edge detector; `true` on the
    /// transition from released to asserted.
    fn cia2_nmi_edge(&mut self, level: bool) -> bool {
        let edge = level && !self.cia2_nmi_active;
        self.cia2_nmi_active = level;
        edge
    }

    fn tick_cycle(&mut self) {
        let base = self.vic_bank_base();
        let result = {
            let view = VicView {
                ram: &self.ram[base..base + 0x4000],
                base,
                char_rom: &self.char_rom,
                color_ram: &self.color_ram,
                ultimax: self.cartridge.as_ref().filter(|c| c.vic_ultimax()),
            };
            self.vic.cycle(&view)
        };
        if result.raster_irq {
            self.pending_irq = true;
        }
        // Sprite collisions stay a per-line scan (see `advance_peripherals`).
        if let Some(line) = result.entered_line {
            let ultimax_copy = self.vic_ultimax_copy();
            let window = ultimax_copy
                .as_deref()
                .unwrap_or(&self.ram[base..base + 0x4000]);
            if self
                .vic
                .scan_sprite_collisions(line, window, &self.char_rom, &self.color_ram)
            {
                self.pending_irq = true;
            }
        }
        self.tick_tape(1);
        self.stamp += 1;
        if let Some((acia, _)) = &mut self.acia
            && acia.tick(1)
        {
            self.pending_nmi = true;
        }
        if let Some(cartridge) = &mut self.cartridge {
            cartridge.tick(1);
        }
        if self.cia1.tick(1) {
            self.pending_irq = true;
        }
        let cia2_level = self.cia2.tick(1);
        if self.cia2_nmi_edge(cia2_level) {
            self.pending_nmi = true;
        }
        self.sid.tick(1);
        self.controllers.tick(1);
        self.step_cycles += 1;
    }

    /// The clock cycle of one CPU bus access (cycle mode): run a cycle, and
    /// if the access is a read while BA is low, keep running cycles until BA
    /// rises -- the 6510's RDY line halts it on a read, but never on a
    /// write, which is why the VIC lowers BA three cycles before it needs
    /// the bus (no instruction writes more than three times in a row).
    fn cpu_cycle(&mut self, write: bool) {
        self.tick_cycle();
        while !write && self.vic.ba_low() {
            self.stalled_cycles += 1;
            self.tick_cycle();
        }
    }

    /// Runs the REU's programmed transfer against this bus and books the
    /// cycles it took for `C64::step` to charge.
    fn run_reu(&mut self) {
        let Some(mut reu) = self.reu.take() else {
            return;
        };
        // The REU moves bytes through `read`/`write`; those are not CPU
        // cycles (the DMA time is charged separately below).
        let was_cpu = std::mem::replace(&mut self.in_cpu, false);
        let cycles = reu.run(self);
        self.in_cpu = was_cpu;
        self.reu = Some(reu);
        self.dma_cycles = self.dma_cycles.saturating_add(cycles);
    }

    /// Does `addr` fall in the I/O window of the fitted serial cartridge?
    fn acia_hit(&self, addr: u16) -> bool {
        self.acia.as_ref().is_some_and(|(a, page)| {
            (addr >> 8) as u8 == *page
                && (addr & 0xFF)
                    < if a.mode() == acia6551::Mode::Turbo232 {
                        8
                    } else {
                        4
                    }
        })
    }

    /// Does `addr` fall in the I/O window of the fitted ethernet cartridge?
    fn rrnet_hit(&self, addr: u16) -> bool {
        self.rrnet
            .as_ref()
            .is_some_and(|r| (addr >> 8) as u8 == r.page && (addr & 0xFF) < 0x10)
    }

    /// True while something occupies the expansion port.
    pub(crate) fn expansion_taken(&self) -> bool {
        self.cartridge.is_some()
            || self.reu.is_some()
            || self.georam.is_some()
            || self.acia.is_some()
            || self.rrnet.is_some()
    }

    /// The PLA configuration index: the expansion-port lines on top of the
    /// 6510 port's LORAM/HIRAM/CHAREN. Without a cartridge both lines are
    /// inactive and the index is `0..=7`, the classic C64 map.
    fn pla_cfg(&self) -> u32 {
        let (loram, hiram, charen) = self.banking();
        let (exrom, game) = self
            .cartridge
            .as_ref()
            .map_or((false, false), crate::cartridge::Cartridge::lines);
        u32::from(loram)
            | u32::from(hiram) << 1
            | u32::from(charen) << 2
            | u32::from(exrom) << 3
            | u32::from(game) << 4
    }

    pub(crate) fn banking(&self) -> (bool, bool, bool) {
        let p = self.port01();
        (p & 0x01 != 0, p & 0x02 != 0, p & 0x04 != 0) // (loram, hiram, charen)
    }

    /// Real hardware fact: CIA2 Port A bits 0-1 (`$DD00`) select which 16K
    /// bank of the CPU's 64K address space the VIC-II reads video
    /// matrix/character/bitmap/sprite data from -- an *inverted* 2-bit
    /// field (`0b11` = bank 0 `$0000`, `0b10` = bank 1 `$4000`, `0b01` =
    /// bank 2 `$8000`, `0b00` = bank 3 `$C000`). With both CIA2 DDR/port
    /// registers starting at `0` (real post-reset state), every Port A bit
    /// floats high via `effective_port_a`'s undriven-pin convention, giving
    /// `0b11` -- bank 0 -- exactly the bank every pre-existing test/boot
    /// path here already assumed before this bank-select wiring existed.
    /// Confirmed against the commonly-cited C64 Programmer's Reference
    /// Guide's own memory map chapter (see `CREDITS.md`).
    ///
    /// `vic_ii::Vic::render`'s own internal offsets
    /// (`video_matrix_base`/`bitmap_base`/the sprite-pointer table) are all
    /// already bank-relative (never past `$3FFF`) -- they were only ever
    /// bank-0-hardcoded because this bus always handed `render` a full 64K
    /// slice starting at absolute `$0000`. Handing it a real bank-relative
    /// 16K window instead (see `render_frame` below) is the entire fix;
    /// `vic_ii` itself needed no change.
    /// The VIC-II's 16K when the cartridge drives an Ultimax map: the
    /// selected RAM bank with the cartridge's ROMH showing at `$3000-$3FFF`
    /// (the VIC sees the expansion port there, not RAM). `None` otherwise.
    pub(crate) fn vic_ultimax_copy(&self) -> Option<Vec<u8>> {
        let cart = self.cartridge.as_ref().filter(|c| c.vic_ultimax())?;
        let base = self.vic_bank_base();
        let mut copy = self.ram[base..base + 0x4000].to_vec();
        for (i, byte) in copy[0x3000..].iter_mut().enumerate() {
            *byte = cart.vic_window(i);
        }
        Some(copy)
    }

    /// Re-derive whether the VIC currently sees the character-ROM shadow:
    /// real hardware maps the ROM at `$1000-$1FFF` of banks 0 and 2 only.
    /// Called whenever CIA2's port A (the bank bits) may have changed.
    pub(crate) fn sync_vic_chargen_shadow(&mut self) {
        let base = self.vic_bank_base();
        self.vic
            .set_chargen_shadow(base == 0x0000 || base == 0x8000);
    }

    pub(crate) fn vic_bank_base(&self) -> usize {
        let bits = self.cia2.effective_port_a() & 0x03;
        (3 - bits as usize) * 0x4000
    }

    /// CIA2 Port A (`$DD00`) read, with the real IEC bus wired into bits
    /// 6/7: driven bits (VIC bank/RS232/ATN-CLOCK-DATA-OUT, whichever of
    /// those `ddr_a` currently configures as output) reflect what was
    /// written, same `effective_port_a` convention every other CIA/VIA
    /// port here already uses; bits 6/7 specifically, *when left as
    /// input* (the real KERNAL's own configuration), reflect the actual
    /// external bus level (`iec_clock_in`/`iec_data_in`) instead of just
    /// floating high -- same "host wires the chip-specific bits" split
    /// `keyboard_port_b` already established for CIA1's own Port B.
    /// Pure/non-mutating -- safe for both `io_read` and `io_peek` to
    /// share.
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

    /// What the C64 is currently asserting onto the 3 IEC lines --
    /// `(atn, clock, data)`. "Asserting" means driven-as-output (`ddr_a`
    /// bit set) *and* the corresponding bit set to `1` (real hardware's
    /// own inverted-driver convention -- see `CIA2_PA_ATN_OUT`'s doc
    /// comment).
    pub(crate) fn iec_output(&self) -> (bool, bool, bool) {
        let driven = self.cia2.port_a & self.cia2.ddr_a;
        (
            driven & CIA2_PA_ATN_OUT != 0,
            driven & CIA2_PA_CLOCK_OUT != 0,
            driven & CIA2_PA_DATA_OUT != 0,
        )
    }

    /// Feeds the IEC bus's own currently computed `CLOCK`/`DATA` levels
    /// back into this bus -- the other half of the real bus's round trip.
    /// This crate has no `IecBus` type of its own; `c1541::IecBus` (see
    /// its own doc comment for why it lives there) is what actually
    /// computes these from this bus's own `iec_output` combined with the
    /// drive's.
    pub(crate) fn set_iec_input(&mut self, clock: bool, data: bool) {
        self.iec_clock_in = clock;
        self.iec_data_in = data;
    }

    fn io_read(&mut self, addr: u16) -> u8 {
        match addr {
            // A CPU read of $D01E/$D01F clears those collision latches.
            0xD000..=0xD3FF => self.vic.read_clearing((addr & 0x3F) as u8),
            // Real SID hardware only decodes 5 address lines, so its
            // 25-register map mirrors every 32 bytes across the whole
            // $D400-$D7FF window.
            0xD400..=0xD7FF => self.sid_read((addr & 0x1F) as u8),
            0xD800..=0xDBFF => self.color_ram[(addr - 0xD800) as usize],
            0xDC00..=0xDCFF if (addr & 0x0F) == 1 => self.keyboard_port_b(),
            0xDC00..=0xDCFF if (addr & 0x0F) == 0 => self.joystick_port_a(),
            0xDC00..=0xDCFF => self.cia1.read((addr & 0x0F) as u8),
            0xDD00..=0xDDFF if (addr & 0x0F) == 0 => self.cia2_port_a_read(),
            0xDD00..=0xDDFF => {
                let value = self.cia2.read((addr & 0x0F) as u8);
                // Reading the ICR releases the line at once; a source that
                // asserts it again before the next step ends is a new edge.
                if (addr & 0x0F) == 0x0D {
                    self.cia2_nmi_active = self.cia2.interrupt_asserted();
                }
                value
            }
            // Cartridge I/O1/I/O2; with nothing driving the bus it floats.
            0xDE00..=0xDFFF if self.acia_hit(addr) => self
                .acia
                .as_mut()
                .map_or(0xFF, |(a, _)| a.read((addr & 0xFF) as u8)),
            0xDE00..=0xDFFF if self.rrnet_hit(addr) => self
                .rrnet
                .as_mut()
                .map_or(0xFF, |r| r.read((addr & 0xFF) as u8)),
            0xDE00..=0xDEFF if self.georam.is_some() => {
                self.georam.as_ref().map_or(0xFF, |g| g.read_window(addr))
            }
            0xDE00..=0xDEFF => self
                .cartridge
                .as_mut()
                .and_then(|cart| cart.io1_read(addr))
                .unwrap_or(0xFF),
            0xDF00..=0xDFFF if self.georam.is_some() => {
                self.georam.as_ref().map_or(0xFF, |g| g.read_register(addr))
            }
            0xDF00..=0xDFFF => {
                if let Some(reu) = &mut self.reu {
                    reu.read(addr & 0x1F)
                } else {
                    self.cartridge
                        .as_mut()
                        .and_then(|cart| cart.io2_read(addr))
                        .unwrap_or(0xFF)
                }
            }
            _ => 0xFF,
        }
    }

    fn io_peek(&self, addr: u16) -> u8 {
        match addr {
            0xD000..=0xD3FF => self.vic.read((addr & 0x3F) as u8),
            0xD400..=0xD7FF => self.sid_read((addr & 0x1F) as u8),
            0xD800..=0xDBFF => self.color_ram[(addr - 0xD800) as usize],
            0xDC00..=0xDCFF if (addr & 0x0F) == 1 => self.keyboard_port_b(),
            0xDC00..=0xDCFF if (addr & 0x0F) == 0 => self.joystick_port_a(),
            0xDC00..=0xDCFF => self.cia1.peek((addr & 0x0F) as u8),
            0xDD00..=0xDDFF if (addr & 0x0F) == 0 => self.cia2_port_a_read(),
            0xDD00..=0xDDFF => self.cia2.peek((addr & 0x0F) as u8),
            0xDE00..=0xDFFF if self.acia_hit(addr) => self
                .acia
                .as_ref()
                .map_or(0xFF, |(a, _)| a.peek((addr & 0xFF) as u8)),
            0xDE00..=0xDFFF if self.rrnet_hit(addr) => self
                .rrnet
                .as_ref()
                .map_or(0xFF, |r| r.peek((addr & 0xFF) as u8)),
            0xDE00..=0xDEFF if self.georam.is_some() => {
                self.georam.as_ref().map_or(0xFF, |g| g.read_window(addr))
            }
            0xDF00..=0xDFFF if self.georam.is_some() => {
                self.georam.as_ref().map_or(0xFF, |g| g.read_register(addr))
            }
            // Peeks must not trigger a cartridge's read side effects.
            0xDF00..=0xDFFF => {
                if let Some(reu) = &self.reu {
                    reu.peek(addr & 0x1F)
                } else {
                    self.cartridge
                        .as_ref()
                        .and_then(|cart| cart.io2_peek(addr))
                        .unwrap_or(0xFF)
                }
            }
            _ => 0xFF,
        }
    }

    fn io_write(&mut self, addr: u16, val: u8) {
        match addr {
            0xD000..=0xD3FF => self.vic.write((addr & 0x3F) as u8, val),
            0xD400..=0xD7FF => self.sid.write((addr & 0x1F) as u8, val),
            0xD800..=0xDBFF => self.color_ram[(addr - 0xD800) as usize] = val & 0x0F,
            0xDC00..=0xDCFF => self.cia1.write((addr & 0x0F) as u8, val),
            0xDD00..=0xDDFF => {
                self.cia2.write((addr & 0x0F) as u8, val);
                self.sync_vic_chargen_shadow();
            }
            0xDE00..=0xDFFF if self.acia_hit(addr) => {
                if let Some((a, _)) = &mut self.acia {
                    a.write((addr & 0xFF) as u8, val);
                }
            }
            0xDE00..=0xDFFF if self.rrnet_hit(addr) => {
                if let Some(r) = &mut self.rrnet {
                    r.write((addr & 0xFF) as u8, val);
                }
            }
            0xDE00..=0xDEFF => {
                if let Some(georam) = &mut self.georam {
                    georam.write_window(addr, val);
                } else if let Some(cart) = &mut self.cartridge {
                    cart.io1_write(addr, val);
                }
            }
            0xDF00..=0xDFFF => {
                if let Some(georam) = &mut self.georam {
                    georam.write_register(addr, val);
                } else if let Some(reu) = &mut self.reu {
                    if reu.write(addr & 0x1F, val) {
                        self.run_reu();
                    }
                } else if let Some(cart) = &mut self.cartridge {
                    cart.io2_write(addr, val);
                }
            }
            _ => {}
        }
    }

    /// Pure read with no side effects (doesn't clear a CIA's pending ICR
    /// flags the way a real CPU-driven read of `$DC0D` would) -- for
    /// `Machine::read_memory`'s "for display purposes" contract, same
    /// role `zx_spectrum::bus::ZxSpectrumBus::peek` plays there.
    pub(crate) fn peek(&self, addr: u16) -> u8 {
        match addr {
            0x0000 => self.port_ddr,
            0x0001 => self.port01(),
            _ => {
                let cfg = self.pla_cfg();
                self.mapped_read(addr, cfg)
            }
        }
    }

    /// One side-effect-free memory read through the PLA map; `read` layers
    /// the real CPU's side effects (chip registers, ROML reactions) on top.
    fn mapped_read(&self, addr: u16, cfg: u32) -> u8 {
        let has = |mask: u32| mask >> cfg & 1 != 0;
        let ultimax = has(PLA_ULTIMAX);
        if addr >= 0x8000
            && let Some(v) = self.cartridge.as_ref().and_then(|c| c.overlay_read(addr))
        {
            return v;
        }
        match addr {
            0x8000..=0x9FFF if has(PLA_ROML) => {
                self.cartridge.as_ref().map_or(0xFF, |c| c.peek_roml(addr))
            }
            0xA000..=0xBFFF if has(PLA_ROMH_A000) => {
                self.cartridge.as_ref().map_or(0xFF, |c| c.read_romh(addr))
            }
            0xA000..=0xBFFF if has(PLA_BASIC) => self.basic_rom[(addr - 0xA000) as usize],
            0xD000..=0xDFFF if has(PLA_IO) => self.io_peek(addr),
            0xD000..=0xDFFF if has(PLA_CHARGEN) => self.char_rom[(addr - 0xD000) as usize],
            0xE000..=0xFFFF if has(PLA_ROMH_E000) => {
                self.cartridge.as_ref().map_or(0xFF, |c| c.read_romh(addr))
            }
            0xE000..=0xFFFF if has(PLA_KERNAL) => self.kernal_rom[(addr - 0xE000) as usize],
            // Ultimax leaves these open: no RAM there.
            0x1000..=0x7FFF | 0xA000..=0xCFFF if ultimax => 0xFF,
            _ => self.ram[addr as usize],
        }
    }
}

impl Bus for C64Bus {
    // `read_io`/`write_io` are left at `machine::Bus`'s defaults -- the
    // 6502 has no separate IO-port space, everything above is
    // memory-mapped and goes through `read`/`write` alone. This is the
    // actual real-hardware-quirk confirmation the M2 plan called out:
    // the 6510-port banking logic below is entirely a `C64Bus`-level
    // address-decode concern, `mos6502::Cpu` needs no awareness of it.

    fn dummy_cycle(&mut self, _addr: u16, write: bool, _value: u8) {
        if self.in_cpu {
            self.cpu_cycle(write);
        }
    }

    fn read(&mut self, addr: u16) -> u8 {
        if self.in_cpu {
            self.cpu_cycle(false);
        }
        match addr {
            0x0000 => self.port_ddr,
            0x0001 => self.port01(),
            _ => {
                let cfg = self.pla_cfg();
                let has = |mask: u32| mask >> cfg & 1 != 0;
                match addr {
                    // A CPU read of ROML may have side effects (Zaxxon's bank
                    // select, Epyx's capacitor).
                    0x8000..=0xFFFF
                        if self
                            .cartridge
                            .as_ref()
                            .is_some_and(|c| c.overlay_read(addr).is_some()) =>
                    {
                        self.cartridge
                            .as_ref()
                            .and_then(|c| c.overlay_read(addr))
                            .unwrap_or(0xFF)
                    }
                    0x8000..=0x9FFF if has(PLA_ROML) => self
                        .cartridge
                        .as_mut()
                        .map_or(0xFF, |cart| cart.read_roml(addr)),
                    0xD000..=0xDFFF if has(PLA_IO) => self.io_read(addr),
                    // The ROMH windows can be busy flash (EasyFlash).
                    0xA000..=0xBFFF if has(PLA_ROMH_A000) => self
                        .cartridge
                        .as_mut()
                        .map_or(0xFF, |cart| cart.cpu_read_romh(addr)),
                    0xE000..=0xFFFF if has(PLA_ROMH_E000) => self
                        .cartridge
                        .as_mut()
                        .map_or(0xFF, |cart| cart.cpu_read_romh(addr)),
                    _ => self.mapped_read(addr, cfg),
                }
            }
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        if self.in_cpu {
            self.cpu_cycle(true);
        }
        match addr {
            0x0000 => {
                self.port_ddr = val;
                self.apply_tape_motor();
            }
            0x0001 => {
                self.port_data = val;
                self.apply_tape_motor();
            }
            _ => {
                let cfg = self.pla_cfg();
                let has = |mask: u32| mask >> cfg & 1 != 0;
                if matches!(addr, 0x8000..=0xBFFF | 0xE000..=0xFFFF) {
                    let visible = match addr {
                        0x8000..=0x9FFF => has(PLA_ROML),
                        0xA000..=0xBFFF => has(PLA_ROMH_A000),
                        _ => has(PLA_ROMH_E000),
                    };
                    let ultimax = has(PLA_ULTIMAX);
                    if let Some(cart) = &mut self.cartridge
                        && cart.write_hook(addr, val, visible, ultimax)
                    {
                        return;
                    }
                }
                if (0xD000..=0xDFFF).contains(&addr) && has(PLA_IO) {
                    self.io_write(addr, val);
                } else if has(PLA_ULTIMAX) {
                    // Ultimax: RAM exists only below $1000; everything else
                    // (ROML, ROMH, open space) swallows the write.
                    if addr < 0x1000 {
                        self.ram[addr as usize] = val;
                    }
                } else {
                    // ROM windows (BASIC, KERNAL, character ROM, ROML, ROMH)
                    // are read-only views over RAM that still takes the write.
                    self.ram[addr as usize] = val;
                    // An REU command armed without "FF00 disable" starts when
                    // the CPU writes $FF00.
                    if addr == 0xFF00 && self.reu.as_ref().is_some_and(crate::reu::Reu::armed) {
                        self.run_reu();
                    }
                }
            }
        }
    }
}

/// What the VIC-II sees in cycle mode: the 16K bank CIA2 selects, with the
/// character ROM shadowing `$1000-$1FFF` in banks 0 and 2 and a cartridge's
/// ROMH showing at `$3000-$3FFF` in the Ultimax configuration.
struct VicView<'a> {
    ram: &'a [u8],
    base: usize,
    char_rom: &'a [u8],
    color_ram: &'a [u8],
    ultimax: Option<&'a crate::cartridge::Cartridge>,
}

impl vic_ii::VicMemory for VicView<'_> {
    fn vic_read(&self, addr: u16) -> u8 {
        let a = usize::from(addr & 0x3FFF);
        if (self.base == 0x0000 || self.base == 0x8000) && (0x1000..0x2000).contains(&a) {
            return self.char_rom[a - 0x1000];
        }
        match self.ultimax {
            Some(cart) if a >= 0x3000 => cart.vic_window(a - 0x3000),
            _ => self.ram[a],
        }
    }

    fn color_ram(&self, index: u16) -> u8 {
        self.color_ram[usize::from(index) % self.color_ram.len()]
    }
}

impl crate::reu::DmaBus for C64Bus {
    fn dma_read(&mut self, addr: u16) -> u8 {
        self.read(addr)
    }

    fn dma_write(&mut self, addr: u16, value: u8) {
        self.write(addr, value);
    }
}

/// A C64 control-port joystick switch; the value is its bit in the CIA1 port
/// (pressed pulls that bit low).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JoystickButton {
    Up = 0x01,
    Down = 0x02,
    Left = 0x04,
    Right = 0x08,
    Fire = 0x10,
}

pub use vic_ii::VicAccuracy;

#[derive(Clone)]
pub struct C64 {
    pub(crate) cpu: mos6502::Cpu,
    pub(crate) bus: C64Bus,
    /// Real T-states-until-next-sample accumulator, same shape as
    /// `zx_spectrum::ZxSpectrum`'s own field of the same name -- `f64` so
    /// its fractional remainder survives across steps rather than
    /// drifting the effective sample rate flat. No separate chip-clock
    /// divider is needed here the way `zx_spectrum` needs one for its
    /// AY-3-8910: a real SID runs at the same clock as the 6510 it sits
    /// beside, so `sid.tick` gets this step's own cycle count directly.
    pub(crate) audio_cycle_accumulator: f64,
    /// Samples generated since the last `drain_audio` call, at
    /// `machine::AUDIO_SAMPLE_RATE`.
    pub(crate) audio_buffer: Vec<f32>,
}

impl C64 {
    /// Selects the video standard: PAL (default) or NTSC. Japan is NTSC.
    /// Changes the VIC-II raster geometry (65 x 263 instead of 63 x 312), the
    /// machine clock and frame rate, and the CIAs' TOD divider. The KERNAL
    /// detects the standard itself at boot (it times the raster), so the
    /// same ROM image works for both; call before running.
    pub fn set_region(&mut self, region: machine::Region) {
        let ntsc = !matches!(region, machine::Region::Pal);
        self.bus.vic.set_ntsc(ntsc);
        self.bus.cia1.set_ntsc(ntsc);
        self.bus.cia2.set_ntsc(ntsc);
        let hz = self.clock_hz() as u32;
        if let Some((acia, _)) = &mut self.bus.acia {
            acia.set_clock(hz);
        }
    }

    /// `reset()` immediately, matching real hardware: the reset vector is
    /// read from KERNAL ROM right away, since default (all-zero)
    /// `port_ddr`/`port_data` already banks KERNAL in (see `port01`'s own
    /// doc comment) -- no separate "power on then reset" step needed.
    pub fn new(kernal_rom: Vec<u8>, basic_rom: Vec<u8>, char_rom: Vec<u8>) -> Result<Self, String> {
        if kernal_rom.len() != KERNAL_ROM_SIZE {
            return Err(format!(
                "KERNAL ROM must be {KERNAL_ROM_SIZE} bytes, got {}",
                kernal_rom.len()
            ));
        }
        if basic_rom.len() != BASIC_ROM_SIZE {
            return Err(format!(
                "BASIC ROM must be {BASIC_ROM_SIZE} bytes, got {}",
                basic_rom.len()
            ));
        }
        if char_rom.len() != CHAR_ROM_SIZE {
            return Err(format!(
                "character ROM must be {CHAR_ROM_SIZE} bytes, got {}",
                char_rom.len()
            ));
        }

        let mut bus = C64Bus::new(kernal_rom, basic_rom, char_rom);
        let mut cpu = mos6502::Cpu::new();
        cpu.reset(&mut bus);
        Ok(Self {
            cpu,
            bus,
            audio_cycle_accumulator: 0.0,
            audio_buffer: Vec::new(),
        })
    }

    /// Human-readable summary of the current 6510 I/O-port memory banking
    /// (register `$01`'s real LORAM/HIRAM/CHAREN lines) for debug-overlay
    /// display -- see `C64Bus::banking`'s own doc comment for what each
    /// bit means on real hardware. Always `Some` -- C64 has no "no paging"
    /// state the way 48K ZX Spectrum does.
    pub fn overlay_memory_summary(&self) -> Option<String> {
        let (loram, hiram, charen) = self.bus.banking();
        Some(format!(
            "LORAM={} HIRAM={} CHAREN={}",
            u8::from(loram),
            u8::from(hiram),
            u8::from(charen)
        ))
    }

    pub fn press_key(&mut self, key: crate::keyboard::C64Key) {
        self.bus.keyboard.press(key);
    }

    pub fn release_key(&mut self, key: crate::keyboard::C64Key) {
        self.bus.keyboard.release(key);
    }

    /// Holds a joystick direction/fire down on a control port (`port` 1 or
    /// 2; anything else is ignored).
    pub fn press_joystick(&mut self, port: u8, button: JoystickButton) {
        if let Some(bits) = self.bus.joystick.get_mut(usize::from(port).wrapping_sub(1)) {
            *bits |= button as u8;
        }
    }

    /// Chooses what the host mouse drives: nothing, a 1351 proportional mouse
    /// or a pair of paddles, in control port 1 or 2.
    pub fn set_mouse_mode(&mut self, mode: crate::controllers::MouseMode) {
        self.bus.controllers.set_mode(mode);
    }

    /// Fits a 6581 (default) or an 8580 SID.
    pub fn set_sid_model(&mut self, model: sid::SidModel) {
        self.bus.sid.set_model(model);
    }

    pub fn sid_model(&self) -> sid::SidModel {
        self.bus.sid.model()
    }

    pub fn mouse_motion(&mut self, dx: i32, dy: i32) {
        self.bus.controllers.mouse_motion(dx, dy);
    }

    pub fn mouse_button(&mut self, right: bool, pressed: bool) {
        self.bus.controllers.mouse_button(right, pressed);
        // A light pen's switch also fires the VIC-II's latch at the spot it
        // points at.
        if !right
            && pressed
            && let Some((x, y)) = self.bus.controllers.pen_position()
            && self.bus.vic.light_pen_trigger(x, y)
        {
            self.cpu.request_irq();
        }
    }

    /// Turns paddle `paddle` (0 = A on POTX, 1 = B on POTY) of control port
    /// `port` one way while held, `None` to let go.
    pub fn paddle_turn(
        &mut self,
        port: u8,
        paddle: u8,
        direction: Option<crate::controllers::PaddleDirection>,
    ) {
        self.bus.controllers.paddle_turn(port, paddle, direction);
    }

    pub fn paddle_fire(&mut self, port: u8, paddle: u8, pressed: bool) {
        self.bus.controllers.paddle_fire(port, paddle, pressed);
    }

    pub fn release_joystick(&mut self, port: u8, button: JoystickButton) {
        if let Some(bits) = self.bus.joystick.get_mut(usize::from(port).wrapping_sub(1)) {
            *bits &= !(button as u8);
        }
    }

    /// What this C64 is currently asserting onto the 3 IEC lines --
    /// `(atn, clock, data)`. See `C64Bus::iec_output`'s own doc comment
    /// for the real hardware convention this reflects.
    pub fn iec_output(&self) -> (bool, bool, bool) {
        self.bus.iec_output()
    }

    /// Feeds this C64 the IEC bus's own currently computed `CLOCK`/`DATA`
    /// line levels (there's no `ATN` input on the C64 side at all -- see
    /// `CIA2_PA_ATN_OUT`'s own doc comment). Nothing in this codebase
    /// calls this continuously yet (no dual-machine stepping loop
    /// exists) -- see `tests/iec_1541.rs` for how the full round trip
    /// between this and a real `c1541::C1541` is meant to be driven.
    pub fn set_iec_input(&mut self, clock: bool, data: bool) {
        self.bus.set_iec_input(clock, data);
    }

    /// Installs a real `.tap` cassette tape for loading, replacing any
    /// tape already installed -- see `C64Bus`'s own top-level doc comment
    /// for the real cassette hardware this wires into (CIA1's FLAG pin,
    /// the 6510 port's own MOTOR/SENSE bits). Real hardware-format `.t64`
    /// containers don't go through this at all (see `c64-tape`'s own doc
    /// comment for why) -- a `.t64`'s extracted program is instead poked
    /// directly via the already-generic `Machine::load`, same as `.prg`.
    pub fn load_tape(&mut self, tape: c64_tape::Tape) {
        self.bus.attach_tape(tape);
    }

    /// Plugs a cartridge into the expansion port and resets the machine, as
    /// switching a real C64 on with one fitted does: the KERNAL's reset
    /// routine looks for the `CBM80` signature at `$8004` and starts the
    /// cartridge, and an Ultimax cartridge supplies the reset vector itself.
    pub fn load_cartridge(&mut self, cartridge: crate::cartridge::Cartridge) {
        // The expansion port holds one thing: a cartridge replaces an REU.
        self.bus.reu = None;
        self.bus.georam = None;
        self.bus.acia = None;
        self.bus.rrnet = None;
        self.bus.cartridge = Some(cartridge);
        self.reset();
    }

    /// Parses a `.crt` image and plugs it in -- see `load_cartridge`.
    pub fn load_cartridge_crt(&mut self, bytes: &[u8]) -> Result<(), String> {
        let crt = c64_cartridge::parse(bytes).map_err(|e| format!("invalid .crt: {e}"))?;
        let cartridge = crate::cartridge::Cartridge::from_crt(&crt)?;
        self.load_cartridge(cartridge);
        Ok(())
    }

    /// Everything that runs alongside the CPU for `cycles` clock cycles: the
    /// raster (and the per-line sprite collision scan), tape, cartridge
    /// timers, both CIAs, the SID and the audio sample accumulator. Returns
    /// the cycles to charge, which for an instruction includes the VIC's
    /// bad-line steal (`steal` false during DMA, when the CPU is stalled
    /// anyway).
    fn advance_peripherals(&mut self, cycles: u32, steal: bool) -> u32 {
        let vic_result = self.bus.vic.tick(cycles);
        if vic_result.raster_irq {
            self.cpu.request_irq();
        }
        // Sprite collisions are a per-raster-line event: scan each line as
        // the raster enters it, against the 16K bank the VIC sees now.
        if let Some(line) = vic_result.entered_line {
            let ultimax_copy = self.bus.vic_ultimax_copy();
            let bank = self.bus.vic_bank_base();
            let bus = &mut self.bus;
            let window = ultimax_copy
                .as_deref()
                .unwrap_or(&bus.ram[bank..bank + 0x4000]);
            if bus
                .vic
                .scan_sprite_collisions(line, window, &bus.char_rom, &bus.color_ram)
            {
                self.cpu.request_irq();
            }
        }

        // Real hardware: a bad line's CPU cycle stealing only stalls the
        // CPU -- CIA1/SID keep running off the same phi2 clock the whole
        // time, and `Machine::step`'s own "cost in clock cycles" contract
        // means the caller's real-time pacing budget must see the stolen
        // time too. The VIC-II's own raster generator does not see it
        // (already excluded inside `Vic::tick` -- see its own doc
        // comment for why).
        let total_cycles = cycles + if steal { vic_result.stolen_cycles } else { 0 };

        // Ticked (and its real READ-line transitions applied to CIA1's own
        // FLAG pin, if any) before CIA1's own timer tick below, so a
        // same-step FLAG-enabled interrupt is visible to that same call's
        // own ICR check -- same ordering fix `c1541::C1541::step` already
        // established for its own byte-ready-before-VIA2-tick case.
        self.bus.tick_tape(total_cycles);
        self.bus.stamp += u64::from(total_cycles);
        // The serial cartridge's interrupt output is wired to NMI.
        if let Some((acia, _)) = &mut self.bus.acia
            && acia.tick(total_cycles)
        {
            self.cpu.request_nmi();
        }
        if let Some(cartridge) = &mut self.bus.cartridge {
            cartridge.tick(total_cycles);
        }
        if self.bus.cia1.tick(total_cycles as u8) {
            self.cpu.request_irq();
        }
        // Real hardware fact: CIA2's IRQ output is wired to the 6510's NMI
        // pin, not its IRQ pin (CIA1 gets the real IRQ pin instead) -- the
        // KERNAL relies on this to keep RS-232/serial-bus timing responsive
        // even while the CPU has interrupts masked off. CIA2 was a pure
        // register-storage stub until this pass (never ticked at all), so
        // its timers/TOD/serial register never actually ran; this is the
        // real fix, not just documentation.
        let cia2_level = self.bus.cia2.tick(total_cycles as u8);
        if self.bus.cia2_nmi_edge(cia2_level) {
            self.cpu.request_nmi();
        }

        self.bus.sid.tick(total_cycles);
        self.bus.controllers.tick(total_cycles);

        self.accumulate_audio(total_cycles);

        total_cycles
    }

    /// Turns elapsed clock cycles into audio samples.
    fn accumulate_audio(&mut self, cycles: u32) {
        self.audio_cycle_accumulator += cycles as f64;
        let cycles_per_sample = self.clock_hz() as f64 / machine::AUDIO_SAMPLE_RATE as f64;
        while self.audio_cycle_accumulator >= cycles_per_sample {
            self.audio_cycle_accumulator -= cycles_per_sample;
            let sample = self.mix_audio_sample();
            self.audio_buffer.push(sample);
        }
    }

    /// Chooses line accuracy (default) or the cycle-accurate VIC-II. Cycle
    /// mode is PAL only and is refused on an NTSC machine.
    pub fn set_vic_accuracy(&mut self, accuracy: VicAccuracy) -> Result<(), String> {
        match accuracy {
            VicAccuracy::Cycle => {
                self.bus.vic.enable_cycle_engine()?;
                self.bus.cycle_mode = true;
            }
            VicAccuracy::Line => {
                self.bus.vic.disable_cycle_engine();
                self.bus.cycle_mode = false;
            }
        }
        Ok(())
    }

    pub fn vic_accuracy(&self) -> VicAccuracy {
        if self.bus.cycle_mode {
            VicAccuracy::Cycle
        } else {
            VicAccuracy::Line
        }
    }

    /// Clock cycles the CPU has spent waiting for the VIC-II's BA line
    /// (cycle mode; always 0 otherwise).
    pub fn ba_stalled_cycles(&self) -> u64 {
        self.bus.stalled_cycles
    }

    /// `Machine::step` in line mode: one instruction, then everything else
    /// catches up by the cycles it took (plus the VIC's bad-line steal).
    fn step_line_mode(&mut self) -> u32 {
        let cpu_cycles = if self.try_fast_kernal_load_trap()
            || self.try_fast_kernal_save_trap()
            || self.try_printer_trap()
        {
            // Near-zero: the entire point is collapsing what would be a
            // real IEC-bus byte-by-byte transfer into effectively nothing,
            // same shape `zx_spectrum`'s own fast tape trap already uses.
            6
        } else {
            self.cpu.step(&mut self.bus)
        };
        let mut total = self.advance_peripherals(cpu_cycles, true);

        // An REU transfer the instruction just started holds the CPU off the
        // bus for its whole length while everything else keeps running, so
        // the cycles are fed through in line-sized pieces (the VIC raster and
        // the CIA timers must not skip past events).
        let mut dma = std::mem::take(&mut self.bus.dma_cycles);
        while dma > 0 {
            let chunk = dma.min(DMA_CHUNK_CYCLES);
            dma -= chunk;
            total += self.advance_peripherals(chunk, false);
        }
        total
    }

    /// `Machine::step` in cycle mode: the CPU runs with `in_cpu` set so each
    /// of its bus cycles advances the machine (and may stall on BA); the
    /// cost is the number of clock cycles that actually passed.
    fn step_cycle_mode(&mut self) -> u32 {
        self.bus.step_cycles = 0;
        if self.try_fast_kernal_load_trap()
            || self.try_fast_kernal_save_trap()
            || self.try_printer_trap()
        {
            // A trap replaces a whole serial transfer by about six cycles.
            for _ in 0..6 {
                self.bus.tick_cycle();
            }
        } else {
            self.bus.in_cpu = true;
            self.cpu.step(&mut self.bus);
            self.bus.in_cpu = false;
        }
        // An REU transfer holds the CPU off the bus while the rest runs.
        let dma = std::mem::take(&mut self.bus.dma_cycles);
        for _ in 0..dma {
            self.bus.tick_cycle();
        }
        if std::mem::take(&mut self.bus.pending_irq) {
            self.cpu.request_irq();
        }
        if std::mem::take(&mut self.bus.pending_nmi) {
            self.cpu.request_nmi();
        }
        let cycles = self.bus.step_cycles;
        self.accumulate_audio(cycles);
        cycles
    }

    /// Fits an REU of 128, 256 or 512 KB (1700/1764/1750). Fails if a
    /// cartridge occupies the expansion port. The DRAM is cleared.
    pub fn attach_reu(&mut self, kilobytes: usize) -> Result<(), String> {
        if self.bus.expansion_taken() {
            return Err("the expansion port is already taken; an REU needs it".to_string());
        }
        self.bus.reu = Some(crate::reu::Reu::new(kilobytes)?);
        Ok(())
    }

    /// Fits a GeoRAM of 512, 1024, 2048 or 4096 KB. Fails if a cartridge or
    /// an REU occupies the expansion port.
    pub fn attach_georam(&mut self, kilobytes: usize) -> Result<(), String> {
        if self.bus.expansion_taken() {
            return Err("the expansion port is already taken; a GeoRAM needs it".to_string());
        }
        self.bus.georam = Some(crate::georam::GeoRam::new(kilobytes)?);
        Ok(())
    }

    pub fn georam_kilobytes(&self) -> Option<usize> {
        self.bus
            .georam
            .as_ref()
            .map(crate::georam::GeoRam::kilobytes)
    }

    /// Fits a SwiftLink (`turbo` false) or Turbo232 serial cartridge at the
    /// I/O page `$DE` or `$DF`. Bytes the program sends leave through
    /// [`C64::serial_take_tx`]; received ones go in with
    /// [`C64::serial_push_rx`].
    pub fn attach_serial(&mut self, turbo: bool, page: u8) -> Result<(), String> {
        if self.bus.expansion_taken() {
            return Err(
                "the expansion port is already taken; a serial cartridge needs it".to_string(),
            );
        }
        if page != 0xDE && page != 0xDF {
            return Err(format!(
                "serial cartridge page must be $DE or $DF, not ${page:02X}"
            ));
        }
        let mode = if turbo {
            acia6551::Mode::Turbo232
        } else {
            acia6551::Mode::SwiftLink
        };
        self.bus.acia = Some((acia6551::Acia::new(mode, self.clock_hz() as u32), page));
        Ok(())
    }

    pub fn serial_attached(&self) -> bool {
        self.bus.acia.is_some()
    }

    /// Bytes the program has transmitted through the serial cartridge.
    pub fn serial_take_tx(&mut self) -> Vec<u8> {
        self.bus
            .acia
            .as_mut()
            .map_or_else(Vec::new, |(a, _)| a.take_tx())
    }

    /// A byte arriving on the serial line.
    pub fn serial_push_rx(&mut self, byte: u8) {
        if let Some((a, _)) = &mut self.bus.acia {
            a.push_rx(byte);
        }
    }

    /// Bytes still queued for the ACIA to receive.
    pub fn serial_rx_backlog(&self) -> usize {
        self.bus.acia.as_ref().map_or(0, |(a, _)| a.rx_backlog())
    }

    /// Whether a peer is connected (the modem's DCD/DSR lines).
    pub fn serial_set_carrier(&mut self, present: bool) {
        if let Some((a, _)) = &mut self.bus.acia {
            a.set_carrier(present);
        }
    }

    /// Fits an RR-Net ethernet cartridge (CS8900A) at `$DE00`/`$DF00`. With
    /// `loopback` every frame the program sends is received back; without it
    /// frames go nowhere.
    pub fn attach_rrnet(&mut self, page: u8, loopback: bool) -> Result<(), String> {
        if self.bus.expansion_taken() {
            return Err("the expansion port is already taken; an RR-Net needs it".to_string());
        }
        if page != 0xDE && page != 0xDF {
            return Err(format!("RR-Net page must be $DE or $DF, not ${page:02X}"));
        }
        self.bus.rrnet = Some(crate::rrnet::RrNet::new(page, loopback));
        Ok(())
    }

    /// Frames the RR-Net program has transmitted so far.
    pub fn rrnet_frames_sent(&self) -> u64 {
        self.bus.rrnet.as_ref().map_or(0, |r| r.sent)
    }

    /// Turns the serial-bus text printer (device 4/5) on or off.
    pub fn set_printer(&mut self, enabled: bool) {
        if enabled {
            self.bus.printer.get_or_insert_with(Default::default);
        } else {
            self.bus.printer = None;
        }
    }

    /// What the printer has printed, as ASCII text; `None` if nothing was.
    pub fn printer_text(&self) -> Option<String> {
        self.bus
            .printer
            .as_ref()
            .filter(|p| !p.is_empty())
            .map(crate::printer::Printer::text)
    }

    pub fn reu_kilobytes(&self) -> Option<usize> {
        self.bus.reu.as_ref().map(crate::reu::Reu::kilobytes)
    }

    /// The cartridge's freeze button (Action Replay, KCS Power, Final
    /// Cartridge III, Retro Replay, Expert in its ON position). Returns
    /// whether the attached cartridge has one and accepted the press.
    pub fn freeze(&mut self) -> bool {
        let Some(cart) = &mut self.bus.cartridge else {
            return false;
        };
        if cart.freeze() {
            self.cpu.request_nmi();
            true
        } else {
            false
        }
    }

    /// True if the attached cartridge has a freezer button.
    pub fn has_freezer(&self) -> bool {
        self.bus
            .cartridge
            .as_ref()
            .is_some_and(crate::cartridge::Cartridge::has_freezer)
    }

    /// Moves an attached Expert cartridge's 3-way switch (0 off, 1 prg, 2 on)
    /// and resets, as flipping it on the real cartridge is followed by one.
    pub fn set_expert_switch(&mut self, position: u8) {
        if let Some(cart) = &mut self.bus.cartridge {
            cart.set_expert_mode(position);
            self.reset();
        }
    }

    /// True once code programmed or erased an EasyFlash.
    pub fn cartridge_modified(&self) -> bool {
        self.bus
            .cartridge
            .as_ref()
            .is_some_and(crate::cartridge::Cartridge::is_modified)
    }

    /// The EasyFlash contents as a `.crt` file, if that is what is attached.
    pub fn cartridge_image(&self) -> Option<Vec<u8>> {
        self.bus
            .cartridge
            .as_ref()
            .and_then(crate::cartridge::Cartridge::to_crt)
    }

    /// Takes the cartridge out and resets.
    pub fn eject_cartridge(&mut self) {
        self.bus.cartridge = None;
        self.reset();
    }

    /// The name stored in the attached cartridge's header.
    pub fn cartridge_name(&self) -> Option<&str> {
        self.bus.cartridge.as_ref().map(|c| c.name())
    }

    /// Presses RECORD on the Datasette (SENSE reads low), so a real-time
    /// SAVE starts without waiting for the button. What a program writes to
    /// the cassette WRITE line is captured either way (see `recorded_tape`).
    pub fn arm_tape_recorder(&mut self) {
        self.bus.record_pressed = true;
    }

    /// The pulses recorded so far as a tape, or `None` if the recorder is
    /// not armed or nothing was written.
    pub fn recorded_tape(&self) -> Option<c64_tape::Tape> {
        let recorder = self.bus.tape_recorder.as_ref()?;
        (!recorder.is_empty()).then(|| c64_tape::Tape::from_pulses(recorder.pulses()))
    }

    /// Serves KERNAL LOAD/SAVE on the cassette instantly (default) instead of
    /// through real-time pulses. A tape the standard-format decoder finds no
    /// programs on is always played in real time.
    pub fn set_fast_tape_load(&mut self, enabled: bool) {
        self.bus.fast_tape = enabled;
    }

    /// Takes the cassette out of the Datasette.
    pub fn eject_tape(&mut self) {
        self.bus.detach_tape();
    }

    /// Takes the disk image out (the fast loader then answers "file not
    /// found"-style failures again, as with no disk). Disks are read-only,
    /// so nothing is lost.
    pub fn eject_disk(&mut self) {
        self.bus.disk = None;
    }

    /// Sets the currently loaded tape's turbo speed multiplier (1 = real
    /// time). A no-op if no tape is installed yet.
    pub fn set_tape_speed(&mut self, multiplier: u32) {
        self.bus.set_tape_speed(multiplier);
    }

    /// Installs a `.d64` disk image, replacing any already attached. See
    /// `try_fast_kernal_load_trap`'s own doc comment for how this is
    /// actually consumed -- there's no simulated 1541/IEC-bus hardware
    /// behind this at all, unlike the real, separately-clocked-machine
    /// wiring `c1541`/`crates/machines/c64/tests/iec_1541.rs` prove.
    pub fn load_disk(&mut self, disk: d64::D64) {
        self.bus.attach_disk(AttachedDisk::D64(disk));
    }

    /// Installs a `.d71` (1571, double-sided) disk image, replacing any
    /// already attached -- same direct-attach, no-simulated-drive shape
    /// `load_disk` uses for `.d64`; see `try_fast_kernal_load_trap`'s own
    /// doc comment.
    pub fn load_disk_d71(&mut self, disk: d71::D71) {
        self.bus.attach_disk(AttachedDisk::D71(disk));
    }

    /// Installs a `.d81` (1581, uniform-sector) disk image, replacing any
    /// already attached -- same direct-attach, no-simulated-drive shape
    /// `load_disk` uses for `.d64`; see `try_fast_kernal_load_trap`'s own
    /// doc comment.
    pub fn load_disk_d81(&mut self, disk: d81::D81) {
        self.bus.attach_disk(AttachedDisk::D81(disk));
    }

    /// Sets one CPU register by name (`A`, `X`, `Y`, `S`/`SP`, `PC`,
    /// `P`/`SR`). Names are case-insensitive.
    pub fn set_register(&mut self, name: &str, value: u64) -> Result<(), String> {
        self.cpu.set_register(name, value)
    }

    /// **IEC-level fast loader.** Traps real KERNAL LOAD's own entry point
    /// (`KERNAL_LOAD_ENTRY`, checked at the top of `step` before the normal
    /// `cpu.step` call, same shape `zx_spectrum::ZxSpectrum::
    /// try_fast_tape_load_trap` already established) and, when a `.d64`/
    /// `.d71`/`.d81` is attached and the requested device is a disk
    /// (`KERNAL_LOAD_MIN_DISK_DEVICE` or above), services the entire LOAD
    /// directly from the image's own logical directory/sector contents --
    /// bypassing every real IEC bus bit/byte transfer entirely, since no
    /// simulated 1541/1571/1581 is stepped alongside this `C64` at all in
    /// this architecture (see `C64Bus::disk`'s own doc comment). Format is
    /// irrelevant past `AttachedDisk::find_program_bytes` -- `.d71`/`.d81`
    /// use the identical CBM DOS directory/file chain format `.d64` does,
    /// just at a different real geometry, so this trap needed zero
    /// format-specific branching to support them. This is deliberately
    /// entered at
    /// LOAD's very first instruction, *before* any of its own real work
    /// happens -- unlike `zx_spectrum`'s trap (which lets the ROM's own
    /// header search run and only substitutes the byte-transfer loop
    /// itself), there is no real "open file on the bus" phase to preserve
    /// here, since this trap *is* the entire substitute for that phase.
    ///
    /// Only real LOAD (`A == 0` on entry, the `VERCK` flag `$F49E`'s own
    /// first instructions haven't stored into zero page yet) is trapped --
    /// VERIFY (`A == 1`) falls through to real (non-functional, since no
    /// disk drive really exists to answer it either way) KERNAL execution,
    /// an explicit, documented scope cut rather than hand-modeling LOAD's
    /// own internal VERIFY-mismatch signaling path, which this trap never
    /// needed to understand to implement LOAD correctly.
    ///
    /// On success: writes the file's own payload bytes (its first 2 bytes
    /// are always a real embedded load-address header, exactly like
    /// `.prg`/`.t64` -- `SA == 0` ignores it and uses the caller's own
    /// given `X`/`Y` address instead, `SA != 0` uses it, the exact real
    /// KERNAL LOAD convention), sets `X`/`Y` (and the real `$AE`/`$AF`
    /// scratch registers some code reads directly) to the real "one past
    /// the last byte loaded" exit convention, clears carry and `STATUS`,
    /// and simulates the routine's own eventual `RTS` back to its caller
    /// by popping a real return address off the stack -- same "never
    /// executes any of the routine's own instructions" principle
    /// `zx_spectrum`'s trap already established. On a file-not-found: sets
    /// carry and `A = KERNAL_ERROR_FILE_NOT_FOUND`, then the same
    /// simulated `RTS`.
    fn try_fast_kernal_load_trap(&mut self) -> bool {
        if self.cpu.pc != KERNAL_LOAD_ENTRY {
            return false;
        }
        if [
            self.bus.read(KERNAL_LOAD_ENTRY),
            self.bus.read(KERNAL_LOAD_ENTRY.wrapping_add(1)),
        ] != KERNAL_LOAD_ENTRY_FIRST_BYTES
        {
            return false;
        }
        if self.cpu.a != 0 {
            return false; // VERIFY -- not trapped, see doc comment above.
        }
        let device = self.bus.read(ZP_FA);
        let on_disk = device >= KERNAL_LOAD_MIN_DISK_DEVICE && self.bus.disk.is_some();
        let on_tape =
            device == KERNAL_TAPE_DEVICE && self.bus.fast_tape && !self.bus.tape_files.is_empty();
        if !on_disk && !on_tape {
            return false;
        }

        let sa = self.bus.read(ZP_SA);
        let fnlen = self.bus.read(ZP_FNLEN) as u16;
        let fnadr =
            u16::from(self.bus.read(ZP_FNADR_LO)) | (u16::from(self.bus.read(ZP_FNADR_HI)) << 8);
        let name: Vec<u8> = (0..fnlen)
            .map(|i| self.bus.read(fnadr.wrapping_add(i)))
            .collect();

        let found = if on_disk {
            let disk = self.bus.disk.as_ref().expect("checked Some above");
            disk.find_program_bytes(&name)
        } else {
            self.bus.next_tape_program(&name)
        };
        let Some((embedded_addr, payload)) = found else {
            self.cpu.p |= 0x01; // carry set = error
            self.cpu.a = KERNAL_ERROR_FILE_NOT_FOUND;
            self.simulate_rts();
            return true;
        };
        let payload = &payload[..];

        let target = if sa == 0 {
            u16::from(self.cpu.x) | (u16::from(self.cpu.y) << 8)
        } else {
            embedded_addr
        };

        for (i, &byte) in payload.iter().enumerate() {
            self.bus.write(target.wrapping_add(i as u16), byte);
        }

        let end = target.wrapping_add(payload.len() as u16);
        self.cpu.x = end as u8;
        self.cpu.y = (end >> 8) as u8;
        self.bus.write(ZP_EAL, end as u8);
        self.bus.write(ZP_EAH, (end >> 8) as u8);
        self.bus.write(ZP_STATUS, 0);
        self.bus.write(ZP_VERCK, 0);
        self.cpu.p &= !0x01; // carry clear = success

        self.simulate_rts();
        true
    }

    /// **IEC-level fast SAVE.** Traps real KERNAL SAVE at `KERNAL_SAVE_ENTRY`
    /// for a disk device (8 or above) when a `.d64` is attached and stores
    /// the memory range as a PRG (load address first), like the 1541 would.
    /// Mirrors the LOAD trap: nothing of the KERNAL routine runs, the return
    /// is simulated. A name starting `@0:` or `@:` replaces an existing file;
    /// a leading `0:` is dropped. Outcomes follow what the KERNAL itself can
    /// see: an empty name is its own error (`8`, carry set); a drive-side
    /// failure (file exists, disk full, name over 16 characters) returns
    /// success with the image untouched, exactly as real hardware does -- the
    /// drive's error channel (`63`/`72`/`33`) is not modelled, so such a
    /// failed save is silent. `.d71`/`.d81` fall through to the real KERNAL
    /// (no writable BAM decode yet), as does a tape device.
    /// Serial-bus printer: the KERNAL's `LISTEN`/`SECOND`/`CIOUT`/`UNLSN` jump
    /// table entries, answered for device 4/5 when a printer is switched on,
    /// so `OPEN`/`PRINT#`/`CLOSE` to the printer succeed without an IEC
    /// device (see `crate::printer`). Anything else falls through.
    fn try_printer_trap(&mut self) -> bool {
        // The routines the jump table's JMPs and the KERNAL's own OPEN/CHKOUT
        // code both reach, with their first two opcode bytes (identical on
        // every committed KERNAL).
        const LISTEN: u16 = 0xED0C; // ORA #$20
        const SECOND: u16 = 0xEDB9; // STA $95
        const CIOUT: u16 = 0xEDDD; // BIT $94
        const UNLSN: u16 = 0xEDFE; // LDA #$3F
        if self.bus.printer.is_none() {
            return false;
        }
        let pc = self.cpu.pc;
        let expected: [u8; 2] = match pc {
            LISTEN => [0x09, 0x20],
            SECOND => [0x85, 0x95],
            CIOUT => [0x24, 0x94],
            UNLSN => [0xA9, 0x3F],
            _ => return false,
        };
        if [self.bus.read(pc), self.bus.read(pc + 1)] != expected {
            return false;
        }
        let a = self.cpu.a;
        let listening = self.bus.printer.as_ref().is_some_and(|p| p.listening());
        match pc {
            LISTEN if matches!(a & 0x1F, 4 | 5) => {
                if let Some(p) = &mut self.bus.printer {
                    p.listen();
                }
            }
            SECOND if listening => {
                if let Some(p) = &mut self.bus.printer {
                    p.set_secondary(a);
                }
            }
            CIOUT if listening => {
                if let Some(p) = &mut self.bus.printer {
                    p.push(a);
                }
            }
            UNLSN if listening => {
                if let Some(p) = &mut self.bus.printer {
                    p.unlisten();
                }
            }
            _ => return false,
        }
        self.bus.write(ZP_STATUS, 0);
        self.cpu.p &= !0x01;
        self.simulate_rts();
        true
    }

    fn try_fast_kernal_save_trap(&mut self) -> bool {
        if self.cpu.pc != KERNAL_SAVE_ENTRY {
            return false;
        }
        if [
            self.bus.read(KERNAL_SAVE_ENTRY),
            self.bus.read(KERNAL_SAVE_ENTRY.wrapping_add(1)),
        ] != KERNAL_SAVE_ENTRY_FIRST_BYTES
        {
            return false;
        }
        let device = self.bus.read(ZP_FA);
        let to_disk = device >= KERNAL_LOAD_MIN_DISK_DEVICE && self.bus.disk.is_some();
        let to_tape =
            device == KERNAL_TAPE_DEVICE && self.bus.fast_tape && self.bus.tape_recorder.is_some();
        if !to_disk && !to_tape {
            return false;
        }

        let fnlen = u16::from(self.bus.read(ZP_FNLEN));
        if fnlen == 0 && to_disk {
            self.cpu.p |= 0x01;
            self.cpu.a = KERNAL_ERROR_MISSING_FILE_NAME;
            self.simulate_rts();
            return true;
        }
        let fnadr =
            u16::from(self.bus.read(ZP_FNADR_LO)) | (u16::from(self.bus.read(ZP_FNADR_HI)) << 8);
        let mut name: Vec<u8> = (0..fnlen)
            .map(|i| self.bus.read(fnadr.wrapping_add(i)))
            .collect();

        let pointer = u16::from(self.cpu.a);
        let start =
            u16::from(self.bus.read(pointer)) | (u16::from(self.bus.read(pointer + 1)) << 8);
        let end = u16::from(self.cpu.x) | (u16::from(self.cpu.y) << 8);
        let mut payload = Vec::new();
        let mut address = start;
        while address != end && payload.len() < 0x1_0000 {
            payload.push(self.bus.read(address));
            address = address.wrapping_add(1);
        }

        if to_tape {
            // Secondary address 0 saves a relocatable BASIC program (header
            // type 1), anything else an absolute one (type 3) -- the KERNAL's
            // own rule, seen on a recording of its real SAVE.
            let kind = if self.bus.read(ZP_SA) == 0 { 1 } else { 3 };
            let pulses = c64_tape::rom_format::encode_program(kind, &name, start, &payload);
            if let Some(recorder) = &mut self.bus.tape_recorder {
                recorder.append_pulses(&pulses);
            }
        } else {
            let replace = name.first() == Some(&b'@');
            if replace {
                name.remove(0);
            }
            if name.len() >= 2 && name[0] == b'0' && name[1] == b':' {
                name.drain(..2);
            } else if replace && name.first() == Some(&b':') {
                name.remove(0);
            }
            let mut data = start.to_le_bytes().to_vec();
            data.extend_from_slice(&payload);
            let saved = self
                .bus
                .disk
                .as_mut()
                .and_then(|disk| disk.save_program(&name, &data, replace));
            if matches!(saved, Some(Ok(()))) {
                self.bus.disk_modified = true;
            }
        }
        self.bus.write(ZP_STATUS, 0);
        self.cpu.p &= !0x01;
        self.simulate_rts();
        true
    }

    /// The attached disk image if the SAVE trap changed it since it was
    /// attached or last exported; clears the "modified" mark only through
    /// `mark_disk_saved`.
    pub fn take_modified_disk_image(&mut self) -> Option<Vec<u8>> {
        if !self.bus.disk_modified {
            return None;
        }
        self.bus.disk.as_ref().map(AttachedDisk::to_bytes)
    }

    /// The attached image, modified or not.
    pub fn disk_image(&self) -> Option<Vec<u8>> {
        self.bus.disk.as_ref().map(AttachedDisk::to_bytes)
    }

    pub fn mark_disk_saved(&mut self) {
        self.bus.disk_modified = false;
    }

    pub fn disk_modified(&self) -> bool {
        self.bus.disk_modified
    }

    /// Pops a real return address off the 6502 stack and resumes execution
    /// there, exactly matching a real `RTS`'s own effect (`PC = popped + 1`)
    /// -- `try_fast_kernal_load_trap` never executes any of `$F49E`'s own
    /// instructions, including its own eventual `RTS`, so it has to
    /// simulate that unwind itself, same principle
    /// `zx_spectrum::try_fast_tape_load_trap` already established for the
    /// Z80's own `RET`.
    fn simulate_rts(&mut self) {
        let mut sp = self.cpu.sp;
        sp = sp.wrapping_add(1);
        let lo = self.bus.read(0x0100 + sp as u16);
        sp = sp.wrapping_add(1);
        let hi = self.bus.read(0x0100 + sp as u16);
        self.cpu.sp = sp;
        self.cpu.pc = ((hi as u16) << 8 | lo as u16).wrapping_add(1);
    }

    /// The SID's own `output()` alone -- no other sound source exists on
    /// this machine to mix it with (unlike `zx_spectrum::mix_audio_sample`,
    /// which combines a beeper and up to two AY chips). Kept as its own
    /// method anyway, matching that same shape, so a future second audio
    /// source (e.g. a digi-sound cartridge) has an obvious place to join
    /// the mix without `step()` itself needing to change.
    fn mix_audio_sample(&mut self) -> f32 {
        self.bus.sid.output()
    }
}

impl Machine for C64 {
    fn clock_hz(&self) -> u64 {
        if self.bus.vic.is_ntsc() {
            NTSC_CLOCK_HZ
        } else {
            PAL_CLOCK_HZ
        }
    }

    fn frame_hz(&self) -> u64 {
        if self.bus.vic.is_ntsc() {
            NTSC_FRAME_HZ
        } else {
            PAL_FRAME_HZ
        }
    }

    fn run(&mut self, running: &mut dyn FnMut(&dyn Machine) -> bool) -> RunStats {
        machine::Clock::new(self.frame_hz()).run(self, running)
    }

    fn step(&mut self) -> u32 {
        let total = if self.bus.cycle_mode {
            self.step_cycle_mode()
        } else {
            self.step_line_mode()
        };
        if self.bus.reu.as_mut().is_some_and(crate::reu::Reu::take_irq) {
            self.cpu.request_irq();
        }
        if let Some(rrnet) = &mut self.bus.rrnet {
            rrnet.pump();
        }
        if self
            .bus
            .cartridge
            .as_mut()
            .is_some_and(crate::cartridge::Cartridge::take_nmi)
        {
            self.cpu.request_nmi();
        }
        total
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

    fn render_frame(&self) -> Option<(u32, u32, Vec<u8>)> {
        // Cycle mode draws the picture as the cycles run (border unit, sprites
        // and all), border included; until the first frame is done fall back
        // to decoding the registers like line mode.
        if self.bus.cycle_mode
            && let Some(frame) = self.bus.vic.cycle_frame()
        {
            return Some(frame);
        }
        let bank = self.bus.vic_bank_base();
        let ultimax_copy = self.bus.vic_ultimax_copy();
        let vic_ram = ultimax_copy
            .as_deref()
            .unwrap_or(&self.bus.ram[bank..bank + 0x4000]);
        let (w, h, rgb) = self
            .bus
            .vic
            .render(vic_ram, &self.bus.char_rom, &self.bus.color_ram);
        let (r, g, b) = self.bus.vic.border_color();
        Some(machine::with_border(
            w,
            h,
            &rgb,
            BORDER_PX,
            BORDER_PX,
            [r, g, b],
        ))
    }

    fn read_memory(&self, addr: u16) -> u8 {
        self.bus.peek(addr)
    }

    fn load(&mut self, addr: u16, bytes: &[u8]) -> usize {
        // Real hardware never truly drops a write anywhere in the address
        // space (a ROM window's write always lands in the RAM underneath,
        // and even the I/O window's writes land somewhere real) -- so
        // every byte always "lands", unlike a machine with genuine
        // unmapped space.
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
        // The RESET line also reaches the expansion port: banked cartridges
        // fall back to their power-on bank/mode.
        if let Some(cartridge) = &mut self.bus.cartridge {
            cartridge.reset();
        }
        if let Some(reu) = &mut self.bus.reu {
            reu.reset();
        }
        if let Some((acia, _)) = &mut self.bus.acia {
            acia.reset();
        }
        if let Some(rrnet) = &mut self.bus.rrnet {
            rrnet.chip.reset();
        }
        if let Some(printer) = &mut self.bus.printer {
            printer.unlisten();
        }
        self.cpu.reset(&mut self.bus);
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.audio_buffer)
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

    fn test_c64() -> C64 {
        C64::new(
            vec![0xAA; KERNAL_ROM_SIZE],
            vec![0xBB; BASIC_ROM_SIZE],
            vec![0xCC; CHAR_ROM_SIZE],
        )
        .unwrap()
    }

    #[test]
    fn the_cpu_port_reads_back_only_its_six_pins() {
        // The KERNAL's start-up values: DDR $2F, port $37. Bits 6 and 7 of a 6510 port do not exist: PEEK(1) is 55.
        let mut c64 = test_c64();
        c64.bus.write(0x0000, 0x2F);
        c64.bus.write(0x0001, 0x37);
        assert_eq!(c64.bus.read(0x0001), 0x37);
        assert_eq!(c64.bus.read(0x0000), 0x2F);
        // Even when every pin is an input (the power-on DDR of 0) the unconnected bits stay low.
        c64.bus.write(0x0000, 0x00);
        assert_eq!(c64.bus.read(0x0001) & 0xC0, 0);
    }

    fn boots_with_kernal_and_basic_and_io_all_visible_by_default() {
        let c64 = test_c64();
        // $E000 should read KERNAL (0xAA), $A000 should read BASIC (0xBB).
        assert_eq!(c64.bus.peek(0xE000), 0xAA);
        assert_eq!(c64.bus.peek(0xA000), 0xBB);
    }

    fn reset_vector_is_read_from_kernal_rom() {
        let mut kernal = vec![0u8; KERNAL_ROM_SIZE];
        // Reset vector lives at the very end of the KERNAL's own mapped
        // window ($FFFC/$FFFD -> KERNAL offsets $1FFC/$1FFD).
        kernal[0x1FFC] = 0x34;
        kernal[0x1FFD] = 0x12;
        let c64 = C64::new(kernal, vec![0; BASIC_ROM_SIZE], vec![0; CHAR_ROM_SIZE]).unwrap();
        assert_eq!(c64.pc(), 0x1234);
    }

    #[test]
    fn writes_to_rom_windows_land_in_the_real_ram_underneath() {
        let mut c64 = test_c64();
        c64.bus.write(0xE000, 0x99); // KERNAL banked in for reads
        assert_eq!(
            c64.bus.peek(0xE000),
            0xAA,
            "read should still see KERNAL ROM"
        );
        // Bank KERNAL out (clear HIRAM) and confirm the RAM write is there.
        c64.bus.write(0x0000, 0xFF); // DDR all-output
        c64.bus.write(0x0001, 0x00); // LORAM=HIRAM=CHAREN=0
        assert_eq!(
            c64.bus.peek(0xE000),
            0x99,
            "the earlier write should surface once RAM shows"
        );
    }

    fn char_rom_and_io_share_the_d000_window_by_charen() {
        let mut c64 = test_c64();
        // Default boot state: CHAREN=1 -> I/O visible at $D000-$D3FF (VIC).
        c64.bus.write(0xD020, 0x05); // VIC border color register
        assert_eq!(c64.bus.vic.border_color(), vic_ii::PALETTE[5]);

        // Clear CHAREN -> character ROM should now read back instead.
        c64.bus.write(0x0000, 0xFF);
        c64.bus.write(0x0001, 0x03); // LORAM=1, HIRAM=1, CHAREN=0
        assert_eq!(c64.bus.peek(0xD000), 0xCC, "char ROM should be visible now");
    }
    /// Merged (every check kept): `boots_with_kernal_and_basic_and_io_all_visible_by_default`, `reset_vector_is_read_from_kernal_rom`, `char_rom_and_io_share_the_d000_window_by_charen`.
    #[test]
    fn default_memory_map() {
        boots_with_kernal_and_basic_and_io_all_visible_by_default();
        reset_vector_is_read_from_kernal_rom();
        char_rom_and_io_share_the_d000_window_by_charen();
    }

    fn cia1_timer_irq_reaches_the_cpu() {
        let mut c64 = test_c64();
        // Real KERNAL init: latch=small value, enable+start timer, unmask
        // the Timer A interrupt in the ICR.
        c64.bus.write(0xDC04, 0x02);
        c64.bus.write(0xDC05, 0x00);
        c64.bus.write(0xDC0D, 0x81);
        c64.bus.write(0xDC0E, 0x01);
        c64.set_pc(0x0300);
        c64.bus.write(0x0300, 0xEA); // NOP, keep the CPU busy while ticking
        c64.cpu.p &= !0x04; // clear the I flag so the IRQ isn't masked

        let mut fired = false;
        for _ in 0..10 {
            c64.step();
            if c64.pc() != 0x0301 && c64.pc() != 0x0300 {
                fired = true;
                break;
            }
        }
        assert!(
            fired,
            "CIA1 Timer A underflow should have raised a real IRQ"
        );
    }

    /// `NOP; JMP $0300` -- an infinite, self-contained loop so the test
    /// can step past whatever cycle count a raster-line match needs (up
    /// to ~312 lines * 63 cycles/line) without ever running off the end
    /// of the loaded program into uninitialized (all-zero, i.e. real
    /// `BRK`) RAM -- a single bare `NOP` at `$0300` would otherwise let
    /// the very next fetch land on `$0301`'s zeroed `BRK` opcode and
    /// jump to the IRQ vector on its own, giving a false pass regardless
    /// of whether the mechanism under test actually fired.
    fn load_nop_loop(c64: &mut C64) {
        c64.load(0x0300, &[0xEA, 0x4C, 0x00, 0x03]);
        c64.set_pc(0x0300);
        c64.cpu.p &= !0x04; // clear the I flag so an IRQ isn't masked
    }

    fn vic_raster_irq_reaches_the_cpu() {
        let mut c64 = test_c64();
        // Real KERNAL-shaped raster IRQ setup: compare line 5, enable the
        // raster source in $D01A.
        c64.bus.write(0xD012, 5);
        c64.bus.write(0xD01A, 0x01);
        load_nop_loop(&mut c64);

        let mut fired = false;
        for _ in 0..1000 {
            c64.step();
            if !matches!(c64.pc(), 0x0300..=0x0303) {
                fired = true;
                break;
            }
        }
        assert!(
            fired,
            "a raster-compare match should have raised a real IRQ"
        );
    }

    /// Two solid sprites overlapping on one row, whose first row is drawn
    /// on raster line `51 + 10`.
    fn place_two_overlapping_sprites(c64: &mut C64) {
        for (i, x) in [(0u16, 100u8), (1, 110)] {
            c64.bus.write(0xD000 + i * 2, x + 24);
            c64.bus.write(0xD001 + i * 2, 10 + 50);
            c64.bus.write(0x03F8 + i, 1 + i as u8);
            for byte in 0..3 {
                c64.bus.write((1 + i) * 64 + byte, 0xFF);
            }
        }
        c64.bus.write(0xD015, 0b11);
    }

    fn sprite_collision_registers_fill_as_the_raster_passes_and_clear_on_read() {
        let mut c64 = test_c64();
        place_two_overlapping_sprites(&mut c64);
        load_nop_loop(&mut c64);

        // Before the raster reaches the sprites' row: nothing yet.
        assert_eq!(c64.bus.peek(0xD01E), 0);
        for _ in 0..20_000 {
            c64.step();
        }
        // ~20k instructions >> one frame: the raster has passed line 61.
        assert_eq!(c64.bus.peek(0xD01E), 0b11, "peek does not clear");
        assert_eq!(c64.bus.read(0xD01E), 0b11);
        assert_eq!(c64.bus.peek(0xD01E), 0, "the CPU read cleared it");
    }

    fn sprite_collision_interrupt_reaches_the_cpu_when_enabled() {
        let mut c64 = test_c64();
        place_two_overlapping_sprites(&mut c64);
        c64.bus.write(0xD01A, 0x04); // IMMC: sprite-sprite
        load_nop_loop(&mut c64);

        let mut fired = false;
        for _ in 0..20_000 {
            c64.step();
            if !matches!(c64.pc(), 0x0300..=0x0303) {
                fired = true;
                break;
            }
        }
        assert!(fired, "a sprite-sprite collision should raise a real IRQ");
        assert_eq!(c64.bus.peek(0xD019) & 0x84, 0x84);
    }

    fn sprite_collision_without_the_enable_bit_stays_a_polled_flag() {
        let mut c64 = test_c64();
        place_two_overlapping_sprites(&mut c64);
        load_nop_loop(&mut c64);
        for _ in 0..20_000 {
            c64.step();
            assert!(matches!(c64.pc(), 0x0300..=0x0303), "no IRQ expected");
        }
        assert_eq!(c64.bus.peek(0xD019) & 0x04, 0x04);
    }
    /// Merged (every check kept): `sprite_collision_registers_fill_as_the_raster_passes_and_clear_on_read`, `sprite_collision_interrupt_reaches_the_cpu_when_enabled`, `sprite_collision_without_the_enable_bit_stays_a_polled_flag`.
    #[test]
    fn sprite_collision_flags() {
        sprite_collision_registers_fill_as_the_raster_passes_and_clear_on_read();
        sprite_collision_interrupt_reaches_the_cpu_when_enabled();
        sprite_collision_without_the_enable_bit_stays_a_polled_flag();
    }

    fn crt_with(
        hardware_type: u16,
        exrom_level: bool,
        game_level: bool,
        chips: &[(u16, u16, u8)],
    ) -> crate::cartridge::Cartridge {
        let chips = chips
            .iter()
            .map(|&(bank, load, fill)| c64_cartridge::Chip {
                kind: c64_cartridge::ChipKind::Rom,
                bank,
                load_address: load,
                data: vec![fill; 0x2000],
            })
            .collect();
        crate::cartridge::Cartridge::from_crt(&c64_cartridge::Crt {
            version: 0x100,
            hardware_type,
            exrom_level,
            game_level,
            name: "TEST".into(),
            chips,
        })
        .unwrap()
    }

    fn an_8k_cartridge_shows_roml_only_while_loram_and_hiram_are_set() {
        let mut c64 = test_c64();
        c64.bus.write(0x8000, 0x77); // RAM underneath
        c64.load_cartridge(crt_with(0, false, true, &[(0, 0x8000, 0xC1)]));
        assert_eq!(c64.bus.read(0x8000), 0xC1, "ROML over RAM");
        assert_eq!(c64.bus.read(0xA000), 0xBB, "BASIC is still there");
        c64.bus.write(0x01, 0x36); // LORAM = 0
        c64.bus.write(0x00, 0x07);
        assert_eq!(c64.bus.read(0x8000), 0x77, "ROML needs LORAM and HIRAM");
    }

    fn a_16k_cartridge_replaces_basic_with_romh() {
        let mut c64 = test_c64();
        c64.load_cartridge(crt_with(
            0,
            false,
            false,
            &[(0, 0x8000, 0xC1), (0, 0xA000, 0xC2)],
        ));
        assert_eq!(c64.bus.read(0x8000), 0xC1);
        assert_eq!(c64.bus.read(0xA000), 0xC2, "ROMH instead of BASIC");
        assert_eq!(c64.bus.read(0xE000), 0xAA, "KERNAL unchanged");
    }

    fn writes_under_cartridge_roms_land_in_ram() {
        let mut c64 = test_c64();
        c64.load_cartridge(crt_with(
            0,
            false,
            false,
            &[(0, 0x8000, 0xC1), (0, 0xA000, 0xC2)],
        ));
        c64.bus.write(0x8000, 0x12);
        assert_eq!(c64.bus.read(0x8000), 0xC1);
        assert_eq!(c64.bus.ram[0x8000], 0x12);
    }
    /// Merged (every check kept): `an_8k_cartridge_shows_roml_only_while_loram_and_hiram_are_set`, `a_16k_cartridge_replaces_basic_with_romh`, `writes_under_cartridge_roms_land_in_ram`.
    #[test]
    fn cartridge_maps() {
        an_8k_cartridge_shows_roml_only_while_loram_and_hiram_are_set();
        a_16k_cartridge_replaces_basic_with_romh();
        writes_under_cartridge_roms_land_in_ram();
    }

    fn ultimax_leaves_most_of_the_map_open_and_takes_its_vectors_from_romh() {
        let mut c64 = test_c64();
        c64.bus.write(0x2000, 0x55); // RAM that Ultimax is about to hide
        // Ultimax: EXROM high (inactive), GAME low (active).
        c64.load_cartridge(crt_with(
            0,
            true,
            false,
            &[(0, 0x8000, 0xC1), (0, 0xE000, 0xC2)],
        ));
        assert_eq!(c64.bus.read(0x8000), 0xC1);
        assert_eq!(c64.bus.read(0xFFFC), 0xC2, "ROMH at $E000");
        assert_eq!(
            c64.pc(),
            0xC2C2,
            "the reset vector comes from the cartridge"
        );
        assert_eq!(c64.bus.read(0x2000), 0xFF, "open: not RAM");
        assert_eq!(c64.bus.read(0xA000), 0xFF);
        assert_eq!(c64.bus.read(0xC000), 0xFF);
        c64.bus.write(0x2000, 0x99);
        assert_eq!(c64.bus.ram[0x2000], 0x55, "no RAM to write to");
        c64.bus.write(0x0800, 0x42);
        assert_eq!(c64.bus.read(0x0800), 0x42, "RAM below $1000 stays");
        c64.bus.write(0xD020, 0x07);
        assert_eq!(c64.bus.read(0xD020) & 0x0F, 0x07, "I/O is still there");
    }

    fn the_vic_sees_romh_at_3000_in_an_ultimax_map() {
        let mut c64 = test_c64();
        c64.bus.ram[0x3000] = 0x11;
        assert!(
            c64.bus.vic_ultimax_copy().is_none(),
            "no cartridge: plain RAM"
        );
        c64.load_cartridge(crt_with(
            0,
            true,
            false,
            &[(0, 0x8000, 0xC1), (0, 0xE000, 0xC2)],
        ));
        let copy = c64.bus.vic_ultimax_copy().expect("Ultimax view");
        assert_eq!(copy.len(), 0x4000);
        assert_eq!(copy[0x3000], 0xC2, "ROMH replaces RAM at $3000-$3FFF");
        assert_eq!(copy[0x0000], c64.bus.ram[0]);
        // A normal 8K cartridge leaves the VIC on RAM.
        c64.load_cartridge(crt_with(0, false, true, &[(0, 0x8000, 0xC1)]));
        assert!(c64.bus.vic_ultimax_copy().is_none());
    }
    /// Merged (every check kept): `ultimax_leaves_most_of_the_map_open_and_takes_its_vectors_from_romh`, `the_vic_sees_romh_at_3000_in_an_ultimax_map`.
    #[test]
    fn ultimax_map() {
        ultimax_leaves_most_of_the_map_open_and_takes_its_vectors_from_romh();
        the_vic_sees_romh_at_3000_in_an_ultimax_map();
    }

    fn freezing_an_action_replay_takes_the_nmi_through_the_cartridge_vectors() {
        let mut c64 = test_c64();
        assert!(!c64.freeze(), "no cartridge, no freezer");
        c64.load_cartridge(crt_with(
            1,
            false,
            true,
            &[
                (0, 0x8000, 0x10),
                (1, 0x8000, 0x11),
                (2, 0x8000, 0x12),
                (3, 0x8000, 0x13),
            ],
        ));
        assert!(c64.has_freezer());
        c64.cpu.pc = 0x0400;
        c64.bus.ram[0x0400] = 0xEA;
        assert!(c64.freeze());
        c64.step();
        // Ultimax from bank 0: the NMI vector at $FFFA/B reads $10 $10.
        assert_eq!(c64.cpu.pc, 0x1010);
        // The cartridge RAM now sits at $8000 and takes writes.
        c64.bus.write(0x8000, 0x5A);
        assert_eq!(c64.bus.read(0x8000), 0x5A);
        assert_eq!(c64.bus.ram[0x8000], 0, "not written to C64 RAM");
    }

    fn an_easyflash_program_cycle_through_the_bus_changes_the_flash() {
        let mut c64 = test_c64();
        let flash_chip = |load_address| c64_cartridge::Chip {
            kind: c64_cartridge::ChipKind::Flash,
            bank: 0,
            load_address,
            data: vec![0xFF; 0x2000],
        };
        let chips = vec![flash_chip(0x8000), flash_chip(0xA000)];
        c64.load_cartridge(
            crate::cartridge::Cartridge::from_crt(&c64_cartridge::Crt {
                version: 0x100,
                hardware_type: 32,
                exrom_level: true,
                game_level: false,
                name: "EF".into(),
                chips,
            })
            .unwrap(),
        );
        assert!(!c64.cartridge_modified());
        // EasyFlash boots in Ultimax: $8000 is the low flash.
        for (addr, value) in [
            (0x8555, 0xAA),
            (0x82AA, 0x55),
            (0x8555, 0xA0),
            (0x8100, 0x3C),
        ] {
            c64.bus.write(addr, value);
        }
        assert_eq!(c64.bus.read(0x8100), 0x3C);
        assert!(c64.cartridge_modified());
        assert!(c64.cartridge_image().is_some());
        // The high chip answers at $E000 (Ultimax) and was not touched.
        assert_eq!(c64.bus.read(0xE100), 0xFF);
        // A normal write to RAM below $1000 is unaffected.
        c64.bus.write(0x0800, 0x42);
        assert_eq!(c64.bus.read(0x0800), 0x42);
    }

    /// RR-Net port `p` of the CS8900A lives at window offset `p ^ 8`.
    fn rr_write16(c64: &mut C64, port: u8, v: u16) {
        c64.bus.write(0xDE00 + u16::from(port ^ 8), v as u8);
        c64.bus
            .write(0xDE00 + u16::from((port + 1) ^ 8), (v >> 8) as u8);
    }

    fn rr_read16(c64: &mut C64, port: u8) -> u16 {
        let lo = c64.bus.read(0xDE00 + u16::from(port ^ 8));
        let hi = c64.bus.read(0xDE00 + u16::from((port + 1) ^ 8));
        u16::from(lo) | (u16::from(hi) << 8)
    }

    fn an_rrnet_frame_sent_with_loopback_comes_back_and_without_it_does_not() {
        for loopback in [true, false] {
            let mut c64 = test_c64();
            c64.attach_rrnet(0xDE, loopback).unwrap();
            let mac = [0x02, 0, 0, 0, 0, 1];
            for (i, w) in mac.chunks(2).enumerate() {
                rr_write16(&mut c64, 0x0A, 0x158 + 2 * i as u16);
                rr_write16(&mut c64, 0x0C, u16::from(w[0]) | (u16::from(w[1]) << 8));
            }
            rr_write16(&mut c64, 0x0A, 0x112);
            rr_write16(&mut c64, 0x0C, 0x00C3); // tx + rx on
            rr_write16(&mut c64, 0x0A, 0x104);
            rr_write16(&mut c64, 0x0C, 0x0D05); // IA + broadcast
            rr_write16(&mut c64, 0x04, 0x00C9);
            rr_write16(&mut c64, 0x06, 64);
            rr_write16(&mut c64, 0x0A, 0x138);
            assert_ne!(rr_read16(&mut c64, 0x0C) & 0x100, 0, "ready to transmit");
            let mut frame = mac.to_vec();
            frame.extend((6..64).map(|i| i as u8));
            for pair in frame.chunks(2) {
                c64.bus.write(0xDE08, pair[0]);
                c64.bus.write(0xDE09, pair[1]);
            }
            c64.step(); // the loopback moves the frame after the instruction
            assert_eq!(c64.rrnet_frames_sent(), 1);
            rr_write16(&mut c64, 0x0A, 0x124);
            let event = rr_read16(&mut c64, 0x0C);
            assert_eq!(event & 0x0100 != 0, loopback, "RxOK only with loopback");
        }
    }

    fn the_swiftlink_interrupt_goes_to_nmi_and_one_expansion_device_excludes_another() {
        let mut c64 = test_c64_with_vectors(0x0000, 0x0900);
        c64.attach_serial(false, 0xDE).unwrap();
        assert!(c64.attach_rrnet(0xDF, true).is_err());
        assert!(c64.attach_georam(512).is_err());
        c64.bus.write(0xDE03, 0x1F); // 19200 bps
        c64.bus.write(0xDE02, 0x01); // DTR on, receive interrupt enabled
        c64.serial_push_rx(0x42);
        c64.cpu.p &= !0x04;
        let mut hit = false;
        for _ in 0..20_000 {
            c64.step();
            if c64.cpu.pc == 0x0900 {
                hit = true;
                break;
            }
        }
        assert!(hit, "NMI taken through the vector");
        assert_eq!(c64.bus.read(0xDE00), 0x42);
    }
    /// Merged (every check kept): `an_rrnet_frame_sent_with_loopback_comes_back_and_without_it_does_not`, `the_swiftlink_interrupt_goes_to_nmi_and_one_expansion_device_excludes_another`.
    #[test]
    fn expansion_devices() {
        an_rrnet_frame_sent_with_loopback_comes_back_and_without_it_does_not();
        the_swiftlink_interrupt_goes_to_nmi_and_one_expansion_device_excludes_another();
    }

    fn io1_and_io2_reach_the_cartridge_and_float_without_one() {
        let mut c64 = test_c64();
        assert_eq!(c64.bus.read(0xDE00), 0xFF);
        assert_eq!(c64.bus.read(0xDF00), 0xFF);
        let chips: Vec<_> = (0..16u16).map(|b| (b, 0x8000u16, b as u8)).collect();
        c64.load_cartridge(crt_with(5, false, false, &chips));
        assert_eq!(c64.bus.read(0x8000), 0);
        c64.bus.write(0xDE00, 3);
        assert_eq!(c64.bus.read(0x8000), 3, "Ocean bank register at $DE00");
        assert_eq!(c64.bus.read(0xA000), 3, "and the same bank at $A000");
    }

    fn a_cartridge_reset_restores_its_power_on_bank_and_eject_restores_ram() {
        let mut c64 = test_c64();
        c64.bus.write(0x8000, 0x66);
        let chips: Vec<_> = (0..16u16).map(|b| (b, 0x8000u16, b as u8)).collect();
        c64.load_cartridge(crt_with(5, false, false, &chips));
        c64.bus.write(0xDE00, 9);
        assert_eq!(c64.bus.read(0x8000), 9);
        c64.reset();
        assert_eq!(c64.bus.read(0x8000), 0, "bank 0 again after RESET");
        c64.eject_cartridge();
        assert_eq!(c64.bus.read(0x8000), 0x66, "RAM is back");
        assert_eq!(c64.cartridge_name(), None);
    }

    fn save_state_carries_cartridge_registers_onto_the_same_image() {
        let mut c64 = test_c64();
        let chips: Vec<_> = (0..16u16).map(|b| (b, 0x8000u16, b as u8)).collect();
        c64.load_cartridge(crt_with(5, false, false, &chips));
        c64.bus.write(0xDE00, 6);
        let state = c64.save_state();

        let mut other = test_c64();
        other.load_cartridge(crt_with(5, false, false, &chips));
        other.load_state(&state).unwrap();
        assert_eq!(other.bus.read(0x8000), 6);
    }
    /// Merged (every check kept): `freezing_an_action_replay_takes_the_nmi_through_the_cartridge_vectors`, `an_easyflash_program_cycle_through_the_bus_changes_the_flash`, `io1_and_io2_reach_the_cartridge_and_float_without_one`, `a_cartridge_reset_restores_its_power_on_bank_and_eject_restores_ram`, `save_state_carries_cartridge_registers_onto_the_same_image`.
    #[test]
    fn cartridge_register_paths() {
        freezing_an_action_replay_takes_the_nmi_through_the_cartridge_vectors();
        an_easyflash_program_cycle_through_the_bus_changes_the_flash();
        io1_and_io2_reach_the_cartridge_and_float_without_one();
        a_cartridge_reset_restores_its_power_on_bank_and_eject_restores_ram();
        save_state_carries_cartridge_registers_onto_the_same_image();
    }

    fn vic_raster_irq_stays_suppressed_when_disabled_in_d01a() {
        let mut c64 = test_c64();
        c64.bus.write(0xD012, 5);
        // $D01A left at 0 -- raster source not enabled.
        load_nop_loop(&mut c64);

        for _ in 0..1000 {
            c64.step();
            assert!(
                matches!(c64.pc(), 0x0300..=0x0303),
                "no IRQ should fire while the raster source is disabled"
            );
        }
        // The latch still sets internally even while masked from the CPU.
        assert_eq!(c64.bus.read(0xD019) & 0x0F, 0x01);
    }
    /// Merged (every check kept): `vic_raster_irq_reaches_the_cpu`, `vic_raster_irq_stays_suppressed_when_disabled_in_d01a`.
    #[test]
    fn vic_raster_irq() {
        vic_raster_irq_reaches_the_cpu();
        vic_raster_irq_stays_suppressed_when_disabled_in_d01a();
    }

    fn bad_line_inflates_the_reported_step_cycle_count() {
        let mut c64 = test_c64();
        c64.bus.write(0xD011, 0x10); // DEN set, YSCROLL=0 (default)
        load_nop_loop(&mut c64);

        // Any single step() costing more than a real 6502 instruction
        // ever legitimately can (documented max is 7 cycles) means the
        // VIC-II's real ~40-cycle bad-line steal got folded into this
        // step's reported cost -- see `vic_ii::Vic::tick`'s own doc
        // comment for why the caller (`C64::step`) must report that back
        // rather than only the CPU instruction's own nominal cost.
        let mut saw_steal = false;
        for _ in 0..2000 {
            if c64.step() > 10 {
                saw_steal = true;
                break;
            }
        }
        assert!(
            saw_steal,
            "a bad line should have inflated at least one step()'s reported cycle cost"
        );
    }

    fn bad_line_never_inflates_step_cycle_count_when_den_stays_clear() {
        let mut c64 = test_c64();
        // DEN left clear (default) -- no line should ever steal cycles.
        load_nop_loop(&mut c64);

        for _ in 0..2000 {
            assert!(
                c64.step() <= 10,
                "no bad line should occur while DEN stays clear"
            );
        }
    }
    /// Merged (every check kept): `bad_line_inflates_the_reported_step_cycle_count`, `bad_line_never_inflates_step_cycle_count_when_den_stays_clear`.
    #[test]
    fn bad_line_step_cycles() {
        bad_line_inflates_the_reported_step_cycle_count();
        bad_line_never_inflates_step_cycle_count_when_den_stays_clear();
    }

    /// Regression test for a real bug, reproducing it exactly: real
    /// KERNAL ROM boot code busy-waits on the VIC-II raster register
    /// (confirmed live by disassembling a real KERNAL ROM at the address
    /// boot got stuck at -- a `LDA $D012 / BNE` loop, waiting for the
    /// raster line to read back to `0`). Before `Vic::tick` wired into
    /// `C64::step` below, `$D012` never changed at all, so any loop
    /// waiting for it to *change* spun forever. `Machine::load`'d
    /// program here waits for it to leave `0` (the mirror-image
    /// condition, since the raster starts at `0` by construction --
    /// same dependency on real progression, just phrased the other way
    /// so the test doesn't trivially pass on step one regardless of
    /// whether the fix is even present).
    #[test]
    fn program_waiting_on_the_vic_raster_register_eventually_terminates() {
        let mut c64 = test_c64();
        c64.load(
            0x0300,
            &[
                0xAD, 0x12, 0xD0, // LDA $D012
                0xF0, 0xFB, // BEQ -5 (loop back to LDA while raster reads 0)
                0x02, // JAM once the raster line finally moves off 0
            ],
        );
        c64.set_pc(0x0300);

        // A real raster line is 63 cycles; one full frame is well under
        // 25,000 cycles, so this is a generous bound, not a tight one --
        // if this loop is spinning forever, it should still show up long
        // before this many steps.
        let mut halted = false;
        for _ in 0..50_000 {
            c64.step();
            if c64.halted() {
                halted = true;
                break;
            }
        }
        assert!(
            halted,
            "a real KERNAL-shaped raster busy-wait must terminate, not spin forever"
        );
    }

    #[test]
    fn the_sid_pot_registers_follow_cia1_port_a_bits_7_6_and_the_attached_device() {
        use crate::controllers::MouseMode;
        let mut c64 = test_c64();
        c64.bus.write(0xDC02, 0xFF); // DDRA: all outputs
        c64.set_mouse_mode(MouseMode::Mouse1351(1));
        c64.mouse_motion(7, -2);

        c64.bus.write(0xDC00, 0x40); // %01: control port 1
        assert_eq!(c64.bus.read(0xD419), 0x47);
        assert_eq!(c64.bus.read(0xD41A), 0x42);
        c64.bus.write(0xDC00, 0x80); // %10: port 2, nothing there
        assert_eq!(c64.bus.read(0xD419), 0xFF);
        c64.bus.write(0xDC00, 0x00); // neither
        assert_eq!(c64.bus.read(0xD419), 0xFF);
        // The SID mirrors across its window; POT is at +$19 in every copy.
        c64.bus.write(0xDC00, 0x40);
        assert_eq!(c64.bus.read(0xD439), 0x47);

        // The 1351's buttons pull Fire/Up low on its port: port 1 -> PRB.
        c64.mouse_button(false, true);
        c64.bus.write(0xDC03, 0x00); // DDRB: inputs
        assert_eq!(c64.bus.read(0xDC01) & 0x10, 0, "left button = port 1 fire");
    }

    #[test]
    fn joystick_ports_pull_the_cia1_lines_low() {
        let mut c64 = test_c64();
        // KERNAL-style setup: Port A output (idle high), Port B input.
        c64.bus.write(0xDC02, 0x00);
        c64.bus.write(0xDC03, 0x00);
        let idle_a = c64.bus.read(0xDC00);
        let idle_b = c64.bus.read(0xDC01);
        assert_eq!(idle_a & 0x1F, 0x1F, "no joystick: all five lines high");

        // Port 2 is Port A: bit 0 up, 1 down, 2 left, 3 right, 4 fire.
        c64.press_joystick(2, JoystickButton::Left);
        c64.press_joystick(2, JoystickButton::Fire);
        assert_eq!(c64.bus.read(0xDC00) & 0x1F, 0x1F & !0x04 & !0x10);
        assert_eq!(c64.bus.read(0xDC01), idle_b, "port 2 does not touch Port B");
        c64.release_joystick(2, JoystickButton::Left);
        assert_eq!(c64.bus.read(0xDC00) & 0x1F, 0x1F & !0x10);
        c64.release_joystick(2, JoystickButton::Fire);
        assert_eq!(c64.bus.read(0xDC00), idle_a);

        // Port 1 is Port B, shared with the keyboard rows.
        c64.press_joystick(1, JoystickButton::Up);
        assert_eq!(c64.bus.read(0xDC01), idle_b & !0x01);
        assert_eq!(c64.bus.read(0xDC00), idle_a, "port 1 does not touch Port A");
        c64.release_joystick(1, JoystickButton::Up);
        assert_eq!(c64.bus.read(0xDC01), idle_b);

        // A bad port number is ignored, not a panic or a wrong port.
        c64.press_joystick(0, JoystickButton::Up);
        c64.press_joystick(3, JoystickButton::Up);
        assert_eq!(c64.bus.read(0xDC00), idle_a);
        assert_eq!(c64.bus.read(0xDC01), idle_b);
    }

    #[test]
    fn keyboard_scan_reaches_the_cpu_through_cia1_ports_a_and_b() {
        let mut c64 = test_c64();
        c64.press_key(crate::keyboard::C64Key::Return); // column 0, row 1

        // Real KERNAL scan setup: Port A all-output, Port B all-input.
        c64.bus.write(0xDC02, 0xFF); // DDRA = all output
        c64.bus.write(0xDC03, 0x00); // DDRB = all input
        c64.bus.write(0xDC00, 0b1111_1110); // select column 0 only

        assert_eq!(
            c64.bus.read(0xDC01) & 0x02,
            0,
            "Return's row (bit 1) should read low with column 0 selected"
        );

        c64.bus.write(0xDC00, 0b1111_1101); // select column 1 instead
        assert_eq!(
            c64.bus.read(0xDC01) & 0x02,
            0x02,
            "Return shouldn't show up on a column it isn't wired to"
        );

        c64.release_key(crate::keyboard::C64Key::Return);
        c64.bus.write(0xDC00, 0b1111_1110); // back to column 0
        assert_eq!(
            c64.bus.read(0xDC01) & 0x02,
            0x02,
            "released key reads high again"
        );
    }

    #[test]
    fn sid_registers_mirror_every_32_bytes_across_the_full_io_window() {
        let mut c64 = test_c64();
        // Real hardware only decodes 5 address lines: the register map
        // mirrors every 32 bytes across the whole $D400-$D7FF window.
        // Drive voice 3 (whose OSC3/ENV3 registers are readable) entirely
        // through a mirrored address block, and confirm it behaves exactly
        // like the primary $D400-$D41F block would.
        let mirror = 0xD400u16 + 32 * 3; // an arbitrary non-zero mirror offset
        c64.bus.write(mirror + 0x0E, 0xFF); // voice 3 freq lo
        c64.bus.write(mirror + 0x0F, 0x0F); // voice 3 freq hi
        c64.bus.write(mirror + 0x12, 0b0010_0001); // sawtooth + gate

        for _ in 0..2000 {
            c64.step();
        }

        assert_ne!(
            c64.bus.read(0xD41B), // OSC3, primary (non-mirrored) address
            0,
            "a mirrored-address write should drive the same real voice 3 oscillator"
        );
    }

    #[test]
    fn sid_voice_produces_audible_samples_once_gated_and_ticked() {
        let mut c64 = test_c64();
        // Voice 1: fast sawtooth, fastest attack, full sustain, gate on.
        c64.bus.write(0xD400, 0xFF); // freq lo
        c64.bus.write(0xD401, 0x0F); // freq hi
        c64.bus.write(0xD405, 0x00); // attack=0, decay=0
        c64.bus.write(0xD406, 0xF0); // sustain=15, release=0
        c64.bus.write(0xD404, 0b0010_0001); // sawtooth + gate
        c64.bus.write(0xD418, 0x0F); // master volume, full

        c64.set_pc(0x0300);
        c64.bus.write(0x0300, 0xEA); // NOP: keep stepping without side effects
        for _ in 0..20_000 {
            c64.step();
        }

        let samples = c64.drain_audio();
        assert!(
            !samples.is_empty(),
            "SID audio pipeline produced no samples"
        );
        assert!(
            samples.iter().any(|&s| s > 0.0),
            "a gated, full-volume sawtooth voice should produce nonzero output"
        );
    }

    fn save_state_round_trips_sid_register_state() {
        let mut c64 = test_c64();
        c64.bus.write(0xD400, 0x42); // voice 1 freq lo
        c64.bus.write(0xD401, 0x13); // voice 1 freq hi
        c64.bus.write(0xD418, 0x0A); // master volume

        let saved = c64.save_state();

        let mut restored = test_c64();
        restored.load_state(&saved).unwrap();

        assert_eq!(restored.bus.sid.read(0x00), 0xFF); // write-only register reads 0xFF either way
        assert_eq!(restored.bus.read(0xD418), 0xFF); // mode/volume is also write-only on read
        // Frequency round-trips through ENV3/OSC3-adjacent internal state
        // indirectly -- assert via behavior instead of a private field:
        // ticking the restored machine should still produce audio at the
        // restored (nonzero) master volume/frequency, same as the original.
        restored.set_pc(0x0300);
        restored.bus.write(0x0300, 0xEA);
        for _ in 0..20_000 {
            restored.step();
        }
        let samples = restored.drain_audio();
        assert!(
            !samples.is_empty(),
            "restored SID state should still produce audio samples"
        );
    }

    fn save_state_round_trips_sid_filter_integrator_state() {
        let mut c64 = test_c64();
        // Voice 1: fast pulse wave, full envelope, routed through the
        // filter with a low cutoff -- enough real signal to charge the
        // filter's own persistent integrator state away from its zeroed
        // default, so a broken round trip (state silently reset to 0.0)
        // would actually change `sid.output()` immediately after restore.
        c64.bus.write(0xD400, 0xFF);
        c64.bus.write(0xD401, 0x0F);
        c64.bus.write(0xD402, 0x00);
        c64.bus.write(0xD403, 0x08);
        c64.bus.write(0xD405, 0x00);
        c64.bus.write(0xD406, 0xF0);
        c64.bus.write(0xD404, 0b0100_0001); // pulse + gate
        c64.bus.write(0xD416, 0x02); // low cutoff
        c64.bus.write(0xD417, 0x01); // route voice 1 through the filter
        c64.bus.write(0xD418, 0x1F); // LP select, full volume

        c64.set_pc(0x0300);
        c64.bus.write(0x0300, 0xEA);
        for _ in 0..20_000 {
            c64.step();
        }
        let before = c64.bus.sid.output();

        let saved = c64.save_state();
        let mut restored = test_c64();
        restored.load_state(&saved).unwrap();

        assert_eq!(
            restored.bus.sid.output(),
            before,
            "filter integrator state must survive a save/load round trip exactly"
        );
    }
    /// Merged (every check kept): `save_state_round_trips_sid_register_state`, `save_state_round_trips_sid_filter_integrator_state`.
    #[test]
    fn save_state_sid() {
        save_state_round_trips_sid_register_state();
        save_state_round_trips_sid_filter_integrator_state();
    }

    /// Builds a machine whose KERNAL has distinct, known IRQ/NMI vectors
    /// (unlike `test_c64`'s uniform `0xAA` fill, which happens to make
    /// `$FFFA`/`$FFFE` read back identically and so can't tell the two
    /// apart) -- needed to prove CIA2's interrupt line really reaches the
    /// CPU's NMI input, not its IRQ input.
    fn test_c64_with_vectors(irq_vector: u16, nmi_vector: u16) -> C64 {
        let mut kernal = vec![0xEAu8; KERNAL_ROM_SIZE]; // NOP fill (inert if ever executed)
        let off = |addr: u16| (addr - 0xE000) as usize;
        kernal[off(0xFFFE)] = irq_vector as u8;
        kernal[off(0xFFFF)] = (irq_vector >> 8) as u8;
        kernal[off(0xFFFA)] = nmi_vector as u8;
        kernal[off(0xFFFB)] = (nmi_vector >> 8) as u8;
        C64::new(kernal, vec![0; BASIC_ROM_SIZE], vec![0; CHAR_ROM_SIZE]).unwrap()
    }

    fn cia2_timer_irq_reaches_the_cpu_as_a_real_nmi_not_an_irq() {
        let mut c64 = test_c64_with_vectors(0x1234, 0x5678);
        load_nop_loop(&mut c64);
        c64.bus.write(0xDD04, 0x02); // CIA2 Timer A latch low = 2
        c64.bus.write(0xDD05, 0x00);
        c64.bus.write(0xDD0D, 0x81); // enable Timer A interrupt on CIA2
        c64.bus.write(0xDD0E, 0x01); // start, continuous

        let mut landed = None;
        for _ in 0..10 {
            c64.step();
            if !matches!(c64.pc(), 0x0300..=0x0303) {
                landed = Some(c64.pc());
                break;
            }
        }
        assert_eq!(
            landed,
            Some(0x5678),
            "CIA2's own IRQ line must reach the CPU through its NMI input, not IRQ"
        );
    }

    fn cia1_timer_still_reaches_the_cpu_as_a_real_irq_not_an_nmi() {
        let mut c64 = test_c64_with_vectors(0x1234, 0x5678);
        load_nop_loop(&mut c64);
        c64.bus.write(0xDC04, 0x02);
        c64.bus.write(0xDC05, 0x00);
        c64.bus.write(0xDC0D, 0x81);
        c64.bus.write(0xDC0E, 0x01);

        let mut landed = None;
        for _ in 0..10 {
            c64.step();
            if !matches!(c64.pc(), 0x0300..=0x0303) {
                landed = Some(c64.pc());
                break;
            }
        }
        assert_eq!(
            landed,
            Some(0x1234),
            "CIA1 must still reach the CPU's real IRQ input"
        );
    }
    /// Merged (every check kept): `cia1_timer_irq_reaches_the_cpu`, `cia1_timer_still_reaches_the_cpu_as_a_real_irq_not_an_nmi`.
    #[test]
    fn cia1_irq() {
        cia1_timer_irq_reaches_the_cpu();
        cia1_timer_still_reaches_the_cpu_as_a_real_irq_not_an_nmi();
    }

    #[test]
    fn cia2_port_a_bits_select_the_real_vic_bank() {
        let mut c64 = test_c64();
        // Unconfigured DDR/port (post-reset default) float every bit high
        // -> bits01 = 0b11 -> bank 0, the bank every other test here
        // already assumed before this bank-select wiring existed.
        assert_eq!(c64.bus.vic_bank_base(), 0x0000);

        c64.bus.write(0xDD02, 0x03); // CIA2 DDRA: drive bits 0-1 as output
        c64.bus.write(0xDD00, 0b0000_0010); // bits01 = 0b10 -> bank 1
        assert_eq!(c64.bus.vic_bank_base(), 0x4000);

        c64.bus.write(0xDD00, 0b0000_0001); // bits01 = 0b01 -> bank 2
        assert_eq!(c64.bus.vic_bank_base(), 0x8000);

        c64.bus.write(0xDD00, 0b0000_0000); // bits01 = 0b00 -> bank 3
        assert_eq!(c64.bus.vic_bank_base(), 0xC000);
    }

    /// First pixel row of cell 0 as RGB bytes (past the border).
    fn first_cell_row(c64: &C64) -> Vec<u8> {
        let (w, _, rgb) = c64.render_frame().unwrap();
        let start = (BORDER_PX as usize * w as usize + BORDER_PX as usize) * 3;
        rgb[start..start + 8 * 3].to_vec()
    }

    #[test]
    fn text_mode_uses_the_chargen_shadow_or_a_custom_ram_charset() {
        let mut char_rom = vec![0u8; CHAR_ROM_SIZE];
        char_rom[8] = 0xFF; // upper set, char 1 row 0 solid
        char_rom[0x800 + 8] = 0x0F; // lowercase set, char 1 row 0 right half
        let mut c64 = C64::new(
            vec![0xAA; KERNAL_ROM_SIZE],
            vec![0xBB; BASIC_ROM_SIZE],
            char_rom,
        )
        .unwrap();
        c64.bus.ram[0x0400] = 1;
        c64.bus.vic.write(0x21, 0); // paper black
        c64.bus.color_ram[0] = 1; // ink white
        c64.bus.vic.write(0x18, 0x14); // matrix $0400, chargen $1000 (upper)
        let upper = first_cell_row(&c64);
        assert!(upper.iter().all(|&b| b != 0), "solid ink row: {upper:?}");

        c64.bus.vic.write(0x18, 0x16); // chargen $1800 (lowercase)
        let lower = first_cell_row(&c64);
        assert!(lower[..12].iter().all(|&b| b == 0), "left half is paper");
        assert!(lower[12..].iter().all(|&b| b != 0), "right half is ink");

        // A custom charset in RAM at $2000 (bank 0) replaces the ROM.
        c64.bus.ram[0x2008] = 0xF0;
        c64.bus.vic.write(0x18, 0x18);
        let custom = first_cell_row(&c64);
        assert!(custom[..12].iter().all(|&b| b != 0));
        assert!(custom[12..].iter().all(|&b| b == 0));

        // In VIC bank 1 ($4000) there is no ROM shadow: $1000 is plain RAM.
        c64.bus.write(0xDD02, 0x03);
        c64.bus.write(0xDD00, 0b10); // bank 1
        c64.bus.vic.write(0x18, 0x14);
        c64.bus.ram[0x4000 + 0x0400] = 1;
        let ram_chargen = first_cell_row(&c64);
        assert!(ram_chargen.iter().all(|&b| b == 0), "RAM glyph is blank");
    }

    #[test]
    fn render_frame_reads_from_the_currently_selected_vic_bank() {
        let mut c64 = test_c64();
        c64.bus.write(0xDD02, 0x03);
        c64.bus.write(0xDD00, 0b0000_0010); // bank 1 ($4000-$7FFF)
        c64.bus.ram[0x4000] = 1; // a nonzero screen-code cell in bank 1's own video matrix

        let frame = c64.render_frame();
        assert!(
            frame.is_some(),
            "render_frame should still produce a picture from a non-zero VIC bank"
        );
    }

    /// Builds a minimal, synthetic real `.tap` (version 0) file: nonzero
    /// pulse bytes only, each `N * 8` real cycles -- see `c64_tape`'s own
    /// doc comment for the full real encoding this constructs by hand.
    fn build_tap(pulse_bytes: &[u8]) -> c64_tape::Tape {
        let mut bytes = vec![0u8; 20];
        bytes[0..12].copy_from_slice(b"C64-TAPE-RAW");
        bytes[12] = 0; // version 0
        bytes.extend_from_slice(pulse_bytes);
        c64_tape::parse_tap(&bytes).unwrap()
    }

    fn attaching_a_tape_asserts_the_real_sense_line_low() {
        let mut c64 = test_c64();
        assert_eq!(
            c64.bus.peek(0x0001) & 0x10,
            0x10,
            "SENSE floats high with no tape attached"
        );
        c64.load_tape(build_tap(&[10, 10]));
        assert_eq!(
            c64.bus.peek(0x0001) & 0x10,
            0,
            "a real Datasette sensed as attached reads SENSE low"
        );
    }

    fn ejecting_the_tape_releases_sense_and_idles_the_read_line() {
        let mut c64 = test_c64();
        c64.load_tape(build_tap(&[10, 10]));
        assert_eq!(c64.bus.peek(0x0001) & 0x10, 0);
        c64.eject_tape();
        assert_eq!(c64.bus.peek(0x0001) & 0x10, 0x10, "SENSE floats high again");
        assert!(c64.bus.tape.is_none());
        // Ejecting an empty deck is harmless.
        c64.eject_tape();
    }
    /// Merged (every check kept): `attaching_a_tape_asserts_the_real_sense_line_low`, `ejecting_the_tape_releases_sense_and_idles_the_read_line`.
    #[test]
    fn tape_sense_line() {
        attaching_a_tape_asserts_the_real_sense_line_low();
        ejecting_the_tape_releases_sense_and_idles_the_read_line();
    }

    fn writing_motor_bit_low_as_output_starts_real_tape_playback() {
        let mut c64 = test_c64();
        c64.load_tape(build_tap(&[10, 10]));
        // Real, confirmed-against-VICE-source hardware fact (see
        // `apply_tape_motor`'s own doc comment): with DDR/data both at
        // their real power-on-reset default of `0`, the motor formula
        // `(dir & data) & 0x20 == 0` reads ON -- the *only* combination
        // that reads OFF is bit 5 driven `1` *as an output*. Real KERNAL
        // init code's own very first job (`LDA #$2F/STA $00`, `LDA
        // #$37/STA $01`) is exactly to establish that OFF combination
        // before anything else runs.
        assert!(
            c64.bus.tape.as_ref().unwrap().playback_state().playing,
            "the real hardware-reset default (dir=0, data=0) reads motor ON"
        );

        c64.bus.write(0x0000, 0x20); // DDR: bit 5 now output
        c64.bus.write(0x0001, 0x20); // driven high -> the one real combination that stops it
        assert!(
            !c64.bus.tape.as_ref().unwrap().playback_state().playing,
            "bit 5 driven high as output is the one real combination that stops the motor"
        );

        c64.bus.write(0x0001, 0x00); // driven low again -> motor on
        assert!(
            c64.bus.tape.as_ref().unwrap().playback_state().playing,
            "bit 5 driven low as output starts the real motor"
        );

        c64.bus.write(0x0000, 0x00); // DDR back to input -- reads ON again by the same formula
        assert!(
            c64.bus.tape.as_ref().unwrap().playback_state().playing,
            "bit 5 left as input reads ON by the same real formula"
        );
    }

    fn a_real_tape_pulse_boundary_fires_cia1s_flag_interrupt() {
        let mut c64 = test_c64_with_vectors(0x1234, 0x5678);
        load_nop_loop(&mut c64);
        // Two 80-cycle pulses (byte value 10 * 8) -- the real negative
        // edge (high -> low) lands at cycle 80, the first transition.
        c64.load_tape(build_tap(&[10, 10]));
        c64.bus.write(0x0000, 0x20); // DDR: motor bit output
        c64.bus.write(0x0001, 0x00); // driven low -> motor on, tape playing
        c64.bus.write(0xDC0D, 0x80 | 0x10); // enable CIA1's FLAG interrupt

        let mut landed = None;
        for _ in 0..40 {
            c64.step();
            if !matches!(c64.pc(), 0x0300..=0x0303) {
                landed = Some(c64.pc());
                break;
            }
        }
        assert_eq!(
            landed,
            Some(0x1234),
            "the real tape pulse's negative edge must reach CIA1's FLAG interrupt"
        );
    }

    fn tape_speed_multiplier_is_a_no_op_with_no_tape_installed() {
        let mut c64 = test_c64();
        c64.set_tape_speed(10); // must not panic with nothing attached
    }
    /// Merged (every check kept): `writing_motor_bit_low_as_output_starts_real_tape_playback`, `a_real_tape_pulse_boundary_fires_cia1s_flag_interrupt`, `tape_speed_multiplier_is_a_no_op_with_no_tape_installed`.
    #[test]
    fn tape_playback() {
        writing_motor_bit_low_as_output_starts_real_tape_playback();
        a_real_tape_pulse_boundary_fires_cia1s_flag_interrupt();
        tape_speed_multiplier_is_a_no_op_with_no_tape_installed();
    }

    /// A KERNAL ROM whose `$F49E` (the real LOAD entry point, confirmed
    /// live against every committed ROM -- see `KERNAL_LOAD_ENTRY`'s own
    /// doc comment) actually holds the real `STX $C3` opcode the trap's
    /// own guard checks for, everywhere else `0xEA` (NOP, inert if ever
    /// executed) filled -- same "distinct, known" shape `test_c64_with_
    /// vectors` already established for a different address pair.
    fn test_c64_with_real_load_entry() -> C64 {
        let mut kernal = vec![0xEAu8; KERNAL_ROM_SIZE];
        let off = (KERNAL_LOAD_ENTRY - 0xE000) as usize;
        kernal[off] = KERNAL_LOAD_ENTRY_FIRST_BYTES[0];
        kernal[off + 1] = KERNAL_LOAD_ENTRY_FIRST_BYTES[1];
        C64::new(kernal, vec![0; BASIC_ROM_SIZE], vec![0; CHAR_ROM_SIZE]).unwrap()
    }

    /// Real 6502 `JSR`'s own push convention: pushes the address of its
    /// own *last* byte (`return_addr - 1`), high byte first. Simulates
    /// having already been `JSR`'d into `KERNAL_LOAD_ENTRY` from some
    /// caller expecting to resume at `return_addr` -- exactly what
    /// `try_fast_kernal_load_trap`'s own `simulate_rts` must unwind
    /// correctly.
    fn call_kernal_load(c64: &mut C64, return_addr: u16) {
        let target_minus_one = return_addr.wrapping_sub(1);
        let sp = c64.cpu.sp;
        c64.bus
            .write(0x0100 + sp as u16, (target_minus_one >> 8) as u8);
        c64.bus
            .write(0x0100 + sp.wrapping_sub(1) as u16, target_minus_one as u8);
        c64.cpu.sp = sp.wrapping_sub(2);
        c64.cpu.pc = KERNAL_LOAD_ENTRY;
    }

    /// Builds a minimal, single-sector-file synthetic `.d64` (35-track,
    /// no error-info channel): a real BAM/directory pointing at one closed
    /// PRG entry named `name`, whose own data sector (track 1, sector 0 --
    /// an arbitrary but always-in-range choice this reader has no reason
    /// to reject, since reading a file never consults the BAM's own
    /// allocation bitmap on real hardware either) holds `load_addr`'s
    /// real 2-byte little-endian header followed by `payload`.
    fn build_test_disk(name: &[u8], load_addr: u16, payload: &[u8]) -> d64::D64 {
        const SECTOR_SIZE: usize = 256;
        fn sectors_per_track(t: u8) -> usize {
            match t {
                1..=17 => 21,
                18..=24 => 19,
                25..=30 => 18,
                31..=35 => 17,
                _ => 0,
            }
        }
        fn track_sector_offset(track: u8, sector: u8) -> usize {
            let mut idx = 0usize;
            for t in 1..track {
                idx += sectors_per_track(t);
            }
            idx += sector as usize;
            idx * SECTOR_SIZE
        }

        let total_sectors: usize = (1..=35).map(sectors_per_track).sum();
        let mut bytes = vec![0u8; total_sectors * SECTOR_SIZE];

        let bam_off = track_sector_offset(18, 0);
        bytes[bam_off] = 18; // directory chain link: track
        bytes[bam_off + 1] = 1; // directory chain link: sector

        let dir_off = track_sector_offset(18, 1);
        bytes[dir_off] = 0; // end of directory chain
        bytes[dir_off + 1] = 0xFF;
        bytes[dir_off + 2] = 0x80 | 0x02; // closed, kind=PRG (real slot-relative +2 offset)
        bytes[dir_off + 3] = 1; // file's own first data track
        bytes[dir_off + 4] = 0; // file's own first data sector
        let name_off = dir_off + 5;
        bytes[name_off..name_off + 16].fill(0xA0);
        bytes[name_off..name_off + name.len()].copy_from_slice(name);

        let data_off = track_sector_offset(1, 0);
        let mut file_bytes = load_addr.to_le_bytes().to_vec();
        file_bytes.extend_from_slice(payload);
        assert!(
            file_bytes.len() <= 254,
            "test helper only supports a single-sector file"
        );
        bytes[data_off] = 0; // end of chain
        bytes[data_off + 1] = (2 + file_bytes.len() - 1) as u8; // last valid offset, inclusive
        bytes[data_off + 2..data_off + 2 + file_bytes.len()].copy_from_slice(&file_bytes);

        d64::D64::parse(&bytes).unwrap()
    }

    /// Same synthetic-disk shape as `build_test_disk`, but for `.d71`:
    /// side 1's BAM/directory sectors (track 18) are byte-identical to a
    /// `.d64`'s own, so this deliberately places the file's own data on a
    /// side-2 track (40) instead of reusing `build_test_disk`'s track 1 --
    /// proving the trap genuinely reaches the wider real track range a
    /// `.d64` could never have, not just that d71 parsing alone works.
    fn build_test_disk_d71(name: &[u8], load_addr: u16, payload: &[u8]) -> d71::D71 {
        const SECTOR_SIZE: usize = 256;
        fn track_sector_offset(track: u8, sector: u8) -> usize {
            let mut idx = 0usize;
            for t in 1..track {
                idx += d71::sectors_per_track(t) as usize;
            }
            idx += sector as usize;
            idx * SECTOR_SIZE
        }
        let total_sectors: usize = (1..=70).map(|t| d71::sectors_per_track(t) as usize).sum();
        let mut bytes = vec![0u8; total_sectors * SECTOR_SIZE];

        let bam_off = track_sector_offset(18, 0);
        bytes[bam_off] = 18;
        bytes[bam_off + 1] = 1;

        let dir_off = track_sector_offset(18, 1);
        bytes[dir_off] = 0;
        bytes[dir_off + 1] = 0xFF;
        bytes[dir_off + 2] = 0x80 | 0x02;
        bytes[dir_off + 3] = 40; // side-2 track -- see this fn's own doc comment
        bytes[dir_off + 4] = 0;
        let name_off = dir_off + 5;
        bytes[name_off..name_off + 16].fill(0xA0);
        bytes[name_off..name_off + name.len()].copy_from_slice(name);

        let data_off = track_sector_offset(40, 0);
        let mut file_bytes = load_addr.to_le_bytes().to_vec();
        file_bytes.extend_from_slice(payload);
        assert!(
            file_bytes.len() <= 254,
            "test helper only supports a single-sector file"
        );
        bytes[data_off] = 0;
        bytes[data_off + 1] = (2 + file_bytes.len() - 1) as u8;
        bytes[data_off + 2..data_off + 2 + file_bytes.len()].copy_from_slice(&file_bytes);

        d71::D71::parse(&bytes).unwrap()
    }

    /// Same shape again, for `.d81` -- a real, genuinely different header/
    /// BAM sector (track 40 sector 0, not track 18) and a uniform 40-
    /// sectors/track geometry, see `d81`'s own module doc comment. The
    /// file's data sector (track 41) is past `.d71`'s own 70-track range,
    /// same "prove the wider real range is reachable" reasoning.
    fn build_test_disk_d81(name: &[u8], load_addr: u16, payload: &[u8]) -> d81::D81 {
        const SECTOR_SIZE: usize = 256;
        fn track_sector_offset(track: u8, sector: u8) -> usize {
            (track as usize - 1) * d81::SECTORS_PER_TRACK as usize + sector as usize
        }
        let total_sectors = d81::TRACK_COUNT as usize * d81::SECTORS_PER_TRACK as usize;
        let mut bytes = vec![0u8; total_sectors * SECTOR_SIZE];

        let header_off = track_sector_offset(40, 0) * SECTOR_SIZE;
        bytes[header_off] = 40; // directory chain link: track
        bytes[header_off + 1] = 3; // directory chain link: sector

        let dir_off = track_sector_offset(40, 3) * SECTOR_SIZE;
        bytes[dir_off] = 0;
        bytes[dir_off + 1] = 0xFF;
        bytes[dir_off + 2] = 0x80 | 0x02;
        bytes[dir_off + 3] = 41;
        bytes[dir_off + 4] = 0;
        let name_off = dir_off + 5;
        bytes[name_off..name_off + 16].fill(0xA0);
        bytes[name_off..name_off + name.len()].copy_from_slice(name);

        let data_off = track_sector_offset(41, 0) * SECTOR_SIZE;
        let mut file_bytes = load_addr.to_le_bytes().to_vec();
        file_bytes.extend_from_slice(payload);
        assert!(
            file_bytes.len() <= 254,
            "test helper only supports a single-sector file"
        );
        bytes[data_off] = 0;
        bytes[data_off + 1] = (2 + file_bytes.len() - 1) as u8;
        bytes[data_off + 2..data_off + 2 + file_bytes.len()].copy_from_slice(&file_bytes);

        d81::D81::parse(&bytes).unwrap()
    }

    fn fast_kernal_load_trap_services_a_real_disk_load_directly() {
        let mut c64 = test_c64_with_real_load_entry();
        c64.load_disk(build_test_disk(b"HELLO", 0xC000, &[0xA9, 0x42, 0x60]));

        let name = b"HELLO";
        for (i, &b) in name.iter().enumerate() {
            c64.bus.write(0x0300 + i as u16, b);
        }
        c64.bus.write(ZP_FNLEN, name.len() as u8);
        c64.bus.write(ZP_FNADR_LO, 0x00);
        c64.bus.write(ZP_FNADR_HI, 0x03);
        c64.bus.write(ZP_SA, 1); // use the file's own embedded load address
        c64.bus.write(ZP_FA, 8); // a real disk device number
        c64.cpu.a = 0; // LOAD, not VERIFY

        call_kernal_load(&mut c64, 0xC050);
        c64.step();

        assert_eq!(
            c64.pc(),
            0xC050,
            "must resume exactly at the real caller's own return address"
        );
        assert_eq!(c64.bus.peek(0xC000), 0xA9);
        assert_eq!(c64.bus.peek(0xC001), 0x42);
        assert_eq!(c64.bus.peek(0xC002), 0x60);
        assert_eq!(c64.cpu.p & 0x01, 0, "carry clear = success");
        assert_eq!(c64.cpu.x, 0x03, "X = end address low (one past last byte)");
        assert_eq!(c64.cpu.y, 0xC0, "Y = end address high");
        assert_eq!(c64.bus.peek(ZP_EAL), 0x03);
        assert_eq!(c64.bus.peek(ZP_EAH), 0xC0);
    }

    /// Proves the fast-loader trap genuinely works through `AttachedDisk`'s
    /// own abstraction for a `.d71`, not just that `.d64` still works --
    /// the file's own data sector lives on track 40, a real side-2 track
    /// no `.d64` image could ever have offered.
    fn fast_kernal_load_trap_services_a_real_d71_disk_load_directly() {
        let mut c64 = test_c64_with_real_load_entry();
        c64.load_disk_d71(build_test_disk_d71(b"HELLO71", 0xC100, &[0xA9, 0x43, 0x60]));

        let name = b"HELLO71";
        for (i, &b) in name.iter().enumerate() {
            c64.bus.write(0x0300 + i as u16, b);
        }
        c64.bus.write(ZP_FNLEN, name.len() as u8);
        c64.bus.write(ZP_FNADR_LO, 0x00);
        c64.bus.write(ZP_FNADR_HI, 0x03);
        c64.bus.write(ZP_SA, 1);
        c64.bus.write(ZP_FA, 9);
        c64.cpu.a = 0;

        call_kernal_load(&mut c64, 0xC050);
        c64.step();

        assert_eq!(c64.pc(), 0xC050);
        assert_eq!(c64.bus.peek(0xC100), 0xA9);
        assert_eq!(c64.bus.peek(0xC101), 0x43);
        assert_eq!(c64.bus.peek(0xC102), 0x60);
        assert_eq!(c64.cpu.p & 0x01, 0, "carry clear = success");
    }

    /// Same, for a `.d81` -- its file's data sector (track 41) is past
    /// `.d71`'s own 70-track range, and its header/BAM sector lives at a
    /// real, different location (track 40) than either `.d64`/`.d71`'s
    /// own track-18 BAM.
    fn fast_kernal_load_trap_services_a_real_d81_disk_load_directly() {
        let mut c64 = test_c64_with_real_load_entry();
        c64.load_disk_d81(build_test_disk_d81(b"HELLO81", 0xC200, &[0xA9, 0x44, 0x60]));

        let name = b"HELLO81";
        for (i, &b) in name.iter().enumerate() {
            c64.bus.write(0x0300 + i as u16, b);
        }
        c64.bus.write(ZP_FNLEN, name.len() as u8);
        c64.bus.write(ZP_FNADR_LO, 0x00);
        c64.bus.write(ZP_FNADR_HI, 0x03);
        c64.bus.write(ZP_SA, 1);
        c64.bus.write(ZP_FA, 10);
        c64.cpu.a = 0;

        call_kernal_load(&mut c64, 0xC050);
        c64.step();

        assert_eq!(c64.pc(), 0xC050);
        assert_eq!(c64.bus.peek(0xC200), 0xA9);
        assert_eq!(c64.bus.peek(0xC201), 0x44);
        assert_eq!(c64.bus.peek(0xC202), 0x60);
        assert_eq!(c64.cpu.p & 0x01, 0, "carry clear = success");
    }
    /// Merged (every check kept): `fast_kernal_load_trap_services_a_real_disk_load_directly`, `fast_kernal_load_trap_services_a_real_d71_disk_load_directly`, `fast_kernal_load_trap_services_a_real_d81_disk_load_directly`.
    #[test]
    fn fast_load_trap_per_format() {
        fast_kernal_load_trap_services_a_real_disk_load_directly();
        fast_kernal_load_trap_services_a_real_d71_disk_load_directly();
        fast_kernal_load_trap_services_a_real_d81_disk_load_directly();
    }

    fn fast_kernal_load_trap_uses_the_callers_own_address_when_sa_is_zero() {
        let mut c64 = test_c64_with_real_load_entry();
        // The file's own embedded header address ($C000) must be ignored
        // in favor of the caller's given X/Y when SA == 0 -- the real
        // "ignore header, use a manually given address" KERNAL convention.
        c64.load_disk(build_test_disk(b"HELLO", 0xC000, &[0x11, 0x22]));
        for (i, &b) in b"HELLO".iter().enumerate() {
            c64.bus.write(0x0300 + i as u16, b);
        }
        c64.bus.write(ZP_FNLEN, 5);
        c64.bus.write(ZP_FNADR_LO, 0x00);
        c64.bus.write(ZP_FNADR_HI, 0x03);
        c64.bus.write(ZP_SA, 0);
        c64.bus.write(ZP_FA, 8);
        c64.cpu.a = 0;
        c64.cpu.x = 0x00;
        c64.cpu.y = 0x08; // caller-given address $0800

        call_kernal_load(&mut c64, 0xC050);
        c64.step();

        assert_eq!(c64.bus.peek(0x0800), 0x11);
        assert_eq!(c64.bus.peek(0x0801), 0x22);
        assert_eq!(
            c64.bus.peek(0xC000),
            0,
            "must NOT have loaded at the file's own header address"
        );
    }

    fn fast_kernal_load_trap_wildcard_loads_the_first_program() {
        let mut c64 = test_c64_with_real_load_entry();
        c64.load_disk(build_test_disk(b"ANYTHING", 0xC000, &[0x42]));
        c64.bus.write(0x0300, b'*');
        c64.bus.write(ZP_FNLEN, 1);
        c64.bus.write(ZP_FNADR_LO, 0x00);
        c64.bus.write(ZP_FNADR_HI, 0x03);
        c64.bus.write(ZP_SA, 1);
        c64.bus.write(ZP_FA, 8);
        c64.cpu.a = 0;

        call_kernal_load(&mut c64, 0xC050);
        c64.step();

        assert_eq!(c64.bus.peek(0xC000), 0x42);
        assert_eq!(c64.cpu.p & 0x01, 0);
    }
    /// Merged (every check kept): `fast_kernal_load_trap_uses_the_callers_own_address_when_sa_is_zero`, `fast_kernal_load_trap_wildcard_loads_the_first_program`.
    #[test]
    fn fast_load_addressing() {
        fast_kernal_load_trap_uses_the_callers_own_address_when_sa_is_zero();
        fast_kernal_load_trap_wildcard_loads_the_first_program();
    }

    fn fast_kernal_load_trap_reports_the_real_file_not_found_error() {
        let mut c64 = test_c64_with_real_load_entry();
        c64.load_disk(build_test_disk(b"HELLO", 0xC000, &[0x42]));
        for (i, &b) in b"NOPE".iter().enumerate() {
            c64.bus.write(0x0300 + i as u16, b);
        }
        c64.bus.write(ZP_FNLEN, 4);
        c64.bus.write(ZP_FNADR_LO, 0x00);
        c64.bus.write(ZP_FNADR_HI, 0x03);
        c64.bus.write(ZP_SA, 1);
        c64.bus.write(ZP_FA, 8);
        c64.cpu.a = 0;

        call_kernal_load(&mut c64, 0xC050);
        c64.step();

        assert_eq!(c64.pc(), 0xC050);
        assert_eq!(c64.cpu.p & 0x01, 0x01, "carry set = error");
        assert_eq!(c64.cpu.a, 4, "FILE NOT FOUND");
    }

    /// A formatted, empty 35-track `.d64` (BAM with every sector free except
    /// 18/0 and 18/1, empty first directory sector).
    fn formatted_test_disk() -> d64::D64 {
        let mut bytes = vec![0u8; 683 * 256];
        let per_track = |t: u8| -> usize {
            match t {
                1..=17 => 21,
                18..=24 => 19,
                25..=30 => 18,
                _ => 17,
            }
        };
        let bam_off: usize = (1..18u8).map(per_track).sum::<usize>() * 256;
        bytes[bam_off] = 18;
        bytes[bam_off + 1] = 1;
        for t in 1..=35u8 {
            let base = bam_off + 4 + (t as usize - 1) * 4;
            let mut free = per_track(t);
            for s in 0..per_track(t) {
                bytes[base + 1 + s / 8] |= 1 << (s % 8);
            }
            if t == 18 {
                bytes[base + 1] &= !0b11; // sectors 0 and 1 are in use
                free -= 2;
            }
            bytes[base] = free as u8;
        }
        bytes[bam_off + 256 + 1] = 0xFF;
        d64::D64::parse(&bytes).unwrap()
    }

    fn test_c64_with_real_save_entry() -> C64 {
        let mut kernal = vec![0xEAu8; KERNAL_ROM_SIZE];
        let off = (KERNAL_SAVE_ENTRY - 0xE000) as usize;
        kernal[off] = KERNAL_SAVE_ENTRY_FIRST_BYTES[0];
        kernal[off + 1] = KERNAL_SAVE_ENTRY_FIRST_BYTES[1];
        C64::new(kernal, vec![0; BASIC_ROM_SIZE], vec![0; CHAR_ROM_SIZE]).unwrap()
    }

    /// Calls SAVE the way BASIC does: name at `$0300`, start pointer in zero
    /// page at `$FB`, A = `$FB`, X/Y = end address.
    fn call_kernal_save(c64: &mut C64, name: &[u8], start: u16, end: u16) {
        for (i, &b) in name.iter().enumerate() {
            c64.bus.write(0x0300 + i as u16, b);
        }
        c64.bus.write(ZP_FNLEN, name.len() as u8);
        c64.bus.write(ZP_FNADR_LO, 0x00);
        c64.bus.write(ZP_FNADR_HI, 0x03);
        c64.bus.write(ZP_FA, 8);
        c64.bus.write(0xFB, start as u8);
        c64.bus.write(0xFC, (start >> 8) as u8);
        c64.cpu.a = 0xFB;
        c64.cpu.x = end as u8;
        c64.cpu.y = (end >> 8) as u8;
        let ret = 0xC050u16 - 1;
        let sp = c64.cpu.sp;
        c64.bus.write(0x0100 + sp as u16, (ret >> 8) as u8);
        c64.bus.write(0x0100 + sp.wrapping_sub(1) as u16, ret as u8);
        c64.cpu.sp = sp.wrapping_sub(2);
        c64.cpu.pc = KERNAL_SAVE_ENTRY;
        c64.step();
    }

    fn fast_kernal_save_trap_stores_a_program_that_the_load_trap_reads_back() {
        let mut c64 = test_c64_with_real_save_entry();
        c64.load_disk(formatted_test_disk());
        for (i, byte) in (0..600u32).map(|i| (i * 7) as u8).enumerate() {
            c64.bus.write(0x4000 + i as u16, byte);
        }
        assert!(!c64.disk_modified());
        call_kernal_save(&mut c64, b"SAVED", 0x4000, 0x4000 + 600);
        assert_eq!(c64.pc(), 0xC050, "resumes at the caller");
        assert_eq!(c64.cpu.p & 0x01, 0, "carry clear = success");
        assert!(c64.disk_modified());

        let image = c64.take_modified_disk_image().expect("modified");
        let disk = d64::D64::parse(&image).unwrap();
        let entries = disk.directory_entries();
        let entry = d64::D64::find_program(&entries, b"SAVED").unwrap();
        let file = disk.read_file(entry).unwrap();
        assert_eq!(&file[..2], &[0x00, 0x40], "load address first");
        let expected: Vec<u8> = (0..600u32).map(|i| (i * 7) as u8).collect();
        assert_eq!(&file[2..], &expected[..]);

        c64.mark_disk_saved();
        assert!(c64.take_modified_disk_image().is_none());
    }

    fn fast_kernal_save_trap_only_replaces_with_the_at_prefix() {
        let mut c64 = test_c64_with_real_save_entry();
        c64.load_disk(formatted_test_disk());
        c64.bus.write(0x4000, 1);
        call_kernal_save(&mut c64, b"GAME", 0x4000, 0x4001);
        c64.mark_disk_saved();

        c64.bus.write(0x4000, 2);
        call_kernal_save(&mut c64, b"GAME", 0x4000, 0x4001);
        assert!(
            !c64.disk_modified(),
            "a plain re-save is refused by the drive"
        );

        call_kernal_save(&mut c64, b"@0:GAME", 0x4000, 0x4001);
        assert!(c64.disk_modified());
        let disk = d64::D64::parse(&c64.disk_image().unwrap()).unwrap();
        let entries = disk.directory_entries();
        assert_eq!(entries.len(), 1);
        let file = disk.read_file(&entries[0]).unwrap();
        assert_eq!(file, vec![0x00, 0x40, 2]);
    }

    /// A formatted 1581 image (everything free but the header, the two BAM
    /// sectors and the first directory sector).
    fn formatted_test_d81() -> d81::D81 {
        use cbm_dos::Image;
        let mut disk = d81::D81::parse(&vec![0u8; 80 * 40 * 256]).unwrap();
        let mut header = [0u8; 256];
        header[0] = 40;
        header[1] = 3;
        disk.write_sector(40, 0, &header).unwrap();
        for track in 1..=80u8 {
            for sector in 0..40u8 {
                disk.mark(track, sector, true);
            }
        }
        for sector in 0..=3u8 {
            disk.mark(40, sector, false);
        }
        let mut dir = [0u8; 256];
        dir[1] = 0xFF;
        disk.write_sector(40, 3, &dir).unwrap();
        disk
    }

    fn fast_kernal_save_trap_reports_a_missing_file_name() {
        let mut c64 = test_c64_with_real_save_entry();
        c64.load_disk(formatted_test_disk());
        call_kernal_save(&mut c64, b"", 0x4000, 0x4001);
        assert_eq!(c64.cpu.p & 0x01, 0x01);
        assert_eq!(c64.cpu.a, 8, "MISSING FILE NAME");
    }
    /// Merged (every check kept): `fast_kernal_save_trap_only_replaces_with_the_at_prefix`, `fast_kernal_save_trap_reports_a_missing_file_name`.
    #[test]
    fn fast_save_trap_rules() {
        fast_kernal_save_trap_only_replaces_with_the_at_prefix();
        fast_kernal_save_trap_reports_a_missing_file_name();
    }

    fn fast_kernal_save_trap_writes_to_d81_and_d71_images_too() {
        let mut c64 = test_c64_with_real_save_entry();
        c64.load_disk_d81(formatted_test_d81());
        c64.bus.write(0x4000, 0x99);
        call_kernal_save(&mut c64, b"ON81", 0x4000, 0x4001);
        assert_eq!(c64.pc(), 0xC050);
        assert!(c64.disk_modified());
        let disk = d81::D81::parse(&c64.disk_image().unwrap()).unwrap();
        let entries = disk.directory_entries();
        let entry = d81::D81::find_program(&entries, b"ON81").unwrap();
        assert_eq!(disk.read_file(entry).unwrap(), vec![0x00, 0x40, 0x99]);
    }
    /// Merged (every check kept): `fast_kernal_save_trap_stores_a_program_that_the_load_trap_reads_back`, `fast_kernal_save_trap_writes_to_d81_and_d71_images_too`.
    #[test]
    fn fast_save_trap() {
        fast_kernal_save_trap_stores_a_program_that_the_load_trap_reads_back();
        fast_kernal_save_trap_writes_to_d81_and_d71_images_too();
    }

    fn fast_kernal_load_trap_does_not_engage_below_the_real_disk_device_range() {
        let mut c64 = test_c64_with_real_load_entry();
        c64.load_disk(build_test_disk(b"HELLO", 0xC000, &[0x42]));
        c64.bus.write(ZP_FA, 1); // a tape device, not disk
        c64.cpu.a = 0;
        c64.cpu.pc = KERNAL_LOAD_ENTRY;

        assert!(
            !c64.try_fast_kernal_load_trap(),
            "device numbers below 8 must fall through to real (unmodified) KERNAL execution"
        );
    }

    fn fast_kernal_load_trap_does_not_engage_for_verify() {
        let mut c64 = test_c64_with_real_load_entry();
        c64.load_disk(build_test_disk(b"HELLO", 0xC000, &[0x42]));
        c64.bus.write(ZP_FA, 8);
        c64.cpu.a = 1; // VERIFY, not LOAD
        c64.cpu.pc = KERNAL_LOAD_ENTRY;

        assert!(
            !c64.try_fast_kernal_load_trap(),
            "VERIFY is an explicit, documented scope cut -- must fall through"
        );
    }

    fn fast_kernal_load_trap_does_not_engage_without_a_disk_attached() {
        let mut c64 = test_c64_with_real_load_entry();
        c64.bus.write(ZP_FA, 8);
        c64.cpu.a = 0;
        c64.cpu.pc = KERNAL_LOAD_ENTRY;

        assert!(!c64.try_fast_kernal_load_trap());
    }
    /// Merged (every check kept): `fast_kernal_load_trap_reports_the_real_file_not_found_error`, `fast_kernal_load_trap_does_not_engage_below_the_real_disk_device_range`, `fast_kernal_load_trap_does_not_engage_for_verify`, `fast_kernal_load_trap_does_not_engage_without_a_disk_attached`.
    #[test]
    fn fast_load_refusals() {
        fast_kernal_load_trap_reports_the_real_file_not_found_error();
        fast_kernal_load_trap_does_not_engage_below_the_real_disk_device_range();
        fast_kernal_load_trap_does_not_engage_for_verify();
        fast_kernal_load_trap_does_not_engage_without_a_disk_attached();
    }

    fn an_unacknowledged_cia2_interrupt_is_one_nmi_not_one_per_instruction() {
        let mut c64 = test_c64();
        // CIA2 timer A: latch 1, continuous, NMI on underflow enabled.
        c64.bus.write(0xDD04, 1);
        c64.bus.write(0xDD05, 0);
        c64.bus.write(0xDD0D, 0x81);
        c64.bus.write(0xDD0E, 0x01);
        // KERNAL stub is $AA everywhere (JAM-like); the NMI vector is what we
        // watch: count how many times an NMI is requested while the ICR is
        // never read.
        let mut edges = 0;
        for _ in 0..50 {
            let level = c64.bus.cia2.tick(2);
            if c64.bus.cia2_nmi_edge(level) {
                edges += 1;
            }
        }
        assert_eq!(edges, 1, "the line stays asserted until the ICR is read");
        let icr = c64.bus.read(0xDD0D); // acknowledge
        assert_eq!(icr & 0x81, 0x81, "the ICR showed the pending source");
        assert!(
            !c64.bus.cia2.interrupt_asserted(),
            "reading the ICR releases the line"
        );
        let level = c64.bus.cia2.tick(2);
        assert!(
            c64.bus.cia2_nmi_edge(level),
            "a new underflow is a new edge"
        );
    }
    /// Merged (every check kept): `cia2_timer_irq_reaches_the_cpu_as_a_real_nmi_not_an_irq`, `an_unacknowledged_cia2_interrupt_is_one_nmi_not_one_per_instruction`.
    #[test]
    fn cia2_nmi() {
        cia2_timer_irq_reaches_the_cpu_as_a_real_nmi_not_an_irq();
        an_unacknowledged_cia2_interrupt_is_one_nmi_not_one_per_instruction();
    }
}
