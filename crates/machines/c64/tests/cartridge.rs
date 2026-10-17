//! Cartridges against the real KERNAL: a normal cartridge autostarts through
//! the `CBM80` signature at `$8004`, an Ultimax one boots from its own reset
//! vector. Needs `roms/c64/`; skipped when absent.

use c64::C64;
use machine::Machine;

fn rom(name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!(
        "{}/../../../roms/c64/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .ok()
}

fn c64() -> Option<C64> {
    C64::new(
        rom("kernal-901227-03.bin")?,
        rom("basic-901226-01.bin")?,
        rom("chargen-901225-01.bin")?,
    )
    .ok()
}

/// A `.crt` file with one 8K CHIP at `load_address`.
fn crt(hardware: u16, exrom: u8, game: u8, chips: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
    let mut file = b"C64 CARTRIDGE   ".to_vec();
    file.extend_from_slice(&0x40u32.to_be_bytes());
    file.extend_from_slice(&0x0100u16.to_be_bytes());
    file.extend_from_slice(&hardware.to_be_bytes());
    file.extend_from_slice(&[exrom, game, 0, 0, 0, 0, 0, 0]);
    let mut name = [0u8; 32];
    name[..4].copy_from_slice(b"TEST");
    file.extend_from_slice(&name);
    for (bank, load, data) in chips {
        file.extend_from_slice(b"CHIP");
        file.extend_from_slice(&((16 + data.len()) as u32).to_be_bytes());
        file.extend_from_slice(&0u16.to_be_bytes());
        file.extend_from_slice(&bank.to_be_bytes());
        file.extend_from_slice(&load.to_be_bytes());
        file.extend_from_slice(&(data.len() as u16).to_be_bytes());
        file.extend_from_slice(data);
    }
    file
}

fn run(c64: &mut C64, cycles: u64) {
    let mut spent = 0;
    while spent < cycles {
        spent += u64::from(c64.step());
    }
}

#[test]
fn a_normal_cartridge_autostarts_through_the_cbm80_signature() {
    let Some(mut c64) = c64() else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    let mut rom = vec![0xEA; 0x2000];
    rom[0..2].copy_from_slice(&[0x09, 0x80]); // cold start -> $8009
    rom[2..4].copy_from_slice(&[0x09, 0x80]); // warm start
    rom[4..9].copy_from_slice(&[0xC3, 0xC2, 0xCD, 0x38, 0x30]); // "CBM80"
    // $8009: LDA #$2A / STA $0500 / JMP $800E (spin)
    rom[9..14].copy_from_slice(&[0xA9, 0x2A, 0x8D, 0x00, 0x05]);
    rom[14..17].copy_from_slice(&[0x4C, 0x0E, 0x80]);
    c64.load_cartridge_crt(&crt(0, 0, 1, &[(0, 0x8000, rom)]))
        .unwrap();
    assert_eq!(c64.cartridge_name(), Some("TEST"));
    run(&mut c64, 1_500_000);
    assert_eq!(c64.read_memory(0x0500), 0x2A, "the cartridge's code ran");
    assert!((0x8009..=0x8011).contains(&c64.pc()), "pc {:#x}", c64.pc());
}

#[test]
fn an_ultimax_cartridge_boots_from_its_own_reset_vector() {
    let Some(mut c64) = c64() else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    // ROMH at $E000: code at $E000, vectors at $FFFA-$FFFF -> $E000.
    let mut romh = vec![0xEA; 0x2000];
    // LDA #$2B / STA $0600 / JMP $E005 (spin)
    romh[0..8].copy_from_slice(&[0xA9, 0x2B, 0x8D, 0x00, 0x06, 0x4C, 0x05, 0xE0]);
    for vector in [0x1FFA, 0x1FFC, 0x1FFE] {
        romh[vector] = 0x00;
        romh[vector + 1] = 0xE0;
    }
    c64.load_cartridge_crt(&crt(
        0,
        1,
        0,
        &[(0, 0x8000, vec![0; 0x2000]), (0, 0xE000, romh)],
    ))
    .unwrap();
    assert_eq!(c64.pc(), 0xE000);
    run(&mut c64, 2_000);
    assert_eq!(c64.read_memory(0x0600), 0x2B);
}

#[test]
fn easyflash_boots_in_ultimax_then_the_program_can_switch_to_8k_mode() {
    let Some(mut c64) = c64() else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    // Switching to 8K mode removes ROMH from $E000, so (like real EasyFlash
    // programs) the cartridge first copies its code into RAM below $1000:
    //   $0300: LDA #$02 / STA $DE02 / LDA $8000 / STA $0700 / JMP $030B
    let code: [u8; 14] = [
        0xA9, 0x02, 0x8D, 0x02, 0xDE, 0xAD, 0x00, 0x80, 0x8D, 0x00, 0x07, 0x4C, 0x0B, 0x03,
    ];
    let mut romh = vec![0xEA; 0x2000];
    let mut at = 0;
    for (i, byte) in code.iter().enumerate() {
        // LDA #byte / STA $0300+i
        romh[at..at + 5].copy_from_slice(&[0xA9, *byte, 0x8D, i as u8, 0x03]);
        at += 5;
    }
    romh[at..at + 3].copy_from_slice(&[0x4C, 0x00, 0x03]); // JMP $0300
    for vector in [0x1FFA, 0x1FFC, 0x1FFE] {
        romh[vector] = 0x00;
        romh[vector + 1] = 0xE0;
    }
    let mut roml = vec![0u8; 0x2000];
    roml[0] = 0x5C;
    c64.load_cartridge_crt(&crt(32, 1, 0, &[(0, 0x8000, roml), (0, 0xE000, romh)]))
        .unwrap();
    run(&mut c64, 5_000);
    assert_eq!(
        c64.read_memory(0x0700),
        0x5C,
        "ROML read back after the switch"
    );
}

/// crystalct's Unified Diagnostic Cartridge: the dead test runs in Ultimax mode, copies its switch routine to `$0334` (as the
/// modified dead test does) and runs it -- `LDA #$37 / STA $01 / LDA #$01 / STA $DE00 / JMP $FCE2` -- and the board's flip-flop turns
/// the cartridge into an ordinary 8K one, so the KERNAL's own reset finds `CBM80` at `$8004` and starts the standard diagnostic.
#[test]
fn the_unified_diagnostic_cartridge_switches_from_ultimax_to_standard_and_restarts() {
    let Some(mut c64) = c64() else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    let routine: [u8; 12] = [
        0xA9, 0x37, 0x85, 0x01, 0xA9, 0x01, 0x8D, 0x00, 0xDE, 0x4C, 0xE2, 0xFC,
    ];
    let mut romh = vec![0xEA; 0x2000];
    // The Ultimax half: leave a marker, copy the routine into RAM, run it.
    romh[0..5].copy_from_slice(&[0xA9, 0xAA, 0x8D, 0x00, 0x06]); // LDA #$AA / STA $0600
    let mut at = 5;
    for (i, byte) in routine.iter().enumerate() {
        let target = 0x0334 + i as u16;
        romh[at..at + 5].copy_from_slice(&[0xA9, *byte, 0x8D, target as u8, (target >> 8) as u8]);
        at += 5;
    }
    romh[at..at + 3].copy_from_slice(&[0x4C, 0x34, 0x03]); // JMP $0334
    for vector in [0x1FFA, 0x1FFC, 0x1FFE] {
        romh[vector] = 0x00;
        romh[vector + 1] = 0xE0;
    }
    // The standard half: a CBM80 cartridge that leaves its own marker.
    let mut roml = vec![0xEA; 0x2000];
    roml[0..2].copy_from_slice(&[0x09, 0x80]);
    roml[2..4].copy_from_slice(&[0x09, 0x80]);
    roml[4..9].copy_from_slice(&[0xC3, 0xC2, 0xCD, 0x38, 0x30]);
    roml[9..14].copy_from_slice(&[0xA9, 0xBB, 0x8D, 0x01, 0x06]); // LDA #$BB / STA $0601
    roml[14..17].copy_from_slice(&[0x4C, 0x0E, 0x80]);
    // The header lines say Ultimax: EXROM high, GAME low.
    c64.load_cartridge_crt(&crt(
        c64::UNIFIED_DIAG_HARDWARE,
        1,
        0,
        &[(0, 0x8000, roml), (0, 0xE000, romh)],
    ))
    .unwrap();
    assert_eq!(c64.pc(), 0xE000, "starts in Ultimax, from ROMH's vectors");
    run(&mut c64, 3_000_000);
    assert_eq!(c64.read_memory(0x0600), 0xAA, "the Ultimax half ran first");
    assert_eq!(
        c64.read_memory(0x0601),
        0xBB,
        "after the switch the KERNAL restarted and found CBM80 in ROML"
    );
    assert!((0x8009..=0x8011).contains(&c64.pc()), "pc {:#x}", c64.pc());
}
