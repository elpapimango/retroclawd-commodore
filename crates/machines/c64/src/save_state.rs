//! Internal save-state encode/decode for `Machine::save_state`/
//! `load_state`. Same two conventions this project's other machines
//! already established (see `CLAUDE.md`'s `zx_spectrum`/`zx81` notes):
//! bincode v2's serde compat feature, and ROM contents deliberately
//! excluded (restoring requires the same `--rom` files already
//! supplied, exactly like `.z80`/`.sna` snapshot loading already
//! requires). Most fields here get a direct serde derive straight onto
//! the real internal type (`mos6502::Cpu`, `vic_ii::Vic`, `crate::cia::
//! Cia`, `sid::Sid`) -- none of them hold or are generic over
//! externally-supplied media, so there's no need for the small
//! mirror-struct shape `nec765::Fdc`/`zx_tape::TapePlayer` use. The
//! cassette tape (`c64_tape::TapePlayer`) *does* hold one (the tape's own
//! parsed pulse data) -- same "own dump/restore shape" precedent, so only
//! its `TapePosition` (playback position, not the tape's own bytes)
//! round-trips here, `None` if no tape is attached.

use crate::C64;

#[derive(serde::Serialize, serde::Deserialize)]
struct SaveState {
    cpu: mos6502::Cpu,
    ram: Vec<u8>,
    color_ram: Vec<u8>,
    port_ddr: u8,
    port_data: u8,
    vic: vic_ii::Vic,
    cia1: crate::cia::Cia,
    cia2: crate::cia::Cia,
    keyboard: crate::keyboard::KeyMatrix,
    sid: sid::Sid,
    audio_cycle_accumulator: f64,
    iec_clock_in: bool,
    iec_data_in: bool,
    tape_position: Option<c64_tape::TapePosition>,
    /// Register state of the cartridge in the port, if any. The ROM itself
    /// stays external like every other medium.
    cartridge: Option<crate::cartridge::CartState>,
    /// The REU's registers and its DRAM contents (the DRAM is machine
    /// state, not media).
    reu: Option<crate::reu::ReuState>,
    georam: Option<crate::georam::GeoRamState>,
    acia: Option<(acia6551::Acia, u8)>,
    rrnet: Option<crate::rrnet::RrNet>,
    printer: Option<crate::printer::Printer>,
}

pub(crate) fn encode(c64: &C64) -> Vec<u8> {
    let state = SaveState {
        cpu: c64.cpu,
        ram: c64.bus.ram.clone(),
        color_ram: c64.bus.color_ram.clone(),
        port_ddr: c64.bus.port_ddr,
        port_data: c64.bus.port_data,
        vic: c64.bus.vic.clone(),
        cia1: c64.bus.cia1.clone(),
        cia2: c64.bus.cia2.clone(),
        keyboard: c64.bus.keyboard.clone(),
        sid: c64.bus.sid.clone(),
        audio_cycle_accumulator: c64.audio_cycle_accumulator,
        iec_clock_in: c64.bus.iec_clock_in,
        iec_data_in: c64.bus.iec_data_in,
        tape_position: c64.bus.tape.as_ref().map(c64_tape::TapePlayer::dump_state),
        cartridge: c64
            .bus
            .cartridge
            .as_ref()
            .map(crate::cartridge::Cartridge::state),
        reu: c64.bus.reu.as_ref().map(crate::reu::Reu::state),
        georam: c64.bus.georam.as_ref().map(crate::georam::GeoRam::state),
        acia: c64.bus.acia.clone(),
        rrnet: c64.bus.rrnet.clone(),
        printer: c64.bus.printer.clone(),
    };
    bincode::serde::encode_to_vec(&state, bincode::config::standard())
        .expect("C64 save state should always be encodable")
}

