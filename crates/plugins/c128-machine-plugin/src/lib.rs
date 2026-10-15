//! The Commodore 128 as a machine plugin: 8502/Z80 + MMU + VIC-IIe + VDC
//! (`c128`), optionally with a real 1541/1570/1571/1581 on the IEC bus
//! (`drive_link`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use c128::{C128, C128Key, JoystickButton, VicAccuracy};
use machine::{AUDIO_SAMPLE_RATE, Machine, Region};
use plugin_api::{
    ABI_MAJOR_V1, ABI_MINOR_V1, AddressSpaceInfo, BinaryBlob, BitmapExplorerDefaults,
    BitmapPaletteColor, ChipSnapshot, CodecError, CpuFamily, DebugCommand, DebugResponse,
    DebugValue, DisassemblyLine, MACHINE_PROFILE_CONTRACT_VERSION, MachineCapability,
    MachineCommand, MachineCreateRequest, MachineCreateResponse, MachineInstanceCallRequest,
    MachineInstanceCallResponse, MachineInstanceHandle, MachineMetadata, MachineOptionInfo,
    MachineOptionKind, MachinePluginVTableV1, MachineProfile, MachineResponse, MediaKind,
    MediaSnapshot, NamedValue, PluginDescriptorV1, PluginKind, REQUIRED_MACHINE_DEBUG_CAPABILITIES,
    RcBuffer, RcCallResult, RcSlice, RcStatus, RegisterField, RegisterSnapshot, StatusCode,
};

const PLUGIN_ID: &[u8] = b"retroclawd.c128.machine";
const PLUGIN_VERSION: &[u8] = env!("CARGO_PKG_VERSION").as_bytes();

use drive_link as drive;

/// The C128's end of the IEC bus for `drive::DriveLink`.
struct IecHost<'a>(&'a mut C128);

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
};

static PLUGIN_DESCRIPTOR_V1: StaticPluginDescriptor = StaticPluginDescriptor(PluginDescriptorV1 {
    abi_major: ABI_MAJOR_V1,
    abi_minor: ABI_MINOR_V1,
    kind: PluginKind::Machine,
    id: static_slice(PLUGIN_ID),
    version: static_slice(PLUGIN_VERSION),
    vtable: (&MACHINE_VTABLE_V1 as *const MachinePluginVTableV1).cast(),
});

struct PluginState {
    next_instance: MachineInstanceHandle,
    instances: HashMap<MachineInstanceHandle, InstanceRecord>,
}

struct InstanceRecord {
    machine: C128,
    region: String,
    paused: bool,
    breakpoints: Vec<u16>,
    /// The opt-in true drive (`true_drive` + `rom_1541`).
    drive: Option<drive::DriveLink>,
    /// Booted into C64 mode (`mode = c64`): picks the idle loop to wait for.
    c64_mode: bool,
    /// A BASIC program waiting for the machine to finish booting: the
    /// BASIC cold start rewrites the start of program memory, so a program
    /// poked before then would be wiped (see `inject_pending_program`).
    pending_program: Option<(u16, Vec<u8>)>,
}

/// BASIC 7.0's text start in C128 mode and BASIC 2.0's in C64 mode.
const C128_BASIC_START: u16 = 0x1C01;
const C64_BASIC_START: u16 = 0x0801;
/// The editor's / KERNAL's "wait for a key" loops: where the machine sits
/// once it has finished booting and is idle at the `READY.` prompt.
const C128_IDLE_PC: std::ops::RangeInclusive<u16> = 0xC25E..=0xC262;
const C64_IDLE_PC: std::ops::RangeInclusive<u16> = 0xE5CD..=0xE5D4;

impl InstanceRecord {
    /// Pokes the pending BASIC program in once the machine idles at its
    /// prompt, then fixes BASIC's pointers so `RUN` works.
    fn inject_pending_program(&mut self) {
        if self.pending_program.is_none() {
            return;
        }
        let idle = if self.c64_mode {
            &C64_IDLE_PC
        } else {
            &C128_IDLE_PC
        };
        if self.machine.cpu_name() != "8502"
            || self.machine.mmu().c64_mode() != self.c64_mode
            || !idle.contains(&self.machine.pc())
        {
            return;
        }
        if let Some((org, bytes)) = self.pending_program.take() {
            self.machine.load(org, &bytes);
            // A malformed program is left as loaded; `RUN` then reports it.
            let _ = finish_basic_program_load(&mut self.machine, org);
        }
    }

