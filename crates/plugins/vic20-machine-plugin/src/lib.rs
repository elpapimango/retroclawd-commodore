//! VIC-20 Plugin. Firmware and Media are never embedded.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

use machine::Machine;
use plugin_api::{
    ABI_MAJOR_V1, ABI_MINOR_V1, AddressSpaceInfo, BitmapExplorerDefaults, BitmapPaletteColor,
    ChipSnapshot, CpuFamily, CpuInfo, DebugCommand, DebugResponse, DebugValue, DisassemblyLine,
    ExecutionMode, ListProfilesRequest, ListProfilesResponse, MACHINE_PROFILE_CONTRACT_VERSION,
    MachineCapability, MachineCommand, MachineCreateRequest, MachineCreateResponse,
    MachineInstanceCallRequest, MachineInstanceCallResponse, MachineKeyInfo, MachineMetadata,
    MachineOptionInfo, MachineOptionKind, MachinePluginVTableV1, MachineProfile, MachineResponse,
    MediaKind, MediaSlot, MediaSnapshot, NamedValue, PluginDescriptorV1, PluginKind,
    REQUIRED_MACHINE_DEBUG_CAPABILITIES, RcBuffer, RcCallResult, RcSlice, RcStatus, RegisterField,
    RegisterSnapshot, StatusCode, StepResult, TapeStatus,
};
use serde::{Deserialize, Serialize};
use vic20::{Expansion, Roms, Standard, Timing, Vic20};

const COMBOS: &[(&str, &str)] = &[
    ("ArrowLeft", "CursorLeftRight"),
    ("ArrowUp", "CursorUpDown"),
    ("F2", "F1"),
    ("F4", "F3"),
    ("F6", "F5"),
    ("F8", "F7"),
    ("Exclamation", "Digit1"),
    ("DoubleQuote", "Digit2"),
    ("Hash", "Digit3"),
    ("Dollar", "Digit4"),
    ("Percent", "Digit5"),
    ("Ampersand", "Digit6"),
    ("Apostrophe", "Digit7"),
    ("LeftParen", "Digit8"),
    ("RightParen", "Digit9"),
    ("LessThan", "Comma"),
    ("GreaterThan", "Period"),
    ("Question", "Slash"),
    ("LeftBracket", "Colon"),
    ("RightBracket", "Semicolon"),
];

fn key_names() -> Vec<MachineKeyInfo> {
    let mut names = plugin_api::key_infos(
        vic20::KEY_MATRIX
            .iter()
            .flatten()
            .copied()
            .chain(vic20::JOYSTICK_KEYS.iter().map(|&(name, _)| name))
            .chain(["Restore"])
            .chain(COMBOS.iter().map(|&(name, _)| name)),
    );
    for key in &mut names {
        key.aliases.extend(match key.name.as_str() {
            "Return" => vec!["Enter".to_string()],
            "Delete" => vec!["Backspace".to_string()],
            "CursorLeftRight" => vec!["ArrowRight".to_string(), "right".to_string()],
            "CursorUpDown" => vec!["ArrowDown".to_string(), "down".to_string()],
            "Control" => vec!["ControlLeft".to_string(), "ControlRight".to_string()],
            _ => Vec::new(),
        });
    }
    plugin_api::set_key_chars(
        &mut names,
        &[
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
        ],
    );
    names
}

fn file_option(key: &str, flag: &str, description: &str) -> MachineOptionInfo {
    MachineOptionInfo {
        key: key.to_string(),
        flag: flag.to_string(),
        short: None,
        kind: MachineOptionKind::File,
        default: None,
        required: true,
        description: description.to_string(),
    }
}

fn profile() -> MachineProfile {
    MachineProfile {
        machine_id: "vic20".to_string(), display_name: "Commodore VIC-20 (experimental)".to_string(),
        cpu_family: CpuFamily::Mos6502, default_region: Some("pal".to_string()),
        contract_version: MACHINE_PROFILE_CONTRACT_VERSION,
        debug_capabilities: REQUIRED_MACHINE_DEBUG_CAPABILITIES.to_vec(),
        capabilities: vec![MachineCapability::Video, MachineCapability::Audio, MachineCapability::Tape,
            MachineCapability::SaveState, MachineCapability::Debug],
        media_kinds: vec![MediaKind::Program, MediaKind::Tape],
        options: vec![
            file_option("basic", "rom-basic", "VIC-20 BASIC ROM (8192 bytes)"),
            file_option("kernal", "rom-kernal", "VIC-20 KERNAL ROM for the selected Region (8192 bytes)"),
            file_option("char", "rom-char", "VIC-20 character ROM (4096 bytes)"),
            ExecutionMode::option("fast (default): instruction-batched Chips; accurate: Chips advance on each CPU bus cycle, including dummy accesses. VIC-I sub-clock effects and interlace remain unimplemented"),
            MachineOptionInfo {
                key: "ram_expansion".to_string(), flag: "ram-expansion".to_string(), short: None,
                kind: MachineOptionKind::Enum { choices: ["none", "3k", "8k", "16k", "24k", "32k", "35k"].map(str::to_string).to_vec() },
                default: Some(DebugValue::Text("none".to_string())), required: false,
                description: "RAM expansion beyond the built-in 5 KiB; 32k maps BLK1/2/3/5, 35k also maps the low 3 KiB".to_string(),
            },
        ],
        key_names: key_names(),
        cpus: vec![CpuInfo { id: "main".to_string(), name: "MOS 6502".to_string(),
            family: CpuFamily::Mos6502, address_space: "cpu".to_string() }],
        axis_names: Vec::new(),
    }
}

struct Instance {
    machine: Vic20,
    paused: bool,
    breakpoints: BTreeSet<u64>,
    held: BTreeSet<String>,
}

