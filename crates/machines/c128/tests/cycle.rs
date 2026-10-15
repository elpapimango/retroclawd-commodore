//! `vic_accuracy = cycle` against the real ROMs (`roms/c128/`; skipped when a
//! ROM is absent, `--ignored` + `--release` to run).

use c128::{C128, C128Key, Roms, VicAccuracy};
use machine::Machine;

fn rom(name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!(
        "{}/../../../roms/c128/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .ok()
}

fn machine(accuracy: VicAccuracy) -> Option<C128> {
    let mut basic = rom("basiclo-318018-04.bin")?;
    basic.extend(rom("basichi-318019-04.bin")?);
    let roms = Roms {
        basic,
        kernal: rom("kernal-318020-05.bin")?,
        chargen: rom("chargen-390059-01.bin")?,
        c64_basic: rom("basic64-901226-01.bin")?,
        c64_kernal: rom("kernal64-901227-03.bin")?,
    };
    let mut m = C128::new(roms).unwrap();
    m.set_vic_accuracy(accuracy).unwrap();
    Some(m)
}

fn screen_text(m: &C128) -> String {
    (0..25)
        .map(|row| {
            (0..40)
                .map(|col| {
                    let code = m.read_memory(0x0400 + row * 40 + col);
                    match code & 0x7F {
                        c @ 0x01..=0x1A => (b'A' + c - 1) as char,
                        c @ 0x20..=0x3F => c as char,
                        _ => ' ',
                    }
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn run(m: &mut C128, cycles: u64) {
    let mut total = 0u64;
    while total < cycles {
        total += u64::from(m.step());
    }
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn the_real_c128_boots_to_basic_in_cycle_mode_with_the_same_screen_as_line_mode() {
    let Some(mut cycle) = machine(VicAccuracy::Cycle) else {
        eprintln!("skipped: ROMs missing");
        return;
    };
    let mut line = machine(VicAccuracy::Line).unwrap();
    run(&mut cycle, 9_000_000);
    run(&mut line, 9_000_000);
    let text = screen_text(&cycle);
    assert!(
        text.contains("COMMODORE BASIC V7.0 122365 BYTES FREE"),
        "{text}"
    );
    assert!(text.contains("READY."), "{text}");
    assert_eq!(text, screen_text(&line));
    assert!(
        cycle.ba_stalled_cycles() > 0,
        "the display steals bus cycles from the 8502"
    );
    assert_eq!(line.ba_stalled_cycles(), 0);
    // The engine's own picture (border included) is what is shown.
    let (w, h, _) = cycle.render_frame().unwrap();
    assert_eq!((w, h), (384, 264));
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn typed_keys_and_go64_work_in_cycle_mode() {
    use C128Key::*;
    let Some(mut m) = machine(VicAccuracy::Cycle) else {
        return;
    };
    run(&mut m, 9_000_000);
    // GO64, Return, answer the "ARE YOU SURE?" with Y + Return.
    for key in [G, O, Digit6, Digit4, Return, Y, Return] {
        m.press_key(key);
        run(&mut m, 60_000);
        m.release_key(key);
        run(&mut m, 60_000);
    }
    run(&mut m, 6_000_000);
    assert!(m.mmu().c64_mode(), "GO64 switched to C64 mode");
    let text = screen_text(&m);
    assert!(
        text.contains("64K RAM SYSTEM  38911 BASIC BYTES FREE"),
        "{text}"
    );
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn the_commodore_key_boots_c64_mode_in_cycle_mode() {
    let Some(mut m) = machine(VicAccuracy::Cycle) else {
        return;
    };
    m.boot_into_c64_mode();
    run(&mut m, 9_000_000);
    assert!(m.mmu().c64_mode());
    let text = screen_text(&m);
    assert!(text.contains("38911 BASIC BYTES FREE"), "{text}");
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn a_cycle_mode_save_state_resumes_exactly_on_the_real_roms() {
    let Some(mut a) = machine(VicAccuracy::Cycle) else {
        return;
    };
    run(&mut a, 9_000_000);
    let saved = a.save_state();
    let mut b = machine(VicAccuracy::Cycle).unwrap();
    b.load_state(&saved).unwrap();
    run(&mut a, 1_000_000);
    run(&mut b, 1_000_000);
    assert_eq!(a.save_state(), b.save_state());
    assert_eq!(a.render_frame(), b.render_frame());
}
