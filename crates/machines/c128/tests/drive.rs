//! The C128's kernal talking to the real 1571/1541/1581 DOS over the IEC bus
//! (`drive-link`). ROM-gated (`roms/c128`, `roms/c64/VC15xx`); run with
//! `--release -- --ignored`.

use c128::{C128, C128Key, Roms};
use drive_link::{Disk, DriveKind, DriveLink, IecHost};
use machine::Machine;

fn read(path: &str) -> Option<Vec<u8>> {
    std::fs::read(format!("{}/../../../{path}", env!("CARGO_MANIFEST_DIR"))).ok()
}

fn machine() -> Option<C128> {
    let mut basic = read("roms/c128/basiclo-318018-04.bin")?;
    basic.extend(read("roms/c128/basichi-318019-04.bin")?);
    Some(
        C128::new(Roms {
            basic,
            kernal: read("roms/c128/kernal-318020-05.bin")?,
            chargen: read("roms/c128/chargen-390059-01.bin")?,
            c64_basic: read("roms/c128/basic64-901226-01.bin")?,
            c64_kernal: read("roms/c128/kernal64-901227-03.bin")?,
        })
        .unwrap(),
    )
}

struct Host<'a>(&'a mut C128);

impl IecHost for Host<'_> {
    fn iec_output(&self) -> (bool, bool, bool) {
        self.0.iec_output()
    }
    fn set_iec_input(&mut self, clock: bool, data: bool) {
        self.0.set_iec_input(clock, data);
    }
    fn clock_hz(&self) -> u64 {
        self.0.clock_hz()
    }
}

fn run(m: &mut C128, link: &mut DriveLink, cycles: u64) {
    let mut total = 0u64;
    while total < cycles {
        let used = m.step();
        link.after_host_step(&mut Host(m), used);
        total += u64::from(used);
    }
}

fn tap(m: &mut C128, link: &mut DriveLink, key: C128Key) {
    m.press_key(key);
    run(m, link, 60_000);
    m.release_key(key);
    run(m, link, 60_000);
}

fn type_text(m: &mut C128, link: &mut DriveLink, text: &str) {
    use C128Key::*;
    for ch in text.chars() {
        let key = match ch.to_ascii_uppercase() {
            'A' => A,
            'B' => B,
            'C' => C,
            'D' => D,
            'E' => E,
            'F' => F,
            'G' => G,
            'H' => H,
            'I' => I,
            'J' => J,
            'K' => K,
            'L' => L,
            'M' => M,
            'N' => N,
            'O' => O,
            'P' => P,
            'Q' => Q,
            'R' => R,
            'S' => S,
            'T' => T,
            'U' => U,
            'V' => V,
            'W' => W,
            'X' => X,
            'Y' => Y,
            'Z' => Z,
            '0' => Digit0,
            '1' => Digit1,
            '2' => Digit2,
            '3' => Digit3,
            '4' => Digit4,
            '5' => Digit5,
            '6' => Digit6,
            '7' => Digit7,
            '8' => Digit8,
            '9' => Digit9,
            ' ' => Space,
            '*' => Asterisk,
            ',' => Comma,
            '\n' => Return,
            '"' | '$' => {
                m.press_key(LeftShift);
                tap(m, link, if ch == '"' { Digit2 } else { Digit4 });
                m.release_key(LeftShift);
                run(m, link, 60_000);
                continue;
            }
            other => panic!("no key for {other:?}"),
        };
        tap(m, link, key);
    }
}