fn physical_keys(held: &BTreeSet<String>) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for key in held {
        if let Some(&(_, base)) = COMBOS.iter().find(|&&(name, _)| name == key) {
            keys.insert("ShiftLeft".to_string());
            keys.insert(base.to_string());
        } else {
            keys.insert(key.clone());
        }
    }
    keys
}

impl Instance {
    fn create(request: MachineCreateRequest) -> Result<Self, String> {
        if request.machine_id != "vic20" {
            return Err(format!("unknown Machine {:?}", request.machine_id));
        }
        let standard = match request.region.as_deref() {
            None | Some("pal") => Standard::Pal,
            Some("ntsc") => Standard::Ntsc,
            Some(region) => {
                return Err(format!(
                    "unsupported VIC-20 Region {region:?}; expected pal or ntsc"
                ));
            }
        };
        let execution = ExecutionMode::from_options(&request.options)?.unwrap_or_default();
        let timing = match execution {
            ExecutionMode::Fast => Timing::Fast,
            ExecutionMode::Accurate => Timing::Accurate,
        };
        let mut expansion = Expansion::None;
        let mut seen = BTreeSet::new();
        for option in &request.options {
            if !seen.insert(&option.key) {
                return Err(format!("duplicate Option {:?}", option.key));
            }
            match (option.key.as_str(), &option.value) {
                ("execution_mode", _) => {}
                ("ram_expansion", DebugValue::Text(value)) => {
                    expansion = match value.as_str() {
                        "none" => Expansion::None,
                        "3k" => Expansion::K3,
                        "8k" => Expansion::K8,
                        "16k" => Expansion::K16,
                        "24k" => Expansion::K24,
                        "32k" => Expansion::K32,
                        "35k" => Expansion::K35,
                        _ => return Err(format!("unsupported RAM expansion {value:?}")),
                    }
                }
                _ => {
                    return Err(format!(
                        "unknown or incorrectly typed VIC-20 Option {:?}",
                        option.key
                    ));
                }
            }
        }
        let mut roms = BTreeMap::new();
        for blob in request.roms {
            if !["basic", "kernal", "char"].contains(&blob.key.as_str()) {
                return Err(format!("unknown VIC-20 ROM {:?}", blob.key));
            }
            if roms.insert(blob.key.clone(), blob.bytes).is_some() {
                return Err(format!("duplicate ROM {:?}", blob.key));
            }
        }
        let mut take = |key| {
            roms.remove(key)
                .ok_or_else(|| format!("missing required VIC-20 ROM {key:?}"))
        };
        let machine = Vic20::new(
            Roms {
                basic: take("basic")?,
                kernal: take("kernal")?,
                characters: take("char")?,
            },
            standard,
            expansion,
            timing,
        )?;
        Ok(Self {
            machine,
            paused: false,
            breakpoints: BTreeSet::new(),
            held: BTreeSet::new(),
        })
    }

    fn step(&mut self) -> u32 {
        if self.paused || self.breakpoints.contains(&self.machine.pc()) {
            self.paused = true;
            return 4;
        }
        self.machine.step()
    }

    fn key(&mut self, name: &str, pressed: bool) -> Result<(), String> {
        let key = key_names()
            .into_iter()
            .find(|key| key.matches(name))
            .ok_or_else(|| format!("unknown VIC-20 Machine key {name:?}"))?
            .name;
        let before = physical_keys(&self.held);
        if pressed {
            self.held.insert(key);
        } else {
            self.held.remove(&key);
        }
        let after = physical_keys(&self.held);
        for key in before.difference(&after) {
            self.machine.key(key, false)?;
        }
        for key in after.difference(&before) {
            self.machine.key(key, true)?;
        }
        Ok(())
    }

    fn metadata(&self) -> MachineMetadata {
        let rate = self.machine.frame_rate();
        MachineMetadata {
            machine_id: "vic20".to_string(), model: Some("vic20".to_string()),
            region: Some(match self.machine.vic().standard() { Standard::Pal => "pal", Standard::Ntsc => "ntsc" }.to_string()),
            notes: vec![
                "Experimental: no firmware boot or reference comparison has been verified".to_string(),
                "Sub-clock VIC effects, interlace, paddles, light pen, cassette, cartridge and disk Media are not implemented".to_string(),
                format!("execution_mode={:?}; ram_expansion={:?}", self.machine.timing(), self.machine.expansion()),
            ],
            bitmap_palette: vic_i::PALETTE.into_iter().enumerate().map(|(index, rgb)| BitmapPaletteColor { name: format!("VIC-I {index}"), rgb }).collect(),
            bitmap_defaults: Some(BitmapExplorerDefaults { address: 0x8000, bytes_width: 8, bitplanes: 1, zoom: 2 }),
            frame_rate: (rate.num, rate.den), audio_channels: 1, audio_sample_rate: vic_i::SAMPLE_RATE,
            ..MachineMetadata::default()
        }
    }

    fn read_memory(&self, address: u64, len: u32, space: Option<&str>) -> Result<Vec<u8>, String> {
        check_memory(address, len as usize, space)?;
        Ok((0..len)
            .map(|offset| {
                self.machine
                    .read_memory((address + u64::from(offset)) & 0xffff)
            })
            .collect())
    }

    fn write_memory(
        &mut self,
        address: u64,
        bytes: &[u8],
        space: Option<&str>,
    ) -> Result<u32, String> {
        check_memory(address, bytes.len(), space)?;
        Ok(self.machine.load(address, bytes) as u32)
    }

