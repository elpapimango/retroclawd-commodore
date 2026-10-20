//! VIC-20 NMOS 6502, VIC-I and two VIAs. See `vic20.intake.toml` for sources.
//!
//! Fast mode batches peripheral clocks at instruction boundaries. Accurate
//! mode advances them at each real CPU bus cycle, including dummy accesses.
//! VIC-I shares the bus by phase and does not impose VIC-II-style BA stalls.

use std::collections::VecDeque;

use drive_link::{Disk, DriveKind, DriveLink, IecHost};
use machine::{Addr, Bus, Clock, FrameRate, Machine, RunStats};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use via6522::Via;
pub use vic_i::Standard;

pub const JOYSTICK_KEYS: [(&str, u8); 5] = [
    ("Joy1Up", 2),
    ("Joy1Down", 3),
    ("Joy1Left", 4),
    ("Joy1Fire1", 5),
    ("Joy1Right", 7),
];

pub const KEY_MATRIX: [[&str; 8]; 8] = [
    [
        "Digit1", "Digit3", "Digit5", "Digit7", "Digit9", "Plus", "Pound", "Delete",
    ],
    [
        "LeftArrow",
        "KeyW",
        "KeyR",
        "KeyY",
        "KeyI",
        "KeyP",
        "Asterisk",
        "Return",
    ],
    [
        "Control",
        "KeyA",
        "KeyD",
        "KeyG",
        "KeyJ",
        "KeyL",
        "Semicolon",
        "CursorLeftRight",
    ],
    [
        "RunStop",
        "ShiftLeft",
        "KeyX",
        "KeyV",
        "KeyN",
        "Comma",
        "Slash",
        "CursorUpDown",
    ],
    [
        "Space",
        "KeyZ",
        "KeyC",
        "KeyB",
        "KeyM",
        "Period",
        "ShiftRight",
        "F1",
    ],
    [
        "Commodore",
        "KeyS",
        "KeyF",
        "KeyH",
        "KeyK",
        "Colon",
        "Equals",
        "F3",
    ],
    [
        "KeyQ", "KeyE", "KeyT", "KeyU", "KeyO", "At", "UpArrow", "F5",
    ],
    [
        "Digit2", "Digit4", "Digit6", "Digit8", "Digit0", "Minus", "Home", "F7",
    ],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Timing {
    Fast,
    Accurate,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Expansion {
    #[default]
    None,
    K3,
    K8,
    K16,
    K24,
    K32,
    K35,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roms() -> Roms {
        let mut kernal = vec![0xea; 8192];
        kernal[..3].copy_from_slice(&[0x4c, 0x00, 0xe0]);
        kernal[0x100..0x103].copy_from_slice(&[0xe6, 0x20, 0x40]);
        kernal[0x200..0x203].copy_from_slice(&[0xe6, 0x21, 0x40]);
        kernal[0x1ffa..].copy_from_slice(&[0x00, 0xe1, 0x00, 0xe0, 0x00, 0xe2]);
        Roms {
            basic: vec![0xa5; 8192],
            kernal,
            characters: vec![0x3c; 4096],
        }
    }

    fn machine(timing: Timing) -> Vic20 {
        Vic20::new(roms(), Standard::Pal, Expansion::None, timing).unwrap()
    }

    #[test]
    fn memory_decode_preserves_cpu_zero_page_and_via_mirrors() {
        let mut m = machine(Timing::Accurate);
        assert_eq!(m.pc(), 0xe000);
        assert_eq!(m.load(0, &[0x53, 0xa7]), 2);
        assert_eq!((m.read_memory(0), m.read_memory(1)), (0x53, 0xa7));
        assert_eq!(m.load(0x3ff, &[1, 2]), 1);
        assert_eq!(m.load(0x8000, &[0]), 0);
        assert_eq!(m.read_memory(0x8000), 0x3c);
        assert_eq!(m.read_memory(0xc000), 0xa5);
        m.bus.write(0x9112, 0xa3);
        assert_eq!(m.read_memory(0x9012), 0xa3);
        assert_eq!(m.read_memory(0x93d2), 0xa3);
        m.bus.write(0x9032, 0x17);
        assert_eq!(m.read_memory(0x9112), 0x17);
        assert_eq!(m.read_memory(0x9122), 0x17);
        m.bus.write(0x9122, 0x71);
        assert_eq!(m.read_memory(0x9032), 0x11);
        m.bus.write(0x9400, 0xbe);
        m.bus.write(0x1000, 0x40);
        assert_eq!(m.read_memory(0x9400), 0x4e);
        m.bus.write(0x900f, 0x27);
        m.bus.write(0x904f, 0x13);
        assert_eq!(m.read_memory(0x900f), 0x27);
        assert_eq!(m.read_memory(0x1_0000), 0x53);
    }

    #[test]
    fn expansions_map_only_the_selected_physical_blocks() {
        for (expansion, expected) in [
            (Expansion::None, [false, false, false, false, false]),
            (Expansion::K3, [true, false, false, false, false]),
            (Expansion::K8, [false, true, false, false, false]),
            (Expansion::K16, [false, true, true, false, false]),
            (Expansion::K24, [false, true, true, true, false]),
            (Expansion::K32, [false, true, true, true, true]),
            (Expansion::K35, [true, true, true, true, true]),
        ] {
            let mut m = Vic20::new(roms(), Standard::Pal, expansion, Timing::Fast).unwrap();
            for (address, accepted) in [0x0400, 0x2000, 0x4000, 0x6000, 0xa000]
                .into_iter()
                .zip(expected)
            {
                assert_eq!(
                    m.load(address, &[0x6a]),
                    usize::from(accepted),
                    "{expansion:?}: {address:04x}"
                );
                if accepted {
                    assert_eq!(m.read_memory(address), 0x6a);
                }
            }
        }
    }

    #[test]
    fn keyboard_uses_vic_matrix_and_scans_in_both_directions() {
        let mut m = machine(Timing::Fast);
        m.key("KeyS", true).unwrap();
        m.bus.write(0x9122, 0xff);
        m.bus.write(0x9120, !(1 << 5));
        assert_eq!(m.read_memory(0x9121), !(1 << 1));
        m.bus.write(0x9122, 0);
        m.bus.write(0x9123, 0xff);
        m.bus.write(0x9121, !(1 << 1));
        assert_eq!(m.read_memory(0x9120), !(1 << 5));
        m.key("KeyS", false).unwrap();
        assert_eq!(m.read_memory(0x9120), 0xff);
        m.key("Joy1Right", true).unwrap();
        m.key("Joy1Fire1", true).unwrap();
        assert_eq!(m.read_memory(0x9120), 0x7f);
        assert_eq!(m.read_memory(0x9111) & 0x20, 0);
        assert!(m.key("C64OnlyKey", true).is_err());
    }

    #[test]
    fn modes_change_timer_visibility_not_elapsed_clocks() {
        for (timing, expected) in [(Timing::Fast, 20), (Timing::Accurate, 16)] {
            let mut m = machine(timing);
            m.load(0x1000, &[0xad, 0x14, 0x91]);
            m.set_pc(0x1000);
            m.bus.write(0x9114, 20);
            m.bus.write(0x9115, 0);
            assert_eq!(m.step(), 4);
            assert_eq!(m.cpu.a, expected);
            assert_eq!(m.cycles(), 4);
            assert_eq!(m.read_memory(0x9114), 16);
        }
    }

    #[test]
    fn real_cpu_bus_cycles_keep_running_when_jammed() {
        for timing in [Timing::Fast, Timing::Accurate] {
            let mut m = machine(timing);
            m.load(
                0x1000,
                &[
                    0xa9, 1, 0x8d, 0, 1, 0xee, 0, 1, 0xa2, 1, 0xbd, 0xff, 0x10, 0x48, 0x68, 0xea,
                    0x02,
                ],
            );
            m.set_pc(0x1000);
            for _ in 0..12 {
                let before = m.cycles();
                let cycles = m.step();
                assert_eq!(m.cycles() - before, u64::from(cycles));
            }
            assert!(m.halted());
            let before = m.cycles();
            while m.cycles() - before < 22_152 {
                m.step();
            }
            assert!(m.vic().frames() >= 1);
            assert!(!m.drain_audio().is_empty());
        }
    }

    #[test]
    fn vias_route_distinct_interrupts_and_debug_reads_do_not_acknowledge() {
        for (via, target, marker) in [(0x9110, 0xe100, 0x20), (0x9120, 0xe200, 0x21)] {
            let mut m = machine(Timing::Accurate);
            m.cpu.p &= !4;
            m.bus.write(via + 14, 0xc0);
            m.bus.write(via + 4, 4);
            m.bus.write(via + 5, 0);
            for _ in 0..10 {
                m.step();
                if m.pc() == target {
                    break;
                }
            }
            assert_eq!(m.pc(), target);
            assert_ne!(m.read_memory(u64::from(via + 13)) & 0x40, 0);
            m.read_memory(u64::from(via + 4));
            assert_ne!(m.read_memory(u64::from(via + 13)) & 0x40, 0);
            m.bus.read(via + 4);
            assert_eq!(m.read_memory(u64::from(via + 13)) & 0x40, 0);
            m.step();
            assert_eq!(m.read_memory(marker), 1);
        }
    }

    #[test]
    fn state_restore_is_deterministic_and_invalid_states_are_atomic() {
        for timing in [Timing::Fast, Timing::Accurate] {
            let mut m = machine(timing);
            m.bus.write(0x900a, 0xe0);
            m.bus.write(0x900e, 15);
            for _ in 0..123 {
                m.step();
            }
            m.drain_audio();
            let state = m.save_state();
            for _ in 0..8000 {
                m.step();
            }
            let audio = m.drain_audio();
            let future = m.save_state();
            m.load_state(&state).unwrap();
            for _ in 0..8000 {
                m.step();
            }
            assert_eq!(m.drain_audio(), audio);
            assert_eq!(m.save_state(), future);
            let before = m.save_state();
            let mut trailing = state.clone();
            trailing.push(0);
            assert!(m.load_state(&trailing).is_err());
            assert_eq!(m.save_state(), before);
            let ((mut malformed, tape, drive), _): (
                (State, Option<TapeState>, Option<DriveState>),
                usize,
            ) = bincode::serde::decode_from_slice(&state, bincode::config::standard()).unwrap();
            malformed.hardware.ram.pop();
            let malformed = bincode::serde::encode_to_vec(
                (malformed, tape, drive),
                bincode::config::standard(),
            )
            .unwrap();
            assert!(
                m.load_state(&malformed)
                    .unwrap_err()
                    .contains("memory or input")
            );
            assert_eq!(m.save_state(), before);
            let other = if timing == Timing::Fast {
                Timing::Accurate
            } else {
                Timing::Fast
            };
            assert!(machine(other).load_state(&state).is_err());
            let mut wrong_roms = roms();
            wrong_roms.basic[0] ^= 1;
            assert!(
                Vic20::new(wrong_roms, Standard::Pal, Expansion::None, timing)
                    .unwrap()
                    .load_state(&state)
                    .is_err()
            );
            m.load(0x20, &[0x5a]);
            m.reset();
            assert_eq!(m.read_memory(0x20), 0x5a);
            assert_eq!(m.pc(), 0xe000);
        }
    }

    #[test]
    fn cassette_motor_sense_and_every_read_edge_follow_vias() {
        let tape = c64_tape::vic20::write(
            c64_tape::vic20::VideoStandard::Pal,
            &c64_tape::Tape::from_pulses(vec![8, 8, 8]),
        )
        .unwrap();
        for timing in [Timing::Fast, Timing::Accurate] {
            let mut m = machine(timing);
            m.bus.write(0x911c, 0x0e);
            assert_ne!(m.read_memory(0x9111) & 0x40, 0);
            m.load_tape(&tape, 1).unwrap();
            assert_eq!(m.read_memory(0x9111) & 0x40, 0);
            for _ in 0..10 {
                m.bus.tick();
            }
            assert_eq!(m.tape_status().unwrap().0.position, 0);
            m.bus.write(0x911c, 0x0c);
            for position in 1..=3 {
                m.bus.read(0x9121);
                assert_eq!(m.read_memory(0x912d) & 2, 0);
                for _ in 0..8 {
                    m.bus.tick();
                }
                assert_ne!(m.read_memory(0x912d) & 2, 0, "READ edge {position}");
                assert_eq!(m.tape_status().unwrap().0.position, position);
            }
            assert!(m.tape_status().unwrap().0.finished);
            m.load_tape(&tape, 1).unwrap();
            for _ in 0..3 {
                m.bus.tick();
            }
            let phase = m.bus.tape.as_ref().unwrap().dump_state();
            m.reset();
            assert_eq!(m.bus.tape.as_ref().unwrap().dump_state(), phase);
            for _ in 0..5 {
                m.bus.tick();
            }
            assert_ne!(m.read_memory(0x912d) & 2, 0, "first READ edge after reset");
            m.eject_tape();
            assert_ne!(m.read_memory(0x9111) & 0x40, 0);
            assert!(m.tape_status().is_none());
        }
    }

    #[test]
    fn cassette_phase_speed_and_state_continue_exactly_and_reject_wrong_media() {
        use c64_tape::vic20::{VideoStandard, write};
        let tape = write(
            VideoStandard::Pal,
            &c64_tape::Tape::from_pulses(vec![100_000; 10]),
        )
        .unwrap();
        for timing in [Timing::Fast, Timing::Accurate] {
            let mut m = machine(timing);
            m.bus.write(0x911c, 0x0c);
            m.load_tape(&tape, 3).unwrap();
            let start = m.cycles();
            for _ in 0..50 {
                m.step();
            }
            let mut expected = c64_tape::TapePlayer::new(c64_tape::vic20::parse(&tape).unwrap().1);
            expected.set_speed_multiplier(3);
            expected.play();
            expected.tick((m.cycles() - start) as u32);
            assert_eq!(
                m.bus.tape.as_ref().unwrap().dump_state(),
                expected.dump_state()
            );
            m.bus.write(0x911c, 0x0e);
            m.step();
            let paused = m.bus.tape.as_ref().unwrap().dump_state();
            for _ in 0..20 {
                m.step();
            }
            assert_eq!(m.bus.tape.as_ref().unwrap().dump_state(), paused);
            m.bus.write(0x911c, 0x0c);
            let state = m.save_state();
            for _ in 0..100 {
                m.step();
            }
            let future = m.save_state();
            m.load_state(&state).unwrap();
            for _ in 0..100 {
                m.step();
            }
            assert_eq!(m.save_state(), future);
            let ((core, mut tape_state, drive), _): (
                (State, Option<TapeState>, Option<DriveState>),
                usize,
            ) = bincode::serde::decode_from_slice(&state, bincode::config::standard()).unwrap();
            // Corrupt the serialized phase; TapePosition's fields are private.
            let bad_position = bincode::serde::encode_to_vec(
                (0usize, 100_000u32, true, true, 3u32),
                bincode::config::standard(),
            )
            .unwrap();
            tape_state.as_mut().unwrap().position =
                bincode::serde::decode_from_slice(&bad_position, bincode::config::standard())
                    .unwrap()
                    .0;
            let corrupt = bincode::serde::encode_to_vec(
                (core, tape_state, drive),
                bincode::config::standard(),
            )
            .unwrap();
            assert!(m.load_state(&corrupt).unwrap_err().contains("playback"));
            assert_eq!(m.save_state(), future);
            assert!(m.load_tape(&tape, 0).is_err());
            let other = write(VideoStandard::Pal, &c64_tape::Tape::from_pulses(vec![20])).unwrap();
            m.load_tape(&other, 1).unwrap();
            let before = m.save_state();
            assert!(
                m.load_state(&state)
                    .unwrap_err()
                    .contains("recording differs")
            );
            assert_eq!(m.save_state(), before);
            let ntsc = write(VideoStandard::Ntsc, &c64_tape::Tape::from_pulses(vec![8])).unwrap();
            assert!(m.load_tape(&ntsc, 1).is_err());
            assert_eq!(m.save_state(), before);
            m.eject_tape();
            assert!(m.load_state(&state).is_err());
        }
    }

    #[test]
    fn iec_buffers_invert_outputs_but_not_clock_and_data_inputs() {
        let mut m = machine(Timing::Accurate);
        m.bus.write(0x9113, 0x80);
        m.bus.write(0x9111, 0);
        m.bus.write(0x912c, 0xcc);
        assert_eq!(m.iec_output(), (false, false, false));
        m.bus.write(0x9111, 0x80);
        m.bus.write(0x912c, 0xee);
        assert_eq!(m.iec_output(), (true, true, true));
        m.set_iec_input(false, true);
        assert_eq!(m.read_memory(0x9111) & 3, 2);
        m.set_iec_input(true, false);
        assert_eq!(m.read_memory(0x9111) & 3, 1);
    }

    #[test]
    fn drive_answers_iec_and_disk_checkpoints_continue_atomically_in_both_modes() {
        let mut rom = vec![0xea; 16_384];
        let code = [
            0xa9, 0x1a, 0x8d, 0x02, 0x18, 0xa9, 0x00, 0x8d, 0x00, 0x18, 0xad, 0x00, 0x18, 0x30,
            0xfb, 0xa9, 0x12, 0x8d, 0x00, 0x18, 0x4c, 0x14, 0xc0,
        ];
        rom[..code.len()].copy_from_slice(&code);
        rom[0x3ffc..0x3ffe].copy_from_slice(&[0, 0xc0]);
        let disk = vec![0; 174_848];
        for timing in [Timing::Fast, Timing::Accurate] {
            let mut m = machine(timing);
            assert!(m.load_disk(&disk).unwrap_err().contains("--true-drive"));
            m.enable_1541(rom.clone()).unwrap();
            m.load_disk(&disk).unwrap();
            m.bus.write(0x912c, 0xcc);
            m.bus.write(0x9113, 0x80);
            m.bus.write(0x9111, 0x80);
            for _ in 0..2000 {
                m.step();
            }
            assert_eq!(m.read_memory(0x9111) & 2, 0, "Drive answers DATA after ATN");
            assert_eq!(m.disk_image(false), Some(disk.clone()));
            m.load_disk(&disk).unwrap();
            let saved = m.save_state();
            for _ in 0..2000 {
                m.step();
            }
            let future = m.save_state();
            m.load_state(&saved).unwrap();
            for _ in 0..2000 {
                m.step();
            }
            assert_eq!(m.save_state(), future);
            let ((core, tape, mut drive), _): (
                (State, Option<TapeState>, Option<DriveState>),
                usize,
            ) = bincode::serde::decode_from_slice(&saved, bincode::config::standard()).unwrap();
            drive.as_mut().unwrap().checkpoint.pop();
            let corrupt =
                bincode::serde::encode_to_vec((core, tape, drive), bincode::config::standard())
                    .unwrap();
            assert!(m.load_state(&corrupt).is_err());
            assert_eq!(m.save_state(), future);
            let mut other = disk.clone();
            other[0] = 1;
            m.load_disk(&other).unwrap();
            let before = m.save_state();
            assert!(
                m.load_state(&saved)
                    .unwrap_err()
                    .contains("attachment differs")
            );
            assert_eq!(m.save_state(), before);
            m.reset();
            assert!(m.disk_status().unwrap().0);
            m.eject_disk();
            assert!(!m.disk_status().unwrap().0);
            assert!(m.disk_image(false).is_none());
        }
    }

    #[test]
    fn old_state_versions_load_without_a_drive_including_version2_tape_phase() {
        let mut m = machine(Timing::Accurate);
        let bytes = m.save_state();
        let (mut state, _): (State, usize) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).unwrap();
        state.version = 1;
        let legacy = bincode::serde::encode_to_vec(state, bincode::config::standard()).unwrap();
        m.step();
        m.load_state(&legacy).unwrap();
        assert_eq!(m.save_state(), bytes);
        let (mut state, _): (State, usize) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).unwrap();
        state.version = 2;
        let legacy =
            bincode::serde::encode_to_vec((state, None::<TapeState>), bincode::config::standard())
                .unwrap();
        m.step();
        m.load_state(&legacy).unwrap();
        assert_eq!(m.save_state(), bytes);
        let recording = c64_tape::vic20::write(
            c64_tape::vic20::VideoStandard::Pal,
            &c64_tape::Tape::from_pulses(vec![1000, 1200]),
        )
        .unwrap();
        m.load_tape(&recording, 1).unwrap();
        for _ in 0..20 {
            m.step();
        }
        let with_tape = m.save_state();
        let ((mut state, tape, _drive), _): (
            (State, Option<TapeState>, Option<DriveState>),
            usize,
        ) = bincode::serde::decode_from_slice(&with_tape, bincode::config::standard()).unwrap();
        state.version = 2;
        let legacy =
            bincode::serde::encode_to_vec((state, tape), bincode::config::standard()).unwrap();
        m.step();
        m.load_state(&legacy).unwrap();
        assert_eq!(m.save_state(), with_tape);
    }
}