pub(crate) fn decode(c64: &mut C64, bytes: &[u8]) -> Result<(), String> {
    let (state, _): (SaveState, usize) =
        bincode::serde::decode_from_slice(bytes, bincode::config::standard())
            .map_err(|e| format!("failed to decode C64 save state: {e}"))?;

    if state.ram.len() != c64.bus.ram.len() {
        return Err(format!(
            "save state RAM size mismatch: expected {}, got {}",
            c64.bus.ram.len(),
            state.ram.len()
        ));
    }
    if state.color_ram.len() != c64.bus.color_ram.len() {
        return Err(format!(
            "save state color RAM size mismatch: expected {}, got {}",
            c64.bus.color_ram.len(),
            state.color_ram.len()
        ));
    }

    if state.vic.cycle_engine_enabled() != c64.bus.vic.cycle_engine_enabled() {
        let name = |on: bool| if on { "cycle" } else { "line" };
        return Err(format!(
            "save state was made with vic_accuracy={}, this machine runs vic_accuracy={}",
            name(state.vic.cycle_engine_enabled()),
            name(c64.bus.vic.cycle_engine_enabled())
        ));
    }

    c64.cpu = state.cpu;
    c64.bus.ram = state.ram;
    c64.bus.color_ram = state.color_ram;
    c64.bus.port_ddr = state.port_ddr;
    c64.bus.port_data = state.port_data;
    c64.bus.vic = state.vic;
    c64.bus.cia1 = state.cia1;
    c64.bus.cia2 = state.cia2;
    // The restored CIA2 may already hold its line asserted: that is not a new edge.
    c64.bus.cia2_nmi_active = c64.bus.cia2.interrupt_asserted();
    c64.bus.sync_vic_chargen_shadow();
    c64.bus.keyboard = state.keyboard;
    c64.bus.sid = state.sid;
    c64.audio_cycle_accumulator = state.audio_cycle_accumulator;
    c64.bus.iec_clock_in = state.iec_clock_in;
    c64.bus.iec_data_in = state.iec_data_in;
    // The tape's own data stays external (same `--rom`/`--tape` convention
    // every other machine here already follows) -- restoring a saved
    // position only makes sense onto an instance that already has the
    // same tape attached; if none is attached here, there's simply
    // nothing to apply the position to, not an error.
    if let (Some(position), Some(tape)) = (state.tape_position, &mut c64.bus.tape) {
        tape.restore_state(position);
    }
    // Same for the cartridge: its registers apply onto the same cartridge
    // image already attached; with none attached there is nothing to restore.
    if let (Some(saved), Some(cartridge)) = (state.cartridge, &mut c64.bus.cartridge) {
        cartridge.restore_state(saved);
    }
    if let (Some(saved), Some(reu)) = (state.reu, &mut c64.bus.reu) {
        reu.restore_state(saved);
    }
    if let (Some(saved), Some(georam)) = (state.georam, &mut c64.bus.georam) {
        georam.restore_state(saved);
    }
    // Serial, ethernet and printer state only applies onto a machine fitted
    // with the same device; their queues travel with the state.
    if let (Some(saved), Some(slot)) = (state.acia, &mut c64.bus.acia)
        && saved.0.mode() == slot.0.mode()
    {
        *slot = saved;
    }
    if let (Some(saved), Some(slot)) = (state.rrnet, &mut c64.bus.rrnet) {
        *slot = saved;
    }
    if let (Some(saved), Some(slot)) = (state.printer, &mut c64.bus.printer) {
        *slot = saved;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{BASIC_ROM_SIZE, CHAR_ROM_SIZE, KERNAL_ROM_SIZE};
    use machine::{Bus, Machine};

    fn save_state_round_trips_cpu_and_memory() {
        let mut c64 = C64::new(
            vec![0; KERNAL_ROM_SIZE],
            vec![0; BASIC_ROM_SIZE],
            vec![0; CHAR_ROM_SIZE],
        )
        .unwrap();
        c64.bus.write(0x0400, 0x42);
        c64.set_pc(0x1234);

        let saved = c64.save_state();

        let mut restored = C64::new(
            vec![0; KERNAL_ROM_SIZE],
            vec![0; BASIC_ROM_SIZE],
            vec![0; CHAR_ROM_SIZE],
        )
        .unwrap();
        restored.load_state(&saved).unwrap();

        assert_eq!(restored.pc(), 0x1234);
        assert_eq!(restored.read_memory(0x0400), 0x42);
    }

    fn save_state_round_trips_the_tapes_own_playback_position() {
        let mut bytes = vec![0u8; 20];
        bytes[0..12].copy_from_slice(b"C64-TAPE-RAW");
        bytes.extend([10, 10, 10]); // three 80-cycle pulses, version 0
        let tape = c64_tape::parse_tap(&bytes).unwrap();

        let mut c64 = C64::new(
            vec![0; KERNAL_ROM_SIZE],
            vec![0; BASIC_ROM_SIZE],
            vec![0; CHAR_ROM_SIZE],
        )
        .unwrap();
        c64.load_tape(tape);
        c64.bus.write(0x0000, 0x20); // DDR: motor bit output
        c64.bus.write(0x0001, 0x00); // driven low -> motor on
        c64.bus.tick_tape(90); // advance partway into the second pulse

        let saved = c64.save_state();

        let same_tape = c64_tape::parse_tap(&bytes).unwrap();
        let mut restored = C64::new(
            vec![0; KERNAL_ROM_SIZE],
            vec![0; BASIC_ROM_SIZE],
            vec![0; CHAR_ROM_SIZE],
        )
        .unwrap();
        restored.load_tape(same_tape);
        restored.load_state(&saved).unwrap();

        assert_eq!(
            restored
                .bus
                .tape
                .as_ref()
                .unwrap()
                .playback_state()
                .position,
            c64.bus.tape.as_ref().unwrap().playback_state().position,
            "the restored tape must resume from the exact same pulse position"
        );
    }
    /// Merged (every check kept): `save_state_round_trips_cpu_and_memory`, `save_state_round_trips_the_tapes_own_playback_position`.
    #[test]
    fn save_state_cases() {
        save_state_round_trips_cpu_and_memory();
        save_state_round_trips_the_tapes_own_playback_position();
    }
}