    /// One emulated instruction, plus the drive catching up with it.
    fn step(&mut self) -> u32 {
        let cycles = self.machine.step();
        self.inject_pending_program();
        if let Some(drive) = &mut self.drive {
            drive.after_host_step(&mut IecHost(&mut self.machine), cycles);
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

fn file_option(key: &str, flag: &str, description: &str, required: bool) -> MachineOptionInfo {
    MachineOptionInfo {
        key: key.to_string(),
        flag: flag.to_string(),
        short: None,
        kind: MachineOptionKind::File,
        default: None,
        required,
        description: description.to_string(),
    }
}

fn machine_profile() -> MachineProfile {
    MachineProfile {
        machine_id: "c128".to_string(),
        display_name: "Commodore 128".to_string(),
        cpu_family: CpuFamily::Mos6502,
        default_region: Some("pal".to_string()),
        contract_version: MACHINE_PROFILE_CONTRACT_VERSION,
        debug_capabilities: REQUIRED_MACHINE_DEBUG_CAPABILITIES.to_vec(),
        capabilities: vec![
            MachineCapability::Video,
            MachineCapability::SaveState,
            MachineCapability::Debug,
        ],
        media_kinds: vec![MediaKind::Program, MediaKind::Disk],
        options: vec![
            file_option(
                "basic",
                "rom-basic",
                "C128 BASIC ROM image (32K: BASIC low followed by BASIC high)",
                true,
            ),
            file_option(
                "kernal",
                "rom-kernal",
                "C128 kernal image (16K: editor, Z80 BIOS and kernal)",
                true,
            ),
            file_option(
                "char",
                "rom-char",
                "C128 character ROM image (8K: C64 set then C128 set)",
                true,
            ),
            file_option(
                "c64_basic",
                "rom-c64-basic",
                "C64-mode BASIC ROM image (8K)",
                true,
            ),
            file_option(
                "c64_kernal",
                "rom-c64-kernal",
                "C64-mode KERNAL ROM image (8K)",
                true,
            ),
            MachineOptionInfo {
                key: "display".to_string(),
                flag: "display".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec!["40".to_string(), "80".to_string()],
                },
                default: Some(DebugValue::Text("40".to_string())),
                required: false,
                description: "Which screen the machine starts on: 40 columns (VIC-IIe) or 80 \
                    columns (VDC); like the real 40/80 DISPLAY key, it is read at reset"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "vic_accuracy".to_string(),
                flag: "vic-accuracy".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec!["line".to_string(), "cycle".to_string()],
                },
                default: Some(DebugValue::Text("line".to_string())),
                required: false,
                description: "VIC-IIe model: line (default; raster and bad-line steal advance \
                    per CPU instruction, fast) or cycle (the real per-cycle schedule: every \
                    8502 bus cycle advances the machine one clock, bad lines and sprite DMA \
                    stall the 8502 through BA, mid-line register writes show; the Z80 and 2 MHz \
                    mode are ticked per instruction and not stalled; PAL only, slower)"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "mode".to_string(),
                flag: "mode".to_string(),
                short: None,
                kind: MachineOptionKind::Enum {
                    choices: vec!["c128".to_string(), "c64".to_string()],
                },
                default: Some(DebugValue::Text("c128".to_string())),
                required: false,
                description: "Which mode the machine boots into: c128 (default) or c64, as if \
                    the Commodore key were held at power-on"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "run".to_string(),
                flag: "run".to_string(),
                short: Some('r'),
                kind: MachineOptionKind::Bool,
                default: Some(DebugValue::Bool(false)),
                required: false,
                description: "Auto-type the startup command for the attached media: RUN\"*\" \
                    for a disk (C128 mode; LOAD\"*\",8,1 then RUN in C64 mode, with a \
                    --true-drive) or RUN for a .prg program"
                    .to_string(),
            },
            file_option(
                "rom_1541",
                "rom-1541",
                "DOS ROM image of the --drive-type drive (16K for the 1541, 32K for the \
                 1570/1571/1581), user supplied; only used with --true-drive",
                false,
            ),
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
                default: Some(DebugValue::Text("1571".to_string())),
                required: false,
                description: "Which drive --true-drive runs: 1541, 1570, 1571 (the C128's own \
                    drive; double-sided .d71) or 1581 (3.5\" .d81). The C128 uses the drive in \
                    its ordinary serial mode (burst mode is not modelled)"
                    .to_string(),
            },
            MachineOptionInfo {
                key: "true_drive".to_string(),
                flag: "true-drive".to_string(),
                short: None,
                kind: MachineOptionKind::Bool,
                default: Some(DebugValue::Bool(false)),
                required: false,
                description: "Run a real drive (its own 6502 and DOS, --rom-1541) on the IEC \
                    bus; the C128 has no instant load traps, so this is how a disk is used"
                    .to_string(),
            },
        ],
        key_names: key_names(),
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

fn required_blob(roms: &[BinaryBlob], key: &str, flag: &str) -> Result<Vec<u8>, String> {
    blob(roms, key)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| format!("missing required ROM: pass --{flag}"))
}

fn option_value<'a>(options: &'a [NamedValue], key: &str) -> Option<&'a DebugValue> {
    options
        .iter()
        .find(|item| item.key.eq_ignore_ascii_case(key))
        .map(|item| &item.value)
}

