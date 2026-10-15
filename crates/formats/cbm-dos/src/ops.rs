//! Everything a disk tool needs on top of [`Image`](crate::Image): the directory as a list, reading a
//! file's bytes, scratching and renaming, checking the disk for damage, and counting free blocks.
//! Like `save_program` it is independent of the image format (`.d64`, `.d71`, `.d81`).

use super::*;

/// The file type of a directory entry (the low bits of its type byte).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Del,
    Seq,
    Prg,
    Usr,
    Rel,
    /// A partition or sub-directory (1581).
    Cbm,
}

impl FileKind {
    pub fn code(self) -> u8 {
        match self {
            FileKind::Del => 0,
            FileKind::Seq => 1,
            FileKind::Prg => 2,
            FileKind::Usr => 3,
            FileKind::Rel => 4,
            FileKind::Cbm => 5,
        }
    }

    pub fn from_code(code: u8) -> Option<FileKind> {
        Some(match code & 0x0F {
            0 => FileKind::Del,
            1 => FileKind::Seq,
            2 => FileKind::Prg,
            3 => FileKind::Usr,
            4 => FileKind::Rel,
            5 => FileKind::Cbm,
            _ => return None,
        })
    }

    /// `"PRG"`, `"SEQ"`, ... as a directory listing shows it.
    pub fn name(self) -> &'static str {
        match self {
            FileKind::Del => "DEL",
            FileKind::Seq => "SEQ",
            FileKind::Prg => "PRG",
            FileKind::Usr => "USR",
            FileKind::Rel => "REL",
            FileKind::Cbm => "CBM",
        }
    }

    pub fn parse(text: &str) -> Option<FileKind> {
        match text.to_ascii_uppercase().as_str() {
            "DEL" => Some(FileKind::Del),
            "SEQ" => Some(FileKind::Seq),
            "PRG" => Some(FileKind::Prg),
            "USR" => Some(FileKind::Usr),
            "REL" => Some(FileKind::Rel),
            "CBM" => Some(FileKind::Cbm),
            _ => None,
        }
    }
}

/// One used directory slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Where the slot is: its directory sector and the slot index in it.
    pub dir_sector: (u8, u8),
    pub slot: usize,
    /// The raw type byte: bit 7 closed, bit 6 locked, low bits the file type.
    pub type_byte: u8,
    pub first: (u8, u8),
    /// Raw PETSCII name, `0xA0` padded to 16 bytes.
    pub name: [u8; NAME_LEN],
    /// The block count the slot claims.
    pub blocks: u16,
}

impl Entry {
    pub fn kind(&self) -> Option<FileKind> {
        FileKind::from_code(self.type_byte)
    }
    pub fn is_closed(&self) -> bool {
        self.type_byte & 0x80 != 0
    }
    pub fn is_locked(&self) -> bool {
        self.type_byte & 0x40 != 0
    }
    /// The name without its padding.
    pub fn name_bytes(&self) -> &[u8] {
        let end = self
            .name
            .iter()
            .position(|&b| b == PETSCII_PAD)
            .unwrap_or(NAME_LEN);
        &self.name[..end]
    }
}

/// Why an operation on an existing file failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    InvalidName,
    /// A file of that name exists.
    Exists,
    /// The file is write protected (locked).
    Locked,
    /// The sector chain does not hold together.
    BrokenChain(String),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FsError::NotFound => f.write_str("file not found"),
            FsError::InvalidName => f.write_str("file name must be 1 to 16 characters"),
            FsError::Exists => f.write_str("file exists"),
            FsError::Locked => f.write_str("file is write protected"),
            FsError::BrokenChain(why) => write!(f, "broken sector chain: {why}"),
        }
    }
}

impl std::error::Error for FsError {}

