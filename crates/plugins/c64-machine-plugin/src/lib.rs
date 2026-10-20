use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use c64::{C64, C64Key, JoystickButton, MouseMode, PaddleDirection, SidModel, VicAccuracy};
use machine::{AUDIO_SAMPLE_RATE, Machine, Region};
use plugin_api::{
    ABI_MAJOR_V1, ABI_MINOR_V1, AddressSpaceInfo, BinaryBlob, BitmapExplorerDefaults,
    BitmapPaletteColor, ChipSnapshot, CodecError, CpuFamily, DebugCommand, DebugResponse,
    DebugValue, DisassemblyLine, ExecutionMode, MACHINE_PROFILE_CONTRACT_VERSION,
    MachineCapability, MachineCommand, MachineCreateRequest, MachineCreateResponse,
    MachineInstanceCallRequest, MachineInstanceCallResponse, MachineInstanceHandle,
    MachineMetadata, MachineOptionInfo, MachineOptionKind, MachinePluginVTableV1, MachineProfile,
    MachineResponse, MediaKind, MediaSnapshot, NamedValue, PluginDescriptorV1, PluginKind,
    REQUIRED_MACHINE_DEBUG_CAPABILITIES, RcBuffer, RcCallResult, RcSlice, RcStatus, RegisterField,
    RegisterSnapshot, StatusCode,
};

const PLUGIN_ID: &[u8] = b"retroclawd.c64.machine";
const PLUGIN_VERSION: &[u8] = build_info::version_tag!().as_bytes();

const KERNAL_ROM_SIZE: usize = 0x2000;
const BASIC_ROM_SIZE: usize = 0x2000;
const CHAR_ROM_SIZE: usize = 0x1000;

/// `MachineMetadata::frame_rate`: one raster frame exactly (the host paces
/// and budgets by it instead of the rounded `frame_hz`).
fn exact_frame_rate(machine: &C64) -> (u32, u32) {
    let rate = Machine::frame_rate(machine);
    (rate.num, rate.den)
}

fn bitmap_palette() -> Vec<BitmapPaletteColor> {
    [
        ("black", (0x00, 0x00, 0x00)),
        ("white", (0xFF, 0xFF, 0xFF)),
        ("red", (0x88, 0x00, 0x00)),
        ("cyan", (0xAA, 0xFF, 0xEE)),
        ("purple", (0xCC, 0x44, 0xCC)),
        ("green", (0x00, 0xCC, 0x55)),
        ("blue", (0x00, 0x00, 0xAA)),
        ("yellow", (0xEE, 0xEE, 0x77)),
        ("orange", (0xDD, 0x88, 0x55)),
        ("brown", (0x66, 0x44, 0x00)),
        ("light red", (0xFF, 0x77, 0x77)),
        ("dark grey", (0x33, 0x33, 0x33)),
        ("grey", (0x77, 0x77, 0x77)),
        ("light green", (0xAA, 0xFF, 0x66)),
        ("light blue", (0x00, 0x88, 0xFF)),
        ("light grey", (0xBB, 0xBB, 0xBB)),
    ]
    .into_iter()
    .map(|(name, (r, g, b))| BitmapPaletteColor {
        name: name.to_string(),
        rgb: [r, g, b],
    })
    .collect()
}

fn bitmap_defaults() -> BitmapExplorerDefaults {
    BitmapExplorerDefaults {
        address: 0x0400,
        bytes_width: 40,
        bitplanes: 1,
        zoom: 2,
    }
}

const fn static_slice(bytes: &'static [u8]) -> RcSlice {
    RcSlice {
        ptr: bytes.as_ptr(),
        len: bytes.len(),
    }
}

struct StaticPluginDescriptor(PluginDescriptorV1);

// SAFETY: descriptor and vtable refer only to immutable static data/function pointers.
unsafe impl Sync for StaticPluginDescriptor {}

static STATE: OnceLock<Mutex<PluginState>> = OnceLock::new();

static MACHINE_VTABLE_V1: MachinePluginVTableV1 = MachinePluginVTableV1 {
    size: MachinePluginVTableV1::expected_size(),
    list_profiles: plugin_list_profiles_v1,
    create_instance: plugin_create_instance_v1,
    destroy_instance: plugin_destroy_instance_v1,
    instance_call: plugin_instance_call_v1,
    free_buffer: plugin_free_buffer_v1,
    step: plugin_step_v1,
    halted: plugin_halted_v1,
    pc: plugin_pc_v1,
    pc_wide: plugin_pc_wide_v1,
};

static PLUGIN_DESCRIPTOR_V1: StaticPluginDescriptor = StaticPluginDescriptor(PluginDescriptorV1 {
    abi_major: ABI_MAJOR_V1,
    abi_minor: ABI_MINOR_V1,
    kind: PluginKind::Machine,
    id: static_slice(PLUGIN_ID),
    version: static_slice(PLUGIN_VERSION),
    vtable: (&MACHINE_VTABLE_V1 as *const MachinePluginVTableV1).cast(),
});

use drive_link as drive;

/// The C64's end of the IEC bus for `drive::DriveLink`.
struct IecHost<'a>(&'a mut C64);

impl drive::IecHost for IecHost<'_> {
    fn iec_output(&self) -> (bool, bool, bool) {
        self.0.iec_output()
    }
    fn set_iec_input(&mut self, clock: bool, data: bool) {
        self.0.set_iec_input(clock, data);
    }
    fn clock_hz(&self) -> u64 {
        Machine::clock_hz(self.0)
    }
}
mod net;

struct PluginState {
    next_instance: MachineInstanceHandle,
    instances: HashMap<MachineInstanceHandle, InstanceRecord>,
}

struct InstanceRecord {
    machine: C64,
    region: String,
    paused: bool,
    breakpoints: Vec<u64>,
    /// The `run` create-time option (see `machine_profile`) -- gates
    /// whether `AttachMedia`'s `Program`/`.t64` arms synthesize BASIC's
    /// own VARTAB/ARYTAB/STREND pointers after a raw poke (see
    /// `relink_basic_program`'s own doc comment for why this is opt-in,
    /// not automatic for every load at `$0801`).
    auto_run: bool,
    /// A BASIC program (`$0801`) waiting for the KERNAL to finish booting: the
    /// BASIC cold start (`NEW`) rewrites the head of program memory, so a
    /// program poked at power-on is gone by the time the prompt appears.
    pending_program: Option<(u16, Vec<u8>)>,
    /// The `expert_switch` option: 0 off, 1 prg, 2 on.
    expert_switch: u8,
    /// Socket behind the serial cartridge, if one was asked for.
    serial_net: Option<net::SerialNet>,
    /// The opt-in true 1541 (`true_drive` + `rom_1541`).
    drive: Option<drive::DriveLink>,
}

/// The KERNAL's "wait for a key" loop (`$E5CD-$E5D4`): the machine is idle at
/// its `READY.` prompt.
const KERNAL_IDLE_PC: std::ops::RangeInclusive<u16> = 0xE5CD..=0xE5D4;

impl InstanceRecord {
    /// Queues a program poked at BASIC's start for injection at the prompt;
    /// anything else (machine code, other addresses) is poked at once.
    fn load_program(&mut self, org: u16, bytes: &[u8]) {
        if org == BASIC_TXTTAB {
            self.pending_program = Some((org, bytes.to_vec()));
        } else {
            self.machine.load(org.into(), bytes);
        }
    }

    fn inject_pending_program(&mut self) {
        if self.pending_program.is_some()
            && KERNAL_IDLE_PC.contains(&(self.machine.pc() as u16))
            && let Some((org, bytes)) = self.pending_program.take()
        {
            self.machine.load(org.into(), &bytes);
            if self.auto_run {
                // A malformed program is left as loaded; `RUN` reports it.
                let _ = finish_basic_program_load(&mut self.machine, org);
            }
        }
    }

    /// One emulated instruction, plus the periodic exchange with the socket.
    fn step(&mut self) -> u32 {
        let cycles = self.machine.step();
        self.inject_pending_program();
        if let Some(drive) = &mut self.drive {
            drive.after_host_step(&mut IecHost(&mut self.machine), cycles);
        }
        if let Some(net) = &mut self.serial_net {
            net.tick(&mut self.machine);
        }
        cycles
    }
}

impl PluginState {
    fn new() -> Self {
        Self {
            next_instance: 1,
            instances: HashMap::new(),
        }
    }

    fn allocate_instance(&mut self) -> MachineInstanceHandle {
        let instance = self.next_instance;
        self.next_instance = self.next_instance.wrapping_add(1).max(1);
        instance
    }

    fn record_mut(
        &mut self,
        instance: MachineInstanceHandle,
    ) -> Result<&mut InstanceRecord, String> {
        self.instances
            .get_mut(&instance)
            .ok_or_else(|| format!("unknown machine instance handle {instance}"))
    }
}

fn state() -> &'static Mutex<PluginState> {
    STATE.get_or_init(|| Mutex::new(PluginState::new()))
}

fn status_from_error(error: &str) -> StatusCode {
    if error.contains("unknown machine instance handle") {
        StatusCode::NotFound
    } else if error.contains("unsupported") {
        StatusCode::Unsupported
    } else {
        StatusCode::InvalidRequest
    }
}

fn decode_request<T: serde::de::DeserializeOwned>(req: RcSlice) -> Result<T, CodecError> {
    // SAFETY: caller owns request bytes for call duration.
    plugin_api::decode_message(unsafe { req.as_bytes() })
}

fn encode_ok<T: serde::Serialize>(value: &T) -> RcCallResult {
    match plugin_api::encode_message(value) {
        Ok(bytes) => RcCallResult::ok_bytes(bytes),
        Err(e) => e.into_call_result(),
    }
}

