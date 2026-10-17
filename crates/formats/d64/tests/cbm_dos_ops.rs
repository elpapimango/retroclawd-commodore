//! The file-system operations of `cbm-dos` on all three image formats: the same checks, one blank
//! formatted disk of each kind.

use cbm_dos::{
    FileKind, FsError, Image, Severity, directory, find, free_blocks, from_ascii, matches_pattern,
    read_file, rename, save_file, save_program, scratch, set_locked, to_ascii, validate,
};

fn payload(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
}

fn check_disk<I: Image>(mut disk: I, expected_free: usize) {
    assert_eq!(
        free_blocks(&disk),
        expected_free,
        "blocks free on a blank disk"
    );
    let blank = validate(&disk);
    assert!(
        blank.is_clean(),
        "a blank disk validates: {:?}",
        blank.problems
    );
    assert_eq!(blank.files, 0);

    // Three files of different kinds and sizes (one spans many sectors).
    let a = payload(10, 1);
    let b = payload(5000, 2);
    let c = payload(254, 3); // exactly one sector's payload
    save_program(&mut disk, b"ALPHA", &a, false).unwrap();
    save_file(&mut disk, b"BETA", FileKind::Seq, &b, false).unwrap();
    save_file(&mut disk, b"GAMMA", FileKind::Usr, &c, false).unwrap();

    let dir = directory(&disk);
    let names: Vec<_> = dir.iter().map(|e| to_ascii(e.name_bytes())).collect();
    assert_eq!(names, ["ALPHA", "BETA", "GAMMA"]);
    assert_eq!(dir[0].kind(), Some(FileKind::Prg));
    assert_eq!(dir[1].kind(), Some(FileKind::Seq));
    assert_eq!(dir[2].kind(), Some(FileKind::Usr));
    assert_eq!(dir[1].blocks, 5000_usize.div_ceil(254) as u16);
    assert_eq!(dir[2].blocks, 1);
    assert!(dir.iter().all(|e| e.is_closed() && !e.is_locked()));
    assert_eq!(read_file(&disk, &dir[0]).unwrap(), a);
    assert_eq!(read_file(&disk, &dir[1]).unwrap(), b);
    assert_eq!(read_file(&disk, &dir[2]).unwrap(), c);
    let used_blocks = 1 + 5000_usize.div_ceil(254) + 1;
    assert_eq!(free_blocks(&disk), expected_free - used_blocks);
    let v = validate(&disk);
    assert!(v.is_clean(), "{:?}", v.problems);
    assert_eq!(v.files, 3);

    // A name that exists needs `replace`.
    assert_eq!(
        save_program(&mut disk, b"ALPHA", &a, false),
        Err(cbm_dos::SaveError::FileExists)
    );
    save_program(&mut disk, b"ALPHA", &payload(600, 9), true).unwrap();
    assert_eq!(
        read_file(&disk, &find(&disk, b"ALPHA").unwrap()).unwrap(),
        payload(600, 9)
    );

    // Rename, lock, scratch.
    rename(&mut disk, b"BETA", b"DELTA").unwrap();
    assert!(find(&disk, b"BETA").is_none());
    assert_eq!(rename(&mut disk, b"DELTA", b"GAMMA"), Err(FsError::Exists));
    assert_eq!(rename(&mut disk, b"NOPE", b"X"), Err(FsError::NotFound));
    assert_eq!(rename(&mut disk, b"DELTA", b""), Err(FsError::InvalidName));
    set_locked(&mut disk, b"GAMMA", true).unwrap();
    assert!(find(&disk, b"GAMMA").unwrap().is_locked());
    assert_eq!(scratch(&mut disk, b"GAMMA"), Err(FsError::Locked));
    set_locked(&mut disk, b"GAMMA", false).unwrap();
    let before = free_blocks(&disk);
    scratch(&mut disk, b"DELTA").unwrap();
    assert!(find(&disk, b"DELTA").is_none());
    assert_eq!(free_blocks(&disk), before + 5000_usize.div_ceil(254));
    assert_eq!(scratch(&mut disk, b"DELTA"), Err(FsError::NotFound));
    let v = validate(&disk);
    assert!(v.is_clean(), "{:?}", v.problems);
    assert_eq!(v.files, 2);

    // Damage is found: point a file at a sector that is free in the BAM.
    let entry = find(&disk, b"GAMMA").unwrap();
    let (t, s) = entry.first;
    let mut sector = disk.read(t, s).unwrap();
    sector[0] = 0; // the chain ends, but the directory still claims the right size
    disk.write(t, s, &sector);
    disk.mark(t, s, true); // and the BAM wrongly frees it
    let broken = validate(&disk);
    assert!(
        broken
            .problems
            .iter()
            .any(|p| p.severity == Severity::Error && p.message.contains("lists it as free")),
        "{:?}",
        broken.problems
    );
}

