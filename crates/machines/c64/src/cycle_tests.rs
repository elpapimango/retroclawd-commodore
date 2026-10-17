//! Exact-timing tests for `vic_accuracy = cycle`: how many cycles the CPU
//! loses to the VIC-II's BA line, which accesses stall and which do not, and
//! that the raster and the cycles `step` reports stay in lockstep. They run
//! hand-assembled 6502 code on a machine with synthetic ROMs, so nothing
//! depends on the real KERNAL.

use crate::bus::{BASIC_ROM_SIZE, CHAR_ROM_SIZE, KERNAL_ROM_SIZE};
use crate::{C64, VicAccuracy};
use machine::{Bus, Machine};

const FRAME_CYCLES: u64 = 63 * 312;

fn cycle_machine() -> C64 {
    let mut c64 = C64::new(
        vec![0; KERNAL_ROM_SIZE],
        vec![0; BASIC_ROM_SIZE],
        vec![0; CHAR_ROM_SIZE],
    )
    .unwrap();
    c64.set_vic_accuracy(VicAccuracy::Cycle).unwrap();
    c64
}

/// Program at `$0200`: set up the VIC from `init`, then spin in
/// `NOP / NOP / JMP loop` -- every cycle of which is a read.
fn spin_program(c64: &mut C64, init: &[u8]) {
    let mut code = init.to_vec();
    let loop_at = 0x0200 + code.len() as u16;
    code.extend_from_slice(&[0xEA, 0xEA, 0x4C, loop_at as u8, (loop_at >> 8) as u8]);
    c64.load(0x0200, &code);
    c64.set_pc(0x0200);
}

fn run_frame(c64: &mut C64) -> u64 {
    let mut spent = 0u64;
    while spent < FRAME_CYCLES {
        spent += u64::from(c64.step());
    }
    spent
}

fn every_bad_line_stalls_a_read_only_program_for_exactly_43_cycles() {
    let mut c64 = cycle_machine();
    // DEN on (the latch needs it during line $30), YSCROLL 3.
    spin_program(&mut c64, &[0xA9, 0x1B, 0x8D, 0x11, 0xD0]);
    run_frame(&mut c64);
    // Lines $33, $3B, ... $F3: 25 bad lines of BA low in cycles 12-54.
    assert_eq!(c64.ba_stalled_cycles(), 25 * 43);
}

fn with_the_display_off_the_cpu_never_waits() {
    let mut c64 = cycle_machine();
    spin_program(&mut c64, &[0xA9, 0x0B, 0x8D, 0x11, 0xD0]); // DEN clear
    run_frame(&mut c64);
    assert_eq!(c64.ba_stalled_cycles(), 0);
}

fn a_lone_sprite_costs_five_cycles_a_line_for_21_lines() {
    let mut c64 = cycle_machine();
    // Sprite 0 enabled at Y = 100; DEN off so only the sprite steals.
    spin_program(
        &mut c64,
        &[0xA9, 100, 0x8D, 0x01, 0xD0, 0xA9, 0x01, 0x8D, 0x15, 0xD0],
    );
    run_frame(&mut c64);
    // 3 warning cycles + 2 DMA cycles, every line the sprite's 21 rows span.
    assert_eq!(c64.ba_stalled_cycles(), 21 * 5);
}

fn three_adjacent_sprites_cost_nine_cycles_a_line() {
    let mut c64 = cycle_machine();
    // Sprites 0-2 at Y = 100: 3 warning cycles + 3 x 2.
    spin_program(
        &mut c64,
        &[
            0xA9, 100, 0x8D, 0x01, 0xD0, 0x8D, 0x03, 0xD0, 0x8D, 0x05, 0xD0, 0xA9, 0x07, 0x8D,
            0x15, 0xD0,
        ],
    );
    run_frame(&mut c64);
    assert_eq!(c64.ba_stalled_cycles(), 21 * 9);
}
/// Merged (every check kept): `every_bad_line_stalls_a_read_only_program_for_exactly_43_cycles`, `with_the_display_off_the_cpu_never_waits`, `a_lone_sprite_costs_five_cycles_a_line_for_21_lines`, `three_adjacent_sprites_cost_nine_cycles_a_line`.
#[test]
fn ba_stall_counts() {
    every_bad_line_stalls_a_read_only_program_for_exactly_43_cycles();
    with_the_display_off_the_cpu_never_waits();
    a_lone_sprite_costs_five_cycles_a_line_for_21_lines();
    three_adjacent_sprites_cost_nine_cycles_a_line();
}

