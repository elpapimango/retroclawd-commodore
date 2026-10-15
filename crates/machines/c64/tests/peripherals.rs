//! Serial-bus printer, SwiftLink and RR-Net against the real KERNAL machine.
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

fn run(c64: &mut C64, cycles: u64) {
    let mut spent = 0;
    while spent < cycles {
        spent += u64::from(c64.step());
    }
}

/// `OPEN 1,4,7 : CMD 1 : PRINT#1,"HI" : CLOSE 1` done through the KERNAL
/// jump table, then a spin loop.
fn printer_program(device: u8) -> Vec<u8> {
    let mut code = vec![
        0xA9, 0x00, 0xA2, 0x00, 0xA0, 0x00, 0x20, 0xBD, 0xFF, // SETNAM len 0
        0xA9, 0x01, 0xA2, device, 0xA0, 0x07, 0x20, 0xBA, 0xFF, // SETLFS 1,dev,7
        0x20, 0xC0, 0xFF, // OPEN
        0x8E, 0x00, 0x03, // (stash X for debugging)
        0xA2, 0x01, 0x20, 0xC9, 0xFF, // CHKOUT 1
    ];
    for ch in [b'H', b'I', 0x0D] {
        code.extend([0xA9, ch, 0x20, 0xD2, 0xFF]); // CHROUT
    }
    code.extend([0x20, 0xCC, 0xFF]); // CLRCHN
    code.extend([0xA9, 0x01, 0x20, 0xC3, 0xFF]); // CLOSE 1
    code.extend([0xA5, 0x90, 0x8D, 0x01, 0x03]); // ST -> $0301
    let spin = 0xC000 + code.len() as u16;
    code.extend([0x4C, spin as u8, (spin >> 8) as u8]);
    code
}

fn boot(c64: &mut C64) {
    run(c64, 3_000_000); // KERNAL init
}

#[test]
fn printing_to_device_4_reaches_the_host_as_text() {
    let Some(mut c64) = c64() else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    c64.set_printer(true);
    boot(&mut c64);
    let code = printer_program(4);
    c64.load(0xC000, &code);
    c64.set_pc(0xC000);
    run(&mut c64, 400_000);
    // Secondary 7 is the printer's lower/upper case mode: $48 prints as "h".
    assert_eq!(c64.printer_text().as_deref(), Some("hi\n"));
    assert_eq!(c64.read_memory(0x0301), 0, "no serial error status");
}

#[test]
fn without_the_printer_trap_device_4_is_not_present() {
    let Some(mut c64) = c64() else {
        return;
    };
    boot(&mut c64);
    let code = printer_program(4);
    c64.load(0xC000, &code);
    c64.set_pc(0xC000);
    run(&mut c64, 3_000_000);
    assert_eq!(c64.printer_text(), None);
}

#[test]
fn a_swiftlink_program_sends_and_receives_through_the_acia() {
    let Some(mut c64) = c64() else {
        return;
    };
    c64.attach_serial(false, 0xDE).unwrap();
    boot(&mut c64);
    // 9600 bps x2 (SwiftLink), 8N1, DTR on, receive interrupts off; send 'A';
    // wait for a received byte and store it at $0400.
    let code = vec![
        0xA9, 0x1E, 0x8D, 0x03, 0xDE, // ctrl
        0xA9, 0x03, 0x8D, 0x02, 0xDE, // cmd: DTR + IRQ disabled
        0xA9, 0x41, 0x8D, 0x00, 0xDE, // data = 'A'
        0xAD, 0x01, 0xDE, 0x29, 0x08, 0xF0, 0xF9, // wait RDRF
        0xAD, 0x00, 0xDE, 0x8D, 0x00, 0x04, // store
        0x4C, 0x1C, 0xC0, // spin
    ];
    c64.load(0xC000, &code);
    c64.set_pc(0xC000);
    c64.serial_set_carrier(true);
    c64.serial_push_rx(0x5A);
    run(&mut c64, 200_000);
    assert_eq!(c64.serial_take_tx(), [0x41]);
    assert_eq!(c64.read_memory(0x0400), 0x5A);
}

#[test]
fn the_rrnet_chip_answers_with_its_product_id_through_the_cartridge_window() {
    let Some(mut c64) = c64() else {
        return;
    };
    c64.attach_rrnet(0xDE, true).unwrap();
    assert!(c64.attach_reu(512).is_err(), "one expansion port");
    boot(&mut c64);
    // PacketPage pointer = 0 (chip port 0x0A -> RR-Net offset 0x02), read
    // data port 0x0C (offset 0x04), low then high.
    let code = vec![
        0xA9, 0x00, 0x8D, 0x02, 0xDE, 0x8D, 0x03, 0xDE, // pointer 0
        0xAD, 0x04, 0xDE, 0x8D, 0x00, 0x04, // lo
        0xAD, 0x05, 0xDE, 0x8D, 0x01, 0x04, // hi
        0x4C, 0x14, 0xC0,
    ];
    c64.load(0xC000, &code);
    c64.set_pc(0xC000);
    run(&mut c64, 5_000);
    assert_eq!(
        (c64.read_memory(0x0400), c64.read_memory(0x0401)),
        (0x0E, 0x63)
    );
}