/// Every used slot of the directory, in directory order. A broken chain ends the walk quietly (use
/// [`validate`] to find out why).
pub fn directory<I: Image>(image: &I) -> Vec<Entry> {
    let mut out = Vec::new();
    let (mut track, mut sector) = image.directory_start();
    let mut seen = HashSet::new();
    while track != 0 && seen.insert((track, sector)) {
        let Some(data) = image.read(track, sector) else {
            break;
        };
        for slot in 0..SLOTS_PER_SECTOR {
            let base = slot * SLOT_SIZE;
            let type_byte = data[base + SLOT_TYPE];
            if type_byte == 0 {
                continue;
            }
            let mut name = [PETSCII_PAD; NAME_LEN];
            name.copy_from_slice(&data[base + SLOT_NAME..base + SLOT_NAME + NAME_LEN]);
            out.push(Entry {
                dir_sector: (track, sector),
                slot,
                type_byte,
                first: (data[base + SLOT_TRACK], data[base + SLOT_SECTOR]),
                name,
                blocks: u16::from_le_bytes([
                    data[base + SLOT_BLOCKS],
                    data[base + SLOT_BLOCKS + 1],
                ]),
            });
        }
        (track, sector) = (data[0], data[1]);
    }
    out
}

/// The first entry whose name is exactly `name` (raw PETSCII, without padding).
pub fn find<I: Image>(image: &I, name: &[u8]) -> Option<Entry> {
    directory(image)
        .into_iter()
        .find(|e| e.name_bytes() == name)
}

/// The bytes of a file: its sector chain with the links stripped and the last sector cut to its
/// recorded length.
pub fn read_file<I: Image>(image: &I, entry: &Entry) -> Result<Vec<u8>, FsError> {
    let mut out = Vec::new();
    let (mut track, mut sector) = entry.first;
    let mut seen = HashSet::new();
    while track != 0 {
        if !seen.insert((track, sector)) {
            return Err(FsError::BrokenChain(format!(
                "track {track} sector {sector} is visited twice"
            )));
        }
        let data = image.read(track, sector).ok_or_else(|| {
            FsError::BrokenChain(format!("track {track} sector {sector} is outside the disk"))
        })?;
        if data[0] == 0 {
            // The last sector: byte 1 is the index of its last used byte.
            let last = data[1] as usize;
            if last >= 2 {
                out.extend_from_slice(&data[2..=last.min(SECTOR_SIZE - 1)]);
            }
            break;
        }
        out.extend_from_slice(&data[2..]);
        (track, sector) = (data[0], data[1]);
    }
    Ok(out)
}

/// Frees the sectors of the chain starting at `first` (cutting short at any damage).
fn free_entry_chain<I: Image>(image: &mut I, first: (u8, u8)) {
    free_chain(image, first.0, first.1);
}

fn write_slot<I: Image>(
    image: &mut I,
    entry: &Entry,
    edit: impl FnOnce(&mut [u8]),
) -> Result<(), FsError> {
    let (track, sector) = entry.dir_sector;
    let mut data = image
        .read(track, sector)
        .ok_or_else(|| FsError::BrokenChain("the directory sector vanished".to_string()))?;
    let base = entry.slot * SLOT_SIZE;
    edit(&mut data[base..base + SLOT_SIZE]);
    if image.write(track, sector, &data) {
        Ok(())
    } else {
        Err(FsError::BrokenChain(
            "the directory sector cannot be written".to_string(),
        ))
    }
}

/// Deletes a file the way `SCRATCH` does: its sectors become free and the slot's type byte becomes 0.
pub fn scratch<I: Image>(image: &mut I, name: &[u8]) -> Result<(), FsError> {
    let entry = find(image, name).ok_or(FsError::NotFound)?;
    if entry.is_locked() {
        return Err(FsError::Locked);
    }
    free_entry_chain(image, entry.first);
    write_slot(image, &entry, |slot| slot[SLOT_TYPE] = 0)
}

/// Renames a file. The new name must be free.
pub fn rename<I: Image>(image: &mut I, old: &[u8], new: &[u8]) -> Result<(), FsError> {
    if new.is_empty() || new.len() > NAME_LEN {
        return Err(FsError::InvalidName);
    }
    let entry = find(image, old).ok_or(FsError::NotFound)?;
    if find(image, new)
        .is_some_and(|other| other.dir_sector != entry.dir_sector || other.slot != entry.slot)
    {
        return Err(FsError::Exists);
    }
    if entry.is_locked() {
        return Err(FsError::Locked);
    }
    let mut padded = [PETSCII_PAD; NAME_LEN];
    padded[..new.len()].copy_from_slice(new);
    write_slot(image, &entry, |slot| {
        slot[SLOT_NAME..SLOT_NAME + NAME_LEN].copy_from_slice(&padded)
    })
}