/// Steps NOPs (and one 3-cycle `LDA zp` when the parity needs it) until the
/// next instruction will start in cycle `start` of raster line `line`.
fn align(c64: &mut C64, line: u32, start: u32) {
    // NOP sled at $0200, a 3-cycle instruction at $0300.
    c64.load(0x0200, &[0xEA; 0x80]);
    c64.load(0x0300, &[0xA5, 0x00]);
    c64.set_pc(0x0200);
    loop {
        let vic = &c64.bus.vic;
        let (l, c) = (
            vic.read(0x12) as u32 | ((vic.read(0x11) as u32 & 0x80) << 1),
            vic.line_cycle(),
        );
        if l == line && c + 1 >= start - 4 {
            break;
        }
        c64.step();
        c64.set_pc(0x0200);
    }
    loop {
        let next = c64.bus.vic.line_cycle() + 1;
        assert!(next <= start, "overshot: next instruction starts in {next}");
        if next == start {
            return;
        }
        if (start - next) % 2 == 1 {
            c64.set_pc(0x0300);
        } else {
            c64.set_pc(0x0200);
        }
        c64.step();
    }
}

fn writes_proceed_while_ba_is_low_but_the_next_read_waits() {
    // BRK pushes three bytes in a row. Started in cycle 10 of a bad line
    // its writes land in 12, 13 and 14 -- BA is already low, yet they run;
    // the vector read in cycle 15 then waits until BA rises in cycle 55.
    let mut c64 = cycle_machine();
    c64.load(0x0100, &[0; 0x100]);
    c64.bus.write(0xD011, 0x1B);
    c64.bus.write(0xD012, 0);
    // Let the DEN latch see line $30 first.
    align(&mut c64, 0x33, 10);
    let stalled_before = c64.ba_stalled_cycles();
    c64.load(0x0310, &[0x00]); // BRK
    c64.set_pc(0x0310);
    let cycles = c64.step();
    assert_eq!(cycles, 47, "cycles 10..=56");
    assert_eq!(
        c64.ba_stalled_cycles() - stalled_before,
        40,
        "cycles 15..=54"
    );

    // The same BRK three cycles earlier writes in 10-12, so only the 12th
    // write overlaps BA: the vector read waits from 13.
    let mut c64 = cycle_machine();
    c64.bus.write(0xD011, 0x1B);
    align(&mut c64, 0x3B, 8);
    let stalled_before = c64.ba_stalled_cycles();
    c64.load(0x0310, &[0x00]);
    c64.set_pc(0x0310);
    let cycles = c64.step();
    assert_eq!(cycles, 7 + 42);
    assert_eq!(
        c64.ba_stalled_cycles() - stalled_before,
        42,
        "cycles 13..=54"
    );
}

fn a_read_started_inside_the_window_waits_from_its_first_cycle() {
    // NOP starting in cycle 11: its opcode read (11) runs, the padding read
    // in cycle 12 meets BA and waits until 55 -- 43 stalled cycles.
    let mut c64 = cycle_machine();
    c64.bus.write(0xD011, 0x1B);
    align(&mut c64, 0x33, 11);
    let before = c64.ba_stalled_cycles();
    c64.load(0x0310, &[0xEA]);
    c64.set_pc(0x0310);
    let cycles = c64.step();
    assert_eq!(c64.ba_stalled_cycles() - before, 43);
    assert_eq!(cycles, 2 + 43);
}
/// Merged (every check kept): `writes_proceed_while_ba_is_low_but_the_next_read_waits`, `a_read_started_inside_the_window_waits_from_its_first_cycle`.
#[test]
fn ba_read_write_rules() {
    writes_proceed_while_ba_is_low_but_the_next_read_waits();
    a_read_started_inside_the_window_waits_from_its_first_cycle();
}

fn the_raster_and_the_cycles_step_reports_stay_in_lockstep() {
    let mut c64 = cycle_machine();
    spin_program(&mut c64, &[0xA9, 0x1B, 0x8D, 0x11, 0xD0]);
    let mut total = 0u64;
    for _ in 0..40_000 {
        total += u64::from(c64.step());
        let pos = (total - 1) % FRAME_CYCLES;
        let line = (pos / 63) as u32;
        let vic = &c64.bus.vic;
        let raster = vic.read(0x12) as u32 | ((vic.read(0x11) as u32 & 0x80) << 1);
        assert_eq!(raster, line, "after {total} cycles");
        assert_eq!(vic.line_cycle(), (pos % 63) as u32 + 1);
    }
}

