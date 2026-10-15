//! GeoRAM against the real KERNAL machine: a 6502 program selects a block and
//! page through `$DFFF`/`$DFFE` and writes and reads the `$DE00` window.
//! Needs `roms/c64/`; skipped when absent.

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

#[test]
fn two_pages_hold_separate_data_and_come_back_after_reselecting() {
    let Some(mut c64) = c64() else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    c64.attach_georam(512).unwrap();
    assert!(c64.attach_reu(512).is_err(), "one expansion port");
    // block 2 page 3: store $11 at $DE05; block 2 page 4: store $22 at $DE05;
    // back to page 3 and copy $DE05 to $0400.
    let sel = |block: u8, page: u8| -> Vec<u8> {
        vec![0xA9, block, 0x8D, 0xFF, 0xDF, 0xA9, page, 0x8D, 0xFE, 0xDF]
    };
    let mut code = Vec::new();
    code.extend(sel(2, 3));
    code.extend([0xA9, 0x11, 0x8D, 0x05, 0xDE]);
    code.extend(sel(2, 4));
    code.extend([0xA9, 0x22, 0x8D, 0x05, 0xDE]);
    code.extend(sel(2, 3));
    code.extend([0xAD, 0x05, 0xDE, 0x8D, 0x00, 0x04]);
    let spin = 0xC000 + code.len() as u16;
    code.extend([0x4C, spin as u8, (spin >> 8) as u8]);
    c64.load(0xC000, &code);
    c64.set_pc(0xC000);
    let mut spent = 0;
    while spent < 5_000 {
        spent += u64::from(c64.step());
    }
    assert_eq!(c64.read_memory(0x0400), 0x11);
}