fn machine_profile() -> MachineProfile {
    MachineProfile {
        machine_id: "c64".to_string(),
        display_name: "c64".to_string(),
        cpu_family: CpuFamily::Mos6502,
        default_region: Some("pal".to_string()),
        contract_version: MACHINE_PROFILE_CONTRACT_VERSION,
        debug_capabilities: REQUIRED_MACHINE_DEBUG_CAPABILITIES.to_vec(),
        capabilities: vec![
            MachineCapability::Video,
            MachineCapability::SaveState,
            MachineCapability::Debug,
        ],
        media_kinds: vec![
            MediaKind::Program,
            MediaKind::RomSet,
            MediaKind::Tape,
            MediaKind::Disk,
            MediaKind::Cartridge,
        ],
        options: vec![
            MachineOptionInfo {
                key: "kernal".to_string(),
                flag: "rom-kernal".to_string(),
                short: None,
                kind: MachineOptionKind::File,
                default: None,
                required: true,
                description: "KERNAL ROM image (8K)".to_string(),
            },
            MachineOptionInfo {
                key: "basic".to_string(),
                flag: "rom-basic".to_string(),
                short: None,
                kind: MachineOptionKind::File,
                default: None,
                required: true,
                description: "BASIC ROM image (8K)".to_string(),
            },
            MachineOptionInfo {
                key: "char".to_string(),
                flag: "rom-char".to_string(),
                short: None,
                kind: MachineOptionKind::File,
                default: None,
                required: true,
                description: "Character ROM image (4K)".to_string(),
            },
            MachineOptionInfo {
                key: "rom_1541".to_string(),
                flag: "rom-1541".to_string(),
                short: None,
                kind: MachineOptionKind::File,
                default: None,
                required: false,
                description: "DOS ROM image of the --drive-type drive (16K for the 1541, 32K \
                    for the 1570/1571/1581), user supplied; only used with --true-drive"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "drive_type".to_string(),
                flag: "drive-type".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec![
                        "1541".to_string(),
                        "1570".to_string(),
                        "1571".to_string(),
                        "1581".to_string(),
                    ],
                },
                default: Some(DebugValue::Text("1541".to_string())),
                required: false,
                description: "Which drive --true-drive runs: 1541, 1570 (single-sided), 1571 \
                    (double-sided .d71, 2 MHz; send U0>M1 for 1571 mode on a C64) or 1581 \
                    (3.5\" .d81)"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "true_drive".to_string(),
                flag: "true-drive".to_string(),
                short: None,
                kind: MachineOptionKind::Bool,
                default: Some(DebugValue::Bool(false)),
                required: false,
                description: "Run a real 1541 (its own 6502 and firmware, --rom-1541) on the IEC \
                    bus instead of the instant KERNAL LOAD/SAVE traps; a real DOS SAVE goes \
                    through the write head into the image (--disk-save/--disk-writeback \
                    apply); .d64/.g64/.x64 images work, .d71 on a 1570/1571, .d81 on a 1581"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "sid_model".to_string(),
                flag: "sid-model".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec!["6581".to_string(), "8580".to_string()],
                },
                default: Some(DebugValue::Text("6581".to_string())),
                required: false,
                description: "SID revision: 6581 (original; filter floor ~220 Hz, audible \
                    master-volume offset so $D418 sample playback works) or 8580"
                    .to_string(),
            },
            ExecutionMode::option(
                "Execution policy: fast (default; instruction-batched raster and DMA) or \
                 accurate (per-bus-cycle VIC-II raster and BA stalls; PAL only). Media traps \
                 are independent: use --true-drive and fast_tape_load=false for real-time media",
            ),
            MachineOptionInfo {
                key: "vic_accuracy".to_string(),
                flag: "vic-accuracy".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec!["line".to_string(), "cycle".to_string()],
                },
                default: None,
                required: false,
                description: "VIC-II model: line (default; raster and bad-line steal advance \
                    per CPU instruction, fast) or cycle (the real per-cycle schedule: every CPU \
                    bus cycle advances the machine one clock, bad lines and sprite DMA stall the \
                    CPU through BA; PAL only, slower)"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "mouse".to_string(),
                flag: "mouse".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec![
                        "off".to_string(),
                        "1351-port1".to_string(),
                        "1351-port2".to_string(),
                        "paddle-port1".to_string(),
                        "paddle-port2".to_string(),
                        "lightpen".to_string(),
                    ],
                },
                default: Some(DebugValue::Text("off".to_string())),
                required: false,
                description: "What the host mouse drives: a 1351 proportional mouse, a pair of \
                    paddles in control port 1 or 2 (read through the SID POT registers), or a light pen"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "serial".to_string(),
                flag: "serial".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec![
                        "off".to_string(),
                        "swiftlink".to_string(),
                        "turbo232".to_string(),
                    ],
                },
                default: Some(DebugValue::Text("off".to_string())),
                required: false,
                description: "Fit a SwiftLink or Turbo232 serial cartridge (6551 ACIA, NMI); \
                    shares the expansion port with --reu, --georam, --rrnet and --cartridge"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "serial_net".to_string(),
                flag: "serial-net".to_string(),
                short: None,
                kind: MachineOptionKind::Text,
                default: Some(DebugValue::Text(String::new())),
                required: false,
                description: "Connect the serial cartridge to a TCP peer: connect:host:port \
                    (retried while down) or listen:port (127.0.0.1 only). Without it the \
                    line is not connected and the carrier stays down"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "rrnet".to_string(),
                flag: "rrnet".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec![
                        "off".to_string(),
                        "loopback".to_string(),
                        "isolated".to_string(),
                    ],
                },
                default: Some(DebugValue::Text("off".to_string())),
                required: false,
                description: "Fit an RR-Net ethernet cartridge (CS8900A). There is no real \
                    network: loopback receives every frame the program sends, isolated drops them"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "io_page".to_string(),
                flag: "io-page".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec!["de00".to_string(), "df00".to_string()],
                },
                default: Some(DebugValue::Text("de00".to_string())),
                required: false,
                description: "I/O page the serial or RR-Net cartridge answers on".to_string(),
            },
            MachineOptionInfo {
                key: "printer".to_string(),
                flag: "printer".to_string(),
                short: None,
                kind: MachineOptionKind::Bool,
                default: Some(DebugValue::Bool(false)),
                required: false,
                description: "Answer for a text printer on the serial bus (device 4/5) so OPEN/\
                    PRINT#/CLOSE to it work; the output is written by --printer-out"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "fast_tape_load".to_string(),
                flag: "fast-tape-load".to_string(),
                short: None,
                kind: MachineOptionKind::Bool,
                default: Some(DebugValue::Bool(true)),
                required: false,
                description: "Serve KERNAL LOAD/SAVE on the cassette (device 1) instantly for \
                    tapes in the standard format; false plays every pulse in real time"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "expert_switch".to_string(),
                flag: "expert-switch".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec!["prg".to_string(), "on".to_string(), "off".to_string()],
                },
                default: Some(DebugValue::Text("prg".to_string())),
                required: false,
                description: "Position of the Trilogic Expert cartridge's 3-way switch when a \
                    cartridge of that type is attached (prg: RAM at $8000, on: freezer active, off)"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "georam".to_string(),
                flag: "georam".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec![
                        "off".to_string(),
                        "512".to_string(),
                        "1024".to_string(),
                        "2048".to_string(),
                        "4096".to_string(),
                    ],
                },
                default: Some(DebugValue::Text("off".to_string())),
                required: false,
                description: "Fit a GeoRAM (banked RAM at $DE00, registers $DFFE/$DFFF) of this \
                    many KB; shares the expansion port with --reu and --cartridge"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "reu".to_string(),
                flag: "reu".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec![
                        "off".to_string(),
                        "128".to_string(),
                        "256".to_string(),
                        "512".to_string(),
                    ],
                },
                default: Some(DebugValue::Text("off".to_string())),
                required: false,
                description: "RAM Expansion Unit on the expansion port (KB: 1700=128, \
                    1764=256, 1750=512); excludes a cartridge"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "run".to_string(),
                flag: "run".to_string(),
                short: Some('r'),
                kind: MachineOptionKind::Bool,
                default: Some(DebugValue::Bool(false)),
                required: false,
                description: "Auto-type LOAD\"*\",8,1 then RUN for an attached .d64, or bare \
                    RUN for a .prg/.t64 program, shortly after boot"
                    .to_string(),
            },
        ],
        key_names: key_names(),
        cpus: Vec::new(),
        axis_names: Vec::new(),
    }
}

fn parse_region(region: Option<&str>) -> Result<Region, String> {
    match region.map(|s| s.to_ascii_lowercase()) {
        None => Ok(Region::Pal),
        Some(value) if value == "pal" => Ok(Region::Pal),
        Some(value) if value == "ntsc" => Ok(Region::Ntsc),
        Some(value) if value == "japan" => Ok(Region::Japan),
        Some(other) => Err(format!("unsupported region \"{other}\"")),
    }
}

fn blob<'a>(roms: &'a [BinaryBlob], key: &str) -> Option<&'a [u8]> {
    roms.iter()
        .find(|blob| blob.key.eq_ignore_ascii_case(key))
        .map(|blob| blob.bytes.as_slice())
}

fn option_value<'a>(options: &'a [NamedValue], key: &str) -> Option<&'a DebugValue> {
    options
        .iter()
        .find(|item| item.key.eq_ignore_ascii_case(key))
        .map(|item| &item.value)
}

/// Extracts the lowercased file extension from an attached-media filename,
/// same "sniff by extension" convention `zx-spectrum-machine-plugin`'s own
/// `media_extension` already established -- picks between the two real,
/// unrelated cassette formats this plugin supports (`.tap`: a real bit-level
/// pulse capture; `.t64`: a directory container invented by an emulator,
/// never a real bitstream -- see `CLAUDE.md`/`TODO.md`).
fn media_extension(name: Option<&str>) -> Option<String> {
    name.and_then(|filename| filename.rsplit('.').next())
        .map(str::to_ascii_lowercase)
}

fn option_bool(options: &[NamedValue], key: &str) -> Result<Option<bool>, String> {
    let Some(value) = option_value(options, key) else {
        return Ok(None);
    };

    match value {
        DebugValue::Bool(v) => Ok(Some(*v)),
        _ => Err(format!("option \"{key}\" must be a boolean")),
    }
}

fn option_text(options: &[NamedValue], key: &str) -> Result<Option<String>, String> {
    match option_value(options, key) {
        None => Ok(None),
        Some(DebugValue::Text(v)) => Ok(Some(v.clone())),
        Some(_) => Err(format!("option \"{key}\" must be text")),
    }
}

fn option_u32(options: &[NamedValue], key: &str) -> Result<Option<u32>, String> {
    let Some(value) = option_value(options, key) else {
        return Ok(None);
    };

    match value {
        DebugValue::U64(v) => u32::try_from(*v)
            .map(Some)
            .map_err(|_| format!("option \"{key}\" out of range for u32")),
        _ => Err(format!("option \"{key}\" must be an unsigned integer")),
    }
}

/// Real, standard BASIC start address on a stock, unexpanded C64 --
/// `relink_basic_program` only ever engages at exactly this address (see
/// its own doc comment for why a different load address must never be
/// treated as BASIC text).
const BASIC_TXTTAB: u16 = 0x0801;
/// Bounds `relink_basic_program`'s own line/scan loops so malformed or
/// non-BASIC data at `$0801` can't hang it -- generous enough for any
/// real BASIC program (the C64's own ~38K of usable BASIC RAM couldn't
/// hold more lines or a longer single line than this regardless).
const MAX_BASIC_LINES: u32 = 10_000;
const MAX_BASIC_LINE_LEN: u32 = 8_192;

/// Synthesizes real BASIC zero-page pointers (VARTAB `$2D`/`$2E`, ARYTAB
/// `$2F`/`$30`, STREND `$31`/`$32`) for a BASIC program that was poked
/// directly into memory (`--program`'s `.prg` auto-detection, or a
/// `.t64` entry) rather than loaded through a real KERNAL LOAD + BASIC's
/// own automatic post-load relink -- real hardware fact this exists to
/// reproduce: a real `LOAD"...",8,1` statement's own BASIC-level command
/// handler always walks and rewrites the just-loaded program's own line-
/// link chain before returning to the READY prompt (defensively, since a
/// `.prg`'s stored links could be stale from wherever it was originally
/// saved), which is what makes a subsequent `RUN` work at all; the IEC
/// fast loader's own `LOAD"*",8,1` autotype sequence goes through that
/// real code path for free (see `try_fast_kernal_load_trap`'s own doc
/// comment), but a raw poke bypasses BASIC/KERNAL entirely, so nothing
/// would otherwise ever do this relink for it.
///
/// Deliberately gated to exactly `BASIC_TXTTAB` (`$0801`) -- a machine-
/// code program loaded at any other address has no such line-link
/// structure at all, and walking/rewriting its bytes as if it did would
/// silently corrupt it. Even at `$0801`, this trusts the loaded bytes to
/// actually be well-formed BASIC text (the overwhelmingly common case for
/// anything conventionally saved at that address) -- real BASIC's own
/// LINKPRG routine makes the identical assumption.
///
/// Real program format walked (commonly documented, not disassembly-
/// verified this session the way the KERNAL LOAD trap's own addresses
/// were): each line is `[u16 link to next line][u16 line number]
/// [tokens...][$00 terminator]`, and a line whose *own* stored link reads
/// exactly `$0000` marks the real end of the program -- VARTAB is the
/// address right after that final terminator. This walk never trusts a
/// non-zero stored link for navigation (only checks it for zero) --
/// instead it always scans forward from the current line's own start to
/// find that line's real token terminator, then rewrites the line's own
/// link to point there, exactly mirroring real LINKPRG's own defensive
/// behavior rather than assuming the file's own links already agree with
/// wherever it just landed in memory.
fn relink_basic_program(machine: &mut C64, start: u16) -> Result<u16, String> {
    let mut addr = start;
    for _ in 0..MAX_BASIC_LINES {
        let lo = machine.read_memory(addr.into());
        let hi = machine.read_memory(addr.wrapping_add(1).into());
        if lo == 0 && hi == 0 {
            return Ok(addr.wrapping_add(2)); // VARTAB
        }

        let mut p = addr.wrapping_add(4); // skip this line's own link + line number
        let mut scanned = 0u32;
        while machine.read_memory(p.into()) != 0 {
            p = p.wrapping_add(1);
            scanned += 1;
            if scanned > MAX_BASIC_LINE_LEN {
                return Err(
                    "--run: malformed BASIC program at $0801 (line too long to relink)".to_string(),
                );
            }
        }
        p = p.wrapping_add(1); // the real next line starts right after the terminator

        machine.load(addr.into(), &p.to_le_bytes());
        addr = p;
    }
    Err("--run: malformed BASIC program at $0801 (too many lines to relink)".to_string())
}

/// `relink_basic_program` plus the one further step a real post-LOAD
/// BASIC also always performs: VARTAB/ARYTAB/STREND (`$2D`-`$32`) all
/// coincide immediately after a fresh load, real hardware fact -- no
/// variables/arrays/strings exist yet, so all three "start of the next
/// region" pointers are the same address until the program actually
/// creates one. Only engages for a BASIC program poked at the real,
/// conventional `$0801` origin -- a no-op (`Ok(())`) for anything else,
/// same "don't touch what isn't BASIC text" stance `relink_basic_program`
/// itself takes.
fn finish_basic_program_load(machine: &mut C64, org: u16) -> Result<(), String> {
    if org != BASIC_TXTTAB {
        return Ok(());
    }
    let vartab = relink_basic_program(machine, org)?;
    let bytes = vartab.to_le_bytes();
    machine.load(0x2D, &bytes); // VARTAB
    machine.load(0x2F, &bytes); // ARYTAB
    machine.load(0x31, &bytes); // STREND
    Ok(())
}

