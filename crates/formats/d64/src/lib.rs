//! Parses a Commodore `.d64` disk image -- the standard 1541 (and
//! 1541-compatible drive) disk format. No dependency on `machine`, any CPU
//! crate, or `c1541` -- same "format crate only parses bytes into a plain
//! struct" precedent as `z80-snapshot`/`zx81-snapshot`/`zx-tape`/`dsk`:
//! this is a fact about the file format, not about how a drive's real GCR
//! hardware consumes it. Wiring an actual `D64` into `c1541::C1541`'s VIA2
//! (the real 1541 disk mechanism -- head stepper, spindle motor, and the
//! GCR shift-register byte-assembly hardware real DOS firmware bit-bangs
//! through) is a separate, not-yet-started item -- see `TODO.md`'s
//! "drives and baseline media" section. This crate only has to get the
//! *logical* track/sector/directory contents right.
//!
//! Every geometry and byte-offset constant below was confirmed directly
//! against VICE's own real source this session (shallow-fetched, not
//! recalled) -- `diskimage/diskimage.c`'s own `sector_map_d64` table,
//! `diskconstants.h`'s own `#define`s, and `vdrive/vdrive-dir.h`'s own
//! directory-slot offsets -- see `CREDITS.md` for the exact constants
//! read and how each derived offset below was cross-checked against them
//! (several, like the directory slot's real `+2` file-type offset, are
//! genuinely easy to get wrong by intuition alone).
//!
//! **Real hardware fact this format encodes**: a 1541 disk spins at a
//! constant angular velocity but is written at a *constant linear bit
//! density*, so outer tracks (which sweep more physical distance per
//! rotation) hold more data than inner ones -- real hardware groups the
//! 35 tracks into 4 "speed zones" with a different sector count each,
//! not a uniform sector count throughout, unlike most other floppy
//! formats. `.d64`'s own byte layout mirrors this directly: it's a flat
//! concatenation of every track's sectors in this same non-uniform
//! per-zone order, not a fixed-stride array.
//!
//! Two known-real size variants are supported: the standard 35-track
//! image, and the extended 40-track image some copy-protected/larger
//! titles used (a real, if less common, physical drive configuration --
//! not this project's own invention). Each may optionally carry one
//! append per-sector real drive error-code byte (`sector_error`) --
//! `parse` detects which of the 4 resulting file sizes it's looking at
//! from length alone, the same way real disk-image tools always have.
//! The rarer 42-track variant some tools also recognize (`MAX_TRACKS_1541`
//! in VICE's own source) isn't supported -- a real, but genuinely uncommon
//! configuration this crate doesn't have a citable byte-layout source for
//! yet, so `parse` errors clearly on that size rather than guessing.

use std::fmt;

pub const SECTOR_SIZE: usize = 256;

/// Real hardware fact (confirmed via VICE's `diskconstants.h`,
/// `DIR_TRACK_1541`/`DIR_SECTOR_1541`/`BAM_TRACK_1541`/`BAM_SECTOR_1541`):
/// the BAM and the first directory sector are fixed, well-known
/// locations, not something a disk's own data ever needs to point
/// `parse` at.
pub const BAM_TRACK: u8 = 18;
pub const BAM_SECTOR: u8 = 0;
pub const DIR_TRACK: u8 = 18;
pub const DIR_SECTOR: u8 = 1;

// BAM (track 18, sector 0) byte offsets. `DIR_LINK_*`/`DOS_VERSION`/
// `NAME`/`ID` confirmed directly (`BAM_NAME_1541`=144, `BAM_ID_1541`=162,
// `BAM_EXT_BIT_MAP_1541`=192 -- the last is the 40-track extension's own
// separate free-bitmap area for tracks 36-40, since the standard 35-track
// bitmap area (`TRACK_BITMAP`, 4-143) has no room for 5 more tracks'
// groups); `DOS_TYPE` is derived (144+16=160 name end, +2 padding = 162
// matching the confirmed ID offset exactly, +2 ID, +1 padding = 165 --
// this arithmetic consistency with the one directly-fetched constant is
// itself a form of confirmation, not a second guess).
const BAM_OFFSET_DIR_LINK_TRACK: usize = 0;
const BAM_OFFSET_DIR_LINK_SECTOR: usize = 1;
const BAM_OFFSET_DOS_VERSION: usize = 2;
const BAM_OFFSET_TRACK_BITMAP: usize = 4;
const BAM_OFFSET_NAME: usize = 144;
const BAM_OFFSET_ID: usize = 162;
const BAM_OFFSET_DOS_TYPE: usize = 165;
const BAM_OFFSET_EXT_BITMAP: usize = 192;

// Directory entry ("slot") layout, confirmed directly against VICE's own
// `vdrive/vdrive-dir.h` (`SLOT_TYPE_OFFSET`=2, `SLOT_FIRST_TRACK`=3,
// `SLOT_FIRST_SECTOR`=4, `SLOT_NAME_OFFSET`=5, `SLOT_NR_BLOCKS`=30,
// `SLOT_SIZE`=32) -- the real, easy-to-get-wrong subtlety this confirms:
// each 32-byte slot's file-type byte sits at slot-relative offset `2`,
// *not* `0` -- bytes 0-1 of only the sector's first slot double as that
// whole sector's own directory-chain link (see `directory_entries`'s own
// doc comment), a real layout quirk this crate would have gotten wrong by
// the more "obvious" guess of the type byte starting a slot at offset 0.
const DIR_SLOT_SIZE: usize = 32;
const DIR_SLOTS_PER_SECTOR: usize = SECTOR_SIZE / DIR_SLOT_SIZE;
const SLOT_TYPE_OFFSET: usize = 2;
const SLOT_TRACK_OFFSET: usize = 3;
const SLOT_SECTOR_OFFSET: usize = 4;
const SLOT_NAME_OFFSET: usize = 5;
const SLOT_NAME_LEN: usize = 16;
const SLOT_SIZE_OFFSET: usize = 30;

const TRACK35_SECTORS: usize = 683;
const TRACK40_SECTORS: usize = 768;
const TRACK35_SIZE: usize = TRACK35_SECTORS * SECTOR_SIZE;
const TRACK35_SIZE_WITH_ERRORS: usize = TRACK35_SIZE + TRACK35_SECTORS;
const TRACK40_SIZE: usize = TRACK40_SECTORS * SECTOR_SIZE;
const TRACK40_SIZE_WITH_ERRORS: usize = TRACK40_SIZE + TRACK40_SECTORS;

