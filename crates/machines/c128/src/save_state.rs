//! Internal save state (`Machine::save_state`/`load_state`): bincode of every
//! piece of machine state, ROM images excluded (the same `--rom` files must
//! be supplied again, like every other machine here).

use crate::C128;

#[derive(serde::Serialize, serde::Deserialize)]
struct SaveState {
    cpu: mos6502::Cpu,
    z80: z80::Cpu,
    ram: Vec<u8>,
    color_ram: Vec<u8>,
    port_ddr: u8,
    port_data: u8,
    mmu: crate::Mmu,
    vic: vic_ii::Vic,
    cia1: cia6526::Cia,
    cia2: cia6526::Cia,
    sid: sid::Sid,
    keyboard: crate::keyboard::KeyMatrix,
    k_lines: u8,
    fast: bool,
    iec_clock_in: bool,
    iec_data_in: bool,
    cia2_nmi_active: bool,
    vdc: vdc8563::Vdc,
    clock_remainder: u32,
    cpu_started: bool,
    audio_cycle_accumulator: f64,
}

pub(crate) fn encode(m: &C128) -> Vec<u8> {
    let b = &m.bus;
    let state = SaveState {
        cpu: m.cpu,
        z80: m.z80,
        ram: b.ram.clone(),
        color_ram: b.color_ram.clone(),
        port_ddr: b.port_ddr,
        port_data: b.port_data,
        mmu: b.mmu.clone(),
        vic: b.vic.clone(),
        cia1: b.cia1.clone(),
        cia2: b.cia2.clone(),
        sid: b.sid.clone(),
        keyboard: b.keyboard.clone(),
        k_lines: b.k_lines,
        fast: b.fast,
        iec_clock_in: b.iec_clock_in,
        iec_data_in: b.iec_data_in,
        cia2_nmi_active: b.cia2_nmi_active,
        vdc: b.vdc.clone(),
        clock_remainder: m.clock_remainder,
        cpu_started: m.cpu_started,
        audio_cycle_accumulator: m.audio_cycle_accumulator,
    };
    bincode::serde::encode_to_vec(&state, bincode::config::standard())
        .expect("C128 save state should always be encodable")
}

pub(crate) fn decode(m: &mut C128, bytes: &[u8]) -> Result<(), String> {
    let (s, _): (SaveState, usize) =
        bincode::serde::decode_from_slice(bytes, bincode::config::standard())
            .map_err(|e| format!("failed to decode C128 save state: {e}"))?;
    if s.ram.len() != m.bus.ram.len() || s.color_ram.len() != m.bus.color_ram.len() {
        return Err("save state RAM size mismatch".to_string());
    }
    if s.vic.cycle_engine_enabled() != m.bus.vic.cycle_engine_enabled() {
        return Err("save state was made with a different vic_accuracy".to_string());
    }
    m.cpu = s.cpu;
    m.z80 = s.z80;
    let b = &mut m.bus;
    b.ram = s.ram;
    b.color_ram = s.color_ram;
    b.port_ddr = s.port_ddr;
    b.port_data = s.port_data;
    b.mmu = s.mmu;
    b.vic = s.vic;
    b.cia1 = s.cia1;
    b.cia2 = s.cia2;
    b.sid = s.sid;
    b.keyboard = s.keyboard;
    b.k_lines = s.k_lines;
    b.fast = s.fast;
    b.iec_clock_in = s.iec_clock_in;
    b.iec_data_in = s.iec_data_in;
    b.cia2_nmi_active = s.cia2_nmi_active;
    b.vdc = s.vdc;
    b.sync_vic();
    m.clock_remainder = s.clock_remainder;
    m.cpu_started = s.cpu_started;
    m.audio_cycle_accumulator = s.audio_cycle_accumulator;
    m.audio_buffer.clear();
    Ok(())
}