fn create_machine(req: MachineCreateRequest) -> Result<MachineCreateResponse, String> {
    if !req.machine_id.eq_ignore_ascii_case("c64") {
        return Err(format!(
            "unsupported machine id \"{}\" (plugin supports only c64)",
            req.machine_id
        ));
    }

    // `run` is the only declared create-time option (see `machine_profile`)
    // -- it's never read here at all, purely a host-side (`main.rs`/`gui`)
    // decision of whether to auto-type a startup command, same "declared
    // but not every plugin needs to consume it" shape `cpc-machine-plugin`
    // already established for its own `run` option.

    let region = parse_region(req.region.as_deref())?;
    let region_string = req
        .region
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "pal".to_string());

    let kernal = blob(&req.roms, "kernal")
        .ok_or_else(|| "missing required binary blob: kernal".to_string())?;
    if kernal.len() != KERNAL_ROM_SIZE {
        return Err(format!(
            "KERNAL ROM must be {KERNAL_ROM_SIZE} bytes, got {}",
            kernal.len()
        ));
    }
    let basic = blob(&req.roms, "basic")
        .ok_or_else(|| "missing required binary blob: basic".to_string())?;
    if basic.len() != BASIC_ROM_SIZE {
        return Err(format!(
            "BASIC ROM must be {BASIC_ROM_SIZE} bytes, got {}",
            basic.len()
        ));
    }
    let char_rom =
        blob(&req.roms, "char").ok_or_else(|| "missing required binary blob: char".to_string())?;
    if char_rom.len() != CHAR_ROM_SIZE {
        return Err(format!(
            "character ROM must be {CHAR_ROM_SIZE} bytes, got {}",
            char_rom.len()
        ));
    }

    let mut machine = C64::new(kernal.to_vec(), basic.to_vec(), char_rom.to_vec())?;
    machine.set_region(region);
    if let Some(reu) = option_text(&req.options, "reu")?.filter(|reu| reu != "off") {
        let kilobytes: usize = reu
            .parse()
            .map_err(|_| format!("option \"reu\" must be off, 128, 256 or 512, not \"{reu}\""))?;
        machine.attach_reu(kilobytes)?;
    }
    let expert_switch = match option_text(&req.options, "expert_switch")?.as_deref() {
        None | Some("prg") => 1u8,
        Some("on") => 2,
        Some("off") => 0,
        Some(other) => {
            return Err(format!(
                "option \"expert_switch\" must be prg, on or off, not \"{other}\""
            ));
        }
    };
    let io_page = match option_text(&req.options, "io_page")?.as_deref() {
        None | Some("de00") => 0xDEu8,
        Some("df00") => 0xDF,
        Some(other) => {
            return Err(format!(
                "option \"io_page\" must be de00 or df00, not \"{other}\""
            ));
        }
    };
    let drive = if option_bool(&req.options, "true_drive")? == Some(true) {
        let kind = match option_text(&req.options, "drive_type")?.as_deref() {
            None => drive::DriveKind::C1541,
            Some(text) => drive::DriveKind::parse(text).ok_or_else(|| {
                format!("option \"drive_type\" must be 1541, 1570, 1571 or 1581, not \"{text}\"")
            })?,
        };
        let rom = blob(&req.roms, "rom_1541").ok_or_else(|| {
            format!(
                "--true-drive needs the {} DOS ROM: pass --rom-1541 <{}K image>",
                kind.name(),
                kind.rom_size() / 1024
            )
        })?;
        Some(drive::DriveLink::new(kind, rom.to_vec(), 8)?)
    } else {
        None
    };
    let mut serial_net = None;
    match option_text(&req.options, "serial")?.as_deref() {
        None | Some("off") => {}
        Some(kind @ ("swiftlink" | "turbo232")) => {
            machine.attach_serial(kind == "turbo232", io_page)?;
            if let Some(spec) = option_text(&req.options, "serial_net")?
                .map(|t| net::Spec::parse(&t))
                .transpose()?
                .flatten()
            {
                serial_net = Some(net::SerialNet::start(spec)?);
            }
        }
        Some(other) => {
            return Err(format!(
                "option \"serial\" must be off, swiftlink or turbo232, not \"{other}\""
            ));
        }
    }
    match option_text(&req.options, "rrnet")?.as_deref() {
        None | Some("off") => {}
        Some(mode @ ("loopback" | "isolated")) => {
            machine.attach_rrnet(io_page, mode == "loopback")?
        }
        Some(other) => {
            return Err(format!(
                "option \"rrnet\" must be off, loopback or isolated, not \"{other}\""
            ));
        }
    }
    if option_bool(&req.options, "printer")? == Some(true) {
        machine.set_printer(true);
    }
    if let Some(geo) = option_text(&req.options, "georam")?.filter(|g| g != "off") {
        let kilobytes: usize = geo.parse().map_err(|_| {
            format!("option \"georam\" must be off, 512, 1024, 2048 or 4096, not \"{geo}\"")
        })?;
        machine.attach_georam(kilobytes)?;
    }
    if let Some(model) = option_text(&req.options, "sid_model")? {
        machine.set_sid_model(match model.as_str() {
            "6581" => SidModel::Mos6581,
            "8580" => SidModel::Mos8580,
            other => {
                return Err(format!(
                    "option \"sid_model\" must be 6581 or 8580, not \"{other}\""
                ));
            }
        });
    }
    let legacy_mode = option_text(&req.options, "vic_accuracy")?
        .map(|accuracy| match accuracy.as_str() {
            "line" => Ok(ExecutionMode::Fast),
            "cycle" => Ok(ExecutionMode::Accurate),
            other => Err(format!(
                "option \"vic_accuracy\" must be line or cycle, not \"{other}\""
            )),
        })
        .transpose()?;
    let mode = ExecutionMode::from_options(&req.options)?;
    if let (Some(mode), Some(legacy)) = (mode, legacy_mode)
        && mode != legacy
    {
        return Err(
            "options \"execution_mode\" and \"vic_accuracy\" contradict each other".to_string(),
        );
    }
    machine.set_vic_accuracy(match mode.or(legacy_mode).unwrap_or_default() {
        ExecutionMode::Fast => VicAccuracy::Line,
        ExecutionMode::Accurate => VicAccuracy::Cycle,
    })?;
    if let Some(mouse) = option_text(&req.options, "mouse")? {
        machine.set_mouse_mode(match mouse.as_str() {
            "off" => MouseMode::Off,
            "1351-port1" => MouseMode::Mouse1351(1),
            "1351-port2" => MouseMode::Mouse1351(2),
            "paddle-port1" => MouseMode::Paddle(1),
            "paddle-port2" => MouseMode::Paddle(2),
            "lightpen" => MouseMode::LightPen,
            other => return Err(format!("option \"mouse\" must be off, 1351-port1, 1351-port2, paddle-port1, paddle-port2 or lightpen, not \"{other}\"")),
        });
    }
    if let Some(fast) = option_bool(&req.options, "fast_tape_load")? {
        machine.set_fast_tape_load(fast);
    }
    let auto_run = option_bool(&req.options, "run")?.unwrap_or(false);

    let frame_rate = exact_frame_rate(&machine);
    let mut state = state()
        .lock()
        .map_err(|_| "plugin state mutex poisoned".to_string())?;
    let instance = state.allocate_instance();
    state.instances.insert(
        instance,
        InstanceRecord {
            machine,
            region: region_string.clone(),
            paused: false,
            breakpoints: Vec::new(),
            auto_run,
            pending_program: None,
            expert_switch,
            serial_net,
            drive,
        },
    );

    Ok(MachineCreateResponse {
        instance,
        metadata: MachineMetadata {
            machine_id: "c64".to_string(),
            model: Some("c64".to_string()),
            region: Some(region_string),
            notes: vec!["external machine plugin v1".to_string()],
            bitmap_palette: bitmap_palette(),
            bitmap_defaults: Some(bitmap_defaults()),
            frame_rate,
            ..MachineMetadata::default()
        },
    })
}

/// Every key name this machine's `KeyEvent` accepts -- built from the same
/// tables the event handler looks names up in, so the list cannot drift.
fn key_names() -> Vec<plugin_api::MachineKeyInfo> {
    let mut infos = plugin_api::key_infos(
        c64_key_table()
            .iter()
            .map(|(name, _)| *name)
            .chain(c64_combo_table().iter().map(|(name, _)| *name))
            .chain(c64_joystick_table().iter().map(|(name, _)| *name))
            .chain(c64_paddle_table().iter().map(|(name, _)| *name)),
    );
    plugin_api::set_key_chars(&mut infos, C64_CHARS);
    infos
}

/// The ASCII character each key types on a real C64 (unshifted legends from
/// the KERNAL's own keyboard decode table, shifted ones for the combo keys
/// below), for the host's symbolic keyboard layout. `UpArrow` is `^` and
/// `LeftArrow` is `_` as in PETSCII; the pound key types `\` (and the host's
/// `£`).
const C64_CHARS: &[(&str, &str)] = &[
    ("Space", " "),
    ("Comma", ","),
    ("Period", "."),
    ("Slash", "/"),
    ("Semicolon", ";"),
    ("Colon", ":"),
    ("At", "@"),
    ("Plus", "+"),
    ("Minus", "-"),
    ("Equals", "="),
    ("Asterisk", "*"),
    ("UpArrow", "^"),
    ("LeftArrow", "_"),
    ("Pound", "\\£"),
    ("Exclamation", "!"),
    ("DoubleQuote", "\""),
    ("Hash", "#"),
    ("Dollar", "$"),
    ("Percent", "%"),
    ("Ampersand", "&"),
    ("Apostrophe", "'"),
    ("LeftParen", "("),
    ("RightParen", ")"),
    ("LessThan", "<"),
    ("GreaterThan", ">"),
    ("Question", "?"),
    ("LeftBracket", "["),
    ("RightBracket", "]"),
];

/// Control-port joystick names: `Joy1*` is control port 1 (shares CIA1 Port B
/// with the keyboard rows), `Joy2*` port 2 (CIA1 Port A, what most games
/// read). Same names the CPC and ZX Spectrum plugins use for their joysticks.
fn c64_joystick_table() -> &'static [(&'static str, (u8, JoystickButton))] {
    use JoystickButton::*;
    &[
        ("Joy1Up", (1, Up)),
        ("Joy1Down", (1, Down)),
        ("Joy1Left", (1, Left)),
        ("Joy1Right", (1, Right)),
        ("Joy1Fire1", (1, Fire)),
        ("Joy2Up", (2, Up)),
        ("Joy2Down", (2, Down)),
        ("Joy2Left", (2, Left)),
        ("Joy2Right", (2, Right)),
        ("Joy2Fire1", (2, Fire)),
    ]
}

fn c64_joystick_from_name(name: &str) -> Option<(u8, JoystickButton)> {
    c64_joystick_table()
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| *value)
}

/// What a paddle key does: turn one way, the other, or press the fire button.
#[derive(Clone, Copy)]
enum PaddleAction {
    Decrease,
    Increase,
    Fire,
}

/// Paddle keys: `Paddle<port><A|B><Dec|Inc|Fire>`. A is the pot on POTX
/// (fire = the port's Left line), B the one on POTY (fire = Right). Turning
/// is timed: a held key moves the pot a step every 2000 cycles.
fn c64_paddle_table() -> &'static [(&'static str, (u8, u8, PaddleAction))] {
    use PaddleAction::*;
    &[
        ("Paddle1ADec", (1, 0, Decrease)),
        ("Paddle1AInc", (1, 0, Increase)),
        ("Paddle1AFire", (1, 0, Fire)),
        ("Paddle1BDec", (1, 1, Decrease)),
        ("Paddle1BInc", (1, 1, Increase)),
        ("Paddle1BFire", (1, 1, Fire)),
        ("Paddle2ADec", (2, 0, Decrease)),
        ("Paddle2AInc", (2, 0, Increase)),
        ("Paddle2AFire", (2, 0, Fire)),
        ("Paddle2BDec", (2, 1, Decrease)),
        ("Paddle2BInc", (2, 1, Increase)),
        ("Paddle2BFire", (2, 1, Fire)),
    ]
}

fn c64_paddle_from_name(name: &str) -> Option<(u8, u8, PaddleAction)> {
    c64_paddle_table()
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| *value)
}

fn c64_key_table() -> &'static [(&'static str, C64Key)] {
    use C64Key::*;
    &[
        ("KeyA", A),
        ("KeyB", B),
        ("KeyC", C),
        ("KeyD", D),
        ("KeyE", E),
        ("KeyF", F),
        ("KeyG", G),
        ("KeyH", H),
        ("KeyI", I),
        ("KeyJ", J),
        ("KeyK", K),
        ("KeyL", L),
        ("KeyM", M),
        ("KeyN", N),
        ("KeyO", O),
        ("KeyP", P),
        ("KeyQ", Q),
        ("KeyR", R),
        ("KeyS", S),
        ("KeyT", T),
        ("KeyU", U),
        ("KeyV", V),
        ("KeyW", W),
        ("KeyX", X),
        ("KeyY", Y),
        ("KeyZ", Z),
        ("Digit0", Digit0),
        ("Digit1", Digit1),
        ("Digit2", Digit2),
        ("Digit3", Digit3),
        ("Digit4", Digit4),
        ("Digit5", Digit5),
        ("Digit6", Digit6),
        ("Digit7", Digit7),
        ("Digit8", Digit8),
        ("Digit9", Digit9),
        ("Space", Space),
        ("Enter", Return),
        ("Backspace", InstDel),
        ("ShiftLeft", LeftShift),
        ("ShiftRight", LeftShift),
        ("ArrowLeft", CursorRight),
        ("ArrowRight", CursorRight),
        ("ArrowUp", CursorDown),
        ("ArrowDown", CursorDown),
        ("Escape", RunStop),
        ("Comma", Comma),
        ("Period", Period),
        ("Slash", Slash),
        ("Semicolon", Semicolon),
        ("Minus", Minus),
        ("Equal", Equals),
        ("F1", F1),
        ("F3", F3),
        ("F5", F5),
        ("F7", F7),
        ("ControlLeft", Control),
        ("ControlRight", Control),
        ("AltLeft", Commodore),
        ("AltRight", Commodore),
        ("Home", ClrHome),
        ("Plus", Plus),
        ("Equals", Equals),
        ("Colon", Colon),
        ("At", At),
        ("Pound", Pound),
        ("Asterisk", Asterisk),
        ("UpArrow", UpArrow),
        ("LeftArrow", LeftArrow),
        ("RightShift", RightShift),
        ("RunStop", RunStop),
        ("Commodore", Commodore),
        ("Delete", InstDel),
        ("BracketLeft", At),
        ("BracketRight", Plus),
        ("Backslash", Pound),
        ("Quote", Asterisk),
    ]
}