fn a_program_computes_the_same_result_in_both_modes() {
    // Sums 1..=100 into $0400/$0401 with indexed and read-modify-write
    // accesses, through a bad-line-heavy display.
    let program: &[u8] = &[
        0xA9, 0x1B, 0x8D, 0x11, 0xD0, // DEN on
        0xA2, 0x64, // LDX #100
        0xA9, 0x00, // LDA #0
        0x8D, 0x00, 0x04, // STA $0400
        // loop:
        0x8A, // TXA
        0x18, // CLC
        0x6D, 0x00, 0x04, // ADC $0400
        0x8D, 0x00, 0x04, // STA $0400
        0xEE, 0x01, 0x04, // INC $0401
        0xBD, 0xF0, 0x05, // LDA $05F0,X (page-crossing reads)
        0xCA, // DEX
        0xD0, 0xEF, // BNE loop
        0x4C, 0x1D, 0x02, // JMP self
    ];
    let run = |accuracy| {
        let mut c64 = C64::new(
            vec![0; KERNAL_ROM_SIZE],
            vec![0; BASIC_ROM_SIZE],
            vec![0; CHAR_ROM_SIZE],
        )
        .unwrap();
        c64.set_vic_accuracy(accuracy).unwrap();
        c64.load(0x0200, program);
        c64.set_pc(0x0200);
        let mut spent = 0u64;
        while spent < 30_000 {
            spent += u64::from(c64.step());
        }
        (c64.read_memory(0x0400), c64.read_memory(0x0401))
    };
    let line = run(VicAccuracy::Line);
    let cycle = run(VicAccuracy::Cycle);
    assert_eq!(line, cycle);
    assert_eq!(line, (0xBA, 100), "5050 & 0xFF, and 100 passes");
}
/// Merged (every check kept): `the_raster_and_the_cycles_step_reports_stay_in_lockstep`, `a_program_computes_the_same_result_in_both_modes`.
#[test]
fn lockstep_and_equivalence() {
    the_raster_and_the_cycles_step_reports_stay_in_lockstep();
    a_program_computes_the_same_result_in_both_modes();
}

fn the_mode_is_checked_when_a_save_state_is_loaded() {
    let cycle = cycle_machine();
    let saved = cycle.save_state();
    let mut line = C64::new(
        vec![0; KERNAL_ROM_SIZE],
        vec![0; BASIC_ROM_SIZE],
        vec![0; CHAR_ROM_SIZE],
    )
    .unwrap();
    let err = line.load_state(&saved).unwrap_err();
    assert!(err.contains("vic_accuracy"), "{err}");
    let mut other = cycle_machine();
    other.load_state(&saved).unwrap();
}

fn cycle_mode_is_refused_on_ntsc() {
    let mut c64 = C64::new(
        vec![0; KERNAL_ROM_SIZE],
        vec![0; BASIC_ROM_SIZE],
        vec![0; CHAR_ROM_SIZE],
    )
    .unwrap();
    c64.set_region(machine::Region::Ntsc);
    assert!(c64.set_vic_accuracy(VicAccuracy::Cycle).is_err());
    assert_eq!(c64.vic_accuracy(), VicAccuracy::Line);
}
/// Merged (every check kept): `the_mode_is_checked_when_a_save_state_is_loaded`, `cycle_mode_is_refused_on_ntsc`.
#[test]
fn mode_checks() {
    the_mode_is_checked_when_a_save_state_is_loaded();
    cycle_mode_is_refused_on_ntsc();
}