/// Sets or clears the lock (write protect) bit of a file.
pub fn set_locked<I: Image>(image: &mut I, name: &[u8], locked: bool) -> Result<(), FsError> {
    let entry = find(image, name).ok_or(FsError::NotFound)?;
    write_slot(image, &entry, |slot| {
        if locked {
            slot[SLOT_TYPE] |= 0x40;
        } else {
            slot[SLOT_TYPE] &= !0x40;
        }
    })
}

/// Free blocks as a directory listing reports them: every free sector except those on the directory
/// track.
pub fn free_blocks<I: Image>(image: &I) -> usize {
    let dir_track = image.directory_track();
    (1..=u8::MAX)
        .filter(|&t| t != dir_track)
        .map(|t| {
            (0..image.sectors_per_track(t))
                .filter(|&s| image.is_free(t, s))
                .count()
        })
        .sum()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Data is lost or damaged, or a later write would corrupt the disk.
    Error,
    /// Odd but harmless to read.
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct Validation {
    pub problems: Vec<Problem>,
    /// Files in the directory (every used slot).
    pub files: usize,
    /// Sectors reachable from the directory and the system sectors.
    pub sectors_used: usize,
    pub blocks_free: usize,
}

impl Validation {
    pub fn errors(&self) -> usize {
        self.problems
            .iter()
            .filter(|p| p.severity == Severity::Error)
            .count()
    }
    pub fn warnings(&self) -> usize {
        self.problems.len() - self.errors()
    }
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }
}

fn label(entry: &Entry) -> String {
    format!("\"{}\"", to_ascii(entry.name_bytes()))
}

/// Checks the disk: the directory chain, every file's chain (loops, sectors outside the disk, sectors
/// shared by two files, block counts, unclosed files) and the BAM against what is really in use.
pub fn validate<I: Image>(image: &I) -> Validation {
    let mut report = Validation::default();
    let mut problem = |severity, message: String| {
        report.problems.push(Problem { severity, message });
    };
    let mut owner: HashMap<(u8, u8), String> = HashMap::new();
    for s in image.system_sectors() {
        owner.insert(s, "the disk header / BAM".to_string());
    }

    // The directory chain.
    let (mut track, mut sector) = image.directory_start();
    let mut dir_seen = HashSet::new();
    while track != 0 {
        if !dir_seen.insert((track, sector)) {
            problem(
                Severity::Error,
                format!("the directory chain loops back to track {track} sector {sector}"),
            );
            break;
        }
        let Some(data) = image.read(track, sector) else {
            problem(
                Severity::Error,
                format!("the directory chain leaves the disk at track {track} sector {sector}"),
            );
            break;
        };
        if let Some(prev) = owner.insert((track, sector), "the directory".to_string()) {
            problem(
                Severity::Error,
                format!("directory sector {track}/{sector} is also used by {prev}"),
            );
        }
        (track, sector) = (data[0], data[1]);
    }

    // Files.
    let entries = directory(image);
    report.files = entries.len();
    for entry in &entries {
        let name = label(entry);
        if !entry.is_closed() {
            problem(
                Severity::Warning,
                format!("{name} is not closed (a \"splat\" file)"),
            );
        }
        if entry.kind().is_none() {
            problem(
                Severity::Warning,
                format!("{name} has an unknown file type {}", entry.type_byte & 0x0F),
            );
        }
        if entry.kind() == Some(FileKind::Del) && entry.first.0 == 0 {
            continue; // an empty DEL entry owns no sectors
        }
        let (mut t, mut s) = entry.first;
        let mut count = 0usize;
        let mut chain_seen = HashSet::new();
        while t != 0 {
            if !chain_seen.insert((t, s)) {
                problem(
                    Severity::Error,
                    format!("{name}: the chain loops back to track {t} sector {s}"),
                );
                break;
            }
            let Some(data) = image.read(t, s) else {
                problem(
                    Severity::Error,
                    format!("{name}: the chain leaves the disk at track {t} sector {s}"),
                );
                break;
            };
            match owner.get(&(t, s)) {
                Some(other) if *other != name => problem(
                    Severity::Error,
                    format!("{name}: sector {t}/{s} is also used by {other} (cross-linked)"),
                ),
                Some(_) => {}
                None => {
                    owner.insert((t, s), name.clone());
                }
            }
            count += 1;
            (t, s) = (data[0], data[1]);
        }
        let side_sectors_may_differ = entry.kind() == Some(FileKind::Rel);
        if count != entry.blocks as usize && !side_sectors_may_differ {
            problem(
                Severity::Warning,
                format!(
                    "{name}: the directory says {} block(s), the chain has {count}",
                    entry.blocks
                ),
            );
        }
    }

    // The BAM against reality.
    let mut orphans: Vec<(u8, u8)> = Vec::new();
    let mut in_use_but_free = 0usize;
    for t in 1..=u8::MAX {
        for s in 0..image.sectors_per_track(t) {
            let used = owner.contains_key(&(t, s));
            let free = image.is_free(t, s);
            if used && free {
                in_use_but_free += 1;
                if in_use_but_free <= 10 {
                    problem(
                        Severity::Error,
                        format!(
                            "sector {t}/{s} belongs to {} but the BAM lists it as free",
                            owner[&(t, s)]
                        ),
                    );
                }
            } else if !used && !free {
                orphans.push((t, s));
            }
        }
    }
    if in_use_but_free > 10 {
        problem(
            Severity::Error,
            format!(
                "... and {} more sectors in use but free in the BAM",
                in_use_but_free - 10
            ),
        );
    }
    if !orphans.is_empty() {
        let shown: Vec<String> = orphans
            .iter()
            .take(8)
            .map(|(t, s)| format!("{t}/{s}"))
            .collect();
        let more = if orphans.len() > 8 {
            format!(" and {} more", orphans.len() - 8)
        } else {
            String::new()
        };
        problem(
            Severity::Warning,
            format!(
                "{} sector(s) are allocated in the BAM but belong to no file: {}{more}",
                orphans.len(),
                shown.join(", ")
            ),
        );
    }
    report.sectors_used = owner.len();
    report.blocks_free = free_blocks(image);
    report
}

