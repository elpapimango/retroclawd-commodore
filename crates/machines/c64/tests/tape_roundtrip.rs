//! The standard cassette format against the real KERNAL: its own SAVE is
//! recorded and decoded, and an encoded tape is read back by its own LOAD
//! in real time. Needs `roms/c64/`; skipped when absent.

use c64::C64;
use c64_tape::rom_format::{decode_files, encode_program};
use machine::Machine;

fn rom(name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!(
        "{}/../../../roms/c64/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .ok()
}

fn booted() -> Option<C64> {
    let mut c64 = C64::new(
        rom("kernal-901227-03.bin")?,
        rom("basic-901226-01.bin")?,
        rom("chargen-901225-01.bin")?,
    )
    .ok()?;
    let mut spent = 0u64;
    while spent < 3_000_000 {
        spent += u64::from(c64.step());
    }
    Some(c64)
}

/// `LDA #a / LDX #x / LDY #y / JSR addr`.
fn call(a: u8, x: u8, y: u8, addr: u16) -> Vec<u8> {
    vec![
        0xA9,
        a,
        0xA2,
        x,
        0xA0,
        y,
        0x20,
        addr as u8,
        (addr >> 8) as u8,
    ]
}

/// Runs a routine at `$C000` until it reaches its final `JMP *`.
fn run_program(c64: &mut C64, mut code: Vec<u8>, max_cycles: u64) {
    let spin = 0xC000 + code.len() as u16;
    code.extend([0x4C, spin as u8, (spin >> 8) as u8]);
    c64.load(0xC000, &code);
    c64.set_pc(0xC000);
    let mut spent = 0u64;
    while spent < max_cycles && c64.pc() != spin.into() {
        spent += u64::from(c64.step());
    }
    assert_eq!(
        c64.pc(),
        spin.into(),
        "the routine did not finish in {max_cycles} cycles"
    );
}

#[test]
fn the_real_kernal_save_is_recorded_and_decodes_to_the_same_program() {
    let Some(mut c64) = booted() else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    c64.set_fast_tape_load(false); // the real routine, in real time
    c64.arm_tape_recorder();
    let program: Vec<u8> = (0..40u8).map(|i| i.wrapping_mul(5)).collect();
    c64.load(0x4000, &program);
    c64.load(0xC100, b"ROUNDTRIP");
    let mut code = call(9, 0x00, 0xC1, 0xFFBD); // SETNAM
    code.extend(call(1, 1, 1, 0xFFBA)); // SETLFS: file 1, device 1 (tape), SA 1
    code.extend([0xA9, 0x00, 0x85, 0xFB, 0xA9, 0x40, 0x85, 0xFC]); // start $4000 in $FB/$FC
    code.extend(call(0xFB, 0x28, 0x40, 0xFFD8)); // SAVE to $4028
    run_program(&mut c64, code, 60_000_000);

    let tape = c64.recorded_tape().expect("the SAVE recorded pulses");
    let files = decode_files(&tape);
    assert_eq!(files.len(), 1, "one program on the tape");
    let f = &files[0];
    assert_eq!(f.trimmed_name(), b"ROUNDTRIP");
    assert_eq!((f.kind, f.start, f.end), (3, 0x4000, 0x4028));
    assert_eq!(f.data, program);
}

#[test]
fn an_encoded_tape_loads_through_the_real_kernal_in_real_time() {
    let Some(mut c64) = booted() else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    c64.set_fast_tape_load(false);
    let program: Vec<u8> = (0..40u8)
        .map(|i| i.wrapping_mul(3).wrapping_add(1))
        .collect();
    c64.load_tape(c64_tape::Tape::from_pulses(encode_program(
        3,
        b"PLAYBACK",
        0x5000,
        &program,
    )));
    c64.load(0xC100, b"PLAYBACK");
    let mut code = call(8, 0x00, 0xC1, 0xFFBD);
    code.extend(call(1, 1, 1, 0xFFBA));
    code.extend(call(0, 0, 0, 0xFFD5)); // LOAD
    run_program(&mut c64, code, 80_000_000);

    for (i, &byte) in program.iter().enumerate() {
        assert_eq!(
            c64.read_memory((0x5000 + i as u16).into()),
            byte,
            "byte {i}"
        );
    }
}

#[test]
fn fast_tape_load_and_save_use_the_same_kernal_calls_and_finish_at_once() {
    let Some(mut c64) = booted() else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    // SAVE two programs to the tape (SA 0 = relocatable BASIC type, SA 1 =
    // absolute), instantly.
    let first: Vec<u8> = (0..30u8).collect();
    let second: Vec<u8> = (0..50u8).map(|i| 0xF0 ^ i).collect();
    c64.load(0x4000, &first);
    c64.load(0x6000, &second);
    c64.load(0xC100, b"FIRST");
    c64.load(0xC120, b"SECOND");
    let mut code = call(5, 0x00, 0xC1, 0xFFBD);
    code.extend(call(1, 1, 0, 0xFFBA));
    code.extend([0xA9, 0x00, 0x85, 0xFB, 0xA9, 0x40, 0x85, 0xFC]);
    code.extend(call(0xFB, 30, 0x40, 0xFFD8));
    code.extend(call(6, 0x20, 0xC1, 0xFFBD));
    code.extend(call(1, 1, 1, 0xFFBA));
    code.extend([0xA9, 0x00, 0x85, 0xFB, 0xA9, 0x60, 0x85, 0xFC]);
    code.extend(call(0xFB, 50, 0x60, 0xFFD8));
    run_program(&mut c64, code, 200_000);
    let tape = c64.recorded_tape().expect("both saves recorded");
    let files = decode_files(&tape);
    assert_eq!(files.len(), 2);
    assert_eq!((files[0].kind, files[1].kind), (1, 3));
    assert_eq!(files[0].data, first);
    assert_eq!(files[1].data, second);

    // A fresh machine plays that tape: a nameless LOAD takes the first
    // program, LOAD"SECOND" the next, both at once.
    let mut c64 = booted().unwrap();
    c64.load_tape(tape);
    let mut code = call(0, 0, 0, 0xFFBD); // SETNAM with an empty name
    code.extend(call(1, 1, 1, 0xFFBA)); // device 1, SA 1: use the header address
    code.extend(call(0, 0, 0, 0xFFD5));
    code.extend(call(6, 0x20, 0xC1, 0xFFBD));
    c64.load(0xC120, b"SECOND");
    code.extend(call(1, 1, 1, 0xFFBA));
    code.extend(call(0, 0, 0, 0xFFD5));
    run_program(&mut c64, code, 200_000);
    for (i, &b) in first.iter().enumerate() {
        assert_eq!(
            c64.read_memory((0x4000 + i as u16).into()),
            b,
            "first byte {i}"
        );
    }
    for (i, &b) in second.iter().enumerate() {
        assert_eq!(
            c64.read_memory((0x6000 + i as u16).into()),
            b,
            "second byte {i}"
        );
    }
}
