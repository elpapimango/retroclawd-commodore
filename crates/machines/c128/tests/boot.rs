//! The real C128 ROMs (`roms/c128/`) booting. Skipped when a ROM is absent.

use c128::{C128, Roms};
use machine::Machine;

fn rom(name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!(
        "{}/../../../roms/c128/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .ok()
}

fn machine() -> Option<C128> {
    let mut basic = rom("basiclo-318018-04.bin")?;
    basic.extend(rom("basichi-318019-04.bin")?);
    let roms = Roms {
        basic,
        kernal: rom("kernal-318020-05.bin")?,
        chargen: rom("chargen-390059-01.bin")?,
        c64_basic: rom("basic64-901226-01.bin")?,
        c64_kernal: rom("kernal64-901227-03.bin")?,
    };
    Some(C128::new(roms).unwrap())
}

/// The 40-column screen as text (screen RAM at $0400 of bank 0).
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

/// Taps a key for roughly two jiffies and lets the machine settle.
fn tap(m: &mut C128, key: c128::C128Key) {
    m.press_key(key);
    run(m, 60_000);
    m.release_key(key);
    run(m, 60_000);
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn boots_to_the_basic_7_prompt_after_the_z80_hands_over() {
    let Some(mut m) = machine() else {
        eprintln!("skipped: ROMs missing");
        return;
    };
    assert_eq!(m.cpu_name(), "Z80", "the Z80 runs first");
    let mut total = 0u64;
    while total < 400_000 && m.cpu_name() == "Z80" {
        total += u64::from(m.step());
    }
    assert_eq!(m.cpu_name(), "8502", "the BIOS switched to the 8502");
    assert!(!m.mmu().c64_mode(), "C128 mode");
    run(&mut m, 8_000_000);
    let text = screen_text(&m);
    assert!(
        text.contains("COMMODORE BASIC V7.0 122365 BYTES FREE"),
        "{text}"
    );
    assert!(text.contains("READY."), "{text}");
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn typed_keys_reach_basic_through_the_matrix() {
    use c128::C128Key::*;
    let Some(mut m) = machine() else { return };
    run(&mut m, 8_000_000);
    for key in [P, Digit1, Plus, Digit2, Return] {
        tap(&mut m, key);
    }
    run(&mut m, 500_000);
    // PRINT-less: `P1+2` is a syntax error; check the echo and the error.
    let text = screen_text(&m);
    assert!(text.contains("P1+2"), "{text}");
    assert!(text.contains("SYNTAX ERROR"), "{text}");
}

#[test]
#[ignore = "writes a picture when OUT is set"]
fn dump_picture() {
    let Some(mut m) = machine() else { return };
    run(&mut m, 8_000_000);
    let Ok(path) = std::env::var("OUT") else {
        return;
    };
    let (w, h, rgb) = m.render_frame().unwrap();
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    ppm.extend(rgb);
    std::fs::write(path, ppm).unwrap();
}

/// The 80-column screen as text, read straight out of the VDC's RAM.
fn vdc_text(m: &C128) -> String {
    let ram = m.vdc_ram();
    let start = usize::from(m.vdc_register(12)) << 8 | usize::from(m.vdc_register(13));
    (0..25)
        .map(|row| {
            (0..80)
                .map(|col| {
                    let code = ram[(start + row * 80 + col) & 0xFFFF];
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

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn boots_to_the_80_column_prompt_on_the_vdc() {
    let Some(mut m) = machine() else { return };
    m.set_80_columns(true);
    run(&mut m, 12_000_000);
    let text = vdc_text(&m);
    assert!(
        text.contains("COMMODORE BASIC V7.0 122365 BYTES FREE"),
        "{text}"
    );
    assert!(text.contains("READY."), "{text}");
    if let Ok(path) = std::env::var("OUT80") {
        let (w, h, rgb) = m.render_frame().unwrap();
        let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
        ppm.extend(rgb);
        std::fs::write(path, ppm).unwrap();
    }
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn go64_switches_to_the_c64_mode_basic() {
    use c128::C128Key::*;
    let Some(mut m) = machine() else { return };
    run(&mut m, 8_000_000);
    for key in [G, O, Digit6, Digit4, Return, Y, Return] {
        tap(&mut m, key);
    }
    run(&mut m, 6_000_000);
    assert!(m.mmu().c64_mode(), "the MMU is in C64 mode");
    let text = screen_text(&m);
    assert!(text.contains("COMMODORE 64 BASIC V2"), "{text}");
    assert!(
        text.contains("64K RAM SYSTEM  38911 BASIC BYTES FREE"),
        "{text}"
    );
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn a_save_state_resumes_exactly() {
    use machine::Machine;
    let Some(mut m) = machine() else { return };
    run(&mut m, 8_000_000);
    let state = m.save_state();
    let mut other = machine().unwrap();
    other.load_state(&state).unwrap();
    run(&mut m, 500_000);
    run(&mut other, 500_000);
    assert_eq!(screen_text(&m), screen_text(&other));
    assert_eq!(m.pc(), other.pc());
    assert_eq!(m.registers(), other.registers());
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn holding_the_commodore_key_at_power_on_boots_the_c64() {
    let Some(mut m) = machine() else { return };
    m.boot_into_c64_mode();
    run(&mut m, 8_000_000);
    assert!(m.mmu().c64_mode());
    let text = screen_text(&m);
    assert!(text.contains("COMMODORE 64 BASIC V2"), "{text}");
    assert!(text.contains("READY."), "{text}");
}

/// What the machine plugin does for `--run --program`: BASIC's cold start
/// wipes the head of program memory, so a program poked before the boot is
/// lost; poked at the prompt with `$1210` (BASIC 7's end-of-text pointer)
/// set it lists and runs.
#[test]
#[ignore = "runs the real ROMs; use --release"]
fn a_basic_program_poked_at_the_prompt_runs() {
    use c128::C128Key::*;
    let Some(mut m) = machine() else { return };
    run(&mut m, 8_000_000);
    // 10 PRINT"HI" -- link to $1C0E, line 10, PRINT token $99, quote, HI, quote, 0, then 0 0.
    let program = [
        0x0D, 0x1C, 0x0A, 0x00, 0x99, 0x22, 0x48, 0x49, 0x22, 0x00, 0x00, 0x00,
    ];
    m.load(0x1C01, &program);
    m.load(0x1210, &0x1C0Du16.to_le_bytes());
    for key in [R, U, N, Return] {
        tap(&mut m, key);
    }
    run(&mut m, 500_000);
    let text = screen_text(&m);
    assert!(text.lines().any(|l| l.trim() == "HI"), "{text}");
}