/// Which of the 2 known-real track counts this image has -- see this
/// module's own doc comment for why only these 2 are supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Track35,
    Track40,
}

impl Variant {
    pub fn track_count(self) -> u8 {
        match self {
            Variant::Track35 => 35,
            Variant::Track40 => 40,
        }
    }

    fn total_sectors(self) -> usize {
        match self {
            Variant::Track35 => TRACK35_SECTORS,
            Variant::Track40 => TRACK40_SECTORS,
        }
    }
}

/// Real hardware fact (confirmed via VICE's own `sector_map_d64` table):
/// the 1541 packs its 35/40 tracks into 4 constant-linear-bit-density
/// speed zones, not a uniform sector count -- see this module's own doc
/// comment. Tracks 36-40 (the 40-track extension) continue the same
/// 17-sectors-per-track outermost zone the standard 35-track image's own
/// tracks 31-35 already use. `0` for anything outside `1..=40`.
pub fn sectors_per_track(track: u8) -> u8 {
    match track {
        1..=17 => 21,
        18..=24 => 19,
        25..=30 => 18,
        31..=40 => 17,
        _ => 0,
    }
}

/// Magic bytes at the start of an `.x64` file.
const X64_MAGIC: [u8; 4] = [0x43, 0x15, 0x41, 0x64];
/// Size of the `.x64` header in front of the embedded `.d64`.
const X64_HEADER_SIZE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidSize(usize),
    TrackOutOfRange(u8),
    SectorOutOfRange {
        track: u8,
        sector: u8,
    },
    /// An `.x64` file without the `43 15 41 64` magic.
    BadX64Magic,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidSize(size) => write!(
                f,
                "not a recognized .d64 size ({size} bytes -- expected {TRACK35_SIZE}, \
                 {TRACK35_SIZE_WITH_ERRORS}, {TRACK40_SIZE}, or {TRACK40_SIZE_WITH_ERRORS})"
            ),
            Error::BadX64Magic => write!(f, "not an .x64 file (missing 43 15 41 64 magic)"),
            Error::TrackOutOfRange(track) => write!(f, "track {track} out of range"),
            Error::SectorOutOfRange { track, sector } => {
                write!(f, "sector {sector} out of range on track {track}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// One parsed directory entry ("slot") -- see this module's own doc
/// comment on the real slot layout this was read from.
#[derive(Debug, Clone, Copy)]
pub struct DirectoryEntry {
    /// Raw file-type byte: bit 7 = closed, bit 6 = locked, bits 3:0 =
    /// file type (0=DEL, 1=SEQ, 2=PRG, 3=USR, 4=REL on real hardware --
    /// commonly documented, not re-derived from source this session).
    pub file_type: u8,
    pub track: u8,
    pub sector: u8,
    /// Raw PETSCII, `0xA0`-padded -- decoding to ASCII/Unicode needs the
    /// PETSCII table, not needed to identify or extract the file, same
    /// "name bytes returned raw/undecoded" precedent `zx81-snapshot`'s
    /// own `.p81` program-name parsing already established.
    pub name: [u8; SLOT_NAME_LEN],
    pub size_blocks: u16,
}

impl DirectoryEntry {
    pub fn is_closed(&self) -> bool {
        self.file_type & 0x80 != 0
    }
    pub fn is_locked(&self) -> bool {
        self.file_type & 0x40 != 0
    }
    /// The file-type bits alone (bits 3:0), undecoded -- callers that
    /// care about a specific value (`2` for PRG, the overwhelmingly
    /// common case) can just compare directly.
    pub fn kind(&self) -> u8 {
        self.file_type & 0x0F
    }
}

/// A parsed `.d64` disk image: flat track/sector storage plus optional
/// per-sector real drive error-code bytes.
#[derive(Debug, Clone)]
pub struct D64 {
    variant: Variant,
    sectors: Vec<u8>,
    errors: Option<Vec<u8>>,
}

impl D64 {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let (variant, has_errors) = match bytes.len() {
            TRACK35_SIZE => (Variant::Track35, false),
            TRACK35_SIZE_WITH_ERRORS => (Variant::Track35, true),
            TRACK40_SIZE => (Variant::Track40, false),
            TRACK40_SIZE_WITH_ERRORS => (Variant::Track40, true),
            other => return Err(Error::InvalidSize(other)),
        };
        let sector_bytes = variant.total_sectors() * SECTOR_SIZE;
        let sectors = bytes[..sector_bytes].to_vec();
        let errors = has_errors.then(|| bytes[sector_bytes..].to_vec());
        Ok(Self {
            variant,
            sectors,
            errors,
        })
    }

    /// Parses a VICE `.x64` image: a fixed 64-byte header (magic
    /// `43 15 41 64`, version and geometry fields this parser ignores)
    /// followed by a plain `.d64` image.
    pub fn parse_x64(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < X64_HEADER_SIZE || bytes[..4] != X64_MAGIC {
            return Err(Error::BadX64Magic);
        }
        Self::parse(&bytes[X64_HEADER_SIZE..])
    }

    pub fn variant(&self) -> Variant {
        self.variant
    }

    pub fn has_error_info(&self) -> bool {
        self.errors.is_some()
    }

    /// Serializes back to the exact same flat layout `parse` reads --
    /// round trip verified by test.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.sectors.clone();
        if let Some(errors) = &self.errors {
            out.extend_from_slice(errors);
        }
        out
    }

    fn sector_index(&self, track: u8, sector: u8) -> Result<usize, Error> {
        if track < 1 || track > self.variant.track_count() {
            return Err(Error::TrackOutOfRange(track));
        }
        let per_track = sectors_per_track(track);
        if sector >= per_track {
            return Err(Error::SectorOutOfRange { track, sector });
        }
        let mut index = 0usize;
        for t in 1..track {
            index += sectors_per_track(t) as usize;
        }
        index += sector as usize;
        Ok(index)
    }

    pub fn read_sector(&self, track: u8, sector: u8) -> Result<&[u8], Error> {
        let index = self.sector_index(track, sector)?;
        let start = index * SECTOR_SIZE;
        Ok(&self.sectors[start..start + SECTOR_SIZE])
    }

    pub fn write_sector(
        &mut self,
        track: u8,
        sector: u8,
        data: &[u8; SECTOR_SIZE],
    ) -> Result<(), Error> {
        let index = self.sector_index(track, sector)?;
        let start = index * SECTOR_SIZE;
        self.sectors[start..start + SECTOR_SIZE].copy_from_slice(data);
        Ok(())
    }

    /// The real drive error-code byte recorded for this sector, if this
    /// image carries per-sector error info at all (`Ok(None)` otherwise --
    /// not itself an error, just "this image has no error channel").
    pub fn sector_error(&self, track: u8, sector: u8) -> Result<Option<u8>, Error> {
        let index = self.sector_index(track, sector)?;
        Ok(self.errors.as_ref().map(|e| e[index]))
    }

    fn bam(&self) -> &[u8] {
        self.read_sector(BAM_TRACK, BAM_SECTOR)
            .expect("BAM track/sector is always in range for any valid Variant")
    }

    pub fn dos_version(&self) -> u8 {
        self.bam()[BAM_OFFSET_DOS_VERSION]
    }

    pub fn disk_name(&self) -> [u8; 16] {
        let bam = self.bam();
        let mut name = [0u8; 16];
        name.copy_from_slice(&bam[BAM_OFFSET_NAME..BAM_OFFSET_NAME + 16]);
        name
    }

    pub fn disk_id(&self) -> [u8; 2] {
        let bam = self.bam();
        [bam[BAM_OFFSET_ID], bam[BAM_OFFSET_ID + 1]]
    }

    pub fn dos_type(&self) -> [u8; 2] {
        let bam = self.bam();
        [bam[BAM_OFFSET_DOS_TYPE], bam[BAM_OFFSET_DOS_TYPE + 1]]
    }

    /// One track's real free-sector-count + 3-byte used/free bitmap
    /// group, straight from the BAM -- `(free_count, bitmap)`. Tracks
    /// 1-35 live in the standard `TRACK_BITMAP` area; tracks 36-40 (the
    /// 40-track extension only) live in the separate `EXT_BITMAP` area
    /// real hardware uses since the standard area has no room for them
    /// (see this module's own doc comment on `BAM_OFFSET_EXT_BITMAP`).
    /// `None` if `track` is out of this image's own range.
    pub fn track_bam_group(&self, track: u8) -> Option<(u8, [u8; 3])> {
        if track < 1 || track > self.variant.track_count() {
            return None;
        }
        let bam = self.bam();
        let base = if track <= 35 {
            BAM_OFFSET_TRACK_BITMAP + (track as usize - 1) * 4
        } else {
            BAM_OFFSET_EXT_BITMAP + (track as usize - 36) * 4
        };
        Some((bam[base], [bam[base + 1], bam[base + 2], bam[base + 3]]))
    }

    /// Walks the real directory sector chain starting from the BAM's own
    /// stored link (conventionally, but not necessarily, `DIR_TRACK`/
    /// `DIR_SECTOR`) and returns every non-empty slot found. A slot with
    /// a `0x00` file-type byte is real DOS's own convention for "unused",
    /// skipped here the same way -- distinguishing "never used" from
    /// "deleted" isn't possible from the type byte alone and isn't
    /// attempted. Guards against a corrupt/circular chain (a sector
    /// pointing back to one already visited) by tracking visited
    /// `(track, sector)` pairs, stopping rather than looping forever --
    /// real hardware has no such guard (a truly circular chain would hang
    /// a real drive too), but a parser has no reason to hang on bad input.
    pub fn directory_entries(&self) -> Vec<DirectoryEntry> {
        let mut out = Vec::new();
        let bam = self.bam();
        let mut track = bam[BAM_OFFSET_DIR_LINK_TRACK];
        let mut sector = bam[BAM_OFFSET_DIR_LINK_SECTOR];
        let mut visited = std::collections::HashSet::new();

        while track != 0 {
            if !visited.insert((track, sector)) {
                break;
            }
            let Ok(dir_sector) = self.read_sector(track, sector) else {
                break;
            };
            let next_track = dir_sector[0];
            let next_sector = dir_sector[1];

            for slot in 0..DIR_SLOTS_PER_SECTOR {
                let base = slot * DIR_SLOT_SIZE;
                let file_type = dir_sector[base + SLOT_TYPE_OFFSET];
                if file_type == 0 {
                    continue;
                }
                let mut name = [0u8; SLOT_NAME_LEN];
                name.copy_from_slice(
                    &dir_sector[base + SLOT_NAME_OFFSET..base + SLOT_NAME_OFFSET + SLOT_NAME_LEN],
                );
                out.push(DirectoryEntry {
                    file_type,
                    track: dir_sector[base + SLOT_TRACK_OFFSET],
                    sector: dir_sector[base + SLOT_SECTOR_OFFSET],
                    name,
                    size_blocks: u16::from_le_bytes([
                        dir_sector[base + SLOT_SIZE_OFFSET],
                        dir_sector[base + SLOT_SIZE_OFFSET + 1],
                    ]),
                });
            }

            track = next_track;
            sector = next_sector;
        }
        out
    }

    /// Reads a file's own real sector chain (starting at `entry.track`/
    /// `entry.sector`, its own first data sector -- not the directory
    /// chain `directory_entries` already walks) and returns its full
    /// content bytes. Same chain-walking shape as `directory_entries`'
    /// own loop, plus the one real, easy-to-get-wrong subtlety a
    /// directory sector never has: a *data* chain's last sector encodes
    /// how many of its 254 payload bytes are actually valid in its own
    /// link's second byte (real hardware fact, confirmed via a
    /// pagetable.com-cited example this session -- a last-sector link of
    /// `$00/$34` means bytes `$02`-`$34` *inclusive* are real data, i.e.
    /// `link_byte - 1` bytes, not `link_byte` or `link_byte - 2`), rather
    /// than every non-final sector's implicit "all 254 bytes are real"
    /// convention `directory_entries` never needed to distinguish. Errors
    /// on a corrupt/circular chain the same guarded way
    /// `directory_entries` already does, rather than hanging.
    pub fn read_file(&self, entry: &DirectoryEntry) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        let mut track = entry.track;
        let mut sector = entry.sector;
        let mut visited = std::collections::HashSet::new();

        while track != 0 {
            if !visited.insert((track, sector)) {
                break;
            }
            let data = self.read_sector(track, sector)?;
            let next_track = data[0];
            let next_sector = data[1];
            if next_track == 0 {
                let last_valid_offset = (next_sector as usize).min(SECTOR_SIZE - 1);
                if last_valid_offset >= 2 {
                    out.extend_from_slice(&data[2..=last_valid_offset]);
                }
                break;
            }
            out.extend_from_slice(&data[2..SECTOR_SIZE]);
            track = next_track;
            sector = next_sector;
        }
        Ok(out)
    }

    /// Finds a loadable entry by real CBM DOS filename-matching rules: a
    /// bare `*` matches the first closed `PRG`-type entry in directory
    /// order (the standard, universally-relied-upon `LOAD"*",8,1` "load
    /// the first program" idiom real disks/fastloaders use) -- `?`
    /// single-character wildcards are a real CBM DOS feature too but
    /// aren't implemented here, an explicit, documented scope cut, not an
    /// oversight. Any other `name` must exactly match a directory entry's
    /// own real (un-padded) name -- real hardware never matches a shorter
    /// given name against a longer stored one, unlike a casual prefix
    /// match.
    pub fn find_program<'e>(
        entries: &'e [DirectoryEntry],
        name: &[u8],
    ) -> Option<&'e DirectoryEntry> {
        if name == b"*" {
            return entries
                .iter()
                .find(|e| e.kind() == FILE_TYPE_PRG && e.is_closed());
        }
        entries.iter().find(|e| {
            let real_len = e
                .name
                .iter()
                .position(|&b| b == PETSCII_PAD)
                .unwrap_or(e.name.len());
            &e.name[..real_len] == name
        })
    }
}

