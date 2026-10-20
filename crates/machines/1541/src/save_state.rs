//! Internal save-state encode/decode for `Machine::save_state`/
//! `load_state` -- same conventions this project's other machines already
//! established (see `CLAUDE.md`'s `c64` notes): bincode v2's serde compat
//! feature, ROM contents deliberately excluded (restoring requires the
//! same ROM bytes already supplied at construction, exactly like every
//! other machine here). `mos6502::Cpu`/`via6522::Via` both get a direct
//! serde derive straight onto the real internal type -- neither holds or
//! is generic over externally-supplied media, same "no mirror-struct
//! needed" tier `c64::save_state` already established for its own
//! `mos6502::Cpu`/`vic_ii::Vic`/`crate::cia::Cia`/`sid::Sid`.
//!
//! `device_jumper_bits` is deliberately excluded -- it's a real hardware
//! fact (physical jumper position), fixed at construction via `C1541::
//! new`'s own `device_number` parameter, not live session state -- same
//! "construction-time configuration stays external to the save blob"
//! stance every other machine's own `--rom` already takes. `decode`
//! re-derives VIA1's external Port B state from the *current* instance's
//! jumper bits after restoring the IEC input latches, so restoring a save
//! state onto a drive constructed with a different `device_number` can
//! never silently resurrect the old device address.
//!
//! `crate::disk::DiskMechanism` holds an `Option<d64::D64>` -- externally
//! supplied media, same shape `nec765::Fdc<D>`'s `disk: Option<D>` and
//! `zx_tape::TapePlayer`'s own `tape: Tape` already have -- so it gets the
//! same small mirror-struct treatment those use (`DiskMechanismState` via
//! `dump_state`/`restore_state`) rather than a direct derive: the attached
//! disk image's own bytes stay external, only the mechanism's own live
//! mechanical/timing state (head position, motor, density, bit-clock
//! phase) round-trips.

use crate::C1541;

#[derive(serde::Serialize, serde::Deserialize)]
struct CompleteState {
    magic: [u8; 4],
    machine: Vec<u8>,
    disk: crate::disk::DiskMechanism,
}

impl C1541 {
    /// Includes the mounted medium, dirty GCR track, write shifter and rotation cache.
    /// The legacy Machine state format remains unchanged.
    pub fn save_complete_state(&self) -> Vec<u8> {
        bincode::serde::encode_to_vec(
            CompleteState {
                magic: *b"D154",
                machine: encode(self),
                disk: self.bus.disk.clone(),
            },
            bincode::config::standard(),
        )
        .expect("complete 1541 state is encodable")
    }