impl Expansion {
    fn maps(self, address: u16) -> bool {
        match address {
            0x0400..=0x0fff => matches!(self, Self::K3 | Self::K35),
            0x2000..=0x3fff => !matches!(self, Self::None | Self::K3),
            0x4000..=0x5fff => matches!(self, Self::K16 | Self::K24 | Self::K32 | Self::K35),
            0x6000..=0x7fff => matches!(self, Self::K24 | Self::K32 | Self::K35),
            0xa000..=0xbfff => matches!(self, Self::K32 | Self::K35),
            _ => false,
        }
    }
}

pub struct Roms {
    pub basic: Vec<u8>,
    pub kernal: Vec<u8>,
    pub characters: Vec<u8>,
}

impl Roms {
    fn validate(&self) -> Result<(), String> {
        for (name, actual, expected) in [
            ("BASIC", self.basic.len(), 8192),
            ("KERNAL", self.kernal.len(), 8192),
            ("character", self.characters.len(), 4096),
        ] {
            if actual != expected {
                return Err(format!(
                    "VIC-20 {name} ROM must be {expected} bytes, got {actual}"
                ));
            }
        }
        Ok(())
    }

    fn fingerprint(&self) -> [u8; 20] {
        let mut hash = Sha1::new();
        hash.update(&self.basic);
        hash.update(&self.kernal);
        hash.update(&self.characters);
        hash.finalize().into()
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Hardware {
    ram: Vec<u8>,
    color: Vec<u8>,
    vic: vic_i::Vic,
    via1: Via,
    via2: Via,
    keys: [u8; 8],
    joystick: u8,
    restore: bool,
    cpu_bus: u8,
    iec_clock: bool,
    iec_data: bool,
    nmi_level: bool,
    nmi_edge: bool,
    cycles: u64,
}

struct VicBus {
    hardware: Hardware,
    roms: Roms,
    expansion: Expansion,
    timing: Timing,
    in_cpu: bool,
    audio: VecDeque<f32>,
    audio_dropped: u64,
    tape: Option<c64_tape::TapePlayer>,
    tape_id: Option<[u8; 20]>,
    drive: Option<Drive>,
}

struct Drive {
    link: DriveLink,
    rom: Vec<u8>,
    source: Option<Vec<u8>>,
}

impl Drive {
    fn new(rom: Vec<u8>) -> Result<Self, String> {
        Ok(Self {
            link: DriveLink::new(DriveKind::C1541, rom.clone(), 8)?,
            rom,
            source: None,
        })
    }
}

impl IecHost for VicBus {
    fn iec_output(&self) -> (bool, bool, bool) {
        let h = &self.hardware;
        (
            h.via1.effective_port_a() & 0x80 != 0,
            h.via2.ca2_output_level().unwrap_or(true),
            h.via2.cb2_output_level().unwrap_or(true),
        )
    }

    fn set_iec_input(&mut self, clock: bool, data: bool) {
        self.hardware.iec_clock = clock;
        self.hardware.iec_data = data;
        self.update_inputs();
    }

    fn clock_hz(&self) -> u64 {
        u64::from(self.hardware.vic.standard().clock_hz())
    }
}

impl VicBus {
    fn new(roms: Roms, standard: Standard, expansion: Expansion, timing: Timing) -> Self {
        let mut hardware = Hardware {
            ram: vec![0; 65536],
            color: vec![0; 1024],
            vic: vic_i::Vic::new(standard),
            via1: Via::new(),
            via2: Via::new(),
            keys: [0; 8],
            joystick: 0,
            restore: false,
            cpu_bus: 0,
            iec_clock: true,
            iec_data: true,
            nmi_level: false,
            nmi_edge: false,
            cycles: 0,
        };
        hardware.via1.set_external_port_b(0xff);
        let mut bus = Self {
            hardware,
            roms,
            expansion,
            timing,
            in_cpu: false,
            audio: VecDeque::with_capacity(1024),
            audio_dropped: 0,
            tape: None,
            tape_id: None,
            drive: None,
        };
        bus.update_inputs();
        bus
    }

    fn ram_mapped(&self, address: u16) -> bool {
        matches!(address, 0x0000..=0x03ff | 0x1000..=0x1fff) || self.expansion.maps(address)
    }

    fn update_inputs(&mut self) {
        let h = &mut self.hardware;
        let mut via1_a = 0x7f & !(h.joystick & 0x3c);
        if self.tape.is_some() {
            via1_a &= !0x40;
        }
        if !h.iec_clock {
            via1_a &= !1;
        }
        if !h.iec_data {
            via1_a &= !2;
        }
        h.via1.set_external_port_a(via1_a);
        h.via1.set_ca1(!h.restore);
        let mut low_a = !h.via2.port_a & h.via2.ddr_a;
        let mut low_b = !h.via2.port_b & h.via2.ddr_b;
        if h.joystick & 0x80 != 0 {
            low_b |= 0x80;
        }
        loop {
            let old = (low_a, low_b);
            for column in 0..8 {
                if low_b & (1 << column) != 0 {
                    low_a |= h.keys[column];
                }
                if h.keys[column] & low_a != 0 {
                    low_b |= 1 << column;
                }
            }
            if old == (low_a, low_b) {
                break;
            }
        }
        h.via2.set_external_port_a(!low_a);
        h.via2.set_external_port_b(!low_b);
    }

    fn tick(&mut self) {
        let h = &mut self.hardware;
        let open = h.vic.bus_value();
        let sample = h.vic.tick(|address| {
            let cpu_address = if address & 0x2000 == 0 {
                0x8000 | address
            } else {
                address & 0x1fff
            };
            let data = match cpu_address {
                0x0000..=0x03ff | 0x1000..=0x1fff => h.ram[usize::from(cpu_address)],
                0x8000..=0x8fff => self.roms.characters[usize::from(cpu_address - 0x8000)],
                0x9400..=0x97ff => (open & 0xf0) | h.color[usize::from(cpu_address & 0x3ff)],
                _ => open,
            };
            (data, h.color[usize::from(address & 0x3ff)])
        });
        if let Some(sample) = sample {
            if self.audio.len() == vic_i::SAMPLE_RATE as usize * 2 {
                self.audio.pop_front();
                self.audio_dropped = self.audio_dropped.saturating_add(1);
            }
            self.audio.push_back(sample);
        }
        h.via1.tick(1);
        h.via2.tick(1);
        if let Some(tape) = &mut self.tape {
            if !h.via1.ca2_output_level().unwrap_or(false) {
                tape.play();
                // TAP v0/v1 durations end at falling READ edges, not alternating half-waves.
                for _ in 0..tape.tick(1) {
                    h.via2.set_ca1(false);
                    h.via2.set_ca1(true);
                }
            } else {
                tape.pause();
            }
        }
        let nmi = h.via1.peek(13) & 0x80 != 0;
        h.nmi_edge |= nmi && !h.nmi_level;
        h.nmi_level = nmi;
        h.cycles = h.cycles.wrapping_add(1);
        if let Some(mut drive) = self.drive.take() {
            drive.link.after_host_step(self, 1);
            self.drive = Some(drive);
        }
    }

    fn access_clock(&mut self) {
        if self.in_cpu && self.timing == Timing::Accurate {
            self.tick();
        }
    }

    fn peek(&self, address: u16) -> u8 {
        let h = &self.hardware;
        match address {
            _ if self.ram_mapped(address) => h.ram[usize::from(address)],
            0x8000..=0x8fff => self.roms.characters[usize::from(address - 0x8000)],
            0x9000..=0x93ff => {
                let mut value = 0xff;
                let mut selected = false;
                if address <= 0x900f {
                    value &= h.vic.read(address as u8);
                    selected = true;
                }
                if address & 0x10 != 0 {
                    value &= h.via1.peek(address as u8);
                    selected = true;
                }
                if address & 0x20 != 0 {
                    value &= h.via2.peek(address as u8);
                    selected = true;
                }
                if selected { value } else { h.vic.bus_value() }
            }
            0x9400..=0x97ff => (h.vic.bus_value() & 0xf0) | h.color[usize::from(address & 0x3ff)],
            0xc000..=0xdfff => self.roms.basic[usize::from(address - 0xc000)],
            0xe000..=0xffff => self.roms.kernal[usize::from(address - 0xe000)],
            0x0400..=0x0fff | 0x9000..=0x9fff => h.vic.bus_value(),
            _ => h.cpu_bus,
        }
    }

    fn write_raw(&mut self, address: u16, value: u8) -> bool {
        self.hardware.cpu_bus = value;
        if matches!(address, 0x0000..=0x03ff | 0x1000..=0x1fff | 0x8000..=0x97ff) {
            self.hardware.vic.drive_bus(value);
        }
        let mapped = self.ram_mapped(address);
        let h = &mut self.hardware;
        match address {
            _ if mapped => h.ram[usize::from(address)] = value,
            0x9000..=0x93ff => {
                if address > 0x900f && address & 0x30 == 0 {
                    return false;
                }
                if address <= 0x900f {
                    h.vic.write(address as u8, value);
                }
                if address & 0x10 != 0 {
                    h.via1.write(address as u8, value);
                }
                if address & 0x20 != 0 {
                    h.via2.write(address as u8, value);
                }
            }
            0x9400..=0x97ff => h.color[usize::from(address & 0x3ff)] = value & 15,
            _ => return false,
        }
        self.update_inputs();
        true
    }
}

impl Bus for VicBus {
    fn read(&mut self, address: u16) -> u8 {
        self.access_clock();
        self.update_inputs();
        let value = self.peek(address);
        if matches!(address, 0x9000..=0x93ff) {
            let h = &mut self.hardware;
            if address & 0x10 != 0 {
                h.via1.read(address as u8);
            }
            if address & 0x20 != 0 {
                h.via2.read(address as u8);
            }
        }
        self.hardware.cpu_bus = value;
        if matches!(address, 0x0000..=0x03ff | 0x1000..=0x1fff | 0x8000..=0x97ff) {
            self.hardware.vic.drive_bus(value);
        }
        value
    }

    fn write(&mut self, address: u16, value: u8) {
        self.access_clock();
        self.write_raw(address, value);
    }

    fn dummy_cycle(&mut self, address: u16, write: bool, value: u8) {
        if write {
            self.write(address, value);
        } else {
            self.read(address);
        }
    }
}

pub struct Vic20 {
    pub cpu: mos6502::Cpu,
    bus: VicBus,
    border: bool,
}

impl Vic20 {
    pub fn new(
        roms: Roms,
        standard: Standard,
        expansion: Expansion,
        timing: Timing,
    ) -> Result<Self, String> {
        roms.validate()?;
        let mut machine = Self {
            cpu: mos6502::Cpu::new(),
            bus: VicBus::new(roms, standard, expansion, timing),
            border: true,
        };
        machine.cpu.reset(&mut machine.bus);
        Ok(machine)
    }

    pub fn vic(&self) -> &vic_i::Vic {
        &self.bus.hardware.vic
    }
    pub fn via(&self, index: usize) -> Option<&Via> {
        match index {
            0 => Some(&self.bus.hardware.via1),
            1 => Some(&self.bus.hardware.via2),
            _ => None,
        }
    }
    pub fn cycles(&self) -> u64 {
        self.bus.hardware.cycles
    }
    pub fn timing(&self) -> Timing {
        self.bus.timing
    }
    pub fn expansion(&self) -> Expansion {
        self.bus.expansion
    }
    pub fn audio_dropped(&self) -> u64 {
        self.bus.audio_dropped
    }
    pub fn set_border(&mut self, enabled: bool) {
        self.border = enabled;
    }
    pub fn can_write(&self, address: u16) -> bool {
        self.bus.ram_mapped(address)
            || matches!(address, 0x9000..=0x900f | 0x9400..=0x97ff)
            || (matches!(address, 0x9000..=0x93ff) && address & 0x30 != 0)
    }

    /// Attach raw program bytes without jumping or changing BASIC pointers.
    /// The host removes a PRG/P00 header before calling this method.
    pub fn load_program(&mut self, address: u16, bytes: &[u8]) -> Result<(), String> {
        let start = usize::from(address);
        let end = start
            .checked_add(bytes.len())
            .filter(|&end| end <= 65_536)
            .ok_or("VIC-20 program extends beyond the 16-bit address space")?;
        if bytes.is_empty() {
            return Err("VIC-20 program is empty".to_string());
        }
        if let Some(unmapped) = (start..end).find(|&addr| !self.bus.ram_mapped(addr as u16)) {
            return Err(format!("VIC-20 program address {unmapped:04x} is not RAM"));
        }
        self.load(address.into(), bytes);
        Ok(())
    }

    pub fn key(&mut self, name: &str, pressed: bool) -> Result<(), String> {
        let name = name.to_ascii_lowercase().replace(['_', '-'], "");
        if name == "restore" {
            self.bus.hardware.restore = pressed;
        } else if let Some((_, bit)) = JOYSTICK_KEYS
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(&name))
        {
            if pressed {
                self.bus.hardware.joystick |= 1 << bit;
            } else {
                self.bus.hardware.joystick &= !(1 << bit);
            }
        } else {
            let mut position = None;
            for (column, keys) in KEY_MATRIX.iter().enumerate() {
                if let Some(row) = keys.iter().position(|key| key.to_ascii_lowercase() == name) {
                    position = Some((column, row));
                    break;
                }
            }
            let (column, row) = position.ok_or_else(|| format!("unknown VIC-20 key \"{name}\""))?;
            if pressed {
                self.bus.hardware.keys[column] |= 1 << row;
            } else {
                self.bus.hardware.keys[column] &= !(1 << row);
            }
        }
        self.bus.update_inputs();
        Ok(())
    }

    pub fn iec_output(&self) -> (bool, bool, bool) {
        self.bus.iec_output()
    }

    pub fn set_iec_input(&mut self, clock: bool, data: bool) {
        self.bus.set_iec_input(clock, data);
    }

    pub fn enable_1541(&mut self, rom: Vec<u8>) -> Result<(), String> {
        if self.bus.drive.is_some() {
            return Err("VIC-20 already has a 1541 Drive".to_string());
        }
        self.bus.drive = Some(Drive::new(rom)?);
        Ok(())
    }

    pub fn load_disk(&mut self, bytes: &[u8]) -> Result<(), String> {
        let drive = self
            .bus
            .drive
            .as_mut()
            .ok_or("VIC-20 disks require --true-drive and --rom-1541")?;
        let disk =
            d64::D64::parse(bytes).map_err(|error| format!("invalid VIC-20 D64: {error}"))?;
        drive
            .link
            .attach_disk(Disk::Gcr(c1541::Medium::D64(disk)))?;
        drive.source = Some(bytes.to_vec());
        Ok(())
    }

    pub fn eject_disk(&mut self) {
        if let Some(drive) = &mut self.bus.drive {
            drive.link.detach_disk();
            drive.source = None;
        }
    }

    pub fn disk_image(&mut self, only_if_modified: bool) -> Option<Vec<u8>> {
        let drive = self.bus.drive.as_mut()?;
        if only_if_modified {
            drive.link.take_modified_disk_image()
        } else {
            drive.link.disk_image().or_else(|| drive.source.clone())
        }
    }

    pub fn mark_disk_saved(&mut self) {
        if let Some(drive) = &mut self.bus.drive {
            drive.link.mark_disk_saved();
        }
    }

    pub fn disk_status(&self) -> Option<(bool, u16, bool)> {
        let drive = self.bus.drive.as_ref()?;
        let (track, motor) = drive.link.gcr_status().expect("VIC-20 has a GCR Drive");
        Some((drive.source.is_some(), u16::from(track), motor))
    }

    /// Attaches a read-only recording with PLAY pressed. The VIA controls its motor.
    pub fn load_tape(&mut self, bytes: &[u8], speed: u32) -> Result<(), String> {
        if speed == 0 {
            return Err("VIC-20 tape speed must be positive".to_string());
        }
        let (video, tape) = c64_tape::vic20::parse(bytes)
            .map_err(|error| format!("invalid VIC-20 TAP: {error}"))?;
        if video.clock_hz() != self.vic().standard().clock_hz() {
            return Err(
                "VIC-20 tape Region differs from the machine; convert the recording first"
                    .to_string(),
            );
        }
        let mut player = c64_tape::TapePlayer::new(tape);
        player.set_speed_multiplier(speed);
        self.bus.tape = Some(player);
        self.bus.tape_id = Some(Sha1::digest(bytes).into());
        self.bus.hardware.via2.set_ca1(true);
        self.bus.update_inputs();
        Ok(())
    }

    pub fn eject_tape(&mut self) {
        self.bus.tape = None;
        self.bus.tape_id = None;
        self.bus.hardware.via2.set_ca1(true);
        self.bus.update_inputs();
    }

    pub fn tape_status(&self) -> Option<(c64_tape::TapePlaybackState, bool)> {
        self.bus.tape.as_ref().map(|tape| {
            let motor = !self.bus.hardware.via1.ca2_output_level().unwrap_or(false);
            let mut status = tape.playback_state();
            status.playing = motor && !status.finished;
            (status, motor)
        })
    }
}

#[derive(Serialize, Deserialize)]
struct State {
    magic: [u8; 4],
    version: u8,
    roms: [u8; 20],
    timing: Timing,
    expansion: Expansion,
    cpu: mos6502::Cpu,
    hardware: Hardware,
}

#[derive(Serialize, Deserialize)]
struct TapeState {
    fingerprint: [u8; 20],
    position: c64_tape::TapePosition,
}

#[derive(Serialize, Deserialize)]
struct DriveState {
    rom: [u8; 20],
    media: Option<[u8; 20]>,
    checkpoint: Vec<u8>,
}

impl Machine for Vic20 {
    fn clock_hz(&self) -> u64 {
        u64::from(self.vic().standard().clock_hz())
    }
    fn frame_hz(&self) -> u64 {
        self.frame_rate().hz().round() as u64
    }
    fn frame_rate(&self) -> FrameRate {
        FrameRate {
            num: self.vic().standard().clock_hz(),
            den: self.vic().standard().cycles_per_frame(),
        }
    }
    fn run(&mut self, running: &mut dyn FnMut(&dyn Machine) -> bool) -> RunStats {
        Clock::new(self.frame_hz()).run(self, running)
    }
    fn step(&mut self) -> u32 {
        if self.bus.hardware.via2.peek(13) & 0x80 != 0 {
            self.cpu.request_irq();
        }
        if std::mem::take(&mut self.bus.hardware.nmi_edge) {
            self.cpu.request_nmi();
        }
        self.bus.in_cpu = true;
        let cycles = self.cpu.step(&mut self.bus);
        self.bus.in_cpu = false;
        if self.bus.timing == Timing::Fast {
            for _ in 0..cycles {
                self.bus.tick();
            }
        }
        cycles
    }
    fn halted(&self) -> bool {
        self.cpu.halted
    }
    fn pc(&self) -> Addr {
        self.cpu.pc.into()
    }
    fn registers(&self) -> Vec<(&'static str, u64)> {
        self.cpu.registers()
    }
    fn render_frame(&self) -> Option<(u32, u32, Vec<u8>)> {
        let (width, height, rgb) = self.vic().frame_rgb();
        if self.border {
            return Some((width, height, rgb));
        }
        let vic = self.vic();
        let left = u32::from(vic.read(0) & 0x7f) * 4;
        let top = u32::from(vic.read(1)) * 2;
        let columns = u32::from(vic.read(2) & 0x7f) * 8;
        let rows = u32::from((vic.read(3) >> 1) & 0x3f) * if vic.read(3) & 1 == 0 { 8 } else { 16 };
        if left >= width || top >= height || columns == 0 || rows == 0 {
            return Some((1, 1, vec![0; 3]));
        }
        let cropped_width = columns.min(width - left);
        let cropped_height = rows.min(height - top);
        let mut cropped = Vec::with_capacity((cropped_width * cropped_height * 3) as usize);
        for y in top..top + cropped_height {
            let start = ((y * width + left) * 3) as usize;
            cropped.extend_from_slice(&rgb[start..start + cropped_width as usize * 3]);
        }
        Some((cropped_width, cropped_height, cropped))
    }
    fn read_memory(&self, address: Addr) -> u8 {
        self.bus.peek(address as u16)
    }
    fn load(&mut self, address: Addr, bytes: &[u8]) -> usize {
        bytes
            .iter()
            .enumerate()
            .filter(|&(offset, &value)| {
                self.bus
                    .write_raw((address as u16).wrapping_add(offset as u16), value)
            })
            .count()
    }
    fn set_pc(&mut self, address: Addr) {
        self.cpu.pc = address as u16;
        self.cpu.halted = false;
    }
    fn drain_audio(&mut self) -> Vec<f32> {
        self.bus.audio.drain(..).collect()
    }
    fn reset(&mut self) {
        if let Some(drive) = &mut self.bus.drive {
            drive.link.reset();
        }
        let h = &mut self.bus.hardware;
        h.via1 = Via::new();
        h.via2 = Via::new();
        if self.bus.tape.is_some() {
            h.via2.set_ca1(true);
        }
        h.via1.set_external_port_b(0xff);
        h.vic.reset();
        h.nmi_level = false;
        h.nmi_edge = false;
        self.bus.update_inputs();
        self.bus.audio.clear();
        self.cpu.reset(&mut self.bus);
    }
    fn save_state(&self) -> Vec<u8> {
        bincode::serde::encode_to_vec(
            (
                State {
                    magic: *b"VIC1",
                    version: 3,
                    roms: self.bus.roms.fingerprint(),
                    timing: self.bus.timing,
                    expansion: self.bus.expansion,
                    cpu: self.cpu,
                    hardware: self.bus.hardware.clone(),
                },
                self.bus.tape.as_ref().map(|tape| TapeState {
                    fingerprint: self.bus.tape_id.expect("attached tape has an identity"),
                    position: tape.dump_state(),
                }),
                self.bus.drive.as_ref().map(|drive| DriveState {
                    rom: Sha1::digest(&drive.rom).into(),
                    media: drive
                        .source
                        .as_ref()
                        .map(|bytes| Sha1::digest(bytes).into()),
                    checkpoint: drive
                        .link
                        .save_gcr_checkpoint()
                        .expect("VIC-20 has a GCR Drive"),
                }),
            ),
            bincode::config::standard(),
        )
        .expect("VIC-20 state is encodable")
    }
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > 1_048_576 {
            return Err("VIC-20 state exceeds 1 MiB".to_string());
        }
        let (state, used): (State, usize) = bincode::serde::decode_from_slice(
            bytes,
            bincode::config::standard().with_limit::<1_048_576>(),
        )
        .map_err(|error| format!("invalid VIC-20 state: {error}"))?;
        let (tape_state, drive_state) = match state.version {
            1 if used == bytes.len() => (None, None),
            2 | 3 => {
                let (tape, count): (Option<TapeState>, usize) = bincode::serde::decode_from_slice(
                    &bytes[used..],
                    bincode::config::standard().with_limit::<1_048_576>(),
                )
                .map_err(|error| format!("invalid VIC-20 tape state: {error}"))?;
                let end = used + count;
                let (drive, count) = if state.version == 3 {
                    bincode::serde::decode_from_slice::<Option<DriveState>, _>(
                        &bytes[end..],
                        bincode::config::standard().with_limit::<1_048_576>(),
                    )
                    .map_err(|error| format!("invalid VIC-20 Drive state: {error}"))?
                } else {
                    (None, 0)
                };
                if end + count != bytes.len() {
                    return Err("invalid VIC-20 state trailing bytes".to_string());
                }
                (tape, drive)
            }
            _ => return Err("invalid VIC-20 state version or trailing bytes".to_string()),
        };
        if state.magic != *b"VIC1" {
            return Err("invalid VIC-20 state header or trailing bytes".to_string());
        }
        if state.roms != self.bus.roms.fingerprint()
            || state.timing != self.bus.timing
            || state.expansion != self.bus.expansion
            || state.hardware.vic.standard() != self.vic().standard()
        {
            return Err(
                "VIC-20 state ROMs, execution mode, RAM expansion, or Region differ".to_string(),
            );
        }
        if state.hardware.ram.len() != 65536
            || state.hardware.color.len() != 1024
            || state.hardware.color.iter().any(|&value| value > 15)
            || state.hardware.joystick & !0xbc != 0
        {
            return Err("invalid VIC-20 memory or input state".to_string());
        }
        state.hardware.vic.validate()?;
        match (&self.bus.tape, &tape_state) {
            (Some(tape), Some(saved)) if self.bus.tape_id == Some(saved.fingerprint) => {
                tape.validate_state(&saved.position)
                    .map_err(|error| format!("invalid VIC-20 tape state: {error}"))?;
            }
            (None, None) => {}
            _ => return Err("VIC-20 state tape attachment or recording differs".to_string()),
        }
        let drive = match (&self.bus.drive, drive_state) {
            (None, None) => None,
            (Some(current), Some(saved))
                if saved.rom == <[u8; 20]>::from(Sha1::digest(&current.rom))
                    && saved.media
                        == current
                            .source
                            .as_ref()
                            .map(|bytes| Sha1::digest(bytes).into()) =>
            {
                let mut restored = Drive::new(current.rom.clone())?;
                restored.source = current.source.clone();
                restored.link.load_gcr_checkpoint(&saved.checkpoint)?;
                Some(restored)
            }
            _ => return Err("VIC-20 state Drive, DOS ROM or disk attachment differs".to_string()),
        };
        if let (Some(tape), Some(saved)) = (&mut self.bus.tape, tape_state) {
            tape.restore_state(saved.position);
        }
        self.cpu = state.cpu;
        self.bus.drive = drive;
        self.bus.hardware = state.hardware;
        self.bus.audio.clear();
        Ok(())
    }
}
