//! A Shift+key chord that the host turns into "release Shift, press the key"
//! must not be applied as one atomic step to the emulated keyboard matrix.
//!
//! The KERNAL scans the matrix column by column inside its IRQ, and the emu
//! thread applies input between ticks -- which can land in the middle of a
//! scan. If the Shift column was already read as down and the key's column
//! is read as down after the switch, the key is decoded with the stale Shift
//! (`*` came out as the shifted `$C0` once in a while). Releasing Shift one
//! whole tick before pressing the key leaves no scan that can see both.
//!
//! Needs the real KERNAL/BASIC/character ROMs under `roms/c64/` (committed in
//! this repo); skipped when absent.

use c64::{C64, C64Key};
use machine::Machine;

const TICK_CYCLES: u64 = 19_705;
/// Screen RAM position the first typed character lands on after `READY.`
/// (row 6 of the 40-column screen); a typed `*` is screen code `$2A`, the
/// shifted one `$40`.
const FIRST_TYPED_CELL: u16 = 0x0400 + 6 * 40;

fn rom(name: &str) -> Option<Vec<u8>> {
    let path = format!("{}/../../../roms/c64/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(path).ok()
}

fn run(c64: &mut C64, cycles: u64) {
    let mut spent = 0;
    while spent < cycles {
        spent += u64::from(c64.step());
    }
}

/// A booted machine's saved state plus the ROMs needed to rebuild it.
struct Booted {
    state: Vec<u8>,
    kernal: Vec<u8>,
    basic: Vec<u8>,
    chargen: Vec<u8>,
}

/// Boots to the BASIC prompt and returns the machine's saved state.
fn booted_state() -> Option<Booted> {
    let kernal = rom("kernal-901227-01.bin")?;
    let basic = rom("basic-901226-01.bin")?;
    let chargen = rom("chargen-901225-01.bin")?;
    let mut c64 = C64::new(kernal.clone(), basic.clone(), chargen.clone()).ok()?;
    run(&mut c64, 3_000_000);
    Some(Booted {
        state: c64.save_state(),
        kernal,
        basic,
        chargen,
    })
}

/// Types Shift+`key` the way the host does and returns the screen code the
/// KERNAL printed for it. `deferral` is how long after the Shift
/// release the key is pressed (0 = the same instant).
fn type_chord(booted: &Booted, phase: u64, deferral: u64) -> u8 {
    let mut c64 = C64::new(
        booted.kernal.clone(),
        booted.basic.clone(),
        booted.chargen.clone(),
    )
    .unwrap();
    c64.load_state(&booted.state).unwrap();
    run(&mut c64, phase);
    c64.press_key(C64Key::LeftShift);
    run(&mut c64, 3 * TICK_CYCLES);
    c64.release_key(C64Key::LeftShift);
    run(&mut c64, deferral);
    c64.press_key(C64Key::Asterisk);
    run(&mut c64, 3 * TICK_CYCLES);
    let typed = c64.read_memory(FIRST_TYPED_CELL);
    c64.release_key(C64Key::Asterisk);
    typed
}

#[test]
fn releasing_shift_a_tick_before_the_key_never_decodes_a_stale_shift() {
    let Some(booted) = booted_state() else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    // Sweep the switch across a whole interrupt period (16 421 cycles on PAL)
    // so one of the offsets lands inside the KERNAL's matrix scan.
    let phases: Vec<u64> = (0..20_000).step_by(23).collect();
    let atomic_wrong = phases
        .iter()
        .filter(|&&phase| type_chord(&booted, phase, 0) != 0x2A)
        .count();
    let deferred_wrong: Vec<u64> = phases
        .iter()
        .copied()
        .filter(|&phase| type_chord(&booted, phase, TICK_CYCLES) != 0x2A)
        .collect();
    eprintln!(
        "{} offsets: atomic switch wrong at {atomic_wrong}, deferred wrong at {}",
        phases.len(),
        deferred_wrong.len()
    );
    assert!(
        deferred_wrong.is_empty(),
        "wrong at offsets {deferred_wrong:?}"
    );
    // The race is real: at least one offset breaks the atomic switch. If this
    // ever stops failing the deferral in emu_thread is no longer needed.
    assert!(atomic_wrong > 0, "the atomic switch never misdecoded");
}
