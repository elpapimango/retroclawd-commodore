//! Exact-timing tests for `vic_accuracy = cycle` on the C128, with synthetic
//! ROMs: how many cycles the 8502 loses to the VIC-II's BA line, that the Z80
//! and the 2 MHz mode keep the raster in lockstep without being stalled, and
//! that save states and NTSC behave. Same hand-assembled style as the C64's
//! `cycle_tests`; the real-ROM checks live in `tests/cycle.rs`.

use crate::bus::{BASIC_ROM_SIZE, C64_ROM_SIZE, CHARGEN_SIZE, KERNAL_IMAGE_SIZE, Roms};
use crate::{C128, VicAccuracy};
use machine::{Bus, Machine};

const FRAME_CYCLES: u64 = 63 * 312;

fn roms() -> Roms {
    Roms {
        basic: vec![0; BASIC_ROM_SIZE],
        kernal: vec![0; KERNAL_IMAGE_SIZE],
        chargen: vec![0; CHARGEN_SIZE],
        c64_basic: vec![0; C64_ROM_SIZE],
        c64_kernal: vec![0; C64_ROM_SIZE],
    }
}

/// A machine in cycle mode with the 8502 in charge (MCR bit 0), as if the
/// BIOS had already handed over.
fn cycle_machine() -> C128 {
    let mut m = C128::new(roms()).unwrap();
    m.set_vic_accuracy(VicAccuracy::Cycle).unwrap();
    m.bus.mmu.write(5, 0x01);
    m
}

/// Program at `$0200`: set up the VIC from `init`, then spin in
/// `NOP / NOP / JMP loop` -- every cycle of which is a read.
fn spin_program(m: &mut C128, init: &[u8]) {
    let mut code = init.to_vec();
    let loop_at = 0x0200 + code.len() as u16;
    code.extend_from_slice(&[0xEA, 0xEA, 0x4C, loop_at as u8, (loop_at >> 8) as u8]);
    m.load(0x0200, &code);
    m.set_pc(0x0200);
}

/// The raster line from `$D012` / `$D011` bit 7.
fn raster_line(m: &C128) -> u32 {
    u32::from(m.bus.vic.read(0x12)) | ((u32::from(m.bus.vic.read(0x11)) & 0x80) << 1)
}

fn run(m: &mut C128, cycles: u64) -> u64 {
    let mut spent = 0u64;
    while spent < cycles {
        spent += u64::from(m.step());
    }
    spent
}

#[test]
fn every_bad_line_stalls_a_read_only_program_for_exactly_43_cycles() {
    let mut m = cycle_machine();
    // DEN on (the latch needs it during line $30), YSCROLL 3.
    spin_program(&mut m, &[0xA9, 0x1B, 0x8D, 0x11, 0xD0]);
    run(&mut m, FRAME_CYCLES);
    assert_eq!(m.ba_stalled_cycles(), 25 * 43);
}

#[test]
fn with_the_display_off_the_cpu_never_waits() {
    let mut m = cycle_machine();
    spin_program(&mut m, &[0xA9, 0x0B, 0x8D, 0x11, 0xD0]); // DEN clear
    run(&mut m, FRAME_CYCLES);
    assert_eq!(m.ba_stalled_cycles(), 0);
}

#[test]
fn a_lone_sprite_costs_five_cycles_a_line_for_21_lines() {
    let mut m = cycle_machine();
    spin_program(
        &mut m,
        &[0xA9, 100, 0x8D, 0x01, 0xD0, 0xA9, 0x01, 0x8D, 0x15, 0xD0],
    );
    run(&mut m, FRAME_CYCLES);
    assert_eq!(m.ba_stalled_cycles(), 21 * 5);
}