fn c64_key_from_name(name: &str) -> Option<C64Key> {
    c64_key_table()
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| *value)
}

fn c64_combo_table() -> &'static [(&'static str, (C64Key, C64Key))] {
    use C64Key::*;
    &[
        ("F2", (LeftShift, F1)),
        ("F4", (LeftShift, F3)),
        ("F6", (LeftShift, F5)),
        ("F8", (LeftShift, F7)),
        // Shifted legends, checked against the KERNAL's shift decode table:
        // Shift+1..9 = ! " # $ % & ' ( ), Shift+, . / = < > ?, Shift+: ; = [ ].
        ("Exclamation", (LeftShift, Digit1)),
        ("DoubleQuote", (LeftShift, Digit2)),
        ("Hash", (LeftShift, Digit3)),
        ("Dollar", (LeftShift, Digit4)),
        ("Percent", (LeftShift, Digit5)),
        ("Ampersand", (LeftShift, Digit6)),
        ("Apostrophe", (LeftShift, Digit7)),
        ("LeftParen", (LeftShift, Digit8)),
        ("RightParen", (LeftShift, Digit9)),
        ("LessThan", (LeftShift, Comma)),
        ("GreaterThan", (LeftShift, Period)),
        ("Question", (LeftShift, Slash)),
        ("LeftBracket", (LeftShift, Colon)),
        ("RightBracket", (LeftShift, Semicolon)),
    ]
}

fn c64_combo_from_name(name: &str) -> Option<(C64Key, C64Key)> {
    c64_combo_table()
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| *value)
}