pub use cbm_dos::SaveError;

impl D64 {
    /// A freshly formatted, empty disk with the given ID: everything free except the BAM sector and
    /// the first directory sector (the disk name is blank; see [`D64::set_disk_name`]).
    pub fn blank_formatted(variant: Variant, id: [u8; 2]) -> D64 {
        use cbm_dos::Image;
        let size = match variant {
            Variant::Track35 => TRACK35_SIZE,
            Variant::Track40 => TRACK40_SIZE,
        };
        let mut disk = D64 {
            variant,
            sectors: vec![0u8; size],
            errors: None,
        };
        let mut bam = [0u8; SECTOR_SIZE];
        bam[BAM_OFFSET_DIR_LINK_TRACK] = DIR_TRACK;
        bam[BAM_OFFSET_DIR_LINK_SECTOR] = DIR_SECTOR;
        bam[BAM_OFFSET_DOS_VERSION] = b'A';
        bam[BAM_OFFSET_NAME..BAM_OFFSET_NAME + 16].fill(PETSCII_PAD);
        bam[BAM_OFFSET_NAME + 16] = PETSCII_PAD;
        bam[BAM_OFFSET_NAME + 17] = PETSCII_PAD;
        bam[BAM_OFFSET_ID] = id[0];
        bam[BAM_OFFSET_ID + 1] = id[1];
        bam[BAM_OFFSET_ID + 2] = PETSCII_PAD;
        bam[BAM_OFFSET_DOS_TYPE] = b'2';
        bam[BAM_OFFSET_DOS_TYPE + 1] = b'A';
        bam[BAM_OFFSET_DOS_TYPE + 2..BAM_OFFSET_DOS_TYPE + 6].fill(PETSCII_PAD);
        let _ = disk.write_sector(BAM_TRACK, BAM_SECTOR, &bam);
        for track in 1..=variant.track_count() {
            for sector in 0..sectors_per_track(track) {
                disk.mark(track, sector, true);
            }
        }
        disk.mark(BAM_TRACK, BAM_SECTOR, false);
        disk.mark(DIR_TRACK, DIR_SECTOR, false);
        let mut dir = [0u8; SECTOR_SIZE];
        dir[1] = 0xFF;
        let _ = disk.write_sector(DIR_TRACK, DIR_SECTOR, &dir);
        disk
    }