    fn chips(&self) -> Vec<ChipSnapshot> {
        let mut chips = vec![ChipSnapshot {
            chip_id: "vic-i".to_string(),
            fields: (0..16)
                .map(|reg| NamedValue {
                    key: format!("r{reg:x}"),
                    value: DebugValue::U64(u64::from(self.machine.vic().read(reg))),
                })
                .collect(),
            blobs: Vec::new(),
        }];
        for index in 0..2 {
            let via = self.machine.via(index).expect("two VIAs");
            chips.push(ChipSnapshot {
                chip_id: format!("via{}", index + 1),
                fields: (0..16)
                    .map(|reg| NamedValue {
                        key: format!("r{reg:x}"),
                        value: DebugValue::U64(u64::from(via.peek(reg))),
                    })
                    .collect(),
                blobs: Vec::new(),
            });
        }
        chips.push(ChipSnapshot {
            chip_id: "overlay".to_string(),
            fields: vec![
                NamedValue {
                    key: "memory_summary".to_string(),
                    value: DebugValue::Text(format!(
                        "5 KiB + {:?}; {:?}; raster {}:{}",
                        self.machine.expansion(),
                        self.machine.timing(),
                        self.machine.vic().raster(),
                        self.machine.vic().cycle()
                    )),
                },
                NamedValue {
                    key: "cycles".to_string(),
                    value: DebugValue::U64(self.machine.cycles()),
                },
                NamedValue {
                    key: "audio_dropped".to_string(),
                    value: DebugValue::U64(self.machine.audio_dropped()),
                },
                NamedValue {
                    key: "unsupported_interlace".to_string(),
                    value: DebugValue::Bool(self.machine.vic().interlace_requested()),
                },
            ],
            blobs: Vec::new(),
        });
        if let Some((tape, motor)) = self.machine.tape_status() {
            let overlay = chips.last_mut().expect("overlay added above");
            overlay.fields.extend([
                NamedValue {
                    key: "tape_motor".to_string(),
                    value: DebugValue::Bool(motor),
                },
                NamedValue {
                    key: "tape_position".to_string(),
                    value: DebugValue::U64(tape.position as u64),
                },
                NamedValue {
                    key: "tape_total".to_string(),
                    value: DebugValue::U64(tape.pulses as u64),
                },
            ]);
        }
        chips
    }

    fn media(&self) -> MediaSnapshot {
        MediaSnapshot {
            tape: self.machine.tape_status().map(|(tape, motor)| TapeStatus {
                attached: true,
                playing: tape.playing,
                finished: tape.finished,
                speed_multiplier: tape.speed_multiplier,
                position: Some(tape.position as u64),
                total: Some(tape.pulses as u64),
                motor_on: Some(motor),
                ear_bit: Some(true),
                cycles_until_next_edge: None,
                spin_down_cycles: None,
            }),
            disks: Vec::new(),
            cartridge: None,
        }
    }

    fn debug(&mut self, mut command: DebugCommand) -> Result<DebugResponse, String> {
        while let DebugCommand::OnCpu {
            cpu,
            command: nested,
        } = command
        {
            if cpu != "main" {
                return Err(format!("unknown VIC-20 CPU {cpu:?}; expected main"));
            }
            command = *nested;
        }
        match command {
            DebugCommand::Pause => {
                self.paused = true;
                Ok(DebugResponse::Ack)
            }
            DebugCommand::Resume => {
                self.paused = false;
                Ok(DebugResponse::Ack)
            }
            DebugCommand::StepInstruction => {
                self.machine.step();
                Ok(DebugResponse::Ack)
            }
            DebugCommand::StepOver => {
                let pc = self.machine.pc();
                let call = self.machine.read_memory(pc) == 0x20;
                let stack = self.machine.cpu.sp;
                self.machine.step();
                if call {
                    let next = (pc + 3) & 0xffff;
                    for _ in 0..100_000 {
                        if self.machine.pc() == next && self.machine.cpu.sp == stack {
                            return Ok(DebugResponse::Ack);
                        }
                        if self.machine.halted() {
                            return Err("VIC-20 CPU jammed during step-over".to_string());
                        }
                        if self.breakpoints.contains(&self.machine.pc()) {
                            self.paused = true;
                            return Ok(DebugResponse::Ack);
                        }
                        self.machine.step();
                    }
                    return Err("VIC-20 step-over exceeded 100000 instructions".to_string());
                }
                Ok(DebugResponse::Ack)
            }
            DebugCommand::SetBreakpoint { address } => {
                check_address(address)?;
                self.breakpoints.insert(address);
                Ok(DebugResponse::Ack)
            }
            DebugCommand::ClearBreakpoint { address } => {
                check_address(address)?;
                self.breakpoints.remove(&address);
                Ok(DebugResponse::Ack)
            }
            DebugCommand::ClearAllBreakpoints => {
                self.breakpoints.clear();
                Ok(DebugResponse::Ack)
            }
            DebugCommand::ListBreakpoints => Ok(DebugResponse::Breakpoints(
                self.breakpoints.iter().copied().collect(),
            )),
            DebugCommand::GetRegisters => Ok(DebugResponse::Registers(RegisterSnapshot {
                cpu_family: CpuFamily::Mos6502,
                registers: self
                    .machine
                    .registers()
                    .into_iter()
                    .map(|(name, value)| RegisterField {
                        bank: "main".to_string(),
                        name: name.to_string(),
                        value,
                        width_bits: if name == "PC" { 16 } else { 8 },
                    })
                    .collect(),
                flags: [
                    ("N", 7),
                    ("V", 6),
                    ("B", 4),
                    ("D", 3),
                    ("I", 2),
                    ("Z", 1),
                    ("C", 0),
                ]
                .into_iter()
                .map(|(name, bit)| NamedValue {
                    key: name.to_string(),
                    value: DebugValue::Bool(self.machine.cpu.p & (1 << bit) != 0),
                })
                .collect(),
            })),
            DebugCommand::SetRegister { name, value } => {
                self.machine.cpu.set_register(&name, value)?;
                Ok(DebugResponse::Ack)
            }
            DebugCommand::GetAddressSpaces => {
                Ok(DebugResponse::AddressSpaces(vec![AddressSpaceInfo {
                    name: "cpu".to_string(),
                    addr_width_bits: 16,
                    bank_context: Vec::new(),
                }]))
            }
            DebugCommand::ReadMemory {
                address,
                len,
                space,
            } => Ok(DebugResponse::Memory(self.read_memory(
                address,
                len,
                space.as_deref(),
            )?)),
            DebugCommand::WriteMemory {
                address,
                bytes,
                space,
            } => {
                check_memory(address, bytes.len(), space.as_deref())?;
                if let Some(offset) = (0..bytes.len()).find(|&offset| {
                    !self
                        .machine
                        .can_write((address as u16).wrapping_add(offset as u16))
                }) {
                    return Err(format!(
                        "VIC-20 address {:04x} is not writable",
                        (address + offset as u64) & 0xffff
                    ));
                }
                self.write_memory(address, &bytes, space.as_deref())?;
                Ok(DebugResponse::Ack)
            }
            DebugCommand::GetDisassembly { start, lines } => {
                let mut address = start.unwrap_or(self.machine.pc());
                check_address(address)?;
                if lines > 65_536 {
                    return Err("VIC-20 disassembly exceeds 65536 instructions".to_string());
                }
                let mut out = Vec::with_capacity(lines as usize);
                for _ in 0..lines {
                    let raw = self.read_memory(address, 3, None)?;
                    let (text, len) = mos6502::disassemble::disassemble(&raw, address as u16);
                    out.push(DisassemblyLine {
                        address,
                        bytes: raw[..len].to_vec(),
                        text,
                    });
                    address = (address + len as u64) & 0xffff;
                }
                Ok(DebugResponse::Disassembly(out))
            }
            DebugCommand::GetMediaSnapshot => Ok(DebugResponse::MediaSnapshot(self.media())),
            DebugCommand::GetChipSnapshots => Ok(DebugResponse::ChipSnapshots(self.chips())),
            DebugCommand::GetMachineMetadata => Ok(DebugResponse::MachineMetadata(self.metadata())),
            DebugCommand::OnCpu { .. } => unreachable!("CPU selector removed above"),
        }
    }