fn media_extension(name: Option<&str>) -> Option<String> {
    name.and_then(|filename| filename.rsplit('.').next())
        .map(str::to_ascii_lowercase)
}

fn option_bool(options: &[NamedValue], key: &str) -> Result<Option<bool>, String> {
    match option_value(options, key) {
        None => Ok(None),
        Some(DebugValue::Bool(v)) => Ok(Some(*v)),
        Some(_) => Err(format!("option \"{key}\" must be a boolean")),
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
    match option_value(options, key) {
        None => Ok(None),
        Some(DebugValue::U64(v)) => u32::try_from(*v)
            .map(Some)
            .map_err(|_| format!("option \"{key}\" out of range for u32")),
        Some(_) => Err(format!("option \"{key}\" must be an unsigned integer")),
    }
}

/// Longest BASIC line / program the relink walk accepts before it calls the
/// data malformed instead of looping on it.
const MAX_BASIC_LINES: usize = 65_536;
const MAX_BASIC_LINE_LEN: usize = 512;

/// Walks the program's line-link chain from `start`, rewriting each link to
/// the line's real next address (as BASIC's own LINKPRG does), and returns
/// the address after the terminating `$0000` link.
fn relink_basic_program(machine: &mut C128, start: u16) -> Result<u16, String> {
    let mut addr = start;
    for _ in 0..MAX_BASIC_LINES {
        if machine.read_memory(addr) == 0 && machine.read_memory(addr.wrapping_add(1)) == 0 {
            return Ok(addr.wrapping_add(2));
        }
        let mut p = addr.wrapping_add(4); // past the link and the line number
        let mut scanned = 0usize;
        while machine.read_memory(p) != 0 {
            p = p.wrapping_add(1);
            scanned += 1;
            if scanned > MAX_BASIC_LINE_LEN {
                return Err("malformed BASIC program (line too long to relink)".to_string());
            }
        }
        p = p.wrapping_add(1);
        machine.load(addr, &p.to_le_bytes());
        addr = p;
    }
    Err("malformed BASIC program (too many lines to relink)".to_string())
}

/// Relinks a program poked at BASIC's text start and tells BASIC where it
/// ends: C128 BASIC keeps that in `$1210/$1211` (its variables live in
/// bank 1), C64 BASIC in VARTAB/ARYTAB/STREND at `$2D-$32`. Any other load
/// address is machine code and is left alone.
fn finish_basic_program_load(machine: &mut C128, org: u16) -> Result<(), String> {
    match org {
        C128_BASIC_START => {
            let end = relink_basic_program(machine, org)?;
            machine.load(0x1210, &end.to_le_bytes());
        }
        C64_BASIC_START => {
            let end = relink_basic_program(machine, org)?.to_le_bytes();
            machine.load(0x2D, &end);
            machine.load(0x2F, &end);
            machine.load(0x31, &end);
        }
        _ => {}
    }
    Ok(())
}

fn create_machine(req: MachineCreateRequest) -> Result<MachineCreateResponse, String> {
    if !req.machine_id.eq_ignore_ascii_case("c128") {
        return Err(format!(
            "unsupported machine id \"{}\" (plugin supports only c128)",
            req.machine_id
        ));
    }
    let region = parse_region(req.region.as_deref())?;
    let region_string = req
        .region
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "pal".to_string());

    let mut basic = required_blob(&req.roms, "basic", "rom-basic")?;
    // Accept the two 16K halves concatenated by the user as one 32K file.
    basic.shrink_to_fit();
    let roms = c128::Roms {
        basic,
        kernal: required_blob(&req.roms, "kernal", "rom-kernal")?,
        chargen: required_blob(&req.roms, "char", "rom-char")?,
        c64_basic: required_blob(&req.roms, "c64_basic", "rom-c64-basic")?,
        c64_kernal: required_blob(&req.roms, "c64_kernal", "rom-c64-kernal")?,
    };
    let mut machine = C128::new(roms)?;
    machine.set_region(region);
    match option_text(&req.options, "vic_accuracy")?.as_deref() {
        None | Some("line") => {}
        Some("cycle") => machine.set_vic_accuracy(VicAccuracy::Cycle)?,
        Some(other) => {
            return Err(format!(
                "option \"vic_accuracy\" must be line or cycle, not \"{other}\""
            ));
        }
    }
    match option_text(&req.options, "display")?.as_deref() {
        None | Some("40") => {}
        Some("80") => machine.set_80_columns(true),
        Some(other) => {
            return Err(format!(
                "option \"display\" must be 40 or 80, not \"{other}\""
            ));
        }
    }
    let c64_mode = match option_text(&req.options, "mode")?.as_deref() {
        None | Some("c128") => false,
        Some("c64") => {
            machine.boot_into_c64_mode();
            true
        }
        Some(other) => {
            return Err(format!(
                "option \"mode\" must be c128 or c64, not \"{other}\""
            ));
        }
    };
    let drive = if option_bool(&req.options, "true_drive")? == Some(true) {
        let kind = match option_text(&req.options, "drive_type")?.as_deref() {
            None => drive::DriveKind::C1571,
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
            drive,
            c64_mode,
            pending_program: None,
        },
    );

    Ok(MachineCreateResponse {
        instance,
        metadata: MachineMetadata {
            machine_id: "c128".to_string(),
            model: Some("c128".to_string()),
            region: Some(region_string),
            notes: vec!["external machine plugin v1".to_string()],
            bitmap_palette: bitmap_palette(),
            bitmap_defaults: Some(bitmap_defaults()),
        },
    })
}