    /// Sets the disk name (at most 16 PETSCII characters, padded with `$A0`).
    pub fn set_disk_name(&mut self, name: &[u8]) {
        let mut bam = [0u8; SECTOR_SIZE];
        bam.copy_from_slice(self.bam());
        bam[BAM_OFFSET_NAME..BAM_OFFSET_NAME + 16].fill(PETSCII_PAD);
        let n = name.len().min(16);
        bam[BAM_OFFSET_NAME..BAM_OFFSET_NAME + n].copy_from_slice(&name[..n]);
        let _ = self.write_sector(BAM_TRACK, BAM_SECTOR, &bam);
    }

    /// The image as an `.x64` file: the 64-byte header (magic, header version 1.2, device 1541, track
    /// count) in front of the plain `.d64` bytes.
    pub fn to_x64_bytes(&self) -> Vec<u8> {
        let mut out = X64_MAGIC.to_vec();
        out.extend_from_slice(&[1, 2, 0, self.variant.track_count()]);
        out.resize(X64_HEADER_SIZE, 0);
        out.extend_from_slice(&self.to_bytes());
        out
    }

    fn bam_group_base(&self, track: u8) -> Option<usize> {
        if track < 1 || track > self.variant.track_count() {
            None
        } else if track <= 35 {
            Some(BAM_OFFSET_TRACK_BITMAP + (track as usize - 1) * 4)
        } else {
            Some(BAM_OFFSET_EXT_BITMAP + (track as usize - 36) * 4)
        }
    }

    /// Stores `data` (a PRG: the two load-address bytes followed by the
    /// program) as a closed PRG file called `name`, the way the 1541 would
    /// for `SAVE`: sectors allocated from the BAM, the chain written, a
    /// directory slot filled. With `replace` an existing file of the same
    /// name is scratched first (the `@0:` form), otherwise it is an error.
    /// The image is left untouched on failure. The file system work is
    /// shared with `.d71`/`.d81` in the `cbm-dos` crate.
    pub fn save_program(
        &mut self,
        name: &[u8],
        data: &[u8],
        replace: bool,
    ) -> Result<(), SaveError> {
        cbm_dos::save_program(self, name, data, replace)
    }
}

impl cbm_dos::Image for D64 {
    fn read(&self, track: u8, sector: u8) -> Option<[u8; SECTOR_SIZE]> {
        let data = self.read_sector(track, sector).ok()?;
        let mut out = [0u8; SECTOR_SIZE];
        out.copy_from_slice(data);
        Some(out)
    }

    fn write(&mut self, track: u8, sector: u8, data: &[u8; SECTOR_SIZE]) -> bool {
        self.write_sector(track, sector, data).is_ok()
    }

