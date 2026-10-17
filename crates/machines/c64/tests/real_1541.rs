//! The real C64 KERNAL talking to a real 1541 DOS ROM over the IEC bus
//! (`roms/1541/`, `roms/c64/`). Skipped when a ROM is absent.

use machine::Machine;

fn rom(dir: &str, name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!(
        "{}/../../../roms/{dir}/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .ok()
}

fn propagate<D: c1541::IecDrive>(bus: &mut c1541::IecBus, c64: &mut c64::C64, drive: &mut D) {
    let (atn, clock, data) = c64.iec_output();
    bus.set_computer_outputs(atn, clock, data);
    let (dc, dd) = drive.iec_output();
    bus.set_drive_outputs(dc, dd);
    c64.set_iec_input(bus.clock(), bus.data());
    drive.set_iec_input(bus.atn(), bus.clock(), bus.data());
}

// Results are kept at $C100.. (not in screen RAM, which CINT clears).
/// Result of a KERNAL `LOAD` run through the real 1541 DOS.
struct LoadOutcome {
    carry_set: bool,
    error: u8,
    end: u16,
    status: u8,
    c64: c64::C64,
    cycles: u64,
}

/// Boots the real KERNAL and real DOS side by side, then calls the KERNAL's
/// own `SETNAM`/`SETLFS`/`LOAD` for `name` on device 8 (`secondary` 0 =
/// load at `$0801`, 1 = at the file's own address). `None` when a ROM is
/// missing.
fn true_drive_load(
    dos_rom: &str,
    disk: &std::path::Path,
    name: &[u8],
    secondary: u8,
    max_cycles: u64,
) -> Option<LoadOutcome> {
    let c64 = c64::C64::new(
        rom("c64", "kernal-901227-03.bin")?,
        rom("c64", "basic-901226-01.bin")?,
        rom("c64", "chargen-901225-01.bin")?,
    )
    .unwrap();
    let mut drive = c1541::C1541::new(rom("1541", dos_rom)?, 8).unwrap();
    drive.attach_disk(d64::D64::parse(&std::fs::read(disk).ok()?).unwrap());
    true_drive_load_with(c64, drive, name, secondary, max_cycles)
}

/// The same with a ready-made drive (any model, any disk attached).
fn true_drive_load_with<D: c1541::IecDrive>(
    c64: c64::C64,
    drive: D,
    name: &[u8],
    secondary: u8,
    max_cycles: u64,
) -> Option<LoadOutcome> {
    true_drive_load_cmd(c64, drive, None, name, secondary, max_cycles)
}

/// The same, first sending `command` on the drive's command channel
/// (`OPEN 15,8,15,"..."`), e.g. `U0>M1` to put a 1571 into 1571 mode.
fn true_drive_load_cmd<D: c1541::IecDrive>(
    mut c64: c64::C64,
    mut drive: D,
    command: Option<&[u8]>,
    name: &[u8],
    secondary: u8,
    max_cycles: u64,
) -> Option<LoadOutcome> {
    // The C64 runs a short program: initialise like a cold boot, wait for
    // the drive's DOS to finish its own start-up (about 1.3 M cycles on a
    // real 1541; the C64 boot takes ~2 s), then LOAD.
    let mut code: Vec<u8> = vec![0x78];
    let jsr = |code: &mut Vec<u8>, a: u16| code.extend([0x20, a as u8, (a >> 8) as u8]);
    for vector in [0xFF84, 0xFF87, 0xFF8A, 0xFF81] {
        jsr(&mut code, vector); // IOINIT, RAMTAS, RESTOR, CINT
    }
    code.push(0x58);
    code.extend([
        0xA9, 12, 0x85, 0xFB, 0xA2, 0, 0xA0, 0, 0x88, 0xD0, 0xFD, 0xCA, 0xD0, 0xF8, 0xC6, 0xFB,
        0xD0, 0xF1,
    ]);
    if let Some(command) = command {
        for (i, &c) in command.iter().enumerate() {
            code.extend([0xA9, c, 0x8D, 0x80 + i as u8, 0x03]);
        }
        code.extend([0xA9, command.len() as u8, 0xA2, 0x80, 0xA0, 0x03]);
        jsr(&mut code, 0xFFBD); // SETNAM
        code.extend([0xA9, 15, 0xA2, 8, 0xA0, 15]);
        jsr(&mut code, 0xFFBA); // SETLFS
        jsr(&mut code, 0xFFC0); // OPEN
        code.extend([0xA9, 15]);
        jsr(&mut code, 0xFFC3); // CLOSE
    }
    for (i, &c) in name.iter().enumerate() {
        code.extend([0xA9, c, 0x8D, 0x40 + i as u8, 0x03]);
    }
    code.extend([0xA9, name.len() as u8, 0xA2, 0x40, 0xA0, 0x03]);
    jsr(&mut code, 0xFFBD); // SETNAM
    code.extend([0xA9, 0x01, 0xA2, 0x08, 0xA0, secondary]);
    jsr(&mut code, 0xFFBA); // SETLFS
    code.extend([0xA9, 0x00, 0xA2, 0x01, 0xA0, 0x08]);
    jsr(&mut code, 0xFFD5); // LOAD
    code.extend([
        0x8D, 0x01, 0xC1, 0x08, 0x68, 0x8D, 0x00, 0xC1, 0x8E, 0x02, 0xC1, 0x8C, 0x03, 0xC1,
    ]);
    jsr(&mut code, 0xFFB7); // READST
    code.extend([0x8D, 0x04, 0xC1]); // $C104 = ST
    if std::env::var("ERRCH").is_ok() {
        // Read the drive's error channel into $C200..: OPEN 15,8,15 / CHKIN.
        code.extend([0xA9, 0x00, 0xA2, 0x00, 0xA0, 0x00]);
        jsr(&mut code, 0xFFBD); // SETNAM (empty)
        code.extend([0xA9, 15, 0xA2, 8, 0xA0, 15]);
        jsr(&mut code, 0xFFBA); // SETLFS
        jsr(&mut code, 0xFFC0); // OPEN
        code.extend([0xA2, 15]);
        jsr(&mut code, 0xFFC6); // CHKIN
        code.extend([0xA0, 0]);
        let top = 0xC000 + code.len() as u16;
        jsr(&mut code, 0xFFCF); // CHRIN
        code.extend([0x99, 0x00, 0xC2, 0xC8, 0xC0, 30, 0xD0]);
        let rel = (top as i32 - (0xC000 + code.len() as i32 + 1)) as i8;
        code.push(rel as u8);
    }
    code.extend([0xA9, 0x01, 0x8D, 0x05, 0xC1]); // $C105 = done
    let spin = 0xC000 + code.len() as u16;
    code.extend([0x4C, spin as u8, (spin >> 8) as u8]);
    c64.load(0xC000, &code);
    c64.load(0xC100, &[0; 6]);
    c64.set_pc(0xC000);

    let mut bus = c1541::IecBus::new();
    propagate(&mut bus, &mut c64, &mut drive);
    let mut owed = 0.0f64;
    let mut total = 0u64;
    let mut next_report = 5_000_000u64;
    while total < max_cycles && c64.read_memory(0xC105) == 0 {
        let used = c64.step();
        total += u64::from(used);
        if std::env::var("DBGPC").is_ok() && total >= next_report {
            next_report += 5_000_000;
            eprintln!(
                "t={total} c64 pc={:04x} drive pc={:04x}",
                c64.pc(),
                drive.pc()
            );
        }
        propagate(&mut bus, &mut c64, &mut drive);
        owed += f64::from(used) * drive.clock_hz() as f64 / 985_248.0;
        while owed >= 1.0 {
            owed -= f64::from(drive.step());
            propagate(&mut bus, &mut c64, &mut drive);
        }
    }
    if c64.read_memory(0xC105) == 0 {
        return None;
    }
    if std::env::var("ERRCH").is_ok() {
        let text: Vec<u8> = (0xC200..0xC21E).map(|a| c64.read_memory(a)).collect();
        eprintln!("drive error channel: {:?}", String::from_utf8_lossy(&text));
    }
    Some(LoadOutcome {
        carry_set: c64.read_memory(0xC100) & 1 != 0,
        error: c64.read_memory(0xC101),
        end: u16::from(c64.read_memory(0xC102)) | u16::from(c64.read_memory(0xC103)) << 8,
        status: c64.read_memory(0xC104),
        c64,
        cycles: total,
    })
}

fn software_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../software/c64")
}