/// Every key name this machine's `KeyEvent` accepts -- built from the same
/// tables the event handler looks names up in, so the list cannot drift.
fn key_names() -> Vec<plugin_api::MachineKeyInfo> {
    let mut infos = plugin_api::key_infos(
        c128_key_table()
            .iter()
            .map(|(name, _)| *name)
            .chain(c128_combo_table().iter().map(|(name, _)| *name))
            .chain(c128_joystick_table().iter().map(|(name, _)| *name)),
    );
    plugin_api::set_key_chars(&mut infos, C128_CHARS);
    infos
}

/// The ASCII character each key types on a real C128 in its power-on
/// (ASCII-keyboard-less) layout: the C64 legends, with the shifted ones on the
/// combo keys.
const C128_CHARS: &[(&str, &str)] = &[
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
    ("Numpad0", "0"),
    ("Numpad1", "1"),
    ("Numpad2", "2"),
    ("Numpad3", "3"),
    ("Numpad4", "4"),
    ("Numpad5", "5"),
    ("Numpad6", "6"),
    ("Numpad7", "7"),
    ("Numpad8", "8"),
    ("Numpad9", "9"),
];

/// Control-port joystick names, the same ones the C64 plugin uses.
fn c128_joystick_table() -> &'static [(&'static str, (u8, JoystickButton))] {
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

fn c128_joystick_from_name(name: &str) -> Option<(u8, JoystickButton)> {
    c128_joystick_table()
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| *value)
}

fn c128_key_table() -> &'static [(&'static str, C128Key)] {
    use C128Key::*;
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
        // The dedicated cursor block; the C64-style pair is `CursorRight`/`CursorDown`.
        ("ArrowLeft", Left),
        ("ArrowRight", Right),
        ("ArrowUp", Up),
        ("ArrowDown", Down),
        ("CursorRight", CursorRight),
        ("CursorDown", CursorDown),
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
        // The C128's own keys.
        ("Tab", Tab),
        ("Esc", Escape),
        ("Backquote", Escape),
        ("Help", Help),
        ("PageUp", Help),
        ("LineFeed", LineFeed),
        ("PageDown", LineFeed),
        ("Alt", Alt),
        ("NoScroll", NoScroll),
        ("ScrollLock", NoScroll),
        ("CapsLock", CapsLock),
        ("Display4080", Display4080),
        ("Numpad0", Keypad0),
        ("Numpad1", Keypad1),
        ("Numpad2", Keypad2),
        ("Numpad3", Keypad3),
        ("Numpad4", Keypad4),
        ("Numpad5", Keypad5),
        ("Numpad6", Keypad6),
        ("Numpad7", Keypad7),
        ("Numpad8", Keypad8),
        ("Numpad9", Keypad9),
        ("NumpadAdd", KeypadPlus),
        ("NumpadSubtract", KeypadMinus),
        ("NumpadEnter", KeypadEnter),
        ("NumpadDecimal", KeypadPeriod),
    ]
}

fn c128_key_from_name(name: &str) -> Option<C128Key> {
    c128_key_table()
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| *value)
}

