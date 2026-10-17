//! REU against the real KERNAL machine: a 6502 program drives the `$DF00`
//! registers, the DMA runs through the C64 memory map. Needs `roms/c64/`;
//! skipped when absent.

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

fn run(c64: &mut C64, cycles: u64) {
    let mut spent = 0;
    while spent < cycles {
        spent += u64::from(c64.step());
    }
}

/// `LDA #value / STA $DF00+reg`.
fn set(reg: u8, value: u8) -> [u8; 5] {
    [0xA9, value, 0x8D, reg, 0xDF]
}

#[test]
fn a_program_stashes_wipes_and_fetches_a_block_through_the_registers() {
    let Some(mut c64) = c64() else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    c64.attach_reu(512).unwrap();
    assert_eq!(c64.reu_kilobytes(), Some(512));
    c64.load(0x0400, &[1, 2, 3, 4, 5, 6, 7, 8]);

    let mut code = Vec::new();
    code.extend(set(0x02, 0x00)); // C64 address $0400
    code.extend(set(0x03, 0x04));
    code.extend(set(0x04, 0x10)); // REU address $001210
    code.extend(set(0x05, 0x12));
    code.extend(set(0x06, 0x00)); // bank 0
    code.extend(set(0x07, 0x08)); // length 8
    code.extend(set(0x08, 0x00));
    code.extend(set(0x01, 0x90)); // execute, FF00 disabled, C64 -> REU
    // wipe: LDX #7 / LDA #0 / STA $0400,X / DEX / BPL -6
    code.extend([0xA2, 0x07, 0xA9, 0x00, 0x9D, 0x00, 0x04, 0xCA, 0x10, 0xFA]);
    code.extend(set(0x02, 0x00)); // registers advanced; point them back
    code.extend(set(0x03, 0x04));
    code.extend(set(0x04, 0x10));
    code.extend(set(0x05, 0x12));
    code.extend(set(0x07, 0x08));
    code.extend(set(0x08, 0x00));
    code.extend(set(0x01, 0x91)); // execute, REU -> C64
    // LDA $DF00 / STA $0500, then spin
    code.extend([0xAD, 0x00, 0xDF, 0x8D, 0x00, 0x05]);
    let spin = 0xC000 + code.len() as u16;
    code.extend([0x4C, spin as u8, (spin >> 8) as u8]);
    c64.load(0xC000, &code);
    c64.set_pc(0xC000);
    run(&mut c64, 5_000);

    for (i, want) in (1..=8u8).enumerate() {
        assert_eq!(c64.read_memory(0x0400 + i as u16), want, "byte {i}");
    }
    let status = c64.read_memory(0x0500);
    assert_ne!(status & 0x40, 0, "end-of-block status {status:#x}");
}