    fn sectors_per_track(&self, track: u8) -> u8 {
        if track <= self.variant.track_count() {
            sectors_per_track(track)
        } else {
            0
        }
    }

    fn is_free(&self, track: u8, sector: u8) -> bool {
        self.bam_group_base(track).is_some_and(|base| {
            self.bam()[base + 1 + sector as usize / 8] & (1 << (sector % 8)) != 0
        })
    }

    fn mark(&mut self, track: u8, sector: u8, free: bool) {
        let Some(base) = self.bam_group_base(track) else {
            return;
        };
        let mut bam = [0u8; SECTOR_SIZE];
        bam.copy_from_slice(self.bam());
        let byte = base + 1 + sector as usize / 8;
        let bit = 1 << (sector % 8);
        let was_free = bam[byte] & bit != 0;
        if free && !was_free {
            bam[byte] |= bit;
            bam[base] = bam[base].saturating_add(1);
        } else if !free && was_free {
            bam[byte] &= !bit;
            bam[base] = bam[base].saturating_sub(1);
        }
        let _ = self.write_sector(BAM_TRACK, BAM_SECTOR, &bam);
    }

    fn directory_start(&self) -> (u8, u8) {
        let bam = self.bam();
        (
            bam[BAM_OFFSET_DIR_LINK_TRACK],
            bam[BAM_OFFSET_DIR_LINK_SECTOR],
        )
    }

    fn directory_track(&self) -> u8 {
        BAM_TRACK
    }

    fn allocation_order(&self) -> Vec<u8> {
        // Outward from the directory track: 17 down to 1, then 19 up.
        (1..BAM_TRACK)
            .rev()
            .chain(BAM_TRACK + 1..=self.variant.track_count())
            .collect()
    }

    fn file_interleave(&self) -> u8 {
        10
    }

    fn directory_interleave(&self) -> u8 {
        3
    }

    fn system_sectors(&self) -> Vec<(u8, u8)> {
        vec![(BAM_TRACK, BAM_SECTOR)]
    }
}

/// Real CBM DOS PRG file-type value (`DirectoryEntry::kind()`).
pub const FILE_TYPE_PRG: u8 = 2;
/// Real CBM DOS directory-name padding byte (PETSCII shifted space, not
/// ASCII space) -- confirmed alongside the slot layout constants above.
const PETSCII_PAD: u8 = 0xA0;

#[cfg(test)]
mod tests {
    use super::*;

    fn blank_track35() -> Vec<u8> {
        vec![0u8; TRACK35_SIZE]
    }

    #[test]
    fn x64_is_a_64_byte_header_in_front_of_a_d64() {
        let mut bytes = X64_MAGIC.to_vec();
        bytes.resize(X64_HEADER_SIZE, 0);
        let mut body = blank_track35();
        body[0] = 0xAB;
        bytes.extend_from_slice(&body);
        let disk = D64::parse_x64(&bytes).expect("x64 parses");
        assert_eq!(disk.to_bytes(), body);
        bytes[0] = 0;
        assert_eq!(D64::parse_x64(&bytes).unwrap_err(), Error::BadX64Magic);
        assert_eq!(D64::parse_x64(&[1, 2]).unwrap_err(), Error::BadX64Magic);
    }

    fn sectors_per_track_matches_the_real_speed_zone_table() {
        assert_eq!(sectors_per_track(1), 21);
        assert_eq!(sectors_per_track(17), 21);
        assert_eq!(sectors_per_track(18), 19);
        assert_eq!(sectors_per_track(24), 19);
        assert_eq!(sectors_per_track(25), 18);
        assert_eq!(sectors_per_track(30), 18);
        assert_eq!(sectors_per_track(31), 17);
        assert_eq!(sectors_per_track(40), 17);
        assert_eq!(sectors_per_track(0), 0);
        assert_eq!(sectors_per_track(41), 0);
    }

    fn parse_recognizes_all_four_real_size_variants() {
        assert_eq!(
            D64::parse(&vec![0u8; TRACK35_SIZE]).unwrap().variant(),
            Variant::Track35
        );
        assert!(
            !D64::parse(&vec![0u8; TRACK35_SIZE])
                .unwrap()
                .has_error_info()
        );
        assert!(
            D64::parse(&vec![0u8; TRACK35_SIZE_WITH_ERRORS])
                .unwrap()
                .has_error_info()
        );
        assert_eq!(
            D64::parse(&vec![0u8; TRACK40_SIZE]).unwrap().variant(),
            Variant::Track40
        );
        assert!(
            D64::parse(&vec![0u8; TRACK40_SIZE_WITH_ERRORS])
                .unwrap()
                .has_error_info()
        );
    }

    fn parse_rejects_an_unrecognized_size() {
        let err = D64::parse(&vec![0u8; 12345]).unwrap_err();
        assert_eq!(err, Error::InvalidSize(12345));
    }
    /// Merged (every check kept): `sectors_per_track_matches_the_real_speed_zone_table`, `parse_recognizes_all_four_real_size_variants`, `parse_rejects_an_unrecognized_size`.
    #[test]
    fn geometry_and_parsing() {
        sectors_per_track_matches_the_real_speed_zone_table();
        parse_recognizes_all_four_real_size_variants();
        parse_rejects_an_unrecognized_size();
    }

    fn read_write_sector_round_trips_and_rejects_out_of_range() {
        let mut disk = D64::parse(&blank_track35()).unwrap();
        let mut data = [0u8; SECTOR_SIZE];
        data[0] = 0xAB;
        data[255] = 0xCD;
        disk.write_sector(1, 0, &data).unwrap();
        assert_eq!(disk.read_sector(1, 0).unwrap(), &data[..]);

        assert_eq!(
            disk.read_sector(36, 0).unwrap_err(),
            Error::TrackOutOfRange(36)
        );
        assert_eq!(
            disk.read_sector(1, 21).unwrap_err(),
            Error::SectorOutOfRange {
                track: 1,
                sector: 21
            }
        );
        // Track 18 has 19 real sectors, not 21 -- a real, easy mistake if
        // the zone table weren't actually consulted per-track.
        assert!(disk.read_sector(18, 18).is_ok());
        assert_eq!(
            disk.read_sector(18, 19).unwrap_err(),
            Error::SectorOutOfRange {
                track: 18,
                sector: 19
            }
        );
    }