fn d64_35_tracks() {
    let disk = d64::D64::blank_formatted(d64::Variant::Track35, *b"01");
    check_disk(disk, 664);
}

fn d64_40_tracks() {
    let disk = d64::D64::blank_formatted(d64::Variant::Track40, *b"01");
    check_disk(disk, 664 + 5 * 17);
}

fn d71() {
    check_disk(d71::D71::blank_formatted(*b"01"), 1328);
}

fn d81() {
    check_disk(d81::D81::blank_formatted(*b"01"), 3160);
}
/// Merged (every check kept): `d64_35_tracks`, `d64_40_tracks`, `d71`, `d81`.
#[test]
fn per_format_operations() {
    d64_35_tracks();
    d64_40_tracks();
    d71();
    d81();
}

#[test]
fn a_full_directory_grows_and_a_full_disk_is_refused() {
    let mut disk = d64::D64::blank_formatted(d64::Variant::Track35, *b"AB");
    for i in 0..40 {
        save_program(
            &mut disk,
            format!("F{i:02}").as_bytes(),
            &payload(10, i as u8),
            false,
        )
        .unwrap();
    }
    assert_eq!(directory(&disk).len(), 40);
    assert!(validate(&disk).is_clean());
    let big = payload(254 * 700, 1);
    assert_eq!(
        save_program(&mut disk, b"TOOBIG", &big, false),
        Err(cbm_dos::SaveError::DiskFull)
    );
    // The failed save left the disk as it was.
    assert!(validate(&disk).is_clean());
    assert_eq!(directory(&disk).len(), 40);
}

#[test]
fn disk_names_and_x64() {
    let mut disk = d64::D64::blank_formatted(d64::Variant::Track35, *b"ID");
    disk.set_disk_name(b"MY DISK");
    assert_eq!(&disk.disk_name()[..7], b"MY DISK");
    assert_eq!(disk.disk_name()[7], 0xA0);
    assert_eq!(disk.disk_id(), *b"ID");
    let x64 = disk.to_x64_bytes();
    assert_eq!(&x64[..4], &[0x43, 0x15, 0x41, 0x64]);
    assert_eq!(x64.len(), 64 + disk.to_bytes().len());
    assert_eq!(
        d64::D64::parse_x64(&x64).unwrap().to_bytes(),
        disk.to_bytes()
    );
    let mut d81 = d81::D81::blank_formatted(*b"ID");
    d81.set_disk_name(b"THREE FIVE");
    assert_eq!(&d81.disk_name()[..10], b"THREE FIVE");
    let mut d71 = d71::D71::blank_formatted(*b"ID");
    d71.set_disk_name(b"SEVEN ONE");
    assert_eq!(&d71.disk_name()[..9], b"SEVEN ONE");
}

#[test]
fn names_convert_and_match_like_cbm_dos() {
    assert_eq!(from_ascii("hello").unwrap(), b"HELLO");
    assert_eq!(from_ascii("A B-1").unwrap(), b"A B-1");
    assert!(from_ascii("").is_err());
    assert!(from_ascii("this name is too long").is_err());
    assert!(from_ascii("caf\u{e9}").is_err());
    assert_eq!(to_ascii(&[0x48, 0xC9, 0x21, 0x01]), "Hi!?");
    assert!(matches_pattern(b"*", b"ANYTHING"));
    assert!(matches_pattern(b"AB*", b"ABCDEF"));
    assert!(matches_pattern(b"A?C", b"ABC"));
    assert!(!matches_pattern(b"A?C", b"ABCD"));
    assert!(!matches_pattern(b"A?C", b"AB"));
    assert!(matches_pattern(b"ABC", b"ABC"));
    assert!(!matches_pattern(b"ABC", b"ABD"));
    assert!(matches_pattern(b"HI", &[0x48, 0xC9])); // shifted letters match their plain twins
}