    pub fn load_complete_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        let (state, used): (CompleteState, usize) = bincode::serde::decode_from_slice(
            bytes,
            bincode::config::standard().with_limit::<2_097_152>(),
        )
        .map_err(|error| format!("invalid complete 1541 state: {error}"))?;
        if state.magic != *b"D154" || used != bytes.len() {
            return Err("invalid complete 1541 state header or trailing bytes".to_string());
        }
        state.disk.validate_complete_state()?;
        let (machine, used): (SaveState, usize) = bincode::serde::decode_from_slice(
            &state.machine,
            bincode::config::standard().with_limit::<2_097_152>(),
        )
        .map_err(|error| format!("invalid complete 1541 CPU state: {error}"))?;
        if used != state.machine.len() {
            return Err("invalid complete 1541 CPU state trailing bytes".to_string());
        }
        apply(self, machine)?;
        self.bus.disk = state.disk;
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SaveState {
    cpu: mos6502::Cpu,
    ram: Vec<u8>,
    via1: via6522::Via,
    via2: via6522::Via,
    iec_atn_in: bool,
    iec_clock_in: bool,
    iec_data_in: bool,
    disk: crate::disk::DiskMechanismState,
    model: crate::DriveModel,
    cia: cia6526::Cia,
    byte_ready_latch: bool,
    fast_serial_out: bool,
}

pub(crate) fn encode(c1541: &C1541) -> Vec<u8> {
    let state = SaveState {
        cpu: c1541.cpu,
        ram: c1541.bus.ram.clone(),
        via1: c1541.bus.via1.clone(),
        via2: c1541.bus.via2.clone(),
        iec_atn_in: c1541.bus.iec_atn_in,
        iec_clock_in: c1541.bus.iec_clock_in,
        iec_data_in: c1541.bus.iec_data_in,
        disk: c1541.bus.disk.dump_state(),
        model: c1541.bus.model,
        cia: c1541.bus.cia.clone(),
        byte_ready_latch: c1541.bus.byte_ready_latch,
        fast_serial_out: c1541.bus.fast_serial_out,
    };
    bincode::serde::encode_to_vec(&state, bincode::config::standard())
        .expect("1541 save state should always be encodable")
}

pub(crate) fn decode(c1541: &mut C1541, bytes: &[u8]) -> Result<(), String> {
    let (state, _): (SaveState, usize) =
        bincode::serde::decode_from_slice(bytes, bincode::config::standard())
            .map_err(|e| format!("failed to decode 1541 save state: {e}"))?;
    apply(c1541, state)
}

fn apply(c1541: &mut C1541, state: SaveState) -> Result<(), String> {
    if state.model != c1541.bus.model {
        return Err(format!(
            "the state was saved from a {}, this drive is a {}",
            state.model.name(),
            c1541.bus.model.name()
        ));
    }
    if state.ram.len() != c1541.bus.ram.len() {
        return Err(format!(
            "save state RAM size mismatch: expected {}, got {}",
            c1541.bus.ram.len(),
            state.ram.len()
        ));
    }

    c1541.cpu = state.cpu;
    c1541.bus.ram = state.ram;
    c1541.bus.via1 = state.via1;
    c1541.bus.via2 = state.via2;
    c1541.bus.iec_atn_in = state.iec_atn_in;
    c1541.bus.iec_clock_in = state.iec_clock_in;
    c1541.bus.iec_data_in = state.iec_data_in;
    c1541.bus.cia = state.cia;
    c1541.bus.byte_ready_latch = state.byte_ready_latch;
    c1541.bus.fast_serial_out = state.fast_serial_out;
    c1541.bus.disk.restore_state(state.disk);
    // Re-derive VIA1's external Port B state from *this* instance's own
    // device jumper bits (never overwritten by the restore above) plus
    // the just-restored IEC latch booleans -- see this module's own doc
    // comment for why.
    c1541.bus.apply_external_port_b();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use machine::Machine;

    fn save_state_round_trips_cpu_ram_and_via_state() {
        let mut drive = C1541::new(vec![0xEA; crate::ROM_SIZE], 8).unwrap();
        drive.load(0x0400, &[0x42]);
        drive.load(0x1802, &[0x1A]); // VIA1 DDRB
        drive.load(0x1800, &[0x08]); // CLOCK OUT asserted
        drive.set_pc(0x1234);

        let saved = drive.save_state();

        let mut restored = C1541::new(vec![0xEA; crate::ROM_SIZE], 8).unwrap();
        restored.load_state(&saved).unwrap();

        assert_eq!(restored.pc(), 0x1234);
        assert_eq!(restored.read_memory(0x0400), 0x42);
        assert_eq!(restored.iec_output(), (true, false));
    }

    #[test]
    fn restoring_onto_a_different_device_number_uses_the_new_jumpers_not_the_saved_ones() {
        let drive8 = C1541::new(vec![0xEA; crate::ROM_SIZE], 8).unwrap();
        let saved = drive8.save_state();

        let mut drive9 = C1541::new(vec![0xEA; crate::ROM_SIZE], 9).unwrap();
        drive9.load_state(&saved).unwrap();
        assert_eq!(
            drive9.read_memory(0x1800) & 0x60,
            0x20,
            "device number is construction-time hardware config, not restorable session state"
        );
    }

    fn save_state_round_trips_the_disk_mechanisms_head_position_and_motor_state() {
        let disk = d64::D64::parse(&vec![0u8; 683 * d64::SECTOR_SIZE]).unwrap();
        let mut drive = C1541::new(vec![0xEA; crate::ROM_SIZE], 8).unwrap();
        drive.attach_disk(disk);
        drive.load(0x1C02, &[0xFF]); // VIA2 DDRB: all output
        drive.load(0x1C00, &[0b0000_0101]); // MTR on, STP1:STP0 = 01
        drive.load(0x1C00, &[0b0000_0110]); // STP1:STP0 = 01 -> 10: a real step

        let saved = drive.save_state();

        // A fresh instance, its own disk re-attached (media stays external
        // to the save blob, same as `--rom`/`--tape`/`--disk` everywhere
        // else in this project) -- only the mechanism's own live
        // mechanical state should come from the save.
        let mut restored = C1541::new(vec![0xEA; crate::ROM_SIZE], 8).unwrap();
        restored.attach_disk(d64::D64::parse(&vec![0u8; 683 * d64::SECTOR_SIZE]).unwrap());
        restored.load_state(&saved).unwrap();

        assert_eq!(
            restored.current_track(),
            2,
            "the real single-step head move must survive the round trip"
        );
    }
    /// Merged (every check kept): `save_state_round_trips_cpu_ram_and_via_state`, `save_state_round_trips_the_disk_mechanisms_head_position_and_motor_state`.
    #[test]
    fn save_state_cases() {
        save_state_round_trips_cpu_ram_and_via_state();
        save_state_round_trips_the_disk_mechanisms_head_position_and_motor_state();
    }
}