/// A directory name in ASCII: the unshifted letters (`$41`-`$5A`) stay upper case, the shifted ones
/// (`$C1`-`$DA`) become lower case, other printable ASCII is kept and the rest becomes `?`. Lossy by
/// nature; [`from_ascii`] is its practical inverse.
pub fn to_ascii(name: &[u8]) -> String {
    name.iter()
        .map(|&b| match b {
            0x41..=0x5A => b as char,
            0xC1..=0xDA => (b - 0xC1 + b'a') as char,
            0x20..=0x40 | 0x5B..=0x5F => b as char,
            _ => '?',
        })
        .collect()
}

/// A name typed in ASCII as PETSCII: letters of either case become the unshifted letters, printable
/// ASCII otherwise passes through.
pub fn from_ascii(name: &str) -> Result<Vec<u8>, FsError> {
    let bytes: Vec<u8> = name
        .chars()
        .map(|c| match c {
            'A'..='Z' => Some(c as u8),
            'a'..='z' => Some(c as u8 - b'a' + b'A'),
            ' '..='@' | '[' | ']' | '_' => Some(c as u8),
            _ => None,
        })
        .collect::<Option<_>>()
        .ok_or(FsError::InvalidName)?;
    if bytes.is_empty() || bytes.len() > NAME_LEN {
        return Err(FsError::InvalidName);
    }
    Ok(bytes)
}

/// CBM DOS pattern matching: `?` matches any one character, `*` matches the rest of the name.
/// Letters match regardless of shift state.
pub fn matches_pattern(pattern: &[u8], name: &[u8]) -> bool {
    let fold = |b: u8| {
        if (0xC1..=0xDA).contains(&b) {
            b - 0x80
        } else {
            b
        }
    };
    for (i, &p) in pattern.iter().enumerate() {
        match p {
            b'*' => return true,
            b'?' => {
                if i >= name.len() {
                    return false;
                }
            }
            _ => {
                if name.get(i).is_none_or(|&n| fold(n) != fold(p)) {
                    return false;
                }
            }
        }
    }
    pattern.len() == name.len()
}