    fn command(&mut self, command: MachineCommand) -> Result<MachineResponse, String> {
        match command {
            MachineCommand::GetClockHz => Ok(MachineResponse::ClockHz(self.machine.clock_hz())),
            MachineCommand::GetFrameHz => Ok(MachineResponse::FrameHz(self.machine.frame_hz())),
            MachineCommand::RequestInterrupt => Ok(MachineResponse::Ack),
            MachineCommand::Step => {
                let cycles = self.step();
                Ok(MachineResponse::Step(StepResult {
                    cycles,
                    pc: self.machine.pc(),
                    halted: self.machine.halted(),
                }))
            }
            MachineCommand::IsHalted => Ok(MachineResponse::Halted(self.machine.halted())),
            MachineCommand::GetPc => Ok(MachineResponse::Pc(self.machine.pc())),
            MachineCommand::Reset => {
                self.machine.reset();
                Ok(MachineResponse::Ack)
            }
            MachineCommand::ReadMemory {
                address,
                len,
                space,
            } => Ok(MachineResponse::Memory(self.read_memory(
                address,
                len,
                space.as_deref(),
            )?)),
            MachineCommand::WriteMemory {
                address,
                bytes,
                space,
            } => Ok(MachineResponse::BytesWritten(self.write_memory(
                address,
                &bytes,
                space.as_deref(),
            )?)),
            MachineCommand::LoadBytes { address, bytes } => Ok(MachineResponse::BytesWritten(
                self.write_memory(address, &bytes, None)?,
            )),
            MachineCommand::AttachMedia {
                slot: MediaSlot::Other(slot),
                kind: MediaKind::Program,
                bytes,
                options,
                ..
            } if slot == "program" => {
                let address = match options.as_slice() {
                    [NamedValue { key, value: DebugValue::U64(address) }] if key == "org" => *address,
                    _ => return Err("VIC-20 program requires exactly one unsigned org Option".to_string()),
                };
                check_address(address)?;
                self.machine.load_program(address as u16, &bytes)?;
                Ok(MachineResponse::Ack)
            }
            MachineCommand::AttachMedia {
                slot: MediaSlot::Tape,
                kind: MediaKind::Tape,
                name,
                bytes,
                options,
            } => {
                if !name.as_deref().is_some_and(|name| name.rsplit_once('.').is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("tap"))) {
                    return Err("VIC-20 tape requires a .tap filename".to_string());
                }
                let speed = match options.as_slice() {
                    [] => 1,
                    [NamedValue { key, value: DebugValue::U64(speed) }] if key == "tape_speed" => {
                        u32::try_from(*speed).ok().filter(|&n| n > 0)
                            .ok_or("VIC-20 tape_speed must be in 1..=4294967295")?
                    }
                    _ => return Err("VIC-20 tape accepts only one unsigned tape_speed Option".to_string()),
                };
                self.machine.load_tape(&bytes, speed)?;
                Ok(MachineResponse::Ack)
            }
            MachineCommand::DetachMedia { slot: MediaSlot::Tape } => {
                self.machine.eject_tape();
                Ok(MachineResponse::Ack)
            }
            MachineCommand::SetPc { address } => {
                check_address(address)?;
                self.machine.set_pc(address);
                Ok(MachineResponse::Ack)
            }
            MachineCommand::SaveState => {
                let bytes = bincode::serde::encode_to_vec(
                    InputState {
                        magic: *b"VI20",
                        machine: self.machine.save_state(),
                        held: self.held.clone(),
                    },
                    bincode::config::standard(),
                )
                .map_err(|error| format!("cannot encode VIC-20 state: {error}"))?;
                Ok(MachineResponse::StateBlob(bytes))
            }
            MachineCommand::LoadState { bytes } => {
                if bytes.len() > 1_048_576 {
                    return Err("VIC-20 state exceeds 1 MiB".to_string());
                }
                let (state, used): (InputState, usize) = bincode::serde::decode_from_slice(
                    &bytes,
                    bincode::config::standard().with_limit::<1_048_576>(),
                )
                .map_err(|error| format!("invalid VIC-20 state: {error}"))?;
                let names = key_names();
                if used != bytes.len()
                    || state.magic != *b"VI20"
                    || state
                        .held
                        .iter()
                        .any(|name| !names.iter().any(|key| key.name == *name))
                {
                    return Err(
                        "invalid VIC-20 state header, held keys, or trailing bytes".to_string()
                    );
                }
                self.machine.load_state(&state.machine)?;
                self.held = state.held;
                Ok(MachineResponse::Ack)
            }
            MachineCommand::GetMediaSnapshot => Ok(MachineResponse::MediaSnapshot(self.media())),
            MachineCommand::SetBorder { enabled } => {
                self.machine.set_border(enabled);
                Ok(MachineResponse::Ack)
            }
            MachineCommand::KeyEvent { key, pressed } => {
                self.key(&key, pressed)?;
                Ok(MachineResponse::Ack)
            }
            MachineCommand::RenderFrame => {
                let (width, height, rgb) =
                    self.machine.render_frame().expect("VIC-I produces video");
                Ok(MachineResponse::Frame { width, height, rgb })
            }
            MachineCommand::DrainAudio => Ok(MachineResponse::AudioPcm {
                channels: 1,
                sample_rate: vic_i::SAMPLE_RATE,
                samples: self.machine.drain_audio(),
            }),
            MachineCommand::Debug(command) => Ok(MachineResponse::Debug(self.debug(command)?)),
            MachineCommand::TakePortHit => Ok(MachineResponse::PortHit(None)),
            MachineCommand::WriteIo { .. } | MachineCommand::SetPortWatches { .. } => Err(
                "unsupported: VIC-20 has memory-mapped I/O, not a separate port space".to_string(),
            ),
            _ => Err("unsupported VIC-20 operation: recording, disk, cartridge, Snapshot, printer, mouse, and axis support is not implemented".to_string()),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct InputState {
    magic: [u8; 4],
    machine: Vec<u8>,
    held: BTreeSet<String>,
}

fn check_address(address: u64) -> Result<(), String> {
    if address > 0xffff {
        Err(format!("VIC-20 address {address:#x} exceeds 16 bits"))
    } else {
        Ok(())
    }
}
fn check_memory(address: u64, len: usize, space: Option<&str>) -> Result<(), String> {
    check_address(address)?;
    if len > 65_536 {
        return Err("VIC-20 memory request exceeds 65536 bytes".to_string());
    }
    if space.is_some_and(|space| space != "cpu") {
        return Err(format!("unknown VIC-20 address space {space:?}"));
    }
    Ok(())
}

struct Registry {
    next: u64,
    instances: BTreeMap<u64, Instance>,
}
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| {
        Mutex::new(Registry {
            next: 1,
            instances: BTreeMap::new(),
        })
    })
}
fn with_instance<T>(
    id: u64,
    action: impl FnOnce(&mut Instance) -> Result<T, String>,
) -> Result<T, String> {
    let mut registry = registry()
        .lock()
        .map_err(|_| "VIC-20 Plugin registry poisoned".to_string())?;
    let instance = registry
        .instances
        .get_mut(&id)
        .ok_or_else(|| format!("unknown Machine instance {id}"))?;
    action(instance)
}
fn encode<T: Serialize>(result: Result<T, String>) -> RcCallResult {
    match result {
        Ok(value) => match plugin_api::codec::encode_message(&value) {
            Ok(bytes) => RcCallResult::ok_bytes(bytes),
            Err(error) => error.into_call_result(),
        },
        Err(error) => RcCallResult::error_with_message(
            if error.starts_with("unsupported") {
                StatusCode::Unsupported
            } else if error.starts_with("unknown Machine instance") {
                StatusCode::NotFound
            } else {
                StatusCode::InvalidRequest
            },
            &error,
        ),
    }
}