    fn sector_error_reports_none_without_error_info_and_the_real_byte_with_it() {
        let disk = D64::parse(&blank_track35()).unwrap();
        assert_eq!(disk.sector_error(1, 0).unwrap(), None);

        let mut bytes = blank_track35();
        bytes.extend(vec![0u8; TRACK35_SECTORS]);
        bytes[TRACK35_SIZE] = 5; // error byte for sector index 0 (track1/sector0)
        let disk = D64::parse(&bytes).unwrap();
        assert_eq!(disk.sector_error(1, 0).unwrap(), Some(5));
        assert_eq!(disk.sector_error(1, 1).unwrap(), Some(0));
    }

    fn to_bytes_round_trips_exactly() {
        let mut bytes = blank_track35();
        bytes[0x1234] = 0x42;
        let disk = D64::parse(&bytes).unwrap();
        assert_eq!(disk.to_bytes(), bytes);

        let mut bytes_with_errors = bytes.clone();
        bytes_with_errors.extend(vec![7u8; TRACK35_SECTORS]);
        let disk = D64::parse(&bytes_with_errors).unwrap();
        assert_eq!(disk.to_bytes(), bytes_with_errors);
    }
    /// Merged (every check kept): `read_write_sector_round_trips_and_rejects_out_of_range`, `sector_error_reports_none_without_error_info_and_the_real_byte_with_it`, `to_bytes_round_trips_exactly`.
    #[test]
    fn sector_io() {
        read_write_sector_round_trips_and_rejects_out_of_range();
        sector_error_reports_none_without_error_info_and_the_real_byte_with_it();
        to_bytes_round_trips_exactly();
    }

    /// A hand-built minimal BAM, matching real values a genuine 1541-
    /// formatted disk would have -- confirms `disk_name`/`disk_id`/
    /// `dos_type` read from the exact offsets confirmed against VICE's
    /// own source, not the more "obvious" but wrong offsets an
    /// unverified guess might have used.
    fn bam_fields_read_from_the_confirmed_real_offsets() {
        let mut bytes = blank_track35();
        let bam_index = {
            // track 18 sector 0 is the very first sector of track 18
            let mut idx = 0usize;
            for t in 1..18 {
                idx += sectors_per_track(t) as usize;
            }
            idx
        };
        let bam = &mut bytes[bam_index * SECTOR_SIZE..bam_index * SECTOR_SIZE + SECTOR_SIZE];
        bam[BAM_OFFSET_DIR_LINK_TRACK] = 18;
        bam[BAM_OFFSET_DIR_LINK_SECTOR] = 1;
        bam[BAM_OFFSET_DOS_VERSION] = b'A';
        bam[BAM_OFFSET_NAME..BAM_OFFSET_NAME + 16]
            .copy_from_slice(b"MY DISK \xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0");
        bam[BAM_OFFSET_ID] = b'1';
        bam[BAM_OFFSET_ID + 1] = b'A';
        bam[BAM_OFFSET_DOS_TYPE] = b'2';
        bam[BAM_OFFSET_DOS_TYPE + 1] = b'A';

        let disk = D64::parse(&bytes).unwrap();
        assert_eq!(disk.dos_version(), b'A');
        assert_eq!(
            &disk.disk_name(),
            b"MY DISK \xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0"
        );
        assert_eq!(disk.disk_id(), [b'1', b'A']);
        assert_eq!(disk.dos_type(), [b'2', b'A']);
    }

    fn directory_entries_reads_from_slot_relative_offset_two_not_zero() {
        let mut bytes = blank_track35();
        let bam_index = {
            let mut idx = 0usize;
            for t in 1..18 {
                idx += sectors_per_track(t) as usize;
            }
            idx
        };
        bytes[bam_index * SECTOR_SIZE + BAM_OFFSET_DIR_LINK_TRACK] = DIR_TRACK;
        bytes[bam_index * SECTOR_SIZE + BAM_OFFSET_DIR_LINK_SECTOR] = DIR_SECTOR;

        let dir_index = bam_index + DIR_SECTOR as usize;
        let dir = &mut bytes[dir_index * SECTOR_SIZE..dir_index * SECTOR_SIZE + SECTOR_SIZE];
        dir[0] = 0; // end of chain
        dir[1] = 0xFF;
        // Slot 0: a closed PRG file named "PROGRAM", 5 blocks, first data
        // block at 2/3.
        dir[SLOT_TYPE_OFFSET] = 0x80 | 0x02;
        dir[SLOT_TRACK_OFFSET] = 2;
        dir[SLOT_SECTOR_OFFSET] = 3;
        dir[SLOT_NAME_OFFSET..SLOT_NAME_OFFSET + 16]
            .copy_from_slice(b"PROGRAM\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0");
        dir[SLOT_SIZE_OFFSET] = 5;
        dir[SLOT_SIZE_OFFSET + 1] = 0;
        // Slot 1: left all-zero (file_type == 0) -- must be skipped.

        let disk = D64::parse(&bytes).unwrap();
        let entries = disk.directory_entries();
        assert_eq!(entries.len(), 1, "the all-zero slot must be skipped");
        let e = &entries[0];
        assert!(e.is_closed());
        assert!(!e.is_locked());
        assert_eq!(e.kind(), 2, "PRG");
        assert_eq!(e.track, 2);
        assert_eq!(e.sector, 3);
        assert_eq!(e.size_blocks, 5);
        assert_eq!(&e.name[..7], b"PROGRAM");
    }

    fn directory_chain_stops_on_a_circular_link_instead_of_hanging() {
        let mut bytes = blank_track35();
        let bam_index = {
            let mut idx = 0usize;
            for t in 1..18 {
                idx += sectors_per_track(t) as usize;
            }
            idx
        };
        bytes[bam_index * SECTOR_SIZE + BAM_OFFSET_DIR_LINK_TRACK] = DIR_TRACK;
        bytes[bam_index * SECTOR_SIZE + BAM_OFFSET_DIR_LINK_SECTOR] = DIR_SECTOR;

        let dir_index = bam_index + DIR_SECTOR as usize;
        let dir = &mut bytes[dir_index * SECTOR_SIZE..dir_index * SECTOR_SIZE + SECTOR_SIZE];
        // Points right back at itself instead of ending the chain.
        dir[0] = DIR_TRACK;
        dir[1] = DIR_SECTOR;

        let disk = D64::parse(&bytes).unwrap();
        // Must return (empty, all slots are file_type==0) rather than hang.
        assert_eq!(disk.directory_entries().len(), 0);
    }
    /// Merged (every check kept): `directory_entries_reads_from_slot_relative_offset_two_not_zero`, `directory_chain_stops_on_a_circular_link_instead_of_hanging`.
    #[test]
    fn directory_reading() {
        directory_entries_reads_from_slot_relative_offset_two_not_zero();
        directory_chain_stops_on_a_circular_link_instead_of_hanging();
    }

