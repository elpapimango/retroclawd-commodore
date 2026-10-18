//! `vic_accuracy = cycle` against the real KERNAL/BASIC/character ROMs: the
//! machine must boot to READY. exactly as in line mode, with the VIC-II's BA
//! line stalling the CPU on every bad line. Needs `roms/c64/`; skipped when
//! absent.

use c64::{C64, VicAccuracy};
use machine::Machine;

const SCREEN: u16 = 0x0400;

fn rom(name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!(
        "{}/../../../roms/c64/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .ok()
}

fn machine(accuracy: VicAccuracy) -> Option<C64> {
    let mut c64 = C64::new(
        rom("kernal-901227-03.bin")?,
        rom("basic-901226-01.bin")?,
        rom("chargen-901225-01.bin")?,
    )
    .ok()?;
    c64.set_vic_accuracy(accuracy).unwrap();
    Some(c64)
}

fn run_cycles(c64: &mut C64, cycles: u64) {
    let mut spent = 0u64;
    while spent < cycles {
        spent += u64::from(c64.step());
    }
}

fn screen_has_ready(c64: &C64) -> bool {
    let ready = [0x12, 0x05, 0x01, 0x04, 0x19, 0x2E];
    (0..1000u16).any(|i| {
        ready
            .iter()
            .enumerate()
            .all(|(k, &b)| c64.read_memory((SCREEN + i + k as u16).into()) == b)
    })
}

#[test]
fn the_real_kernal_boots_to_ready_with_the_cycle_accurate_vic() {
    let Some(mut c64) = machine(VicAccuracy::Cycle) else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    run_cycles(&mut c64, 3_200_000);
    assert!(screen_has_ready(&c64), "cycle mode reached READY.");
    // The screen is up and the display enabled, so bad lines stalled the
    // CPU: 25 per frame, 43 cycles each, for dozens of frames.
    assert!(
        c64.ba_stalled_cycles() > 20 * 25 * 40,
        "BA stalled the CPU: {}",
        c64.ba_stalled_cycles()
    );
}

fn line_and_cycle_mode_boot_to_the_same_screen() {
    let (Some(mut line), Some(mut cycle)) =
        (machine(VicAccuracy::Line), machine(VicAccuracy::Cycle))
    else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    run_cycles(&mut line, 3_200_000);
    run_cycles(&mut cycle, 3_200_000);
    assert!(screen_has_ready(&line) && screen_has_ready(&cycle));
    let text = |m: &C64| {
        (0..1000u16)
            .map(|i| m.read_memory((SCREEN + i).into()))
            .collect::<Vec<_>>()
    };
    assert_eq!(text(&line), text(&cycle), "same boot screen in both modes");
}

fn the_cycle_picture_of_the_boot_screen_matches_the_decoded_one() {
    let (Some(mut line), Some(mut cycle)) =
        (machine(VicAccuracy::Line), machine(VicAccuracy::Cycle))
    else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    run_cycles(&mut line, 3_200_000);
    run_cycles(&mut cycle, 3_200_000);
    let (w1, h1, a) = line.render_frame().unwrap();
    let (w2, h2, b) = cycle.render_frame().unwrap();
    assert_eq!((w1, h1), (w2, h2));
    // The two machines stop at different points of the cursor's blink, so at
    // most the one cursor cell (8x8 pixels) may differ.
    let differing = a.chunks(3).zip(b.chunks(3)).filter(|(x, y)| x != y).count();
    assert!(differing <= 64, "{differing} pixels differ");
    // And the picture is really the blue-on-light-blue C64 boot screen.
    assert_ne!(a, vec![0; a.len()]);
}
/// Merged (every check kept): `line_and_cycle_mode_boot_to_the_same_screen`, `the_cycle_picture_of_the_boot_screen_matches_the_decoded_one`.
#[test]
fn cycle_boot_screens() {
    line_and_cycle_mode_boot_to_the_same_screen();
    the_cycle_picture_of_the_boot_screen_matches_the_decoded_one();
}