unsafe extern "C" fn list_profiles(request: RcSlice) -> RcCallResult {
    plugin_api::guard::guarded_call("list_profiles", || {
        // SAFETY: the caller keeps the request buffer alive for this call.
        match plugin_api::codec::decode_message::<ListProfilesRequest>(unsafe {
            request.as_bytes()
        }) {
            Ok(_) => encode(Ok(ListProfilesResponse {
                profiles: vec![profile()],
            })),
            Err(error) => error.into_call_result(),
        }
    })
}
unsafe extern "C" fn create_instance(request: RcSlice) -> RcCallResult {
    plugin_api::guard::guarded_call("create_instance", || {
        // SAFETY: the caller keeps the request buffer alive for this call.
        let request = match plugin_api::codec::decode_message::<MachineCreateRequest>(unsafe {
            request.as_bytes()
        }) {
            Ok(request) => request,
            Err(error) => return error.into_call_result(),
        };
        encode((|| {
            let instance = Instance::create(request)?;
            let metadata = instance.metadata();
            let mut registry = registry()
                .lock()
                .map_err(|_| "VIC-20 Plugin registry poisoned".to_string())?;
            let id = registry.next;
            registry.next = id
                .checked_add(1)
                .ok_or("VIC-20 instance handles exhausted")?;
            registry.instances.insert(id, instance);
            Ok(MachineCreateResponse {
                instance: id,
                metadata,
            })
        })())
    })
}
unsafe extern "C" fn destroy_instance(id: u64) -> RcStatus {
    plugin_api::guard::guarded(
        "destroy_instance",
        RcStatus::from_code(StatusCode::InternalError),
        || {
            let mut registry = registry().lock().expect("VIC-20 Plugin registry poisoned");
            if registry.instances.remove(&id).is_some() {
                RcStatus::ok()
            } else {
                RcStatus::from_code(StatusCode::NotFound)
            }
        },
    )
}
unsafe extern "C" fn instance_call(id: u64, request: RcSlice) -> RcCallResult {
    plugin_api::guard::guarded_call("instance_call", || {
        // SAFETY: the caller keeps the request buffer alive for this call.
        let request =
            match plugin_api::codec::decode_message::<MachineInstanceCallRequest>(unsafe {
                request.as_bytes()
            }) {
                Ok(request) => request,
                Err(error) => return error.into_call_result(),
            };
        encode(with_instance(id, |instance| {
            Ok(MachineInstanceCallResponse {
                response: instance.command(request.command)?,
            })
        }))
    })
}
unsafe extern "C" fn free_buffer(buffer: RcBuffer) {
    plugin_api::guard::guarded("free_buffer", (), || {
        // SAFETY: this allocation originated from this Plugin's RcBuffer.
        drop(unsafe { buffer.into_vec() });
    });
}
extern "C" fn step(id: u64) -> u32 {
    if plugin_api::guard::has_crashed() {
        return 4;
    }
    plugin_api::guard::guarded("step", 4, || {
        with_instance(id, |instance| Ok(instance.step())).expect("VIC-20 fast step failed")
    })
}
extern "C" fn halted(id: u64) -> u8 {
    if plugin_api::guard::has_crashed() {
        return 1;
    }
    plugin_api::guard::guarded("halted", 1, || {
        with_instance(id, |instance| Ok(u8::from(instance.machine.halted())))
            .expect("VIC-20 fast halted query failed")
    })
}
extern "C" fn pc_wide(id: u64) -> u64 {
    plugin_api::guard::guarded("pc_wide", 0, || {
        with_instance(id, |instance| Ok(instance.machine.pc()))
            .expect("VIC-20 fast PC query failed")
    })
}
extern "C" fn pc(id: u64) -> u16 {
    pc_wide(id) as u16
}