fn screen(m: &C128) -> String {
    (0..25)
        .map(|row| {
            (0..40)
                .map(|col| match m.read_memory(0x0400 + row * 40 + col) & 0x7F {
                    c @ 0x01..=0x1A => (b'A' + c - 1) as char,
                    c @ 0x20..=0x3F => c as char,
                    _ => ' ',
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn with_drive(kind: DriveKind, rom: &str, disk: Disk) -> Option<(C128, DriveLink)> {
    let mut m = machine()?;
    let mut link = DriveLink::new(kind, read(rom)?, 8).unwrap();
    link.attach_disk(disk).unwrap();
    run(&mut m, &mut link, 8_000_000);
    Some((m, link))
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn a_1571_lists_a_d64_directory_for_the_c128_kernal() {
    let Some(image) = read("software/c64/Donkey Kong (Ocean) (Europe).D64") else {
        return;
    };
    let disk = Disk::Gcr(c1541::Medium::D64(d64::D64::parse(&image).unwrap()));
    let Some((mut m, mut link)) = with_drive(DriveKind::C1571, "roms/c64/VC1571", disk) else {
        return;
    };
    type_text(&mut m, &mut link, "DIRECTORY\n");
    run(&mut m, &mut link, 12_000_000);
    let text = screen(&m);
    assert!(text.contains("BLOCKS FREE"), "{text}");
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn a_1581_lists_a_blank_d81_for_the_c128_kernal() {
    let disk = Disk::D81(d81::D81::blank_formatted(*b"81"));
    let Some((mut m, mut link)) = with_drive(DriveKind::C1581, "roms/c64/VC1581", disk) else {
        return;
    };
    type_text(&mut m, &mut link, "DIRECTORY\n");
    run(&mut m, &mut link, 12_000_000);
    let text = screen(&m);
    assert!(text.contains("3160 BLOCKS FREE"), "{text}");
}

#[test]
#[ignore = "runs the real ROMs; use --release"]
fn the_c64_mode_kernal_loads_a_directory_from_a_1541() {
    let Some(image) = read("software/c64/Donkey Kong (Ocean) (Europe).D64") else {
        return;
    };
    let disk = Disk::Gcr(c1541::Medium::D64(d64::D64::parse(&image).unwrap()));
    let Some((mut m, mut link)) = with_drive(
        DriveKind::C1541,
        "roms/1541/dos1541-325302-01+901229-05.bin",
        disk,
    ) else {
        return;
    };
    type_text(&mut m, &mut link, "GO64\nY\n");
    run(&mut m, &mut link, 6_000_000);
    assert!(m.mmu().c64_mode());
    type_text(&mut m, &mut link, "LOAD\"$\",8\n");
    run(&mut m, &mut link, 12_000_000);
    type_text(&mut m, &mut link, "LIST\n");
    run(&mut m, &mut link, 4_000_000);
    let text = screen(&m);
    assert!(text.contains("BLOCKS FREE"), "{text}");
}

fn screen80(m: &C128) -> String {
    screen80_as(m, std::env::var("ASCII").is_ok())
}

/// The VDC text screen; `ascii` shows codes `0x20..=0x7E` as plain ASCII
/// (CP/M writes them that way) instead of screen codes.
fn screen80_as(m: &C128, ascii: bool) -> String {
    let ram = m.vdc_ram();
    let start = usize::from(m.vdc_register(12)) << 8 | usize::from(m.vdc_register(13));
    (0..25)
        .map(|row| {
            (0..80)
                .map(|col| match ram[(start + row * 80 + col) & 0xFFFF] & 0x7F {
                    c @ 0x01..=0x1A if !ascii => (b'A' + c - 1) as char,
                    c @ 0x20..=0x7E if ascii || c < 0x40 => c as char,
                    _ => ' ',
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Exploration harness: `DISK=<image> [DRIVE=1541|1571|1581] [C64=1] [COLS80=1]
/// [TYPE=<text, \n = Return>] [SECONDS=n] cargo test --release -p c128 --test
/// drive probe -- --ignored --nocapture`.
#[test]
#[ignore = "exploration harness"]
fn probe_disk() {
    let Ok(path) = std::env::var("DISK") else {
        return;
    };
    let bytes = std::fs::read(&path).unwrap();
    let ext = path.rsplit('.').next().unwrap().to_ascii_lowercase();
    let disk = match ext.as_str() {
        "d71" => Disk::Gcr(c1541::Medium::D71(d71::D71::parse(&bytes).unwrap())),
        "d81" => Disk::D81(d81::D81::parse(&bytes).unwrap()),
        _ => Disk::Gcr(c1541::Medium::D64(d64::D64::parse(&bytes).unwrap())),
    };
    let (kind, rom) = match std::env::var("DRIVE").as_deref() {
        Ok("1541") => (
            DriveKind::C1541,
            "roms/1541/dos1541-325302-01+901229-05.bin",
        ),
        Ok("1581") => (DriveKind::C1581, "roms/c64/VC1581"),
        _ => (DriveKind::C1571, "roms/c64/VC1571"),
    };
    let mut m = machine().unwrap();
    let mut link = DriveLink::new(kind, read(rom).unwrap(), 8).unwrap();
    link.attach_disk(disk).unwrap();
    let cols80 = std::env::var("COLS80").is_ok();
    if cols80 {
        m.set_80_columns(true);
    }
    if std::env::var("C64").is_ok() {
        m.boot_into_c64_mode();
    }
    run(&mut m, &mut link, 8_000_000);
    if let Ok(text) = std::env::var("TYPE") {
        type_text(&mut m, &mut link, &text.replace("\\n", "\n"));
    }
    let seconds: u64 = std::env::var("SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    if std::env::var("TRACE").is_ok() {
        for _ in 0..seconds * 4 {
            run(&mut m, &mut link, 246_312);
            println!("pc {:04X} {} ", m.pc(), m.cpu_name());
        }
    } else {
        run(&mut m, &mut link, seconds * 985_248);
    }
    println!("pc {:04X} cpu {}", m.pc(), m.cpu_name());
    println!("mode: {}", if m.mmu().c64_mode() { "c64" } else { "c128" });
    println!("{}", if cols80 { screen80(&m) } else { screen(&m) });
}

/// The CP/M Plus system disk (a CBM-format `.d64` with a boot sector) boots on
/// the Z80 through the real 1571 DOS: `BOOT` hands the machine to the Z80, whose
/// BIOS calls the 8502 for every disk read and gets the machine back where it
/// stopped, until CP/M prints its banner and `A>` prompt on the VDC.
#[test]
#[ignore = "runs the real ROMs for two emulated minutes; use --release"]
fn the_cpm_plus_system_disk_boots_to_the_a_prompt() {
    let Some(image) = read("software/c128/c128-cpm.system.622-3282252-1.d64") else {
        return;
    };
    let disk = Disk::Gcr(c1541::Medium::D64(d64::D64::parse(&image).unwrap()));
    let (Some(mut m), Some(rom)) = (machine(), read("roms/c64/VC1571")) else {
        return;
    };
    // The kernal reads the 40/80 key at reset, so set it before running.
    m.set_80_columns(true);
    let mut link = DriveLink::new(DriveKind::C1571, rom, 8).unwrap();
    link.attach_disk(disk).unwrap();
    run(&mut m, &mut link, 8_000_000);
    type_text(&mut m, &mut link, "BOOT\n");
    run(&mut m, &mut link, 120 * 985_248);
    assert_eq!(m.cpu_name(), "Z80", "CP/M runs on the Z80");
    let text = screen80_as(&m, true);
    assert!(text.contains("CP/M 3.0"), "{text}");
    assert!(text.contains("Commodore 128"), "{text}");
    assert!(text.contains("A>"), "{text}");
}