    fn track_bam_group_reads_the_correct_area_for_standard_and_extended_tracks() {
        let mut bytes = blank_track35();
        let bam_index = {
            let mut idx = 0usize;
            for t in 1..18 {
                idx += sectors_per_track(t) as usize;
            }
            idx
        };
        let bam = &mut bytes[bam_index * SECTOR_SIZE..bam_index * SECTOR_SIZE + SECTOR_SIZE];
        // Track 1's group is the very first 4 bytes of the bitmap area.
        bam[BAM_OFFSET_TRACK_BITMAP] = 21; // free count
        bam[BAM_OFFSET_TRACK_BITMAP + 1] = 0xFF;
        bam[BAM_OFFSET_TRACK_BITMAP + 2] = 0xFF;
        bam[BAM_OFFSET_TRACK_BITMAP + 3] = 0x1F;

        let disk = D64::parse(&bytes).unwrap();
        assert_eq!(disk.track_bam_group(1), Some((21, [0xFF, 0xFF, 0x1F])));
        assert_eq!(
            disk.track_bam_group(36),
            None,
            "track 36 doesn't exist on a 35-track image"
        );
        assert_eq!(disk.track_bam_group(0), None);
    }
    /// Merged (every check kept): `bam_fields_read_from_the_confirmed_real_offsets`, `track_bam_group_reads_the_correct_area_for_standard_and_extended_tracks`.
    #[test]
    fn bam_reading() {
        bam_fields_read_from_the_confirmed_real_offsets();
        track_bam_group_reads_the_correct_area_for_standard_and_extended_tracks();
    }

    fn track_sector_byte_offset(track: u8, sector: u8) -> usize {
        let mut idx = 0usize;
        for t in 1..track {
            idx += sectors_per_track(t) as usize;
        }
        idx += sector as usize;
        idx * SECTOR_SIZE
    }

    fn make_entry(track: u8, sector: u8) -> DirectoryEntry {
        DirectoryEntry {
            file_type: 0x80 | FILE_TYPE_PRG,
            track,
            sector,
            name: *b"PROGRAM\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0",
            size_blocks: 1,
        }
    }

    fn read_file_returns_exactly_the_valid_bytes_of_a_single_sector_file() {
        let mut bytes = blank_track35();
        let off = track_sector_byte_offset(2, 3);
        bytes[off] = 0; // end of chain
        bytes[off + 1] = 0x34; // last valid offset -- 0x02..=0x34 inclusive is real data
        for i in 2..=0x34usize {
            bytes[off + i] = i as u8;
        }
        // Bytes past the real end are padding garbage that must NOT be returned.
        bytes[off + 0x35] = 0xEE;

        let disk = D64::parse(&bytes).unwrap();
        let entry = make_entry(2, 3);
        let data = disk.read_file(&entry).unwrap();
        assert_eq!(data.len(), 0x34 - 2 + 1);
        assert_eq!(data[0], 2);
        assert_eq!(*data.last().unwrap(), 0x34);
    }

    fn read_file_walks_a_real_multi_sector_chain() {
        let mut bytes = blank_track35();
        let first = track_sector_byte_offset(2, 3);
        bytes[first] = 2; // next: track 2
        bytes[first + 1] = 4; // sector 4
        for i in 2..SECTOR_SIZE {
            bytes[first + i] = 0xAA;
        }
        let second = track_sector_byte_offset(2, 4);
        bytes[second] = 0; // end of chain
        bytes[second + 1] = 5; // bytes 2..=5 inclusive are real (4 bytes)
        bytes[second + 2] = 1;
        bytes[second + 3] = 2;
        bytes[second + 4] = 3;
        bytes[second + 5] = 4;

        let disk = D64::parse(&bytes).unwrap();
        let entry = make_entry(2, 3);
        let data = disk.read_file(&entry).unwrap();
        assert_eq!(data.len(), (SECTOR_SIZE - 2) + 4);
        assert!(data[..SECTOR_SIZE - 2].iter().all(|&b| b == 0xAA));
        assert_eq!(&data[SECTOR_SIZE - 2..], &[1, 2, 3, 4]);
    }

    fn read_file_stops_on_a_circular_chain_instead_of_hanging() {
        let mut bytes = blank_track35();
        let off = track_sector_byte_offset(2, 3);
        // Points right back at itself instead of ending the chain.
        bytes[off] = 2;
        bytes[off + 1] = 3;

        let disk = D64::parse(&bytes).unwrap();
        let entry = make_entry(2, 3);
        // Must return without hanging; content is unspecified for a corrupt chain.
        let _ = disk.read_file(&entry).unwrap();
    }
    /// Merged (every check kept): `read_file_returns_exactly_the_valid_bytes_of_a_single_sector_file`, `read_file_walks_a_real_multi_sector_chain`, `read_file_stops_on_a_circular_chain_instead_of_hanging`.
    #[test]
    fn read_file_cases() {
        read_file_returns_exactly_the_valid_bytes_of_a_single_sector_file();
        read_file_walks_a_real_multi_sector_chain();
        read_file_stops_on_a_circular_chain_instead_of_hanging();
    }

    fn find_program_wildcard_matches_the_first_closed_prg_entry() {
        let entries = vec![
            DirectoryEntry {
                file_type: 0x80, // DEL (kind 0), not PRG -- must be skipped
                track: 1,
                sector: 0,
                name: *b"JUNK\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0",
                size_blocks: 1,
            },
            make_entry(2, 3),
        ];
        let found = D64::find_program(&entries, b"*").unwrap();
        assert_eq!((found.track, found.sector), (2, 3));
    }

    fn find_program_matches_the_real_unpadded_name_exactly() {
        let entries = vec![make_entry(2, 3)];
        assert!(D64::find_program(&entries, b"PROGRAM").is_some());
        assert!(
            D64::find_program(&entries, b"PROG").is_none(),
            "a shorter given name must not match a longer stored one"
        );
        assert!(D64::find_program(&entries, b"OTHER").is_none());
    }
    /// Merged (every check kept): `find_program_wildcard_matches_the_first_closed_prg_entry`, `find_program_matches_the_real_unpadded_name_exactly`.
    #[test]
    fn find_program_cases() {
        find_program_wildcard_matches_the_first_closed_prg_entry();
        find_program_matches_the_real_unpadded_name_exactly();
    }