#[test]
fn the_raster_stays_in_lockstep_with_the_cycles_step_reports() {
    let mut m = cycle_machine();
    spin_program(&mut m, &[0xA9, 0x1B, 0x8D, 0x11, 0xD0]);
    let spent = run(&mut m, 3 * FRAME_CYCLES + 1_000);
    // However many cycles went to waiting, step() counted them: the raster
    // is exactly where that many clocks put it.
    let into_frame = spent % FRAME_CYCLES;
    let line = u32::try_from(into_frame / 63).unwrap();
    assert_eq!(raster_line(&m), line);
}

#[test]
fn the_z80_is_not_stalled_but_the_raster_keeps_running() {
    let mut m = C128::new(roms()).unwrap();
    m.set_vic_accuracy(VicAccuracy::Cycle).unwrap();
    // The Z80 runs its NOP sled (zeroed BIOS) with the display on.
    m.bus.write(0xD011, 0x1B);
    assert_eq!(m.cpu_name(), "Z80");
    let spent = run(&mut m, 2 * FRAME_CYCLES + 1_000);
    assert_eq!(m.cpu_name(), "Z80");
    assert_eq!(m.ba_stalled_cycles(), 0);
    let line = u32::try_from((spent % FRAME_CYCLES) / 63).unwrap();
    assert_eq!(raster_line(&m), line);
}

#[test]
fn the_2_mhz_mode_is_not_stalled_either() {
    let mut m = cycle_machine();
    // $D030 bit 0: 2 MHz, then spin with the display on.
    spin_program(
        &mut m,
        &[0xA9, 0x1B, 0x8D, 0x11, 0xD0, 0xA9, 0x01, 0x8D, 0x30, 0xD0],
    );
    run(&mut m, FRAME_CYCLES);
    assert!(m.bus.fast);
    assert_eq!(m.ba_stalled_cycles(), 0);
}

#[test]
fn cycle_mode_draws_its_own_picture_border_included() {
    let mut m = cycle_machine();
    spin_program(&mut m, &[0xA9, 0x1B, 0x8D, 0x11, 0xD0]);
    run(&mut m, 2 * FRAME_CYCLES);
    let (w, h, rgb) = m.render_frame().unwrap();
    assert_eq!((w, h), (384, 264));
    assert_eq!(rgb.len(), 384 * 264 * 3);
}

#[test]
fn cycle_mode_is_refused_on_ntsc_and_line_mode_comes_back() {
    let mut m = C128::new(roms()).unwrap();
    m.set_region(machine::Region::Ntsc);
    assert!(m.set_vic_accuracy(VicAccuracy::Cycle).is_err());
    assert_eq!(m.vic_accuracy(), VicAccuracy::Line);

    let mut p = cycle_machine();
    assert_eq!(p.vic_accuracy(), VicAccuracy::Cycle);
    p.set_vic_accuracy(VicAccuracy::Line).unwrap();
    assert_eq!(p.vic_accuracy(), VicAccuracy::Line);
}

#[test]
fn a_restored_cycle_mode_state_continues_identically() {
    let mut a = cycle_machine();
    spin_program(&mut a, &[0xA9, 0x1B, 0x8D, 0x11, 0xD0]);
    run(&mut a, FRAME_CYCLES + 9_000); // mid-frame, mid-line
    let saved = a.save_state();
    let mut b = cycle_machine();
    b.load_state(&saved).unwrap();
    run(&mut a, 2 * FRAME_CYCLES);
    run(&mut b, 2 * FRAME_CYCLES);
    assert_eq!(a.render_frame(), b.render_frame());
    let (sa, sb) = (a.save_state(), b.save_state());
    let diffs: Vec<usize> = (0..sa.len().min(sb.len()))
        .filter(|&i| sa[i] != sb[i])
        .collect();
    assert!(
        sa.len() == sb.len() && diffs.is_empty(),
        "lens {} {} diffs {:?}",
        sa.len(),
        sb.len(),
        &diffs[..diffs.len().min(20)]
    );

    // A state made in one accuracy mode does not load into the other.
    let mut line = C128::new(roms()).unwrap();
    assert!(line.load_state(&saved).is_err());
}