fn register_snapshot(registers: Vec<(&'static str, u64)>) -> RegisterSnapshot {
    let registers = registers
        .into_iter()
        .map(|(name, value)| RegisterField {
            bank: "main".to_string(),
            name: name.to_string(),
            value,
            width_bits: inferred_width_bits(value),
        })
        .collect();

    RegisterSnapshot {
        cpu_family: CpuFamily::Mos6502,
        registers,
        flags: Vec::new(),
    }
}

fn inferred_width_bits(value: u64) -> u16 {
    if value <= u8::MAX as u64 {
        8
    } else if value <= u16::MAX as u64 {
        16
    } else if value <= u32::MAX as u64 {
        32
    } else {
        64
    }
}

fn nv_text(key: &str, value: impl Into<String>) -> NamedValue {
    NamedValue {
        key: key.to_string(),
        value: DebugValue::Text(value.into()),
    }
}

/// Machine-agnostic overlay chip snapshot (`chip_id: "overlay"`) consumed
/// by the on-screen `--show-overlay` GUI panel -- same generic field
/// vocabulary as `cpc-machine-plugin`/`zx-spectrum-machine-plugin`'s own
/// `generic_overlay_chip_snapshot`, but C64 has no tape/disk (see
/// `CLAUDE.md`), so only `memory_summary` is ever present here -- no
/// `fdc_phase`/`disk_motor`/`tape_motor` fields, no `tape_waveform` blob.
fn generic_overlay_chip_snapshot(record: &InstanceRecord) -> ChipSnapshot {
    let mut fields = Vec::new();
    if let Some(summary) = record.machine.overlay_memory_summary() {
        fields.push(nv_text("memory_summary", summary));
    }
    ChipSnapshot {
        chip_id: "overlay".to_string(),
        fields,
        blobs: Vec::new(),
    }
}

fn handle_machine_command(
    record: &mut InstanceRecord,
    command: MachineCommand,
) -> Result<MachineResponse, String> {
    match command {
        MachineCommand::GetClockHz => Ok(MachineResponse::ClockHz(record.machine.clock_hz())),
        MachineCommand::GetFrameHz => Ok(MachineResponse::FrameHz(record.machine.frame_hz())),
        MachineCommand::RequestInterrupt => {
            record.machine.request_interrupt();
            Ok(MachineResponse::Ack)
        }
        MachineCommand::Step => {
            let cycles = record.step();
            Ok(MachineResponse::Step(plugin_api::StepResult {
                cycles,
                pc: record.machine.pc(),
                halted: record.machine.halted(),
            }))
        }
        MachineCommand::IsHalted => Ok(MachineResponse::Halted(record.machine.halted())),
        MachineCommand::GetPc => Ok(MachineResponse::Pc(record.machine.pc())),
        MachineCommand::Reset => {
            record.machine.reset();
            if let Some(drive) = &mut record.drive {
                drive.reset();
            }
            Ok(MachineResponse::Ack)
        }
        MachineCommand::ReadMemory { address, len, .. } => {
            let bytes = (0..len)
                .map(|offset| record.machine.read_memory(address.wrapping_add(offset.into())))
                .collect();
            Ok(MachineResponse::Memory(bytes))
        }
        // A `space` request (banked/paged poke, e.g. the debugger's own
        // PokeBank command) is a clear error here, not a silent flat write
        // -- the C64 has no real banked-RAM debugger-poke concept modeled
        // (its PLA/6510-port banking is address-range-based, not a set of
        // named switchable banks the way ZX Spectrum's 128K RAM banks are),
        // so honoring an unrecognized `space` by writing flat memory
        // instead would silently poke the wrong place.
        MachineCommand::WriteMemory {
            space: Some(_), ..
        } => Err("banked/paged memory writes are not supported for the c64 plugin".to_string()),
        MachineCommand::WriteMemory {
            address,
            bytes,
            space: None,
        }
        | MachineCommand::LoadBytes { address, bytes } => {
            let written = record.machine.load(address, &bytes);
            Ok(MachineResponse::BytesWritten(written as u32))
        }
        MachineCommand::SetPc { address } => {
            record.machine.set_pc(address);
            Ok(MachineResponse::Ack)
        }
        MachineCommand::SaveState => {
            let mut blob = record.machine.save_state();
            if let Some(drive) = &record.drive {
                blob = drive::pack_drive_state(&blob, &drive.save_state());
            }
            Ok(MachineResponse::StateBlob(blob))
        }
        MachineCommand::SaveSnapshot { extension } => Err(format!(
            "unsupported snapshot save extension \".{extension}\" for c64 plugin"
        )),
        MachineCommand::LoadState { bytes } => {
            match (drive::unpack_drive_state(&bytes)?, &mut record.drive) {
                (Some((c64_state, drive_state)), Some(drive)) => {
                    record.machine.load_state(c64_state)?;
                    drive.load_state(drive_state)?;
                }
                (Some(_), None) => {
                    return Err(
                        "the state was saved with --true-drive; start with --true-drive to load it"
                            .to_string(),
                    );
                }
                (None, Some(_)) => {
                    return Err(
                        "the state has no 1541 in it; load it without --true-drive".to_string(),
                    );
                }
                (None, None) => record.machine.load_state(&bytes)?,
            }
            Ok(MachineResponse::Ack)
        }
        MachineCommand::AttachMedia {
            slot: _,
            kind,
            name,
            bytes,
            options,
        } => match kind {
            MediaKind::Program => {
                let org = option_u32(&options, "org")?.unwrap_or(0x0400);
                let org =
                    u16::try_from(org).map_err(|_| format!("program org out of range for u16: {org}"))?;
                record.load_program(org, &bytes);
                Ok(MachineResponse::Ack)
            }
            MediaKind::RomSet => Err(
                "RomSet attachment is unsupported after create_instance; pass kernal/basic/char ROMs in create request"
                    .to_string(),
            ),
            MediaKind::Cartridge => {
                // Plugging a cartridge in resets the machine (see
                // `C64::load_cartridge`), so the KERNAL finds its CBM80
                // signature or an Ultimax cartridge supplies the vectors.
                record.machine.load_cartridge_crt(&bytes)?;
                if record.expert_switch != 1 {
                    record.machine.set_expert_switch(record.expert_switch);
                }
                Ok(MachineResponse::Ack)
            }
            MediaKind::Snapshot => {
                Err("--snapshot isn't supported for --machine c64 yet".to_string())
            }
            MediaKind::Tape => {
                let extension = media_extension(name.as_deref()).ok_or_else(|| {
                    "tape media requires a filename to determine its format (.tap or .t64)"
                        .to_string()
                })?;
                match extension.as_str() {
                    "tap" => {
                        let tape = c64_tape::parse_tap(&bytes).map_err(|e| e.to_string())?;
                        let speed = option_u32(&options, "tape_speed")?.unwrap_or(1);
                        record.machine.load_tape(tape);
                        record.machine.set_tape_speed(speed);
                        Ok(MachineResponse::Ack)
                    }
                    // `.t64` is a directory container invented by an emulator, never a
                    // real bitstream -- no real Datasette ever produced one, so there's
                    // no real-time signal to simulate. Treated like `.prg`: the first
                    // loadable (NORMAL-type) entry's bytes are poked straight into
                    // memory at its own stored load address, same "poke bytes, no
                    // BASIC-pointer/auto-RUN magic" semantic `--program`'s `.prg`
                    // auto-detection already established.
                    "t64" => {
                        let t64 = c64_tape::parse_t64(&bytes).map_err(|e| e.to_string())?;
                        let (address, data) = t64.first_program().ok_or_else(|| {
                            "no loadable (NORMAL-type) program entry found in .t64 file"
                                .to_string()
                        })?;
                        record.load_program(address, data);
                        Ok(MachineResponse::Ack)
                    }
                    other => Err(format!(
                        "unsupported tape extension for c64 plugin: .{other} (expected .tap or .t64)"
                    )),
                }
            }
            MediaKind::Disk => {
                // No name defaults to `.d64` -- preserves this handler's
                // own pre-existing, name-agnostic behavior from before
                // `.d71`/`.d81` support existed.
                let extension = media_extension(name.as_deref()).unwrap_or_else(|| "d64".to_string());
                if let Some(drive) = &mut record.drive {
                    // True drive: the disk goes into the drive (GCR mechanism with
                    // a write head), not the C64's fast loader.
                    let disk = match extension.as_str() {
                        "d64" => drive::Disk::Gcr(c1541::Medium::D64(
                            d64::D64::parse(&bytes).map_err(|e| e.to_string())?,
                        )),
                        "x64" => drive::Disk::Gcr(c1541::Medium::D64(
                            d64::D64::parse_x64(&bytes).map_err(|e| e.to_string())?,
                        )),
                        "g64" => drive::Disk::Gcr(c1541::Medium::D64(
                            g64::parse(&bytes)
                                .map_err(|e| e.to_string())?
                                .to_d64()
                                .map_err(|e| e.to_string())?
                                .disk,
                        )),
                        "d71" => drive::Disk::Gcr(c1541::Medium::D71(
                            d71::D71::parse(&bytes).map_err(|e| e.to_string())?,
                        )),
                        "d81" => drive::Disk::D81(d81::D81::parse(&bytes).map_err(|e| e.to_string())?),
                        other => {
                            return Err(format!(
                                "a true drive reads .d64, .x64, .g64, .d71 and .d81, not .{other}"
                            ));
                        }
                    };
                    drive.attach_disk(disk)?;
                    return Ok(MachineResponse::Ack);
                }
                match extension.as_str() {
                    "d64" => {
                        let disk = d64::D64::parse(&bytes).map_err(|e| e.to_string())?;
                        record.machine.load_disk(disk);
                    }
                    "x64" => {
                        let disk = d64::D64::parse_x64(&bytes).map_err(|e| e.to_string())?;
                        record.machine.load_disk(disk);
                    }
                    "g64" => {
                        // A G64 is a raw GCR bitstream; the fast loader works on
                        // logical sectors, so decode it to a .d64 (copy-protected
                        // tracks that don't decode to sectors are not representable).
                        let decoded = g64::parse(&bytes)
                            .map_err(|e| e.to_string())?
                            .to_d64()
                            .map_err(|e| e.to_string())?;
                        record.machine.load_disk(decoded.disk);
                    }
                    "nib" => {
                        return Err(
                            ".nib disk images are not supported (convert to .g64 or .d64)"
                                .to_string(),
                        );
                    }
                    "d71" => {
                        let disk = d71::D71::parse(&bytes).map_err(|e| e.to_string())?;
                        record.machine.load_disk_d71(disk);
                    }
                    "d81" => {
                        let disk = d81::D81::parse(&bytes).map_err(|e| e.to_string())?;
                        record.machine.load_disk_d81(disk);
                    }
                    other => {
                        return Err(format!(
                            "unsupported disk extension for c64 plugin: .{other} (expected .d64, .x64, .g64, .d71, or .d81)"
                        ));
                    }
                }
                Ok(MachineResponse::Ack)
            }
            other => Err(format!("unsupported media kind for c64 plugin: {:?}", other)),
        },
        MachineCommand::DetachMedia { slot } => match slot {
            plugin_api::MediaSlot::Tape => {
                record.machine.eject_tape();
                Ok(MachineResponse::Ack)
            }
            plugin_api::MediaSlot::Disk { drive: 0 | 8 } => {
                if let Some(drive) = &mut record.drive {
                    drive.detach_disk();
                }
                record.machine.eject_disk();
                Ok(MachineResponse::Ack)
            }
            plugin_api::MediaSlot::Disk { drive } => Err(format!("drive {drive} does not exist (only one disk)")),
            plugin_api::MediaSlot::Cartridge { .. } => {
                record.machine.eject_cartridge();
                Ok(MachineResponse::Ack)
            }
            other => Err(format!(
                "nothing to eject from slot {other:?} on the c64 plugin (tape, the disk or the cartridge)"
            )),
        },
        MachineCommand::ExportDisk {
            drive,
            only_if_modified,
        } => {
            if drive != 0 {
                return Err(format!("drive {drive} does not exist (only one disk)"));
            }
            // With a true drive the disk lives in the drive (the C64 holds none).
            let image = match (&mut record.drive, only_if_modified) {
                (Some(drive), true) => drive.take_modified_disk_image(),
                (Some(drive), false) => drive.disk_image(),
                (None, true) => record.machine.take_modified_disk_image(),
                (None, false) => record.machine.disk_image(),
            };
            Ok(MachineResponse::DiskImage(image))
        }
        MachineCommand::MarkDiskSaved { drive } => {
            if drive != 0 {
                return Err(format!("drive {drive} does not exist (only one disk)"));
            }
            record.machine.mark_disk_saved();
            if let Some(drive) = &mut record.drive {
                drive.mark_disk_saved();
            }
            Ok(MachineResponse::Ack)
        }
        MachineCommand::Freeze => {
            if !record.machine.has_freezer() {
                return Err(
                    "the attached cartridge has no freeze button (or none is attached)".to_string(),
                );
            }
            if record.machine.freeze() {
                Ok(MachineResponse::Ack)
            } else {
                Err("the cartridge did not accept the freeze (switch off or disabled)".to_string())
            }
        }
        MachineCommand::ExportPrinter => Ok(MachineResponse::PrinterText(
            record.machine.printer_text().map(String::into_bytes),
        )),
        MachineCommand::ExportCartridge => {
            let image = record
                .machine
                .cartridge_modified()
                .then(|| record.machine.cartridge_image())
                .flatten();
            Ok(MachineResponse::CartridgeImage(image))
        }
        MachineCommand::ExportTape { format } => {
            if !format.eq_ignore_ascii_case("tap") {
                return Err(format!(
                    "unsupported tape format \"{format}\" for the c64 (expected tap)"
                ));
            }
            let image = record.machine.recorded_tape().map(|tape| c64_tape::write_tap(&tape));
            Ok(MachineResponse::TapeImage(image))
        }
        MachineCommand::GetMediaSnapshot => Ok(MachineResponse::MediaSnapshot(MediaSnapshot {
            tape: None,
            disks: Vec::new(),
            cartridge: record
                .machine
                .cartridge_name()
                .map(|name| plugin_api::CartridgeStatus {
                    attached: true,
                    mapper: Some(name.to_string()),
                    active_banks: Vec::new(),
                }),
        })),
        MachineCommand::SetBorder { enabled: _ } => Ok(MachineResponse::Ack),
        MachineCommand::KeyEvent { key, pressed } => {
            if let Some((port, button)) = c64_joystick_from_name(&key) {
                if pressed {
                    record.machine.press_joystick(port, button);
                } else {
                    record.machine.release_joystick(port, button);
                }
                return Ok(MachineResponse::Ack);
            }
            if let Some((port, paddle, action)) = c64_paddle_from_name(&key) {
                match action {
                    PaddleAction::Fire => record.machine.paddle_fire(port, paddle, pressed),
                    PaddleAction::Decrease => record.machine.paddle_turn(
                        port,
                        paddle,
                        pressed.then_some(PaddleDirection::Decrease),
                    ),
                    PaddleAction::Increase => record.machine.paddle_turn(
                        port,
                        paddle,
                        pressed.then_some(PaddleDirection::Increase),
                    ),
                }
                return Ok(MachineResponse::Ack);
            }
            let keys = c64_combo_from_name(&key)
                .map(|(modifier, base)| vec![modifier, base])
                .or_else(|| c64_key_from_name(&key).map(|key| vec![key]));
            if let Some(keys) = keys {
                for key in keys {
                    if pressed {
                        record.machine.press_key(key);
                    } else {
                        record.machine.release_key(key);
                    }
                }
            }
            Ok(MachineResponse::Ack)
        }
        MachineCommand::MouseMotion { dx, dy } => {
            record.machine.mouse_motion(i32::from(dx), i32::from(dy));
            Ok(MachineResponse::Ack)
        }
        MachineCommand::MouseButton { button, pressed } => {
            match button {
                plugin_api::HostMouseButton::Left => record.machine.mouse_button(false, pressed),
                plugin_api::HostMouseButton::Right => record.machine.mouse_button(true, pressed),
                _ => {}
            }
            Ok(MachineResponse::Ack)
        }
        MachineCommand::MouseWheel { .. } => Ok(MachineResponse::Ack),
        MachineCommand::WriteIo { .. } => {
            Err("the C64 has no I/O-port space (every device is memory-mapped)".to_string())
        }
        MachineCommand::SetPortWatches { .. } => {
            Err("the C64 has no I/O-port space (every device is memory-mapped)".to_string())
        }
        MachineCommand::TakePortHit => Ok(MachineResponse::PortHit(None)),
        MachineCommand::RenderFrame => {
            let (width, height, rgb) = record
                .machine
                .render_frame()
                .ok_or_else(|| "render_frame unavailable for c64".to_string())?;
            Ok(MachineResponse::Frame { width, height, rgb })
        }
        MachineCommand::AxisEvent { .. } => {
            Err("this machine has no analogue axes".to_string())
        }
        MachineCommand::DrainAudio => Ok(MachineResponse::AudioPcm {
            channels: 1,
            sample_rate: AUDIO_SAMPLE_RATE,
            samples: record.machine.drain_audio(),
        }),
        MachineCommand::Debug(command) => {
            let response = handle_debug_command(record, command)?;
            Ok(MachineResponse::Debug(response))
        }
    }
}

fn handle_debug_command(
    record: &mut InstanceRecord,
    command: DebugCommand,
) -> Result<DebugResponse, String> {
    match command {
        DebugCommand::Pause => {
            record.paused = true;
            Ok(DebugResponse::Ack)
        }
        DebugCommand::Resume => {
            record.paused = false;
            Ok(DebugResponse::Ack)
        }
        DebugCommand::StepInstruction | DebugCommand::StepOver => {
            record.step();
            Ok(DebugResponse::Ack)
        }
        DebugCommand::SetBreakpoint { address } => {
            if !record.breakpoints.contains(&address) {
                record.breakpoints.push(address);
                record.breakpoints.sort_unstable();
            }
            Ok(DebugResponse::Ack)
        }
        DebugCommand::ClearBreakpoint { address } => {
            record.breakpoints.retain(|bp| *bp != address);
            Ok(DebugResponse::Ack)
        }
        DebugCommand::ClearAllBreakpoints => {
            record.breakpoints.clear();
            Ok(DebugResponse::Ack)
        }
        DebugCommand::ListBreakpoints => Ok(DebugResponse::Breakpoints(record.breakpoints.clone())),
        DebugCommand::GetRegisters => Ok(DebugResponse::Registers(register_snapshot(
            record.machine.registers(),
        ))),
        DebugCommand::SetRegister { name, value } => {
            record.machine.set_register(&name, value)?;
            Ok(DebugResponse::Ack)
        }
        DebugCommand::GetAddressSpaces => {
            Ok(DebugResponse::AddressSpaces(vec![AddressSpaceInfo {
                name: "cpu".to_string(),
                addr_width_bits: 16,
                bank_context: Vec::new(),
            }]))
        }
        DebugCommand::ReadMemory { address, len, .. } => {
            let bytes = (0..len)
                .map(|offset| {
                    record
                        .machine
                        .read_memory(address.wrapping_add(offset.into()))
                })
                .collect();
            Ok(DebugResponse::Memory(bytes))
        }
        DebugCommand::WriteMemory { address, bytes, .. } => {
            record.machine.load(address, &bytes);
            Ok(DebugResponse::Ack)
        }
        DebugCommand::GetDisassembly { start, lines } => {
            let mut cursor = start.unwrap_or(record.machine.pc());
            let mut out = Vec::with_capacity(lines as usize);
            for _ in 0..lines {
                let raw: Vec<u8> = (0..3u16)
                    .map(|offset| {
                        record
                            .machine
                            .read_memory(cursor.wrapping_add(offset.into()))
                    })
                    .collect();
                let (text, consumed) =
                    mos6502::disassemble::disassemble(&raw, cursor.try_into().unwrap());
                out.push(DisassemblyLine {
                    address: cursor,
                    bytes: raw[..consumed].to_vec(),
                    text,
                });
                cursor = cursor.wrapping_add((consumed as u16).into());
            }
            Ok(DebugResponse::Disassembly(out))
        }
        DebugCommand::GetMediaSnapshot => Ok(DebugResponse::MediaSnapshot(MediaSnapshot {
            tape: None,
            disks: Vec::new(),
            cartridge: None,
        })),
        DebugCommand::GetChipSnapshots => Ok(DebugResponse::ChipSnapshots(vec![
            generic_overlay_chip_snapshot(record),
        ])),
        DebugCommand::OnCpu { cpu, command } => {
            if cpu == "main" {
                handle_debug_command(record, *command)
            } else {
                Err(format!(
                    "unknown cpu \"{cpu}\" (this machine has only \"main\")"
                ))
            }
        }
        DebugCommand::GetMachineMetadata => Ok(DebugResponse::MachineMetadata(MachineMetadata {
            machine_id: "c64".to_string(),
            model: Some("c64".to_string()),
            region: Some(record.region.clone()),
            notes: vec!["external machine plugin v1".to_string()],
            bitmap_palette: bitmap_palette(),
            bitmap_defaults: Some(bitmap_defaults()),
            frame_rate: exact_frame_rate(&record.machine),
            ..MachineMetadata::default()
        })),
    }
}

fn handle_instance_call(
    instance: MachineInstanceHandle,
    request: MachineInstanceCallRequest,
) -> Result<MachineInstanceCallResponse, String> {
    let mut state = state()
        .lock()
        .map_err(|_| "plugin state mutex poisoned".to_string())?;
    let record = state.record_mut(instance)?;
    let response = handle_machine_command(record, request.command)?;
    Ok(MachineInstanceCallResponse { response })
}

unsafe extern "C" fn plugin_free_buffer_v1(buffer: RcBuffer) {
    // SAFETY: caller guarantees this buffer originated from RcBuffer::from_vec.
    let _ = unsafe { buffer.into_vec() };
}

unsafe extern "C" fn plugin_list_profiles_v1(req: RcSlice) -> RcCallResult {
    plugin_api::guard::guarded_call("list_profiles", || unsafe {
        plugin_list_profiles_inner(req)
    })
}

unsafe fn plugin_list_profiles_inner(req: RcSlice) -> RcCallResult {
    let request: plugin_api::ListProfilesRequest = match decode_request(req) {
        Ok(req) => req,
        Err(e) => return e.into_call_result(),
    };
    let _ = request.include_experimental;

    let response = plugin_api::ListProfilesResponse {
        profiles: vec![machine_profile()],
    };
    encode_ok(&response)
}

unsafe extern "C" fn plugin_create_instance_v1(req: RcSlice) -> RcCallResult {
    plugin_api::guard::guarded_call("create_instance", || unsafe {
        plugin_create_instance_inner(req)
    })
}

unsafe fn plugin_create_instance_inner(req: RcSlice) -> RcCallResult {
    let request: MachineCreateRequest = match decode_request(req) {
        Ok(req) => req,
        Err(e) => return e.into_call_result(),
    };

    match create_machine(request) {
        Ok(response) => encode_ok(&response),
        Err(error) => RcCallResult::error_with_message(status_from_error(&error), &error),
    }
}

unsafe extern "C" fn plugin_destroy_instance_v1(instance: MachineInstanceHandle) -> RcStatus {
    plugin_api::guard::guarded(
        "destroy_instance",
        RcStatus::from_code(StatusCode::InternalError),
        || unsafe { plugin_destroy_instance_inner(instance) },
    )
}

unsafe fn plugin_destroy_instance_inner(instance: MachineInstanceHandle) -> RcStatus {
    let mut state = match state().lock() {
        Ok(guard) => guard,
        Err(_) => return RcStatus::from_code(StatusCode::InternalError),
    };

    if state.instances.remove(&instance).is_some() {
        RcStatus::ok()
    } else {
        RcStatus::from_code(StatusCode::NotFound)
    }
}

unsafe extern "C" fn plugin_instance_call_v1(
    instance: MachineInstanceHandle,
    req: RcSlice,
) -> RcCallResult {
    plugin_api::guard::guarded_call("instance_call", || unsafe {
        plugin_instance_call_inner(instance, req)
    })
}

unsafe fn plugin_instance_call_inner(
    instance: MachineInstanceHandle,
    req: RcSlice,
) -> RcCallResult {
    let request: MachineInstanceCallRequest = match decode_request(req) {
        Ok(req) => req,
        Err(e) => return e.into_call_result(),
    };

    match handle_instance_call(instance, request) {
        Ok(response) => encode_ok(&response),
        Err(error) => RcCallResult::error_with_message(status_from_error(&error), &error),
    }
}

/// Fast path for `MachinePluginVTableV1::step` -- see that field's own doc
/// comment. No `MachineCommand`/bincode round trip at all: looks the
/// instance up in the same `state()` registry `handle_instance_call` uses
/// and calls `Machine::step` directly. Returns `0` for an unrecognized
/// instance handle (never happens in practice -- the host only ever passes
/// a handle it just got back from `create_instance`) and if the state
/// mutex is poisoned, same defensive posture `handle_instance_call` already
/// takes for its own mutex lock, just with nowhere to report an error
/// through this narrower signature.
extern "C" fn plugin_step_v1(instance: MachineInstanceHandle) -> u32 {
    if plugin_api::guard::has_crashed() {
        // A crashed machine idles like a halted CPU; 0 would spin the host.
        return 4;
    }
    plugin_api::guard::guarded("step", 4, || plugin_step_inner(instance))
}

fn plugin_step_inner(instance: MachineInstanceHandle) -> u32 {
    let Ok(mut state) = state().lock() else {
        return 0;
    };
    match state.instances.get_mut(&instance) {
        Some(record) => record.step(),
        None => 0,
    }
}

/// Fast path for `MachinePluginVTableV1::halted`. Reads `1` or an
/// unrecognized instance handle/poisoned mutex, the safer sentinel for a
/// caller that only ever uses this to decide whether to keep stepping.
extern "C" fn plugin_halted_v1(instance: MachineInstanceHandle) -> u8 {
    if plugin_api::guard::has_crashed() {
        return 1;
    }
    plugin_api::guard::guarded("halted", 1, || plugin_halted_inner(instance))
}

fn plugin_halted_inner(instance: MachineInstanceHandle) -> u8 {
    let Ok(state) = state().lock() else {
        return 1;
    };
    match state.instances.get(&instance) {
        Some(record) => u8::from(record.machine.halted()),
        None => 1,
    }
}

/// Fast path for `MachinePluginVTableV1::pc`.
extern "C" fn plugin_pc_v1(instance: MachineInstanceHandle) -> u16 {
    plugin_api::guard::guarded("pc", 0, || plugin_pc_inner(instance) as u16)
}

/// Fast path for `MachinePluginVTableV1::pc_wide`.
extern "C" fn plugin_pc_wide_v1(instance: MachineInstanceHandle) -> u64 {
    plugin_api::guard::guarded("pc_wide", 0, || plugin_pc_inner(instance))
}

fn plugin_pc_inner(instance: MachineInstanceHandle) -> u64 {
    let Ok(state) = state().lock() else {
        return 0;
    };
    match state.instances.get(&instance) {
        Some(record) => record.machine.pc(),
        None => 0,
    }
}

/// # Safety
///
/// Returns immutable descriptor storage owned by this plugin.
pub unsafe extern "C" fn entry_v1() -> *const PluginDescriptorV1 {
    &PLUGIN_DESCRIPTOR_V1.0
}

#[cfg(test)]
mod tests {
    /// The shipped US-keyboard keymap (`keyboard/us_c64.toml`) may only name keys
    /// this machine lists, or the host rejects it at startup.
    #[test]
    fn the_shipped_us_keymap_only_names_listed_keys() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../keyboard/us_c64.toml");
        let text = std::fs::read_to_string(path).expect("keyboard/us_c64.toml");
        let listed: Vec<String> = key_names()
            .iter()
            .map(|info| plugin_api::normalize_key_name(&info.name))
            .collect();
        let mut in_c64 = false;
        let mut checked = 0;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') {
                in_c64 = line == "[machines.c64.keys]";
            } else if in_c64 && let Some((_, target)) = line.split_once('=') {
                let name = target.trim().trim_matches('"');
                assert!(
                    listed.contains(&plugin_api::normalize_key_name(name)),
                    "us_c64.toml names {name:?}, which the C64 does not list"
                );
                checked += 1;
            }
        }
        assert!(checked > 20, "only {checked} bindings were read");
    }

    #[test]
    fn every_typed_character_belongs_to_a_listed_key() {
        let mut infos = key_names();
        assert!(plugin_api::set_key_chars(&mut infos, C64_CHARS).is_empty());
        let all: String = infos.iter().map(|k| k.chars.as_str()).collect();
        let unique: std::collections::HashSet<char> = all.chars().collect();
        assert_eq!(all.chars().count(), unique.len(), "{all}");
        // The shifted keys are Shift + the base key the KERNAL table says.
        assert_eq!(
            c64_combo_from_name("DoubleQuote"),
            Some((C64Key::LeftShift, C64Key::Digit2))
        );
        assert_eq!(
            c64_combo_from_name("LeftBracket"),
            Some((C64Key::LeftShift, C64Key::Colon))
        );
    }

    #[test]
    fn key_names_list_everything_the_key_event_path_accepts() {
        let names = key_names();
        let listed: Vec<&str> = names.iter().map(|k| k.name.as_str()).collect();
        for expected in [
            "KeyA",
            "RunStop",
            "Commodore",
            "At",
            "F1",
            "Home",
            "F2",
            "F8",
            "Joy1Fire1",
            "Joy2Left",
        ] {
            assert!(
                listed.contains(&expected),
                "{expected} missing from {listed:?}"
            );
        }
        // Unique (ignoring case/underscores), and each entry matches its own name.
        let mut seen = std::collections::HashSet::new();
        for key in &names {
            assert!(
                seen.insert(plugin_api::normalize_key_name(&key.name)),
                "{}",
                key.name
            );
            assert!(key.matches(&key.name));
        }
        // The free aliases work: `a` finds KeyA.
        assert!(names.iter().any(|k| k.name == "KeyA" && k.matches("a")));
    }

    use super::*;

    fn payload(result: RcCallResult) -> Vec<u8> {
        assert_eq!(result.status.code, StatusCode::Ok as u32);
        // SAFETY: tests consume payload exactly once.
        unsafe { result.payload.into_vec() }
    }

    fn test_c64() -> C64 {
        C64::new(
            vec![0; KERNAL_ROM_SIZE],
            vec![0; BASIC_ROM_SIZE],
            vec![0; CHAR_ROM_SIZE],
        )
        .unwrap()
    }

    fn finish_basic_program_load_synthesizes_real_pointers_for_a_basic_program_at_0801() {
        let mut c64 = test_c64();
        // One BASIC line (line number 10, arbitrary 3-byte token content),
        // then a real end-of-program $0000 link -- the line's own leading
        // link is deliberately stale ($FFFF) to prove it gets corrected,
        // not just trusted.
        let bytes = [0xFF, 0xFF, 10, 0, 0xAA, 0xBB, 0xCC, 0x00, 0x00, 0x00];
        c64.load(0x0801, &bytes);

        finish_basic_program_load(&mut c64, 0x0801).unwrap();

        assert_eq!(
            (c64.read_memory(0x0801), c64.read_memory(0x0802)),
            (0x09, 0x08),
            "the stale leading link must be rewritten to the real next-line address"
        );
        for (lo, hi) in [(0x2D, 0x2E), (0x2F, 0x30), (0x31, 0x32)] {
            assert_eq!(
                (c64.read_memory(lo), c64.read_memory(hi)),
                (0x0B, 0x08),
                "VARTAB/ARYTAB/STREND must all coincide right after a fresh load"
            );
        }
    }

    fn finish_basic_program_load_is_a_no_op_for_a_non_0801_load() {
        let mut c64 = test_c64();
        finish_basic_program_load(&mut c64, 0xC000).unwrap();
        assert_eq!(
            c64.read_memory(0x2D),
            0,
            "a machine-code load at a non-BASIC origin must never touch VARTAB"
        );
    }

    fn relink_basic_program_errors_instead_of_hanging_on_data_with_no_real_terminator() {
        let mut c64 = test_c64();
        // Fills the entire always-plain-RAM range below the BASIC ROM
        // window ($A000) with a byte that's never a real $00 terminator --
        // far more than MAX_BASIC_LINE_LEN, so the within-line scan must
        // hit its own bound before it would otherwise run off real RAM.
        let bytes = vec![0xFFu8; 0xA000 - 0x0801];
        c64.load(0x0801, &bytes);
        let err = finish_basic_program_load(&mut c64, 0x0801).unwrap_err();
        assert!(err.contains("too long"), "got: {err}");
    }
    /// Merged (every check kept): `finish_basic_program_load_synthesizes_real_pointers_for_a_basic_program_at_0801`, `finish_basic_program_load_is_a_no_op_for_a_non_0801_load`, `relink_basic_program_errors_instead_of_hanging_on_data_with_no_real_terminator`.
    #[test]
    fn basic_program_loading() {
        finish_basic_program_load_synthesizes_real_pointers_for_a_basic_program_at_0801();
        finish_basic_program_load_is_a_no_op_for_a_non_0801_load();
        relink_basic_program_errors_instead_of_hanging_on_data_with_no_real_terminator();
    }

    fn c64_request() -> MachineCreateRequest {
        MachineCreateRequest {
            machine_id: "c64".to_string(),
            region: Some("pal".to_string()),
            roms: vec![
                BinaryBlob {
                    key: "kernal".to_string(),
                    bytes: vec![0; KERNAL_ROM_SIZE],
                },
                BinaryBlob {
                    key: "basic".to_string(),
                    bytes: vec![0; BASIC_ROM_SIZE],
                },
                BinaryBlob {
                    key: "char".to_string(),
                    bytes: vec![0; CHAR_ROM_SIZE],
                },
            ],
            options: Vec::new(),
        }
    }

    #[test]
    fn punctuation_and_shifted_function_names_map_to_c64_keys() {
        assert_eq!(c64_key_from_name("Plus"), Some(C64Key::Plus));
        assert_eq!(c64_key_from_name("Pound"), Some(C64Key::Pound));
        assert_eq!(
            c64_combo_from_name("F2"),
            Some((C64Key::LeftShift, C64Key::F1))
        );
        assert_eq!(
            c64_combo_from_name("F8"),
            Some((C64Key::LeftShift, C64Key::F7))
        );
    }

    fn descriptor_is_machine_plugin_v1() {
        let ptr = unsafe { entry_v1() };
        assert!(!ptr.is_null());
        // SAFETY: entry function returns pointer to static descriptor.
        let descriptor = unsafe { &*ptr };
        assert_eq!(descriptor.abi_major, ABI_MAJOR_V1);
        assert_eq!(descriptor.abi_minor, ABI_MINOR_V1);
        assert_eq!(descriptor.kind, PluginKind::Machine);
    }

    fn list_profiles_returns_c64_profile() {
        let req = plugin_api::ListProfilesRequest {
            include_experimental: false,
        };
        let req_bytes = plugin_api::encode_message(&req).expect("encode request");
        // SAFETY: ABI function consumes request bytes only during call.
        let result = unsafe { plugin_list_profiles_v1(RcSlice::from_bytes(&req_bytes)) };
        let bytes = payload(result);
        let response: plugin_api::ListProfilesResponse =
            plugin_api::decode_message(&bytes).expect("decode response");

        assert_eq!(response.profiles.len(), 1);
        assert_eq!(response.profiles[0].machine_id, "c64");
    }
    /// Merged (every check kept): `descriptor_is_machine_plugin_v1`, `list_profiles_returns_c64_profile`.
    #[test]
    fn descriptor_and_profile() {
        descriptor_is_machine_plugin_v1();
        list_profiles_returns_c64_profile();
    }

    /// The debugger's disassembly view asks the machine plugin for 6502 text, not raw bytes.
    #[test]
    fn disassembly_decodes_6502_instructions() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");
        // LDX #$FF / STX $D020 / BNE back to the start.
        let code = [0xA2, 0xFF, 0x8E, 0x20, 0xD0, 0xD0, 0xF9];
        let wrote = payload(call(
            create.instance,
            MachineCommand::LoadBytes {
                address: 0xC000,
                bytes: code.to_vec(),
            },
        ));
        let wrote: MachineInstanceCallResponse = plugin_api::decode_message(&wrote).unwrap();
        assert_eq!(wrote.response, MachineResponse::BytesWritten(7));
        let result = payload(call(
            create.instance,
            MachineCommand::Debug(DebugCommand::GetDisassembly {
                start: Some(0xC000),
                lines: 3,
            }),
        ));
        let result: MachineInstanceCallResponse = plugin_api::decode_message(&result).unwrap();
        let MachineResponse::Debug(DebugResponse::Disassembly(lines)) = result.response else {
            panic!("expected a disassembly, got {:?}", result.response);
        };
        let shown: Vec<(u64, usize, &str)> = lines
            .iter()
            .map(|l| (l.address, l.bytes.len(), l.text.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (0xC000, 2, "LDX #$FF"),
                (0xC002, 3, "STX $D020"),
                (0xC005, 2, "BNE $C000"),
            ]
        );
        // SAFETY: instance handle came from the create response above.
        assert!(unsafe { plugin_destroy_instance_v1(create.instance) }.is_ok());
    }

    fn create_instance_and_get_pc_round_trip() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create_payload = payload(create_result);
        let create: MachineCreateResponse =
            plugin_api::decode_message(&create_payload).expect("decode create response");

        let call_req = MachineInstanceCallRequest {
            command: MachineCommand::GetPc,
        };
        let call_req_bytes = plugin_api::encode_message(&call_req).expect("encode call request");
        // SAFETY: ABI function consumes request bytes only during call.
        let call_result = unsafe {
            plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&call_req_bytes))
        };
        let call_payload = payload(call_result);
        let call: MachineInstanceCallResponse =
            plugin_api::decode_message(&call_payload).expect("decode call response");

        assert_eq!(call.response, MachineResponse::Pc(0));

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }

    fn attach_program_and_read_memory_round_trip() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create_payload = payload(create_result);
        let create: MachineCreateResponse =
            plugin_api::decode_message(&create_payload).expect("decode create response");

        let attach_req = MachineInstanceCallRequest {
            command: MachineCommand::AttachMedia {
                slot: plugin_api::MediaSlot::Other("program".to_string()),
                kind: MediaKind::Program,
                name: Some("program.bin".to_string()),
                bytes: vec![0xEA, 0xEA],
                options: vec![NamedValue {
                    key: "org".to_string(),
                    value: DebugValue::U64(0xC000),
                }],
            },
        };
        let attach_bytes = plugin_api::encode_message(&attach_req).expect("encode attach request");
        // SAFETY: ABI function consumes request bytes only during call.
        let attach_result =
            unsafe { plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&attach_bytes)) };
        let attach_payload = payload(attach_result);
        let attach_response: MachineInstanceCallResponse =
            plugin_api::decode_message(&attach_payload).expect("decode attach response");
        assert!(matches!(attach_response.response, MachineResponse::Ack));

        let read_req = MachineInstanceCallRequest {
            command: MachineCommand::ReadMemory {
                address: 0xC000,
                len: 2,
                space: None,
            },
        };
        let read_bytes = plugin_api::encode_message(&read_req).expect("encode read request");
        // SAFETY: ABI function consumes request bytes only during call.
        let read_result =
            unsafe { plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&read_bytes)) };
        let read_payload = payload(read_result);
        let read_response: MachineInstanceCallResponse =
            plugin_api::decode_message(&read_payload).expect("decode read response");
        assert_eq!(
            read_response.response,
            MachineResponse::Memory(vec![0xEA, 0xEA])
        );

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }
    /// Merged (every check kept): `create_instance_and_get_pc_round_trip`, `attach_program_and_read_memory_round_trip`.
    #[test]
    fn instance_round_trips() {
        create_instance_and_get_pc_round_trip();
        attach_program_and_read_memory_round_trip();
    }

    fn accuracy_request(region: &str, accuracy: &str) -> MachineCreateRequest {
        let mut req = c64_request();
        req.region = Some(region.to_string());
        req.options.push(NamedValue {
            key: "vic_accuracy".to_string(),
            value: DebugValue::Text(accuracy.to_string()),
        });
        req
    }

    fn vic_accuracy_cycle_creates_a_machine_that_steps_with_bus_cycles() {
        let create_req =
            plugin_api::encode_message(&accuracy_request("pal", "cycle")).expect("encode");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");
        // The all-zero KERNAL runs BRK forever; every step still costs real
        // clock cycles (7 per BRK, more when BA stalls a read).
        let cycles = plugin_step_v1(create.instance);
        assert!(cycles >= 7, "{cycles}");
        // SAFETY: instance handle came from create response above.
        assert!(unsafe { plugin_destroy_instance_v1(create.instance) }.is_ok());
    }

    fn vic_accuracy_cycle_on_ntsc_is_refused_with_a_clear_message() {
        let create_req =
            plugin_api::encode_message(&accuracy_request("ntsc", "cycle")).expect("encode");
        // SAFETY: ABI function consumes request bytes only during call.
        let result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        assert_ne!(result.status.code, StatusCode::Ok as u32);
        // SAFETY: tests consume payload exactly once.
        let message = String::from_utf8_lossy(&unsafe { result.payload.into_vec() }).to_string();
        assert!(message.contains("PAL"), "{message}");
    }

    fn an_unknown_vic_accuracy_value_is_rejected() {
        let create_req =
            plugin_api::encode_message(&accuracy_request("pal", "exact")).expect("encode");
        // SAFETY: ABI function consumes request bytes only during call.
        let result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        assert_ne!(result.status.code, StatusCode::Ok as u32);
        // SAFETY: tests consume payload exactly once.
        drop(unsafe { result.payload.into_vec() });
    }
    /// Merged (every check kept): `vic_accuracy_cycle_creates_a_machine_that_steps_with_bus_cycles`, `vic_accuracy_cycle_on_ntsc_is_refused_with_a_clear_message`, `an_unknown_vic_accuracy_value_is_rejected`.
    #[test]
    fn vic_accuracy_option() {
        vic_accuracy_cycle_creates_a_machine_that_steps_with_bus_cycles();
        vic_accuracy_cycle_on_ntsc_is_refused_with_a_clear_message();
        an_unknown_vic_accuracy_value_is_rejected();
    }

    #[test]
    fn execution_modes_select_the_legacy_engines_and_restore_deterministically() {
        for (mode, legacy, expected) in [
            (None, "line", VicAccuracy::Line),
            (Some("fast"), "line", VicAccuracy::Line),
            (Some("accurate"), "cycle", VicAccuracy::Cycle),
        ] {
            let mut req = c64_request();
            if let Some(mode) = mode {
                req.options.push(NamedValue {
                    key: "execution_mode".to_string(),
                    value: DebugValue::Text(mode.to_string()),
                });
            }
            let selected = create_machine(req).unwrap().instance;
            let alias = create_machine(accuracy_request("pal", legacy))
                .unwrap()
                .instance;
            {
                let mut state = state().lock().unwrap();
                let machine = &mut state.instances.get_mut(&selected).unwrap().machine;
                assert_eq!(machine.vic_accuracy(), expected);
                for _ in 0..100 {
                    machine.step();
                }
                let saved = machine.save_state();
                for _ in 0..100 {
                    machine.step();
                }
                let advanced = machine.save_state();
                machine.load_state(&saved).unwrap();
                for _ in 0..100 {
                    machine.step();
                }
                assert_eq!(machine.save_state(), advanced);
                let machine = &mut state.instances.get_mut(&alias).unwrap().machine;
                for _ in 0..200 {
                    machine.step();
                }
                assert_eq!(machine.save_state(), advanced);
                machine
                    .set_vic_accuracy(if expected == VicAccuracy::Line {
                        VicAccuracy::Cycle
                    } else {
                        VicAccuracy::Line
                    })
                    .unwrap();
                assert!(
                    machine
                        .load_state(&saved)
                        .unwrap_err()
                        .contains("vic_accuracy")
                );
            }
            // SAFETY: both handles came from successful creation above.
            assert!(unsafe { plugin_destroy_instance_v1(selected) }.is_ok());
            assert!(unsafe { plugin_destroy_instance_v1(alias) }.is_ok());
        }
    }

    #[test]
    fn execution_mode_rejects_invalid_and_contradictory_options() {
        for (mode, region, legacy, message) in [
            ("slow", "pal", None, "fast or accurate"),
            ("accurate", "ntsc", None, "PAL"),
            ("fast", "pal", Some("cycle"), "contradict"),
            ("accurate", "pal", Some("line"), "contradict"),
        ] {
            let mut req = c64_request();
            req.region = Some(region.to_string());
            req.options.push(NamedValue {
                key: "execution_mode".to_string(),
                value: DebugValue::Text(mode.to_string()),
            });
            if let Some(legacy) = legacy {
                req.options.push(NamedValue {
                    key: "vic_accuracy".to_string(),
                    value: DebugValue::Text(legacy.to_string()),
                });
            }
            let error = create_machine(req).unwrap_err();
            assert!(error.contains(message), "{error}");
        }
        let mut req = accuracy_request("pal", "cycle");
        req.options.push(NamedValue {
            key: "execution_mode".to_string(),
            value: DebugValue::Text("accurate".to_string()),
        });
        let instance = create_machine(req).unwrap().instance;
        // SAFETY: handle came from successful creation above.
        assert!(unsafe { plugin_destroy_instance_v1(instance) }.is_ok());
    }

    fn attach_media(
        instance: MachineInstanceHandle,
        kind: MediaKind,
        name: Option<&str>,
        bytes: Vec<u8>,
        options: Vec<NamedValue>,
    ) -> RcCallResult {
        let attach_req = MachineInstanceCallRequest {
            command: MachineCommand::AttachMedia {
                slot: plugin_api::MediaSlot::Tape,
                kind,
                name: name.map(str::to_string),
                bytes,
                options,
            },
        };
        let attach_bytes = plugin_api::encode_message(&attach_req).expect("encode attach request");
        // SAFETY: ABI function consumes request bytes only during call.
        unsafe { plugin_instance_call_v1(instance, RcSlice::from_bytes(&attach_bytes)) }
    }

    fn attach_tap_media_installs_a_real_tape_and_acks() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        let mut tap_bytes = vec![0u8; 20];
        tap_bytes[0..12].copy_from_slice(b"C64-TAPE-RAW");
        tap_bytes.extend([10, 10]); // two 80-cycle pulses, version 0

        let result = attach_media(
            create.instance,
            MediaKind::Tape,
            Some("tape.tap"),
            tap_bytes,
            vec![NamedValue {
                key: "tape_speed".to_string(),
                value: DebugValue::U64(1),
            }],
        );
        let response: MachineInstanceCallResponse =
            plugin_api::decode_message(&payload(result)).expect("decode attach response");
        assert!(matches!(response.response, MachineResponse::Ack));

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }

    fn attach_t64_media_pokes_its_first_program_directly_into_memory() {
        // A `.t64` container has no real cassette signal (see `c64-tape`'s own
        // doc comment) -- attaching one must poke the first NORMAL entry's
        // bytes straight into memory at its own stored load address, exactly
        // like `.prg`, not install a `TapePlayer`.
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        const HEADER_SIZE: usize = 64;
        const RECORD_SIZE: usize = 32;
        let mut t64_bytes = vec![0u8; HEADER_SIZE];
        t64_bytes[34..36].copy_from_slice(&1u16.to_le_bytes()); // max entries
        t64_bytes[36..38].copy_from_slice(&1u16.to_le_bytes()); // used entries

        let data_offset = (HEADER_SIZE + RECORD_SIZE) as u32;
        let mut record = vec![0u8; RECORD_SIZE];
        record[0] = 1; // T64_FILE_RECORD_NORMAL
        record[2..4].copy_from_slice(&0xC000u16.to_le_bytes());
        record[4..6].copy_from_slice(&(0xC000u16 + 2).to_le_bytes());
        record[8..12].copy_from_slice(&data_offset.to_le_bytes());
        t64_bytes.extend(record);
        t64_bytes.extend([0xEA, 0xEA]);

        let result = attach_media(
            create.instance,
            MediaKind::Tape,
            Some("tape.t64"),
            t64_bytes,
            Vec::new(),
        );
        let response: MachineInstanceCallResponse =
            plugin_api::decode_message(&payload(result)).expect("decode attach response");
        assert!(matches!(response.response, MachineResponse::Ack));

        let read_req = MachineInstanceCallRequest {
            command: MachineCommand::ReadMemory {
                address: 0xC000,
                len: 2,
                space: None,
            },
        };
        let read_bytes = plugin_api::encode_message(&read_req).expect("encode read request");
        // SAFETY: ABI function consumes request bytes only during call.
        let read_result =
            unsafe { plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&read_bytes)) };
        let read_response: MachineInstanceCallResponse =
            plugin_api::decode_message(&payload(read_result)).expect("decode read response");
        assert_eq!(
            read_response.response,
            MachineResponse::Memory(vec![0xEA, 0xEA])
        );

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }
    /// Merged (every check kept): `attach_tap_media_installs_a_real_tape_and_acks`, `attach_t64_media_pokes_its_first_program_directly_into_memory`.
    #[test]
    fn attach_tape_media() {
        attach_tap_media_installs_a_real_tape_and_acks();
        attach_t64_media_pokes_its_first_program_directly_into_memory();
    }

    fn attach_tape_media_without_a_filename_is_a_clear_error() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        let result = attach_media(
            create.instance,
            MediaKind::Tape,
            None,
            vec![0; 4],
            Vec::new(),
        );
        assert_ne!(result.status.code, StatusCode::Ok as u32);

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }

    fn attach_disk_media_installs_a_real_d64_and_acks() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        // A real 35-track .d64 is always exactly 683 sectors * 256 bytes --
        // an all-zero image is a valid (if empty) one, since `D64::parse`
        // only checks length, not directory contents.
        let disk_bytes = vec![0u8; 683 * 256];
        let attach_req = MachineInstanceCallRequest {
            command: MachineCommand::AttachMedia {
                slot: plugin_api::MediaSlot::Disk { drive: 0 },
                kind: MediaKind::Disk,
                name: Some("disk.d64".to_string()),
                bytes: disk_bytes,
                options: Vec::new(),
            },
        };
        let attach_bytes = plugin_api::encode_message(&attach_req).expect("encode attach request");
        // SAFETY: ABI function consumes request bytes only during call.
        let attach_result =
            unsafe { plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&attach_bytes)) };
        let attach_response: MachineInstanceCallResponse =
            plugin_api::decode_message(&payload(attach_result)).expect("decode attach response");
        assert!(matches!(attach_response.response, MachineResponse::Ack));

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }

    fn attach_disk_media_installs_a_real_d71_and_acks() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        // A real 70-track .d71 is always exactly 1366 sectors * 256 bytes.
        let disk_bytes = vec![0u8; 1366 * 256];
        let attach_req = MachineInstanceCallRequest {
            command: MachineCommand::AttachMedia {
                slot: plugin_api::MediaSlot::Disk { drive: 0 },
                kind: MediaKind::Disk,
                name: Some("disk.d71".to_string()),
                bytes: disk_bytes,
                options: Vec::new(),
            },
        };
        let attach_bytes = plugin_api::encode_message(&attach_req).expect("encode attach request");
        // SAFETY: ABI function consumes request bytes only during call.
        let attach_result =
            unsafe { plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&attach_bytes)) };
        let attach_response: MachineInstanceCallResponse =
            plugin_api::decode_message(&payload(attach_result)).expect("decode attach response");
        assert!(matches!(attach_response.response, MachineResponse::Ack));

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }

    fn attach_disk_media_installs_a_real_d81_and_acks() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        // A real 80-track/40-sector .d81 is always exactly 819200 bytes.
        let disk_bytes = vec![0u8; 80 * 40 * 256];
        let attach_req = MachineInstanceCallRequest {
            command: MachineCommand::AttachMedia {
                slot: plugin_api::MediaSlot::Disk { drive: 0 },
                kind: MediaKind::Disk,
                name: Some("disk.d81".to_string()),
                bytes: disk_bytes,
                options: Vec::new(),
            },
        };
        let attach_bytes = plugin_api::encode_message(&attach_req).expect("encode attach request");
        // SAFETY: ABI function consumes request bytes only during call.
        let attach_result =
            unsafe { plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&attach_bytes)) };
        let attach_response: MachineInstanceCallResponse =
            plugin_api::decode_message(&payload(attach_result)).expect("decode attach response");
        assert!(matches!(attach_response.response, MachineResponse::Ack));

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }
    /// Merged (every check kept): `attach_disk_media_installs_a_real_d64_and_acks`, `attach_disk_media_installs_a_real_d71_and_acks`, `attach_disk_media_installs_a_real_d81_and_acks`.
    #[test]
    fn attach_disk_media() {
        attach_disk_media_installs_a_real_d64_and_acks();
        attach_disk_media_installs_a_real_d71_and_acks();
        attach_disk_media_installs_a_real_d81_and_acks();
    }

    fn attach_disk_media_rejects_an_unknown_disk_extension() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        let result = attach_media(
            create.instance,
            MediaKind::Disk,
            Some("disk.xyz"),
            vec![0u8; 683 * 256],
            Vec::new(),
        );
        assert_ne!(result.status.code, StatusCode::Ok as u32);

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }

    fn attach_disk_media_with_a_bad_size_is_a_clear_error() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        let attach_req = MachineInstanceCallRequest {
            command: MachineCommand::AttachMedia {
                slot: plugin_api::MediaSlot::Disk { drive: 0 },
                kind: MediaKind::Disk,
                name: Some("disk.d64".to_string()),
                bytes: vec![0u8; 4],
                options: Vec::new(),
            },
        };
        let attach_bytes = plugin_api::encode_message(&attach_req).expect("encode attach request");
        // SAFETY: ABI function consumes request bytes only during call.
        let attach_result =
            unsafe { plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&attach_bytes)) };
        assert_ne!(attach_result.status.code, StatusCode::Ok as u32);

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }
    /// Merged (every check kept): `attach_disk_media_rejects_an_unknown_disk_extension`, `attach_disk_media_with_a_bad_size_is_a_clear_error`.
    #[test]
    fn attach_disk_errors() {
        attach_disk_media_rejects_an_unknown_disk_extension();
        attach_disk_media_with_a_bad_size_is_a_clear_error();
    }

    /// A banked/paged poke (`WriteMemory` with a `space` set -- the
    /// debugger's own `PokeBank` command) must be a clear error, not a
    /// silent flat write to `address` regardless of the requested bank --
    /// the C64 has no debugger-facing banked-write concept implemented, so
    /// silently ignoring `space` (as an earlier version of this handler
    /// did, matching `WriteMemory { address, bytes, .. }`) would poke the
    /// wrong place without telling the caller.
    fn write_memory_with_a_space_is_a_clear_error() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        let write_req = MachineInstanceCallRequest {
            command: MachineCommand::WriteMemory {
                address: 0xC000,
                bytes: vec![0x42],
                space: Some("ram-bank:1".to_string()),
            },
        };
        let write_bytes = plugin_api::encode_message(&write_req).expect("encode write request");
        // SAFETY: ABI function consumes request bytes only during call.
        let write_result =
            unsafe { plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&write_bytes)) };
        assert_ne!(write_result.status.code, StatusCode::Ok as u32);

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }

    /// The `mouse` option wires the host mouse to the SID POT registers, and
    /// the paddle keys reach the pots and the fire lines.
    #[test]
    fn the_mouse_option_and_paddle_keys_reach_the_sid_pots() {
        let mut request = c64_request();
        request.options.push(NamedValue {
            key: "mouse".to_string(),
            value: DebugValue::Text("1351-port1".to_string()),
        });
        let create_req = plugin_api::encode_message(&request).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");
        let send = |command: MachineCommand| {
            let bytes = plugin_api::encode_message(&MachineInstanceCallRequest { command })
                .expect("encode");
            // SAFETY: ABI function consumes request bytes only during call.
            let result =
                unsafe { plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&bytes)) };
            assert_eq!(result.status.code, StatusCode::Ok as u32);
        };
        let peek = |address: u16| {
            let state = state().lock().unwrap();
            state
                .instances
                .get(&create.instance)
                .unwrap()
                .machine
                .read_memory(address.into())
        };

        // CIA1 Port A reads high at power-on (no DDR), selecting both ports.
        assert_eq!(peek(0xD419), 0x40, "an idle 1351 reads 0x40");
        send(MachineCommand::MouseMotion { dx: 9, dy: 0 });
        assert_eq!(peek(0xD419), 0x49);
        send(MachineCommand::MouseButton {
            button: plugin_api::HostMouseButton::Left,
            pressed: true,
        });
        // Left button = port 1 fire, which shows on CIA1 Port B bit 4.
        assert_eq!(peek(0xDC01) & 0x10, 0);

        send(MachineCommand::KeyEvent {
            key: "Paddle2AFire".to_string(),
            pressed: true,
        });
        // Paddle A fire is the port's Left line: port 2 -> Port A bit 2.
        assert_eq!(peek(0xDC00) & 0x04, 0);
        send(MachineCommand::KeyEvent {
            key: "Paddle2AFire".to_string(),
            pressed: false,
        });
        assert_eq!(peek(0xDC00) & 0x04, 0x04);

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }

    /// The C64 has no I/O-port space, so a debugger `write-port` must be a
    /// clear error rather than being silently dropped or mapped onto memory.
    fn write_io_is_a_clear_error_because_the_c64_has_no_port_space() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        let request = MachineInstanceCallRequest {
            command: MachineCommand::WriteIo {
                port: 0xD020,
                value: 1,
            },
        };
        let bytes = plugin_api::encode_message(&request).expect("encode request");
        // SAFETY: ABI function consumes request bytes only during call.
        let result =
            unsafe { plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&bytes)) };
        assert_ne!(result.status.code, StatusCode::Ok as u32);

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }
    /// Merged (every check kept): `write_memory_with_a_space_is_a_clear_error`, `write_io_is_a_clear_error_because_the_c64_has_no_port_space`.
    #[test]
    fn address_space_errors() {
        write_memory_with_a_space_is_a_clear_error();
        write_io_is_a_clear_error_because_the_c64_has_no_port_space();
    }

    fn attach_tape_media_with_an_unknown_extension_is_a_clear_error() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        let result = attach_media(
            create.instance,
            MediaKind::Tape,
            Some("tape.xyz"),
            vec![0; 4],
            Vec::new(),
        );
        assert_ne!(result.status.code, StatusCode::Ok as u32);

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }
    /// Merged (every check kept): `attach_tape_media_without_a_filename_is_a_clear_error`, `attach_tape_media_with_an_unknown_extension_is_a_clear_error`.
    #[test]
    fn attach_tape_errors() {
        attach_tape_media_without_a_filename_is_a_clear_error();
        attach_tape_media_with_an_unknown_extension_is_a_clear_error();
    }

    #[test]
    fn debug_chip_snapshots_expose_generic_overlay_chip_for_gui_panel() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create_payload = payload(create_result);
        let create: MachineCreateResponse =
            plugin_api::decode_message(&create_payload).expect("decode create response");

        let call_req = MachineInstanceCallRequest {
            command: MachineCommand::Debug(DebugCommand::GetChipSnapshots),
        };
        let call_req_bytes = plugin_api::encode_message(&call_req).expect("encode call request");
        // SAFETY: ABI function consumes request bytes only during call.
        let call_result = unsafe {
            plugin_instance_call_v1(create.instance, RcSlice::from_bytes(&call_req_bytes))
        };
        let call_payload = payload(call_result);
        let call: MachineInstanceCallResponse =
            plugin_api::decode_message(&call_payload).expect("decode call response");

        let MachineResponse::Debug(DebugResponse::ChipSnapshots(snapshots)) = call.response else {
            panic!("expected debug chip snapshots response");
        };
        let overlay = snapshots
            .iter()
            .find(|chip| chip.chip_id == "overlay")
            .expect("generic overlay snapshot for the --show-overlay GUI panel");
        assert!(
            overlay.fields.iter().any(|f| f.key == "memory_summary"),
            "C64's 6510 I/O-port banking always has a value -- overlay must report memory_summary"
        );
        assert!(
            !overlay.fields.iter().any(|f| f.key == "fdc_phase"),
            "C64 has no disk -- overlay must NOT report fdc_phase"
        );
        assert!(
            !overlay.fields.iter().any(|f| f.key == "tape_motor"),
            "C64 has no tape -- overlay must NOT report tape_motor"
        );
        assert!(
            overlay.blobs.is_empty(),
            "C64 has no tape -- overlay must have no tape_waveform blob"
        );

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }

    fn call(instance: MachineInstanceHandle, command: MachineCommand) -> RcCallResult {
        let req = MachineInstanceCallRequest { command };
        let bytes = plugin_api::encode_message(&req).expect("encode request");
        // SAFETY: ABI function consumes request bytes only during call.
        unsafe { plugin_instance_call_v1(instance, RcSlice::from_bytes(&bytes)) }
    }

    fn crt_file(hardware: u16, chips: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
        c64_cartridge::write(&c64_cartridge::Crt {
            version: 0x100,
            hardware_type: hardware,
            exrom_level: false,
            game_level: true,
            name: "T".into(),
            chips: chips
                .iter()
                .map(|(bank, load, data)| c64_cartridge::Chip {
                    kind: c64_cartridge::ChipKind::Rom,
                    bank: *bank,
                    load_address: *load,
                    data: data.clone(),
                })
                .collect(),
        })
    }

    #[test]
    fn freeze_needs_a_freezer_cartridge_and_export_needs_programmed_flash() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");

        let no_cart = call(create.instance, MachineCommand::Freeze);
        assert_ne!(no_cart.status.code, StatusCode::Ok as u32, "no cartridge");

        let ar: Vec<_> = (0..4u16)
            .map(|b| (b, 0x8000, vec![0x10 + b as u8; 0x2000]))
            .collect();
        let attach = attach_media(
            create.instance,
            MediaKind::Cartridge,
            Some("ar.crt"),
            crt_file(1, &ar),
            Vec::new(),
        );
        assert_eq!(attach.status.code, StatusCode::Ok as u32, "{attach:?}");
        let frozen = call(create.instance, MachineCommand::Freeze);
        assert_eq!(frozen.status.code, StatusCode::Ok as u32);

        let export = call(create.instance, MachineCommand::ExportCartridge);
        assert_eq!(export.status.code, StatusCode::Ok as u32);
        let response: MachineResponse =
            plugin_api::decode_message(&payload(export)).expect("decode export response");
        assert_eq!(
            response,
            MachineResponse::CartridgeImage(None),
            "nothing flashed"
        );

        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }

    #[test]
    fn unsupported_cartridge_types_name_the_new_ones_in_the_error() {
        let create_req = plugin_api::encode_message(&c64_request()).expect("encode create req");
        // SAFETY: ABI function consumes request bytes only during call.
        let create_result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&create_req)) };
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_result)).expect("decode create response");
        let result = attach_media(
            create.instance,
            MediaKind::Cartridge,
            Some("x.crt"),
            crt_file(99, &[(0, 0x8000, vec![0; 0x2000])]),
            Vec::new(),
        );
        assert_ne!(result.status.code, StatusCode::Ok as u32);
        // SAFETY: instance handle came from create response above.
        let status = unsafe { plugin_destroy_instance_v1(create.instance) };
        assert!(status.is_ok());
    }

    /// `firmware.toml` (next to the family's `Cargo.toml`) tells `--firmware auto` which file fills which machine
    /// option. An entry naming an option this plugin does not declare as a file option can never apply: this is
    /// exactly how the C64/C128 manifest once matched nothing (`rom_kernal` against the declared `kernal`).
    #[test]
    fn firmware_toml_names_only_file_options_the_plugin_declares() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../firmware.toml");
        let text = std::fs::read_to_string(path).expect("the family's firmware.toml");
        let manifest: toml::Value = text.parse().expect("firmware.toml is valid TOML");
        let profiles = [machine_profile()];
        let mut checked = 0;
        for entry in manifest["rom"].as_array().expect("[[rom]] entries") {
            let option = entry["option"].as_str().expect("option = \"..\"");
            for machine in entry["machines"].as_array().expect("machines = [..]") {
                let id = machine.as_str().expect("a machine id");
                // Another plugin of the family serves the machines this one does not.
                let Some(profile) = profiles.iter().find(|p| p.machine_id == id) else {
                    continue;
                };
                let declared: Vec<&str> = profile
                    .options
                    .iter()
                    .filter(|o| o.kind == plugin_api::MachineOptionKind::File)
                    .map(|o| o.key.as_str())
                    .collect();
                assert!(
                    declared.contains(&option),
                    "firmware.toml: \"{}\" names option \"{option}\" for {id}, which declares the file options {declared:?}",
                    entry["label"].as_str().unwrap_or("?")
                );
                checked += 1;
            }
        }
        assert!(
            checked > 0,
            "no firmware.toml entry applies to this plugin's machines"
        );
    }
}
