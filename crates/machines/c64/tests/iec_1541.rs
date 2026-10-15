//! Proves the real IEC bus wiring between `c64::C64` and `c1541::C1541`
//! actually works end to end -- a genuine integration test needing both
//! crates, kept as a Cargo `tests/` file (not a unit test inside either
//! crate) specifically so neither `c64` nor `c1541` needs a real,
//! production dependency on the other; `c1541` is a `[dev-dependencies]`
//! entry of this crate only (see `Cargo.toml`).
//!
//! Every DDR value poked below is the *real* hardware configuration real
//! KERNAL/DOS firmware actually uses (`$3F` for CIA2 DDRA, `$1A` for VIA1
//! DDRB -- both confirmed against VICE's own real source this session,
//! see `CREDITS.md`), not an arbitrary test fixture, so a passing test
//! here is evidence the wiring would also work against real ROMs.

use machine::Machine;

const CIA2_DDRA_REAL: u8 = 0x3F; // bits 0-5 output (VIC bank, RS232, ATN/CLOCK/DATA OUT), 6-7 input
const VIA1_DDRB_REAL: u8 = 0x1A; // bits 1/3/4 output (DATA/CLOCK OUT, ATN ACK), rest input

fn synth_c64() -> c64::C64 {
    c64::C64::new(vec![0xEA; 0x2000], vec![0xEA; 0x2000], vec![0; 0x1000]).unwrap()
}

fn synth_1541() -> c1541::C1541 {
    c1541::C1541::new(vec![0xEA; c1541::ROM_SIZE], 8).unwrap()
}

/// Copies each side's current output onto the shared bus, then feeds the
/// bus's newly computed levels back into each side's own input state --
/// the same propagation loop a future `retroclawd` dual-machine stepping
/// loop will need to run continuously once `.d64` support exists (see
/// `TODO.md`); written out explicitly here since nothing wires it up
/// automatically yet.
fn propagate(bus: &mut c1541::IecBus, c64: &mut c64::C64, drive: &mut c1541::C1541) {
    let (atn, clock, data) = c64.iec_output();
    bus.set_computer_outputs(atn, clock, data);
    let (drive_clock, drive_data) = drive.iec_output();
    bus.set_drive_outputs(drive_clock, drive_data);
    c64.set_iec_input(bus.clock(), bus.data());
    drive.set_iec_input(bus.atn(), bus.clock(), bus.data());
}

#[test]
fn idle_bus_reads_released_high_on_both_sides() {
    let mut c64 = synth_c64();
    let mut drive = synth_1541();
    let mut bus = c1541::IecBus::new();

    propagate(&mut bus, &mut c64, &mut drive);

    assert!(bus.atn() && bus.clock() && bus.data());
    // The drive's 7406 receivers invert: idle-high lines read 0.
    assert_eq!(drive.read_memory(0x1800) & 0x85, 0);
}

#[test]
fn c64_asserting_atn_reaches_the_drive_as_a_real_edge_and_a_port_b_level() {
    let mut c64 = synth_c64();
    let mut drive = synth_1541();
    let mut bus = c1541::IecBus::new();

    c64.load(0xDD02, &[CIA2_DDRA_REAL]);
    drive.load(0x1802, &[VIA1_DDRB_REAL]);
    drive.load(0x180E, &[0x80 | 0x02]); // enable VIA1's CA1 (ATN) interrupt
    drive.load(0x180C, &[0x01]); // PCR as the real DOS sets it: positive edge
    propagate(&mut bus, &mut c64, &mut drive);

    c64.load(0xDD00, &[0x08]); // ATN OUT asserted (bit 3)
    propagate(&mut bus, &mut c64, &mut drive);

    assert!(!bus.atn(), "ATN line should now read asserted (low)");
    assert_eq!(
        drive.read_memory(0x1800) & 0x80,
        0x80,
        "VIA1 Port B bit 7 (ATN IN) reads 1 while ATN is asserted"
    );
    assert_ne!(
        drive.read_memory(0x180D) & 0x02, // IFR bit 1 = CA1
        0,
        "the ATN edge should have set VIA1's own CA1 interrupt flag"
    );

    c64.load(0xDD00, &[0x00]); // release ATN
    propagate(&mut bus, &mut c64, &mut drive);
    assert!(
        bus.atn(),
        "releasing ATN should let the line float back high"
    );
}

#[test]
fn drive_asserting_data_out_is_seen_by_the_c64_as_data_in_wired_and() {
    let mut c64 = synth_c64();
    let mut drive = synth_1541();
    let mut bus = c1541::IecBus::new();

    c64.load(0xDD02, &[CIA2_DDRA_REAL]);
    drive.load(0x1802, &[VIA1_DDRB_REAL]);
    propagate(&mut bus, &mut c64, &mut drive);

    drive.load(0x1800, &[0x02]); // DATA OUT asserted (bit 1)
    propagate(&mut bus, &mut c64, &mut drive);

    assert!(!bus.data(), "DATA line should read asserted (low)");
    assert_eq!(
        c64.read_memory(0xDD00) & 0x80,
        0,
        "CIA2 Port A bit 7 (DATA IN) should reflect the drive pulling DATA low"
    );

    drive.load(0x1800, &[0x00]); // release DATA OUT
    propagate(&mut bus, &mut c64, &mut drive);
    assert_ne!(
        c64.read_memory(0xDD00) & 0x80,
        0,
        "releasing DATA OUT should let the line float back high"
    );
}

#[test]
fn both_sides_pulling_the_same_line_wire_and_correctly() {
    let mut c64 = synth_c64();
    let mut drive = synth_1541();
    let mut bus = c1541::IecBus::new();

    c64.load(0xDD02, &[CIA2_DDRA_REAL]);
    drive.load(0x1802, &[VIA1_DDRB_REAL]);

    c64.load(0xDD00, &[0x10]); // CLOCK OUT asserted (bit 4)
    drive.load(0x1800, &[0x08]); // CLOCK OUT asserted (bit 3) on the drive side too
    propagate(&mut bus, &mut c64, &mut drive);
    assert!(
        !bus.clock(),
        "either side alone should already pull the line low"
    );

    c64.load(0xDD00, &[0x00]); // release the C64 side; drive still pulling
    propagate(&mut bus, &mut c64, &mut drive);
    assert!(
        !bus.clock(),
        "still asserted -- the drive alone still holds it low"
    );

    drive.load(0x1800, &[0x00]); // release the drive side too
    propagate(&mut bus, &mut c64, &mut drive);
    assert!(
        bus.clock(),
        "released by both sides -- line floats back high"
    );
}
