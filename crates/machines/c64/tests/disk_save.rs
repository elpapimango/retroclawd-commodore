//! SAVE to an attached `.d64` through the real KERNAL's own entry points
//! (`SETNAM`/`SETLFS`/`SAVE`). Needs `roms/c64/`; skipped when absent.

use c64::C64;
use machine::Machine;

fn rom(name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!(
        "{}/../../../roms/c64/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .ok()
}

fn formatted_d64() -> d64::D64 {
    let per_track = |t: u8| -> usize {
        match t {
            1..=17 => 21,
            18..=24 => 19,
            25..=30 => 18,
            _ => 17,
        }
    };
    let mut bytes = vec![0u8; 683 * 256];
    let bam: usize = (1..18u8).map(per_track).sum::<usize>() * 256;
    bytes[bam] = 18;
    bytes[bam + 1] = 1;
    for t in 1..=35u8 {
        let base = bam + 4 + (t as usize - 1) * 4;
        let mut free = per_track(t);
        for s in 0..per_track(t) {
            bytes[base + 1 + s / 8] |= 1 << (s % 8);
        }
        if t == 18 {
            bytes[base + 1] &= !0b11;
            free -= 2;
        }
        bytes[base] = free as u8;
    }
    bytes[bam + 256 + 1] = 0xFF;
    d64::D64::parse(&bytes).unwrap()
}

#[test]
fn a_program_saves_a_file_through_the_real_kernal_save_vector() {
    let (Some(kernal), Some(basic), Some(chars)) = (
        rom("kernal-901227-03.bin"),
        rom("basic-901226-01.bin"),
        rom("chargen-901225-01.bin"),
    ) else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    let mut c64 = C64::new(kernal, basic, chars).unwrap();
    c64.load_disk(formatted_d64());

    // Data to save at $4000..$4010, file name "SAVETEST" at $0340.
    c64.load(0x4000, &(0..16u8).collect::<Vec<_>>());
    c64.load(0x0340, b"SAVETEST");
    #[rustfmt::skip]
    let code: [u8; 41] = [
        0xA9, 0x08, 0xA2, 0x40, 0xA0, 0x03,       // SETNAM: A = length 8, X/Y = $0340
        0x20, 0xBD, 0xFF,                         // JSR SETNAM
        0xA9, 0x01, 0xA2, 0x08, 0xA0, 0x01,       // SETLFS: file 1, device 8, SA 1
        0x20, 0xBA, 0xFF,                         // JSR SETLFS
        0xA9, 0x00, 0x85, 0xFB,                   // start pointer $4000 at $FB/$FC
        0xA9, 0x40, 0x85, 0xFC,
        0xA9, 0xFB, 0xA2, 0x10, 0xA0, 0x40,       // A=$FB, X/Y = end $4010
        0x20, 0xD8, 0xFF,                         // JSR SAVE
        0x8E, 0x00, 0x05,                         // STX $0500 (marker the call returned)
        0x4C, 0x00, 0x00,                         // JMP (patched spin)
    ];
    let mut code = code.to_vec();
    let spin = 0xC000 + code.len() as u16 - 3;
    let n = code.len();
    code[n - 2] = spin as u8;
    code[n - 1] = (spin >> 8) as u8;
    c64.load(0xC000, &code);
    c64.set_pc(0xC000);
    // SETNAM/SETLFS/SAVE are KERNAL entry points that need the ROM mapped
    // and interrupts quiet; give the machine a little time to run them.
    let mut spent = 0u64;
    while spent < 20_000 {
        spent += u64::from(c64.step());
    }

    let image = c64
        .take_modified_disk_image()
        .expect("SAVE modified the disk");
    let disk = d64::D64::parse(&image).unwrap();
    let entries = disk.directory_entries();
    let entry = d64::D64::find_program(&entries, b"SAVETEST").expect("directory entry");
    let file = disk.read_file(entry).unwrap();
    assert_eq!(&file[..2], &[0x00, 0x40]);
    assert_eq!(&file[2..], &(0..16u8).collect::<Vec<_>>()[..]);
}

/// The screen as text, one string per row (screen codes of letters,
/// digits and the common punctuation; anything else is `?`).
fn screen_rows(c64: &C64) -> Vec<String> {
    (0..25u16)
        .map(|row| {
            (0..40u16)
                .map(
                    |col| match c64.read_memory(u64::from(0x0400 + row * 40 + col)) {
                        c @ 1..=26 => char::from(b'A' + c - 1),
                        c @ 0x20..=0x3F => char::from(c),
                        _ => '?',
                    },
                )
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn run_cycles(c64: &mut C64, cycles: u64) {
    let mut spent = 0u64;
    while spent < cycles {
        spent += u64::from(c64.step());
    }
}

/// Puts `text` in the KERNAL keyboard buffer (`$0277`, count at `$C6`) and
/// runs until BASIC has taken it.
fn type_text(c64: &mut C64, text: &[u8]) {
    assert!(text.len() <= 10, "the keyboard buffer holds ten characters");
    c64.load(0x0277, text);
    c64.load(0x00C6, &[text.len() as u8]);
    while c64.read_memory(0xC6) != 0 {
        run_cycles(c64, 1_000);
    }
}

/// The fast loader serves `LOAD"*",8,1` at once, but the screen must show
/// what the real KERNAL LOAD prints in direct mode: SEARCHING FOR <name>,
/// LOADING, then READY. (the trap runs the KERNAL's own message routines).
#[test]
fn a_fast_disk_load_prints_the_kernals_searching_and_loading_lines() {
    let (Some(kernal), Some(basic), Some(chars)) = (
        rom("kernal-901227-03.bin"),
        rom("basic-901226-01.bin"),
        rom("chargen-901225-01.bin"),
    ) else {
        eprintln!("skipped: roms/c64 not present");
        return;
    };
    let mut c64 = C64::new(kernal, basic, chars).unwrap();
    let mut disk = formatted_d64();
    disk.save_program(b"PROG", &[0x00, 0xC0, 0x11, 0x22, 0x33], false)
        .unwrap();
    c64.load_disk(disk);
    run_cycles(&mut c64, 3_000_000); // boot to READY.

    type_text(&mut c64, b"LOAD\"*\",8");
    type_text(&mut c64, b",1\r");
    run_cycles(&mut c64, 200_000);

    let rows = screen_rows(&c64);
    let at = rows
        .iter()
        .position(|r| r == "LOAD\"*\",8,1")
        .unwrap_or_else(|| panic!("the typed line is on screen: {rows:#?}"));
    assert_eq!(
        &rows[at + 1..at + 5],
        &["", "SEARCHING FOR *", "LOADING", "READY."],
        "{rows:#?}"
    );
    assert_eq!(
        [0xC000, 0xC001, 0xC002].map(|a| c64.read_memory(a)),
        [0x11, 0x22, 0x33]
    );
}