    /// A freshly formatted 35-track disk: every sector free except the BAM
    /// and the first directory sector.
    fn formatted() -> D64 {
        let mut disk = D64::parse(&blank_track35()).unwrap();
        let mut bam = [0u8; SECTOR_SIZE];
        bam[0] = BAM_TRACK;
        bam[1] = DIR_SECTOR;
        bam[2] = 0x41;
        for track in 1..=35u8 {
            let base = BAM_OFFSET_TRACK_BITMAP + (track as usize - 1) * 4;
            let per_track = sectors_per_track(track);
            for sector in 0..per_track {
                bam[base + 1 + sector as usize / 8] |= 1 << (sector % 8);
            }
            bam[base] = per_track;
        }
        disk.write_sector(BAM_TRACK, BAM_SECTOR, &bam).unwrap();
        cbm_dos::Image::mark(&mut disk, BAM_TRACK, BAM_SECTOR, false);
        cbm_dos::Image::mark(&mut disk, BAM_TRACK, DIR_SECTOR, false);
        let mut dir = [0u8; SECTOR_SIZE];
        dir[1] = 0xFF;
        disk.write_sector(BAM_TRACK, DIR_SECTOR, &dir).unwrap();
        disk
    }

    fn free_sectors(disk: &D64) -> u32 {
        (1..=35u8)
            .filter(|&t| t != BAM_TRACK)
            .map(|t| u32::from(disk.track_bam_group(t).unwrap().0))
            .sum()
    }

    fn read_back(disk: &D64, name: &[u8]) -> Option<Vec<u8>> {
        let entries = disk.directory_entries();
        let entry = D64::find_program(&entries, name)?;
        disk.read_file(entry).ok()
    }

    #[test]
    fn a_saved_program_reads_back_and_its_sectors_leave_the_bam() {
        let mut disk = formatted();
        let before = free_sectors(&disk);
        let program: Vec<u8> = (0..700u32).map(|i| (i % 251) as u8).collect();
        disk.save_program(b"HELLO", &program, false).unwrap();
        assert_eq!(read_back(&disk, b"HELLO").unwrap(), program);
        // 700 bytes = 3 data sectors of 254.
        assert_eq!(before - free_sectors(&disk), 3);
        let entry = disk.directory_entries()[0];
        assert_eq!((entry.file_type, entry.size_blocks), (0x82, 3));
        // Track 18 is never used for file data; free counts match bitmaps.
        for track in 1..=35u8 {
            let (count, bitmap) = disk.track_bam_group(track).unwrap();
            let bits: u32 = bitmap.iter().map(|b| b.count_ones()).sum();
            assert_eq!(u32::from(count), bits, "track {track}");
        }
    }

    fn a_one_byte_and_an_empty_payload_each_take_one_sector() {
        let mut disk = formatted();
        disk.save_program(b"ONE", &[7], false).unwrap();
        disk.save_program(b"NONE", &[], false).unwrap();
        assert_eq!(read_back(&disk, b"ONE").unwrap(), vec![7]);
        assert_eq!(read_back(&disk, b"NONE").unwrap(), Vec::<u8>::new());
    }

    fn saving_over_an_existing_name_needs_replace_and_frees_the_old_chain() {
        let mut disk = formatted();
        disk.save_program(b"GAME", &[1; 600], false).unwrap();
        let used = free_sectors(&disk);
        assert_eq!(
            disk.save_program(b"GAME", &[2; 10], false),
            Err(SaveError::FileExists)
        );
        assert_eq!(read_back(&disk, b"GAME").unwrap(), vec![1; 600]);
        assert_eq!(free_sectors(&disk), used);

        disk.save_program(b"GAME", &[2; 10], true).unwrap();
        assert_eq!(read_back(&disk, b"GAME").unwrap(), vec![2; 10]);
        assert_eq!(disk.directory_entries().len(), 1);
        assert_eq!(free_sectors(&disk), used + 3 - 1);
    }

    fn a_full_directory_sector_grows_the_directory_chain() {
        let mut disk = formatted();
        for i in 0..10u8 {
            disk.save_program(&[b'F', b'0' + i], &[i], false).unwrap();
        }
        assert_eq!(disk.directory_entries().len(), 10);
        for i in 0..10u8 {
            assert_eq!(read_back(&disk, &[b'F', b'0' + i]).unwrap(), vec![i]);
        }
    }

    fn a_full_disk_is_an_error_and_leaves_the_image_unchanged() {
        let mut disk = formatted();
        let free = free_sectors(&disk) as usize;
        let too_big = vec![0u8; (free + 1) * 254];
        let before = disk.to_bytes();
        assert_eq!(
            disk.save_program(b"BIG", &too_big, false),
            Err(SaveError::DiskFull)
        );
        assert_eq!(disk.to_bytes(), before);
        // Exactly the free space does fit.
        disk.save_program(b"FIT", &too_big[..free * 254], false)
            .unwrap();
        assert_eq!(free_sectors(&disk), 0);
    }
    /// Merged (every check kept): `a_full_directory_sector_grows_the_directory_chain`, `a_full_disk_is_an_error_and_leaves_the_image_unchanged`.
    #[test]
    fn save_program_limits() {
        a_full_directory_sector_grows_the_directory_chain();
        a_full_disk_is_an_error_and_leaves_the_image_unchanged();
    }

    fn names_must_be_one_to_sixteen_bytes() {
        let mut disk = formatted();
        assert_eq!(
            disk.save_program(b"", &[1], false),
            Err(SaveError::InvalidName)
        );
        assert_eq!(
            disk.save_program(&[b'A'; 17], &[1], false),
            Err(SaveError::InvalidName)
        );
        disk.save_program(&[b'A'; 16], &[1], false).unwrap();
    }
    /// Merged (every check kept): `a_one_byte_and_an_empty_payload_each_take_one_sector`, `saving_over_an_existing_name_needs_replace_and_frees_the_old_chain`, `names_must_be_one_to_sixteen_bytes`.
    #[test]
    fn save_program_cases() {
        a_one_byte_and_an_empty_payload_each_take_one_sector();
        saving_over_an_existing_name_needs_replace_and_frees_the_old_chain();
        names_must_be_one_to_sixteen_bytes();
    }
}