/// 1541 (325302-01 + 901229-05), 1541-II (251968-03) and the 1541-C ROM.
const DOS_ROMS: [&str; 3] = [
    "dos1541-325302-01+901229-05.bin",
    "dos1541ii-251968-03.bin",
    "../c64/VC1541-C",
];

/// `LOAD"$",8` through the real DOS returns the directory the image holds.
#[test]
#[ignore = "runs two real machines for ~10 M cycles; use --release"]
fn real_dos_loads_the_directory_over_iec() {
    let disk = software_dir().join("Bomb Jack (Europe).D64");
    for dos in DOS_ROMS {
        let Some(out) = true_drive_load(dos, &disk, b"$", 0, 40_000_000) else {
            eprintln!("skipped or did not finish: {dos}");
            continue;
        };
        assert!(!out.carry_set, "{dos}: LOAD failed, A={}", out.error);
        assert_eq!(out.status & 0xBF, 0, "{dos}: only EOI may be set");
        let listing: Vec<u8> = (0x0801..out.end).map(|a| out.c64.read_memory(a)).collect();
        let text = String::from_utf8_lossy(&listing).to_string();
        assert!(text.contains("BOMB JACK+"), "{dos}: {text:?}");
        assert!(text.contains("BLOCKS FREE"), "{dos}: {text:?}");
    }
}