fn c128_combo_table() -> &'static [(&'static str, (C128Key, C128Key))] {
    use C128Key::*;
    &[
        ("F2", (LeftShift, F1)),
        ("F4", (LeftShift, F3)),
        ("F6", (LeftShift, F5)),
        ("F8", (LeftShift, F7)),
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

fn c128_combo_from_name(name: &str) -> Option<(C128Key, C128Key)> {
    c128_combo_table()
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| *value)
}

fn register_snapshot(registers: Vec<(&'static str, u64)>, z80_active: bool) -> RegisterSnapshot {
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
        cpu_family: if z80_active {
            CpuFamily::Z80
        } else {
            CpuFamily::Mos6502
        },
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

/// Machine-agnostic overlay chip snapshot (`chip_id: "overlay"`) for the
/// `--show-overlay` panel: which CPU runs and the MMU's mapping.
fn generic_overlay_chip_snapshot(record: &InstanceRecord) -> ChipSnapshot {
    let m = &record.machine;
    let mmu = m.mmu();
    let summary = format!(
        "cpu={} mode={} cr={:02X} mcr={:02X} rcr={:02X} display={}",
        m.cpu_name(),
        if mmu.c64_mode() { "c64" } else { "c128" },
        mmu.cr(),
        mmu.mcr(),
        mmu.rcr(),
        if m.is_80_columns() { "80" } else { "40" },
    );
    ChipSnapshot {
        chip_id: "overlay".to_string(),
        fields: vec![nv_text("memory_summary", summary)],
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
                .map(|offset| record.machine.read_memory(address.wrapping_add(offset)))
                .collect();
            Ok(MachineResponse::Memory(bytes))
        }
        // A `space` request is a clear error, not a silent flat write: the
        // C128's banking is address-range based and not exposed as named banks.
        MachineCommand::WriteMemory { space: Some(_), .. } => {
            Err("banked/paged memory writes are not supported for the c128 plugin".to_string())
        }
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
            "unsupported snapshot save extension \".{extension}\" for c128 plugin"
        )),
        MachineCommand::LoadState { bytes } => {
            match (drive::unpack_drive_state(&bytes)?, &mut record.drive) {
                (Some((machine_state, drive_state)), Some(drive)) => {
                    record.machine.load_state(machine_state)?;
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
                        "the state has no drive in it; load it without --true-drive".to_string()
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
                let default_org = if record.c64_mode {
                    C64_BASIC_START
                } else {
                    C128_BASIC_START
                };
                let org = option_u32(&options, "org")?.unwrap_or(u32::from(default_org));
                let org = u16::try_from(org)
                    .map_err(|_| format!("program org out of range for u16: {org}"))?;
                if org == C128_BASIC_START || org == C64_BASIC_START {
                    // BASIC's cold start would wipe the head of the program.
                    record.pending_program = Some((org, bytes));
                } else {
                    record.machine.load(org, &bytes);
                }
                Ok(MachineResponse::Ack)
            }
            MediaKind::Disk => {
                let Some(drive) = &mut record.drive else {
                    return Err(
                        "a disk needs a drive: start with --true-drive and --rom-1541 \
                         (the C128 has no instant load traps)"
                            .to_string(),
                    );
                };
                let extension =
                    media_extension(name.as_deref()).unwrap_or_else(|| "d64".to_string());
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
                Ok(MachineResponse::Ack)
            }
            other => Err(format!("unsupported media kind for c128 plugin: {other:?}")),
        },
        MachineCommand::DetachMedia { slot } => match slot {
            plugin_api::MediaSlot::Disk { drive: 0 | 8 } => {
                if let Some(drive) = &mut record.drive {
                    drive.detach_disk();
                }
                Ok(MachineResponse::Ack)
            }
            other => Err(format!(
                "nothing to eject from slot {other:?} on the c128 plugin (only the disk)"
            )),
        },
        MachineCommand::ExportDisk {
            drive,
            only_if_modified,
        } => {
            if drive != 0 {
                return Err(format!("drive {drive} does not exist (only one disk)"));
            }
            let image = match (&mut record.drive, only_if_modified) {
                (Some(drive), true) => drive.take_modified_disk_image(),
                (Some(drive), false) => drive.disk_image(),
                (None, _) => None,
            };
            Ok(MachineResponse::DiskImage(image))
        }
        MachineCommand::MarkDiskSaved { drive } => {
            if drive != 0 {
                return Err(format!("drive {drive} does not exist (only one disk)"));
            }
            if let Some(drive) = &mut record.drive {
                drive.mark_disk_saved();
            }
            Ok(MachineResponse::Ack)
        }
        MachineCommand::ExportTape { .. } => {
            Err("unsupported: the c128 plugin has no cassette".to_string())
        }
        MachineCommand::Freeze
        | MachineCommand::ExportCartridge
        | MachineCommand::ExportPrinter => {
            Err("unsupported: the c128 plugin has no freezer, cartridge or printer".to_string())
        }
        MachineCommand::GetMediaSnapshot => Ok(MachineResponse::MediaSnapshot(MediaSnapshot {
            tape: None,
            disks: Vec::new(),
            cartridge: None,
        })),
        MachineCommand::SetBorder { enabled: _ } => Ok(MachineResponse::Ack),
        MachineCommand::KeyEvent { key, pressed } => {
            if let Some((port, button)) = c128_joystick_from_name(&key) {
                if pressed {
                    record.machine.press_joystick(port, button);
                } else {
                    record.machine.release_joystick(port, button);
                }
                return Ok(MachineResponse::Ack);
            }
            let keys = c128_combo_from_name(&key)
                .map(|(modifier, base)| vec![modifier, base])
                .or_else(|| c128_key_from_name(&key).map(|key| vec![key]));
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
        MachineCommand::MouseMotion { .. }
        | MachineCommand::MouseButton { .. }
        | MachineCommand::MouseWheel { .. } => Ok(MachineResponse::Ack),
        MachineCommand::WriteIo { .. } | MachineCommand::SetPortWatches { .. } => {
            Err("the c128's 8502 has no I/O-port space (the Z80 only runs during boot)".to_string())
        }
        MachineCommand::TakePortHit => Ok(MachineResponse::PortHit(None)),
        MachineCommand::RenderFrame => {
            let (width, height, rgb) = record
                .machine
                .render_frame()
                .ok_or_else(|| "render_frame unavailable for c128".to_string())?;
            Ok(MachineResponse::Frame { width, height, rgb })
        }
        MachineCommand::DrainAudio => Ok(MachineResponse::AudioPcmMono {
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
            record.machine.cpu_name() == "Z80",
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
                .map(|offset| record.machine.read_memory(address.wrapping_add(offset)))
                .collect();
            Ok(DebugResponse::Memory(bytes))
        }
        DebugCommand::WriteMemory { address, bytes, .. } => {
            record.machine.load(address, &bytes);
            Ok(DebugResponse::Ack)
        }
        DebugCommand::GetDisassembly { start, lines } => {
            // The C128 runs an 8502 or a Z80 (MMU mode): disassemble for whichever is active.
            let z80_active = record.machine.cpu_name() == "Z80";
            let mut cursor = start.unwrap_or(record.machine.pc());
            let mut out = Vec::with_capacity(lines as usize);
            for _ in 0..lines {
                let raw: Vec<u8> = (0..4u16)
                    .map(|offset| record.machine.read_memory(cursor.wrapping_add(offset)))
                    .collect();
                let (text, consumed) = if z80_active {
                    let (text, consumed) = z80::disassemble::disassemble(&raw);
                    (text, consumed.clamp(1, 4))
                } else {
                    mos6502::disassemble::disassemble(&raw, cursor)
                };
                out.push(DisassemblyLine {
                    address: cursor,
                    bytes: raw[..consumed].to_vec(),
                    text,
                });
                cursor = cursor.wrapping_add(consumed as u16);
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
        DebugCommand::GetMachineMetadata => Ok(DebugResponse::MachineMetadata(MachineMetadata {
            machine_id: "c128".to_string(),
            model: Some("c128".to_string()),
            region: Some(record.region.clone()),
            notes: vec!["external machine plugin v1".to_string()],
            bitmap_palette: bitmap_palette(),
            bitmap_defaults: Some(bitmap_defaults()),
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

/// Fast path for `MachinePluginVTableV1::step` (no serialization).
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

/// Fast path for `MachinePluginVTableV1::halted`.
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
    plugin_api::guard::guarded("pc", 0, || plugin_pc_inner(instance))
}

fn plugin_pc_inner(instance: MachineInstanceHandle) -> u16 {
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
    use super::*;

    fn payload(result: RcCallResult) -> Vec<u8> {
        assert_eq!(result.status.code, StatusCode::Ok as u32);
        // SAFETY: tests consume payload exactly once.
        unsafe { result.payload.into_vec() }
    }

    fn request() -> MachineCreateRequest {
        let blob = |key: &str, len: usize| BinaryBlob {
            key: key.to_string(),
            bytes: vec![0xEA; len],
        };
        MachineCreateRequest {
            machine_id: "c128".to_string(),
            region: Some("pal".to_string()),
            roms: vec![
                blob("basic", c128::BASIC_ROM_SIZE),
                blob("kernal", c128::KERNAL_IMAGE_SIZE),
                blob("char", c128::CHARGEN_SIZE),
                blob("c64_basic", c128::C64_ROM_SIZE),
                blob("c64_kernal", c128::C64_ROM_SIZE),
            ],
            options: Vec::new(),
        }
    }

    fn create() -> MachineInstanceHandle {
        let bytes = plugin_api::encode_message(&request()).unwrap();
        // SAFETY: ABI function consumes request bytes only during call.
        let result = unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&bytes)) };
        let response: MachineCreateResponse = plugin_api::decode_message(&payload(result)).unwrap();
        response.instance
    }

    fn call(instance: MachineInstanceHandle, command: MachineCommand) -> RcCallResult {
        let bytes = plugin_api::encode_message(&MachineInstanceCallRequest { command }).unwrap();
        // SAFETY: ABI function consumes request bytes only during call.
        unsafe { plugin_instance_call_v1(instance, RcSlice::from_bytes(&bytes)) }
    }

    fn response(result: RcCallResult) -> MachineResponse {
        plugin_api::decode_message::<MachineInstanceCallResponse>(&payload(result))
            .unwrap()
            .response
    }

    /// The C128 powers up on its Z80: registers and disassembly are Z80's, not 6502 text.
    #[test]
    fn disassembly_and_register_family_follow_the_running_cpu() {
        let instance = create();
        let MachineResponse::Debug(DebugResponse::Registers(snapshot)) = response(call(
            instance,
            MachineCommand::Debug(DebugCommand::GetRegisters),
        )) else {
            panic!("expected registers");
        };
        assert_eq!(snapshot.cpu_family, CpuFamily::Z80);
        // LD A,5 is `3E 05` on a Z80; an 8502 would read the same bytes as `*RLA ...`-style garbage.
        let MachineResponse::BytesWritten(_) = response(call(
            instance,
            MachineCommand::LoadBytes {
                address: 0x2000,
                bytes: vec![0x3E, 0x05],
            },
        )) else {
            panic!("expected BytesWritten");
        };
        let MachineResponse::Debug(DebugResponse::Disassembly(lines)) = response(call(
            instance,
            MachineCommand::Debug(DebugCommand::GetDisassembly {
                start: Some(0x2000),
                lines: 2,
            }),
        )) else {
            panic!("expected a disassembly");
        };
        assert_eq!(lines.len(), 2);
        assert!(lines[0].text.starts_with("LD A,"), "{:?}", lines[0].text);
        assert_eq!(lines[0].bytes, vec![0x3E, 0x05]);
        assert_eq!(lines[1].address, 0x2002);
        // SAFETY: instance handle came from create().
        assert!(unsafe { plugin_destroy_instance_v1(instance) }.is_ok());
    }

    #[test]
    fn descriptor_is_machine_plugin_v1() {
        // SAFETY: entry function returns pointer to static descriptor.
        let descriptor = unsafe { &*entry_v1() };
        assert_eq!(descriptor.abi_major, ABI_MAJOR_V1);
        assert_eq!(descriptor.abi_minor, ABI_MINOR_V1);
        assert_eq!(descriptor.kind, PluginKind::Machine);
    }

    #[test]
    fn the_profile_declares_the_five_roms_and_the_drive_options() {
        let profile = machine_profile();
        assert_eq!(profile.machine_id, "c128");
        let keys: Vec<&str> = profile.options.iter().map(|o| o.key.as_str()).collect();
        for expected in [
            "basic",
            "kernal",
            "char",
            "c64_basic",
            "c64_kernal",
            "display",
            "vic_accuracy",
            "rom_1541",
            "drive_type",
            "true_drive",
        ] {
            assert!(keys.contains(&expected), "{expected} missing from {keys:?}");
        }
    }

    fn accuracy_request(region: &str, accuracy: &str) -> MachineCreateRequest {
        let mut req = request();
        req.region = Some(region.to_string());
        req.options.push(NamedValue {
            key: "vic_accuracy".to_string(),
            value: DebugValue::Text(accuracy.to_string()),
        });
        req
    }

    fn create_with(req: &MachineCreateRequest) -> RcCallResult {
        let bytes = plugin_api::encode_message(req).unwrap();
        // SAFETY: ABI function consumes request bytes only during call.
        unsafe { plugin_create_instance_v1(RcSlice::from_bytes(&bytes)) }
    }

    #[test]
    fn vic_accuracy_cycle_creates_a_machine_that_steps_with_bus_cycles() {
        let create: MachineCreateResponse =
            plugin_api::decode_message(&payload(create_with(&accuracy_request("pal", "cycle"))))
                .unwrap();
        // The BIOS is a NOP sled; a step costs real clock cycles.
        assert!(plugin_step_v1(create.instance) >= 1);
        // SAFETY: instance handle came from the create response above.
        assert!(unsafe { plugin_destroy_instance_v1(create.instance) }.is_ok());
    }

    #[test]
    fn vic_accuracy_cycle_on_ntsc_and_unknown_values_are_refused() {
        for (region, value, expect) in [("ntsc", "cycle", "PAL"), ("pal", "exact", "line or cycle")]
        {
            let result = create_with(&accuracy_request(region, value));
            assert_ne!(result.status.code, StatusCode::Ok as u32);
            // SAFETY: tests consume payload exactly once.
            let message =
                String::from_utf8_lossy(&unsafe { result.payload.into_vec() }).to_string();
            assert!(message.contains(expect), "{message}");
        }
    }

    #[test]
    fn a_missing_or_wrong_sized_rom_is_a_clear_error() {
        let mut req = request();
        req.roms.retain(|b| b.key != "c64_kernal");
        assert!(create_machine(req).unwrap_err().contains("rom-c64-kernal"));
        let mut req = request();
        req.roms[1].bytes.truncate(10);
        assert!(create_machine(req).unwrap_err().contains("kernal image"));
    }

    #[test]
    fn a_disk_without_a_true_drive_says_how_to_get_one() {
        let instance = create();
        let result = call(
            instance,
            MachineCommand::AttachMedia {
                slot: plugin_api::MediaSlot::Disk { drive: 0 },
                kind: MediaKind::Disk,
                name: Some("a.d64".to_string()),
                bytes: Vec::new(),
                options: Vec::new(),
            },
        );
        assert_ne!(result.status.code, StatusCode::Ok as u32);
        // SAFETY: instance handle came from create above.
        assert!(unsafe { plugin_destroy_instance_v1(instance) }.is_ok());
    }

    #[test]
    fn steps_and_a_key_event_round_trip_through_the_abi() {
        let instance = create();
        assert!(plugin_step_v1(instance) > 0);
        assert!(matches!(
            response(call(instance, MachineCommand::GetClockHz)),
            MachineResponse::ClockHz(985_248)
        ));
        let ack = response(call(
            instance,
            MachineCommand::KeyEvent {
                key: "KeyA".to_string(),
                pressed: true,
            },
        ));
        assert_eq!(ack, MachineResponse::Ack);
        let state = response(call(instance, MachineCommand::SaveState));
        let MachineResponse::StateBlob(blob) = state else {
            panic!("expected a state blob");
        };
        let ack = response(call(instance, MachineCommand::LoadState { bytes: blob }));
        assert_eq!(ack, MachineResponse::Ack);
        // SAFETY: instance handle came from create above.
        assert!(unsafe { plugin_destroy_instance_v1(instance) }.is_ok());
    }

    fn attach_program(instance: MachineInstanceHandle, org: u32, bytes: Vec<u8>) {
        let ack = response(call(
            instance,
            MachineCommand::AttachMedia {
                slot: plugin_api::MediaSlot::Other("program".to_string()),
                kind: MediaKind::Program,
                name: None,
                bytes,
                options: vec![NamedValue {
                    key: "org".to_string(),
                    value: DebugValue::U64(u64::from(org)),
                }],
            },
        ));
        assert_eq!(ack, MachineResponse::Ack);
    }

    fn read_byte(instance: MachineInstanceHandle, address: u16) -> u8 {
        match response(call(
            instance,
            MachineCommand::ReadMemory {
                address,
                len: 1,
                space: None,
            },
        )) {
            MachineResponse::Memory(bytes) => bytes[0],
            other => panic!("unexpected {other:?}"),
        }
    }

    /// BASIC's cold start wipes the head of program memory, so a program at
    /// BASIC's start waits for the prompt; anything else is poked at once.
    #[test]
    fn a_basic_program_is_held_back_but_machine_code_is_poked_at_once() {
        let instance = create();
        attach_program(instance, 0x3000, vec![0xAA]);
        assert_eq!(read_byte(instance, 0x3000), 0xAA);
        attach_program(instance, 0x1C01, vec![0xBB]);
        assert_eq!(
            read_byte(instance, 0x1C01),
            0,
            "held until the machine is idle"
        );
        // SAFETY: instance handle came from create above.
        assert!(unsafe { plugin_destroy_instance_v1(instance) }.is_ok());
    }

    #[test]
    fn the_basic_relink_fixes_links_and_reports_the_program_end() {
        let rom = |len| vec![0xEA; len];
        let mut machine = C128::new(c128::Roms {
            basic: rom(c128::BASIC_ROM_SIZE),
            kernal: rom(c128::KERNAL_IMAGE_SIZE),
            chargen: rom(c128::CHARGEN_SIZE),
            c64_basic: rom(c128::C64_ROM_SIZE),
            c64_kernal: rom(c128::C64_ROM_SIZE),
        })
        .unwrap();
        // 10 A / 20 B with wrong links.
        machine.load(
            0x1C01,
            &[0xFF, 0xFF, 10, 0, 0x41, 0, 0xFF, 0xFF, 20, 0, 0x42, 0, 0, 0],
        );
        finish_basic_program_load(&mut machine, C128_BASIC_START).unwrap();
        assert_eq!(machine.read_memory(0x1C01), 0x07);
        assert_eq!(machine.read_memory(0x1C02), 0x1C);
        assert_eq!(machine.read_memory(0x1C07), 0x0D);
        // The end pointer at $1210 is not read back here: before the boot the
        // Z80's map has colour RAM at $1000-$13FF (the real-ROM test in the
        // `c128` crate covers it from the 8502).
    }

    #[test]
    fn key_names_list_everything_the_key_event_path_accepts() {
        let names = key_names();
        let listed: Vec<&str> = names.iter().map(|k| k.name.as_str()).collect();
        for expected in [
            "KeyA",
            "Enter",
            "F2",
            "Tab",
            "Numpad5",
            "Joy2Fire1",
            "Display4080",
        ] {
            assert!(listed.contains(&expected), "{expected} missing");
        }
        let mut seen = std::collections::HashSet::new();
        for key in &names {
            assert!(
                seen.insert(plugin_api::normalize_key_name(&key.name)),
                "{}",
                key.name
            );
            assert!(key.matches(&key.name));
        }
        let mut infos = plugin_api::key_infos(listed.iter().copied());
        assert!(plugin_api::set_key_chars(&mut infos, C128_CHARS).is_empty());
    }
}