const fn static_slice(bytes: &'static [u8]) -> RcSlice {
    RcSlice {
        ptr: bytes.as_ptr(),
        len: bytes.len(),
    }
}
struct StaticDescriptor(PluginDescriptorV1);
// SAFETY: the descriptor references immutable static data and function pointers.
unsafe impl Sync for StaticDescriptor {}
static VTABLE: MachinePluginVTableV1 = MachinePluginVTableV1 {
    size: MachinePluginVTableV1::expected_size(),
    list_profiles,
    create_instance,
    destroy_instance,
    instance_call,
    free_buffer,
    step,
    halted,
    pc,
    pc_wide,
};
static DESCRIPTOR: StaticDescriptor = StaticDescriptor(PluginDescriptorV1 {
    abi_major: ABI_MAJOR_V1,
    abi_minor: ABI_MINOR_V1,
    kind: PluginKind::Machine,
    id: static_slice(b"retroclawd.vic20.machine"),
    version: static_slice(build_info::version_tag!().as_bytes()),
    vtable: (&VTABLE as *const MachinePluginVTableV1).cast(),
});

/// # Safety
///
/// The returned descriptor is immutable static storage owned by this Plugin.
pub unsafe extern "C" fn entry_v1() -> *const PluginDescriptorV1 {
    &DESCRIPTOR.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_api::BinaryBlob;

    fn request() -> MachineCreateRequest {
        let mut kernal = vec![0xea; 8192];
        kernal[..3].copy_from_slice(&[0x4c, 0, 0xe0]);
        kernal[8190..].copy_from_slice(&[0, 0xe0]);
        kernal[8188..8190].copy_from_slice(&[0, 0xe0]);
        MachineCreateRequest {
            machine_id: "vic20".to_string(),
            region: None,
            options: Vec::new(),
            roms: vec![
                BinaryBlob {
                    key: "basic".to_string(),
                    bytes: vec![0xea; 8192],
                },
                BinaryBlob {
                    key: "kernal".to_string(),
                    bytes: kernal,
                },
                BinaryBlob {
                    key: "char".to_string(),
                    bytes: vec![0; 4096],
                },
            ],
        }
    }

    #[test]
    fn creation_rejects_bad_firmware_regions_and_options() {
        let mut req = request();
        req.roms[0].bytes.pop();
        assert!(Instance::create(req).is_err());
        let mut req = request();
        req.region = Some("japan".to_string());
        assert!(Instance::create(req).is_err());
        let mut req = request();
        req.options.push(NamedValue {
            key: "ram_expansion".to_string(),
            value: DebugValue::U64(8),
        });
        assert!(Instance::create(req).is_err());
        let mut req = request();
        req.options.push(NamedValue {
            key: "execution_mode".to_string(),
            value: DebugValue::Text("accurate".to_string()),
        });
        let instance = Instance::create(req).unwrap();
        assert_eq!(instance.machine.timing(), Timing::Accurate);
        assert_eq!(instance.metadata().frame_rate, (1_108_405, 22_152));
        assert_eq!(
            Instance::create(request()).unwrap().machine.timing(),
            Timing::Fast
        );
    }

    #[test]
    fn declared_keys_share_the_event_path_and_combo_sources_survive_state() {
        let mut instance = Instance::create(request()).unwrap();
        let names = key_names();
        let mut chars = BTreeSet::new();
        for key in names {
            for ch in key.chars.chars() {
                assert!(chars.insert(ch), "duplicate character {ch:?}");
            }
            instance.key(&key.name, true).unwrap();
            instance.key(&key.name, false).unwrap();
        }
        instance.key("ShiftLeft", true).unwrap();
        instance.key("F2", true).unwrap();
        instance.key("ArrowUp", true).unwrap();
        instance.key("ShiftLeft", false).unwrap();
        assert!(physical_keys(&instance.held).contains("ShiftLeft"));
        let MachineResponse::StateBlob(bytes) =
            instance.command(MachineCommand::SaveState).unwrap()
        else {
            panic!("expected state");
        };
        instance.key("F2", false).unwrap();
        instance.key("ArrowUp", false).unwrap();
        assert!(!physical_keys(&instance.held).contains("ShiftLeft"));
        instance
            .command(MachineCommand::LoadState { bytes })
            .unwrap();
        assert!(physical_keys(&instance.held).contains("ShiftLeft"));
        instance.key("F2", false).unwrap();
        assert!(physical_keys(&instance.held).contains("ShiftLeft"));
        instance.key("ArrowUp", false).unwrap();
        assert!(physical_keys(&instance.held).is_empty());
    }

    #[test]
    fn debugger_executes_real_code_and_checks_spaces_bounds_and_writability() {
        let mut instance = Instance::create(request()).unwrap();
        instance
            .command(MachineCommand::LoadBytes {
                address: 0x1000,
                bytes: vec![0x20, 0x08, 0x10, 0xa9, 0x55, 0xea, 0xea, 0xea, 0xe8, 0x60],
            })
            .unwrap();
        instance
            .command(MachineCommand::SetPc { address: 0x1000 })
            .unwrap();
        instance.debug(DebugCommand::Pause).unwrap();
        let cycles = instance.machine.cycles();
        assert_eq!(instance.step(), 4);
        assert_eq!(instance.machine.cycles(), cycles);
        instance.debug(DebugCommand::StepOver).unwrap();
        assert_eq!(instance.machine.pc(), 0x1003);
        assert_eq!(instance.machine.cpu.x, 1);
        instance.debug(DebugCommand::StepInstruction).unwrap();
        assert_eq!(instance.machine.cpu.a, 0x55);
        let DebugResponse::Disassembly(lines) = instance
            .debug(DebugCommand::GetDisassembly {
                start: Some(0x1000),
                lines: 2,
            })
            .unwrap()
        else {
            panic!("expected disassembly");
        };
        assert!(lines[0].text.starts_with("JSR"));
        assert_eq!(lines[1].address, 0x1003);
        let DebugResponse::Registers(registers) =
            instance.debug(DebugCommand::GetRegisters).unwrap()
        else {
            panic!("expected registers");
        };
        assert_eq!(
            registers
                .registers
                .iter()
                .find(|r| r.name == "PC")
                .unwrap()
                .width_bits,
            16
        );
        assert_eq!(
            registers
                .flags
                .iter()
                .map(|flag| (flag.key.as_str(), &flag.value))
                .collect::<Vec<_>>(),
            [
                ("N", &DebugValue::Bool(false)),
                ("V", &DebugValue::Bool(false)),
                ("B", &DebugValue::Bool(false)),
                ("D", &DebugValue::Bool(false)),
                ("I", &DebugValue::Bool(true)),
                ("Z", &DebugValue::Bool(false)),
                ("C", &DebugValue::Bool(false)),
            ]
        );
        assert!(instance.read_memory(0, 65_537, None).is_err());
        assert!(instance.read_memory(0x1_0000, 1, None).is_err());
        assert!(instance.read_memory(0, 1, Some("ram")).is_err());
        assert!(
            instance
                .debug(DebugCommand::OnCpu {
                    cpu: "z80".to_string(),
                    command: Box::new(DebugCommand::GetRegisters)
                })
                .is_err()
        );
        assert!(
            instance
                .debug(DebugCommand::WriteMemory {
                    address: 0x3ff,
                    bytes: vec![0x71, 0x72],
                    space: None
                })
                .is_err()
        );
        assert_eq!(instance.machine.read_memory(0x3ff), 0);
        for command in [
            DebugCommand::GetAddressSpaces,
            DebugCommand::GetMachineMetadata,
            DebugCommand::GetChipSnapshots,
            DebugCommand::GetMediaSnapshot,
        ] {
            instance.debug(command).unwrap();
        }
        instance
            .debug(DebugCommand::SetBreakpoint { address: 0x1005 })
            .unwrap();
        instance.debug(DebugCommand::Resume).unwrap();
        instance.step();
        assert!(instance.paused);
        assert_eq!(instance.machine.pc(), 0x1005);
        instance.debug(DebugCommand::ClearAllBreakpoints).unwrap();
        instance.debug(DebugCommand::Resume).unwrap();
        instance.step();
        assert_eq!(instance.machine.pc(), 0x1006);
    }

    #[test]
    fn tape_attachment_status_ejection_and_validation_use_the_same_media_path() {
        let mut instance = Instance::create(request()).unwrap();
        let bytes = [b"C64-TAPE-RAW".as_slice(), &[1, 1, 0, 0, 2, 0, 0, 0, 1, 2]].concat();
        let attach = |name: &str, bytes: Vec<u8>, options| MachineCommand::AttachMedia {
            slot: MediaSlot::Tape,
            kind: MediaKind::Tape,
            name: Some(name.to_string()),
            bytes,
            options,
        };
        instance
            .command(attach(
                "test.TAP",
                bytes.clone(),
                vec![NamedValue {
                    key: "tape_speed".to_string(),
                    value: DebugValue::U64(2),
                }],
            ))
            .unwrap();
        let media = instance.media();
        let tape = media.tape.as_ref().unwrap();
        assert!(tape.attached && !tape.finished);
        assert_eq!(
            (tape.total, tape.position, tape.speed_multiplier),
            (Some(2), Some(0), 2)
        );
        assert_eq!(
            instance.debug(DebugCommand::GetMediaSnapshot).unwrap(),
            DebugResponse::MediaSnapshot(media.clone()),
        );
        assert_eq!(
            instance.command(MachineCommand::GetMediaSnapshot).unwrap(),
            MachineResponse::MediaSnapshot(media),
        );
        let before = instance.machine.save_state();
        for command in [
            attach("bad.t64", bytes.clone(), Vec::new()),
            attach("bad.tap", vec![0; 20], Vec::new()),
            attach(
                "bad.tap",
                bytes.clone(),
                vec![NamedValue {
                    key: "tape_speed".to_string(),
                    value: DebugValue::U64(0),
                }],
            ),
            attach(
                "bad.tap",
                bytes,
                vec![NamedValue {
                    key: "unknown".to_string(),
                    value: DebugValue::Bool(true),
                }],
            ),
        ] {
            assert!(instance.command(command).is_err());
            assert_eq!(instance.machine.save_state(), before);
        }
        instance
            .command(MachineCommand::DetachMedia {
                slot: MediaSlot::Tape,
            })
            .unwrap();
        assert!(instance.media().tape.is_none());
    }

    #[test]
    fn unsupported_media_fails_without_echoing_payload_or_mutating_state() {
        let mut instance = Instance::create(request()).unwrap();
        let before = instance.machine.save_state();
        let error = instance
            .command(MachineCommand::AttachMedia {
                slot: MediaSlot::Disk { drive: 0 },
                kind: MediaKind::Disk,
                name: None,
                bytes: vec![137; 100_000],
                options: Vec::new(),
            })
            .unwrap_err();
        assert!(error.starts_with("unsupported"));
        assert!(error.len() < 200);
        assert_eq!(instance.machine.save_state(), before);
    }

    #[test]
    fn program_attachment_checks_the_entire_ram_range_before_loading() {
        let mut instance = Instance::create(request()).unwrap();
        let attach = |org, bytes| MachineCommand::AttachMedia {
            slot: MediaSlot::Other("program".to_string()),
            kind: MediaKind::Program,
            name: None,
            bytes,
            options: vec![NamedValue {
                key: "org".to_string(),
                value: DebugValue::U64(org),
            }],
        };
        instance
            .command(attach(0x1000, vec![0xa9, 0x55, 0x02]))
            .unwrap();
        assert_eq!(
            instance.read_memory(0x1000, 3, None).unwrap(),
            [0xa9, 0x55, 0x02]
        );
        assert_eq!(instance.machine.pc(), 0xe000, "attachment does not jump");
        let before = instance.machine.save_state();
        for (org, bytes) in [
            (0x3ff, vec![1, 2]),
            (0x1fff, vec![1, 2]),
            (0x900f, vec![1]),
            (0xffff, vec![1, 2]),
            (0x1_0000, vec![1]),
            (0x1000, Vec::new()),
            (0x1000, vec![1; 65_537]),
        ] {
            assert!(instance.command(attach(org, bytes)).is_err());
            assert_eq!(instance.machine.save_state(), before);
        }
        let mut command = attach(0x1000, vec![1]);
        if let MachineCommand::AttachMedia { options, .. } = &mut command {
            options.push(options[0].clone());
        }
        assert!(instance.command(command).is_err());
        assert_eq!(instance.machine.save_state(), before);
    }

    #[test]
    fn c_abi_owns_buffers_and_fast_calls_advance_the_same_machine() {
        fn payload(result: RcCallResult) -> Vec<u8> {
            assert_eq!(result.status.code, StatusCode::Ok as u32);
            // SAFETY: this test consumes each Plugin-owned buffer once.
            unsafe { result.payload.into_vec() }
        }
        let bytes = plugin_api::encode_message(&request()).unwrap();
        // SAFETY: the request bytes outlive the synchronous ABI call.
        let created: MachineCreateResponse = plugin_api::decode_message(&payload(unsafe {
            create_instance(RcSlice::from_bytes(&bytes))
        }))
        .unwrap();
        let id = created.instance;
        assert_eq!(pc_wide(id), 0xe000);
        assert_eq!(step(id), 3);
        let bytes = plugin_api::encode_message(&MachineInstanceCallRequest {
            command: MachineCommand::SaveState,
        })
        .unwrap();
        // SAFETY: the request bytes outlive the synchronous ABI call.
        let result: MachineInstanceCallResponse = plugin_api::decode_message(&payload(unsafe {
            instance_call(id, RcSlice::from_bytes(&bytes))
        }))
        .unwrap();
        assert!(matches!(result.response, MachineResponse::StateBlob(_)));
        // SAFETY: this handle came from create_instance and is destroyed once.
        assert!(unsafe { destroy_instance(id) }.is_ok());
        // SAFETY: the encoded request remains valid, but the handle is gone.
        let result = unsafe { instance_call(id, RcSlice::from_bytes(&bytes)) };
        assert_eq!(result.status.code, StatusCode::NotFound as u32);
        // SAFETY: the error payload belongs to this Plugin.
        unsafe {
            free_buffer(result.payload);
        }
    }
}