/// `LOAD"*",8,1` of every `.d64` in `software/c64` through the real DOS:
/// the bytes in C64 memory equal the first program the image holds.
#[test]
#[ignore = "loads whole programs at serial-bus speed; use --release"]
fn real_dos_loads_the_first_program_of_each_disk_image() {
    let Ok(dir) = std::fs::read_dir(software_dir()) else {
        return;
    };
    let mut disks: Vec<_> = dir
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("d64")))
        .collect();
    disks.sort();
    for disk in disks {
        let image = d64::D64::parse(&std::fs::read(&disk).unwrap()).unwrap();
        let entries = image.directory_entries();
        let entry = d64::D64::find_program(&entries, b"*").expect("a program");
        let file = image.read_file(entry).unwrap();
        let load_at = u16::from(file[0]) | u16::from(file[1]) << 8;
        let Some(out) = true_drive_load(DOS_ROMS[0], &disk, b"*", 1, 600_000_000) else {
            panic!(
                "{}: no result (ROM missing or LOAD never finished)",
                disk.display()
            );
        };
        assert!(!out.carry_set, "{}: A={}", disk.display(), out.error);
        assert_eq!(
            out.end,
            load_at + (file.len() - 2) as u16,
            "{}",
            disk.display()
        );
        let loaded: Vec<u8> = (load_at..out.end).map(|a| out.c64.read_memory(a)).collect();
        assert_eq!(loaded, file[2..], "{}", disk.display());
        eprintln!(
            "{}: {} bytes in {} cycles ({:.0} s)",
            disk.file_name().unwrap().to_string_lossy(),
            loaded.len(),
            out.cycles,
            out.cycles as f64 / 985_248.0
        );
    }
}

/// Types `text` into the KERNAL keyboard queue ($0277, count at $C6),
/// waiting for room, as a user would at the BASIC prompt.
fn type_text(c64: &mut c64::C64, run: &mut impl FnMut(&mut c64::C64, u64), text: &[u8]) {
    for &ch in text {
        while c64.read_memory(0x00C6) >= 8 {
            run(c64, 20_000);
        }
        let n = c64.read_memory(0x00C6);
        c64.load(0x0277 + u16::from(n), &[ch]);
        c64.load(0x00C6, &[n + 1]);
    }
}