/// Sets up a red border / blue background text screen and loops forever; with
/// `open_border` the loop also flips RSEL off in raster line 248 and on again
/// in line 255 -- the classic way to open the lower border.
fn border_program(open_border: bool) -> Vec<u8> {
    let mut code = vec![
        0x78, // SEI
        0xA9, 0x1B, 0x8D, 0x11, 0xD0, // LDA #$1B : STA $D011 (DEN, 25 rows)
        0xA9, 0x02, 0x8D, 0x20, 0xD0, // border red
        0xA9, 0x06, 0x8D, 0x21, 0xD0, // background blue
        0xA9, 0x14, 0x8D, 0x18, 0xD0, // matrix $0400, character ROM
    ];
    let loop_at = 0x0200 + code.len() as u16;
    if open_border {
        code.extend_from_slice(&[
            0xAD, 0x12, 0xD0, 0xC9, 248, 0xD0, 0xF9, // wait for raster 248
            0xA9, 0x13, 0x8D, 0x11, 0xD0, // 24 rows: bottom compare is now 247
            0xAD, 0x12, 0xD0, 0xC9, 255, 0xD0, 0xF9, // wait for raster 255
            0xA9, 0x1B, 0x8D, 0x11, 0xD0, // 25 rows again before the next frame
        ]);
    }
    code.extend_from_slice(&[0x4C, loop_at as u8, (loop_at >> 8) as u8]);
    code
}

fn frame_after(open_border: bool, accuracy: VicAccuracy) -> Vec<u8> {
    let mut c64 = C64::new(
        vec![0; KERNAL_ROM_SIZE],
        vec![0; BASIC_ROM_SIZE],
        vec![0; CHAR_ROM_SIZE],
    )
    .unwrap();
    c64.set_vic_accuracy(accuracy).unwrap();
    c64.load(0x0200, &border_program(open_border));
    c64.set_pc(0x0200);
    let mut spent = 0u64;
    while spent < 3 * FRAME_CYCLES {
        spent += u64::from(c64.step());
    }
    let (w, h, rgb) = c64.render_frame().expect("a picture");
    assert_eq!((w, h), (384, 264));
    rgb
}

/// RGB at raster `line` (frame row `line - 19`), frame column `col`.
fn at(rgb: &[u8], line: usize, col: usize) -> (u8, u8, u8) {
    let i = ((line - 19) * 384 + col) * 3;
    (rgb[i], rgb[i + 1], rgb[i + 2])
}

#[test]
fn a_6502_program_opens_the_lower_border_in_cycle_mode_only() {
    let red = vic_ii::PALETTE[2];
    let blue = vic_ii::PALETTE[6];

    // The program runs the trick every frame: lines 251-254 show the
    // background where there would be border.
    let opened = frame_after(true, VicAccuracy::Cycle);
    for line in 251..=254 {
        assert_eq!(at(&opened, line, 192), blue, "line {line} opened");
        assert_eq!(
            at(&opened, line, 4),
            red,
            "side border stays at line {line}"
        );
    }
    // The 24 text rows above it are unaffected: background in the window...
    assert_eq!(at(&opened, 100, 192), blue);
    // ...and the usual border above and to the sides.
    assert_eq!(at(&opened, 40, 192), red);
    assert_eq!(at(&opened, 100, 10), red);

    // The same program without the write leaves the border closed.
    let closed = frame_after(false, VicAccuracy::Cycle);
    for line in 251..=254 {
        assert_eq!(at(&closed, line, 192), red, "line {line} closed");
    }

    // Line mode cannot show it: the picture is decoded from the registers.
    let line_mode = frame_after(true, VicAccuracy::Line);
    assert_eq!(at(&line_mode, 252, 192), red);
}

#[test]
fn a_restored_cycle_mode_state_draws_the_same_picture_as_the_original() {
    let make = || {
        let mut c64 = C64::new(
            vec![0; KERNAL_ROM_SIZE],
            vec![0; BASIC_ROM_SIZE],
            vec![0; CHAR_ROM_SIZE],
        )
        .unwrap();
        c64.set_vic_accuracy(VicAccuracy::Cycle).unwrap();
        c64
    };
    let run = |c64: &mut C64, cycles: u64| {
        let mut spent = 0u64;
        while spent < cycles {
            spent += u64::from(c64.step());
        }
    };
    let mut a = make();
    a.load(0x0200, &border_program(true));
    a.set_pc(0x0200);
    // Stop in the middle of a frame, mid-line, with the border logic busy.
    run(&mut a, FRAME_CYCLES + 9_000);
    let saved = a.save_state();
    let mut b = make();
    b.load_state(&saved).unwrap();
    run(&mut a, 2 * FRAME_CYCLES);
    run(&mut b, 2 * FRAME_CYCLES);
    assert_eq!(a.render_frame(), b.render_frame());
    assert_eq!(a.save_state(), b.save_state());
}