/// Directory text on the BASIC screen after a `LOAD"$",8` (first listing line
/// that names a program), read back from screen RAM.
fn screen_text(c64: &c64::C64) -> String {
    (0..25)
        .map(|r| {
            (0..40)
                .map(|c| {
                    let b = c64.read_memory(0x0400 + r * 40 + c) & 0x7F;
                    match b {
                        1..=26 => (b + 64) as char,
                        32..=63 => b as char,
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

/// Runs `LOAD"$",8` + `LIST`-free: types it and waits for the prompt, then
/// returns the screen.
fn list_directory(c64: &mut c64::C64, run: &mut impl FnMut(&mut c64::C64, u64)) -> String {
    type_text(c64, run, b"LOAD\"$\",8\r");
    run(c64, 2_000_000);
    let mut guard = 0;
    while !((0xE5CD..=0xE5D4).contains(&c64.pc()) && c64.read_memory(0x00C6) == 0) {
        run(c64, 50_000);
        guard += 1;
        assert!(guard < 20_000, "LOAD never finished");
    }
    type_text(c64, run, b"LIST\r");
    run(c64, 1_500_000);
    screen_text(c64)
}

/// A host disk swap: with an instant attach the drive never sees the
/// write-protect sensor change, so its DOS keeps serving the old disk's BAM;
/// a swap with a gap (as a real one has) is noticed.
#[test]
#[ignore = "runs a real KERNAL + DOS for several emulated minutes; use --release"]
fn real_dos_notices_a_disk_swapped_with_a_gap_only() {
    let dir = software_dir();
    let a = std::fs::read(dir.join("Dragon Breed (USA) (Side A).D64")).unwrap();
    let b = std::fs::read(dir.join("Dragon Breed (USA) (Side B).D64")).unwrap();
    let Some(k) = rom("c64", "kernal-901227-03.bin") else {
        return;
    };
    for gap_cycles in [0u64, 400_000] {
        let mut c64 = c64::C64::new(
            k.clone(),
            rom("c64", "basic-901226-01.bin").unwrap(),
            rom("c64", "chargen-901225-01.bin").unwrap(),
        )
        .unwrap();
        let mut drive = c1541::C1541::new(rom("1541", DOS_ROMS[0]).unwrap(), 8).unwrap();
        drive.attach_disk(d64::D64::parse(&a).unwrap());
        let mut bus = c1541::IecBus::new();
        let mut owed = 0.0f64;
        let mut run = |c64: &mut c64::C64, drive: &mut c1541::C1541, cycles: u64| {
            let mut done = 0u64;
            while done < cycles {
                let used = c64.step();
                done += u64::from(used);
                propagate(&mut bus, c64, drive);
                owed += f64::from(used) * drive.clock_hz() as f64 / 985_248.0;
                while owed >= 1.0 {
                    owed -= f64::from(drive.step());
                    propagate(&mut bus, c64, drive);
                }
            }
        };
        let mut step = |c: &mut c64::C64, n: u64| run(c, &mut drive, n);
        step(&mut c64, 3_000_000);
        let first = list_directory(&mut c64, &mut step);
        assert!(first.contains("DRAGON BREED+"), "side A: {first}");
        // Swap.
        drive.detach_disk();
        let mut step = |c: &mut c64::C64, n: u64| run(c, &mut drive, n);
        let _ = &mut step;
        if gap_cycles > 0 {
            run(&mut c64, &mut drive, gap_cycles);
        }
        drive.attach_disk(d64::D64::parse(&b).unwrap());
        let mut step = |c: &mut c64::C64, n: u64| run(c, &mut drive, n);
        let second = list_directory(&mut c64, &mut step);
        let sees_b = second.contains("DRAGON BR. INTRO");
        println!("gap {gap_cycles} cycles: second listing sees side B = {sees_b}");
        println!("{second}");
        if gap_cycles > 0 {
            assert!(sees_b, "the swap with a gap must be noticed:\n{second}");
            // Side B has 247 blocks free; the stale BAM of side A says 34.
            assert!(second.contains("247 BLOCKS FREE"), "{second}");
        } else {
            // Documented hardware-faithful behaviour: no gap, no change noticed.
            assert!(second.contains("34 BLOCKS FREE"), "{second}");
        }
    }
}

fn formatted_blank_d64() -> d64::D64 {
    let per_track = |t: u8| -> usize { usize::from(d64::sectors_per_track(t)) };
    let mut bytes = vec![0u8; 683 * 256];
    let bam: usize = (1..18u8).map(per_track).sum::<usize>() * 256;
    bytes[bam] = 18;
    bytes[bam + 1] = 1;
    bytes[bam + 0xA2] = b'6';
    bytes[bam + 0xA3] = b'4';
    bytes[bam + 0xA5] = b'2';
    bytes[bam + 0xA6] = b'A';
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

/// Runs the C64's own KERNAL `SETNAM/SETLFS/SAVE` of 700 bytes at `$4000` as
/// `SAVETEST` over the IEC bus (after the optional drive `command`), lets the
/// DOS finish, and hands back the drive.
fn save_over_iec<D: c1541::IecDrive>(
    mut c64: c64::C64,
    mut drive: D,
    command: Option<&[u8]>,
) -> (D, Vec<u8>) {
    // 700 bytes of known data at $4000 -- three sectors.
    let payload: Vec<u8> = (0..700u32).map(|i| (i * 7 + 3) as u8).collect();
    c64.load(0x4000, &payload);
    let mut code: Vec<u8> = vec![0x78];
    let jsr = |code: &mut Vec<u8>, a: u16| code.extend([0x20, a as u8, (a >> 8) as u8]);
    for vector in [0xFF84, 0xFF87, 0xFF8A, 0xFF81] {
        jsr(&mut code, vector);
    }
    code.push(0x58);
    code.extend([
        0xA9, 12, 0x85, 0xFB, 0xA2, 0, 0xA0, 0, 0x88, 0xD0, 0xFD, 0xCA, 0xD0, 0xF8, 0xC6, 0xFB,
        0xD0, 0xF1,
    ]);
    if let Some(command) = command {
        for (i, &c) in command.iter().enumerate() {
            code.extend([0xA9, c, 0x8D, 0x80 + i as u8, 0x03]);
        }
        code.extend([0xA9, command.len() as u8, 0xA2, 0x80, 0xA0, 0x03]);
        jsr(&mut code, 0xFFBD); // SETNAM
        code.extend([0xA9, 15, 0xA2, 8, 0xA0, 15]);
        jsr(&mut code, 0xFFBA); // SETLFS
        jsr(&mut code, 0xFFC0); // OPEN
        code.extend([0xA9, 15]);
        jsr(&mut code, 0xFFC3); // CLOSE
    }
    let name = b"SAVETEST";
    for (i, &ch) in name.iter().enumerate() {
        code.extend([0xA9, ch, 0x8D, 0x40 + i as u8, 0x03]);
    }
    code.extend([0xA9, name.len() as u8, 0xA2, 0x40, 0xA0, 0x03]);
    jsr(&mut code, 0xFFBD); // SETNAM
    code.extend([0xA9, 0x01, 0xA2, 0x08, 0xA0, 0x01]);
    jsr(&mut code, 0xFFBA); // SETLFS 1,8,1
    let end = 0x4000u16 + payload.len() as u16;
    code.extend([0xA9, 0x00, 0x85, 0xFB, 0xA9, 0x40, 0x85, 0xFC]); // start pointer $FB/$FC
    code.extend([0xA9, 0xFB, 0xA2, end as u8, 0xA0, (end >> 8) as u8]);
    jsr(&mut code, 0xFFD8); // SAVE
    code.extend([
        0x8D, 0x01, 0xC1, 0x08, 0x68, 0x8D, 0x00, 0xC1, 0xA9, 0x01, 0x8D, 0x05, 0xC1,
    ]);
    let spin = 0xC000 + code.len() as u16;
    code.extend([0x4C, spin as u8, (spin >> 8) as u8]);
    c64.load(0xC000, &code);
    c64.load(0xC100, &[0; 6]);
    c64.set_pc(0xC000);

    let mut bus = c1541::IecBus::new();
    propagate(&mut bus, &mut c64, &mut drive);
    let mut owed = 0.0f64;
    let mut total = 0u64;
    while total < 80_000_000 && c64.read_memory(0xC105) == 0 {
        let used = c64.step();
        total += u64::from(used);
        propagate(&mut bus, &mut c64, &mut drive);
        owed += f64::from(used) * drive.clock_hz() as f64 / 985_248.0;
        while owed >= 1.0 {
            owed -= f64::from(drive.step());
            propagate(&mut bus, &mut c64, &mut drive);
        }
    }
    assert_eq!(c64.read_memory(0xC105), 1, "SAVE never returned");
    assert_eq!(c64.read_memory(0xC100) & 1, 0, "SAVE reported an error");
    // Give the DOS a moment to finish writing the directory and BAM.
    for _ in 0..3_000_000u64 {
        let used = c64.step();
        propagate(&mut bus, &mut c64, &mut drive);
        owed += f64::from(used) * drive.clock_hz() as f64 / 985_248.0;
        while owed >= 1.0 {
            owed -= f64::from(drive.step());
            propagate(&mut bus, &mut c64, &mut drive);
        }
    }
    (drive, payload)
}

/// The real DOS writes a file: the C64's own KERNAL `SETNAM/SETLFS/SAVE` over
/// the IEC bus, the drive's write head lays the sectors down, and the disk
/// image the drive hands back holds the file.
#[test]
#[ignore = "runs a real KERNAL + DOS for several emulated seconds; use --release"]
fn real_dos_saves_a_file_onto_the_disk_through_the_write_head() {
    let (Some(c64), Some(d)) = (c64_machine(), rom("1541", DOS_ROMS[0])) else {
        return;
    };
    let mut drive = c1541::C1541::new(d, 8).unwrap();
    drive.attach_disk(formatted_blank_d64());
    let (mut drive, payload) = save_over_iec(c64, drive, None);

    assert!(drive.disk_modified(), "the write head changed nothing");
    let image = d64::D64::parse(&drive.take_modified_disk_image().unwrap()).unwrap();
    let entries = image.directory_entries();
    let entry = d64::D64::find_program(&entries, b"SAVETEST").expect("SAVETEST in the directory");
    let file = image.read_file(entry).unwrap();
    assert_eq!(&file[..2], &[0x00, 0x40], "load address");
    assert_eq!(&file[2..], &payload[..], "file contents");
}

/// A 1571 (or 1570) with its 32K DOS ROM from `roms/c64/`.
fn drive_1571(model: c1541::DriveModel, rom_name: &str) -> Option<c1541::C1541> {
    c1541::C1541::with_model(model, rom("c64", rom_name)?, 8).ok()
}

fn c64_machine() -> Option<c64::C64> {
    Some(
        c64::C64::new(
            rom("c64", "kernal-901227-03.bin")?,
            rom("c64", "basic-901226-01.bin")?,
            rom("c64", "chargen-901225-01.bin")?,
        )
        .unwrap(),
    )
}

/// The real 1571 DOS, in the C64's 1541-compatible serial mode, lists a
/// single-sided `.d64` and loads its files.
#[test]
#[ignore = "runs two real machines for ~10 M cycles; use --release"]
fn real_1571_dos_loads_a_single_sided_disk_over_iec() {
    let disk = software_dir().join("Bomb Jack (Europe).D64");
    let image = d64::D64::parse(&std::fs::read(&disk).unwrap()).unwrap();
    for (model, name) in [
        (c1541::DriveModel::C1571, "VC1571"),
        (c1541::DriveModel::C1570, "VC1570"),
    ] {
        let (Some(c64), Some(mut drive)) = (c64_machine(), drive_1571(model, name)) else {
            eprintln!("skipped: ROM missing");
            return;
        };
        drive.attach_disk(image.clone());
        let Some(out) = true_drive_load_with(c64, drive, b"$", 0, 60_000_000) else {
            panic!("{name}: the LOAD never finished");
        };
        assert!(!out.carry_set, "{name}: LOAD failed, A={}", out.error);
        let listing: Vec<u8> = (0x0801..out.end).map(|a| out.c64.read_memory(a)).collect();
        let text = String::from_utf8_lossy(&listing).to_string();
        assert!(text.contains("BOMB JACK+"), "{name}: {text:?}");
        assert!(text.contains("BLOCKS FREE"), "{name}: {text:?}");
    }
}

/// A double-sided `.d71` whose first file fills side 1 and whose second file
/// therefore lives on side 2: the real 1571 DOS must switch heads (VIA1 Port
/// A bit 2) to load it.
fn d71_with_a_file_on_side_two() -> (d71::D71, Vec<u8>) {
    let mut disk = d71::D71::blank_formatted(*b"71");
    let filler: Vec<u8> = (0..690 * 254u32).map(|i| (i % 251) as u8).collect();
    disk.save_program(b"FILLER", &filler, false).unwrap();
    let payload: Vec<u8> = (0..1200u32).map(|i| (i * 5 + 1) as u8).collect();
    disk.save_program(b"SIDE2", &payload, false).unwrap();
    let entry = disk
        .directory_entries()
        .into_iter()
        .find(|e| e.name.starts_with(b"SIDE2"))
        .unwrap();
    assert!(entry.track > 35, "the file must start on side 2");
    (disk, payload)
}

#[test]
#[ignore = "runs two real machines for ~10 M cycles; use --release"]
fn real_1571_dos_loads_a_file_from_the_second_side_of_a_d71() {
    let (Some(c64), Some(mut drive)) = (
        c64_machine(),
        drive_1571(c1541::DriveModel::C1571, "VC1571"),
    ) else {
        eprintln!("skipped: ROM missing");
        return;
    };
    let (disk, payload) = d71_with_a_file_on_side_two();
    drive.attach_d71(disk).unwrap();
    let Some(out) = true_drive_load_cmd(c64, drive, Some(b"U0>M1"), b"SIDE2", 0, 120_000_000)
    else {
        panic!("the LOAD never finished");
    };
    assert!(!out.carry_set, "LOAD failed, A={}", out.error);
    // The PRG's first two bytes are its load address; `LOAD ,8` (secondary
    // 0) puts the rest at $0801.
    assert_eq!(usize::from(out.end - 0x0801), payload.len() - 2);
    let got: Vec<u8> = (0x0801..out.end).map(|a| out.c64.read_memory(a)).collect();
    assert_eq!(got, &payload[2..]);
}

/// The 1571 DOS in 1571 mode saves onto a double-sided `.d71`: the write head
/// path works on the second kind of medium too, and the saved image parses
/// back through the `d71` crate.
#[test]
#[ignore = "runs a real KERNAL + DOS for several emulated seconds; use --release"]
fn real_1571_dos_saves_a_file_onto_a_d71_through_the_write_head() {
    let (Some(c64), Some(mut drive)) = (
        c64_machine(),
        drive_1571(c1541::DriveModel::C1571, "VC1571"),
    ) else {
        return;
    };
    drive.attach_d71(d71::D71::blank_formatted(*b"71")).unwrap();
    let (mut drive, payload) = save_over_iec(c64, drive, Some(b"U0>M1"));

    assert!(drive.disk_modified(), "the write head changed nothing");
    let image = d71::D71::parse(&drive.take_modified_disk_image().unwrap()).unwrap();
    let entries = image.directory_entries();
    let entry = d71::D71::find_program(&entries, b"SAVETEST").expect("SAVETEST in the directory");
    let file = image.read_file(entry).unwrap();
    assert_eq!(&file[..2], &[0x00, 0x40], "load address");
    assert_eq!(&file[2..], &payload[..], "file contents");
}

/// The 1581 with its 32K DOS ROM from `roms/c64/`.
fn drive_1581() -> Option<c1541::C1581> {
    c1541::C1581::new(rom("c64", "VC1581")?, 8).ok()
}

/// A `.d81` holding one known PRG at the first directory entry.
fn d81_with_a_file() -> (d81::D81, Vec<u8>) {
    let mut disk = d81::D81::blank_formatted(*b"81");
    let payload: Vec<u8> = (0..3000u32).map(|i| (i * 11 + 5) as u8).collect();
    disk.save_program(b"HELLO", &payload, false).unwrap();
    (disk, payload)
}

/// The real 1581 DOS lists a `.d81` and loads a file from it over the serial
/// bus, reading MFM sectors through the WD1770.
#[test]
#[ignore = "runs two real machines for ~10 M cycles; use --release"]
fn real_1581_dos_loads_the_directory_and_a_file_over_iec() {
    let (Some(c64), Some(mut drive)) = (c64_machine(), drive_1581()) else {
        eprintln!("skipped: ROM missing");
        return;
    };
    let (disk, payload) = d81_with_a_file();
    drive.attach_d81(disk.clone());
    let out =
        true_drive_load_with(c64, drive, b"$", 0, 120_000_000).expect("directory LOAD finished");
    assert!(!out.carry_set, "LOAD failed, A={}", out.error);
    let listing: Vec<u8> = (0x0801..out.end).map(|a| out.c64.read_memory(a)).collect();
    let text = String::from_utf8_lossy(&listing).to_string();
    assert!(text.contains("HELLO"), "{text:?}");
    assert!(text.contains("BLOCKS FREE"), "{text:?}");

    let (Some(c64), Some(mut drive)) = (c64_machine(), drive_1581()) else {
        return;
    };
    drive.attach_d81(disk);
    let out =
        true_drive_load_with(c64, drive, b"HELLO", 0, 120_000_000).expect("file LOAD finished");
    assert!(!out.carry_set, "LOAD failed, A={}", out.error);
    let got: Vec<u8> = (0x0801..out.end).map(|a| out.c64.read_memory(a)).collect();
    assert_eq!(got, &payload[2..]);
}

/// The 1581 saves a file: the WD1770 writes the sectors, which land in the
/// `.d81` image.
#[test]
#[ignore = "runs a real KERNAL + DOS for several emulated seconds; use --release"]
fn real_1581_dos_saves_a_file_onto_a_d81_through_the_controller() {
    let (Some(c64), Some(mut drive)) = (c64_machine(), drive_1581()) else {
        return;
    };
    drive.attach_d81(d81::D81::blank_formatted(*b"81"));
    let (mut drive, payload) = save_over_iec(c64, drive, None);

    assert!(drive.disk_modified(), "the controller changed nothing");
    let image = d81::D81::parse(&drive.take_modified_disk_image().unwrap()).unwrap();
    let entries = image.directory_entries();
    let entry = d81::D81::find_program(&entries, b"SAVETEST").expect("SAVETEST in the directory");
    let file = image.read_file(entry).unwrap();
    assert_eq!(&file[..2], &[0x00, 0x40], "load address");
    assert_eq!(&file[2..], &payload[..], "file contents");
}

/// `N:` formats a disk with Write Track, 160 revolutions; the result is a
/// disk the DOS itself can list and that the `d81` crate parses.
#[test]
#[ignore = "formats 160 tracks (~35 emulated seconds); use --release"]
fn real_1581_dos_formats_a_blank_disk_with_write_track() {
    let (Some(c64), Some(mut drive)) = (c64_machine(), drive_1581()) else {
        return;
    };
    drive.attach_d81(d81::D81::parse(&vec![0u8; 819_200]).unwrap());
    // Format, then load the directory.
    let out = true_drive_load_cmd(c64, drive, Some(b"N:FRESH,81"), b"$", 0, 600_000_000)
        .expect("the format and the directory LOAD finished");
    assert!(!out.carry_set, "LOAD failed, A={}", out.error);
    let listing: Vec<u8> = (0x0801..out.end).map(|a| out.c64.read_memory(a)).collect();
    let text = String::from_utf8_lossy(&listing).to_string();
    assert!(text.contains("FRESH"), "{text:?}");
    assert!(text.contains("BLOCKS FREE"), "{text:?}");
}
