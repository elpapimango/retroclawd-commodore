//! Parses a Commodore `.d71` disk image -- the standard 1571 disk format.
//! No dependency on `machine`, any CPU crate, or `c1541`/`c64` -- same
//! "format crate only parses bytes into a plain struct" precedent as the
//! sibling `d64` crate (and `z80-snapshot`/`zx81-snapshot`/`zx-tape`/`dsk`
//! before it): this is a fact about the file format, not about how a
//! drive's real GCR hardware consumes it.
//!
//! **Real hardware fact this format encodes**: a 1571 is electrically and
//! mechanically a double-sided 1541 -- the exact same per-side speed-zone
//! layout `d64` already documents (21/19/18/17 sectors/track across 4
//! zones) is simply repeated for a second, physically flipped side. Side 1
//! (tracks 1-35) is byte-for-byte the same DOS layout a real `.d64` uses,
//! including its own BAM/header sector (track 18, sector 0) and directory
//! chain (starting at track 18, sector 1) -- a `.d71` is not a distinct DOS
//! format, just a `.d64`-shaped side 1 plus a second physical side appended
//! after it. Side 2 (tracks 36-70) continues the identical 4-zone pattern
//! (tracks 36-52 = 21 sectors, 53-59 = 19, 60-65 = 18, 66-70 = 17), and gets
//! its own second BAM sector at track 53 sector 0 (`BAM_TRACK_1571 +
//! NUM_TRACKS_1571/2`, `BAM_SECTOR_1571` unchanged) -- real hardware fact,
//! not a guess: the first BAM sector's own 35-track bitmap area (inherited
//! unchanged from the 1541 DOS layout) has no room for 35 more tracks'
//! groups, so the second side needs a sector of its own.
//!
//! Every geometry and byte-offset constant below was confirmed directly
//! against VICE's own real source this session (`diskconstants.h`'s
//! `NUM_TRACKS_1571`/`NUM_BLOCKS_1571`/`DIR_TRACK_1571`/`DIR_SECTOR_1571`/
//! `BAM_TRACK_1571`/`BAM_SECTOR_1571`/`BAM_NAME_1571`/`BAM_ID_1571`, and
//! `vdrive-bam.c`'s own `vdrive_bam_setup_bam` for the second BAM sector's
//! real track/sector) -- see `CREDITS.md` for the exact constants read.
//! `BAM_NAME_1571`/`BAM_ID_1571` (144/162) match `d64`'s own confirmed
//! offsets exactly, which makes sense: side 1's header sector is the same
//! DOS-family layout, not a 1571-specific reinvention.
//!
//! **Deliberately out of scope this pass**: the second side's per-track
//! free-sector bitmap (the `BAM_EXT_BIT_MAP_1571`=221 free-count array in
//! the first BAM sector, plus the actual 3-byte-per-track usage bitmaps in
//! the second BAM sector at track 53/0) is not decoded here -- unlike
//! `d64::D64::track_bam_group`, this crate has no equivalent. Attaching and
//! *reading* a `.d71` (directory listing, file extraction) needs only the
//! real track/sector chain-walking this crate does implement; nothing here
//! allocates space or writes new files, so the free-space bitmap was never
//! load-bearing for that -- and this session couldn't independently
//! reverify the exact per-track byte arithmetic for the second BAM sector
//! against a second source, so it's left undone rather than guessed (same
//! "don't ship unverified byte arithmetic" stance this whole codebase
//! already takes elsewhere).
//!
//! Two known-real size variants are supported, mirroring `d64`'s own two:
//! the standard 70-track image, and the same optional per-sector real
//! drive error-code byte channel. `parse` detects which of the 2 resulting
//! file sizes it's looking at from length alone.

use std::fmt;

pub const SECTOR_SIZE: usize = 256;

/// Real hardware fact (confirmed via VICE's `diskconstants.h`): side 1's
/// BAM/header sector is the exact same track/sector a plain `.d64` uses.
pub const BAM_TRACK: u8 = 18;
pub const BAM_SECTOR: u8 = 0;
pub const DIR_TRACK: u8 = 18;
pub const DIR_SECTOR: u8 = 1;

/// Real hardware fact (confirmed via VICE's `vdrive_bam_setup_bam`): side
/// 2's own BAM sector, holding tracks 36-70's free-count/bitmap data (not
/// decoded by this crate this pass -- see the module doc comment).
pub const SECOND_BAM_TRACK: u8 = 53;
pub const SECOND_BAM_SECTOR: u8 = 0;

// BAM (track 18, sector 0) byte offsets -- identical to `d64`'s own
// confirmed offsets, since side 1's header sector is the same DOS-family
// layout (see module doc comment).
const BAM_OFFSET_DIR_LINK_TRACK: usize = 0;
const BAM_OFFSET_DIR_LINK_SECTOR: usize = 1;
const BAM_OFFSET_DOS_VERSION: usize = 2;
const BAM_OFFSET_NAME: usize = 144;
const BAM_OFFSET_ID: usize = 162;
const BAM_OFFSET_DOS_TYPE: usize = 165;

// Directory entry ("slot") layout -- identical 32-byte CBM DOS format
// `d64` already documents (including the real slot-relative `+2`
// file-type offset), unchanged across the whole DOS family.
const DIR_SLOT_SIZE: usize = 32;
const DIR_SLOTS_PER_SECTOR: usize = SECTOR_SIZE / DIR_SLOT_SIZE;
const SLOT_TYPE_OFFSET: usize = 2;
const SLOT_TRACK_OFFSET: usize = 3;
const SLOT_SECTOR_OFFSET: usize = 4;
const SLOT_NAME_OFFSET: usize = 5;
const SLOT_NAME_LEN: usize = 16;
const SLOT_SIZE_OFFSET: usize = 30;

/// Real hardware fact (confirmed via VICE's `NUM_BLOCKS_1571`): 1366
/// total sectors -- exactly twice `d64`'s own 683 (two identical 4-zone
/// sides), confirming the doubled-track-range zone table below is right.
const TOTAL_SECTORS: usize = 1366;
const IMAGE_SIZE: usize = TOTAL_SECTORS * SECTOR_SIZE;
const IMAGE_SIZE_WITH_ERRORS: usize = IMAGE_SIZE + TOTAL_SECTORS;

/// Real hardware fact: side 2 (tracks 36-70) repeats side 1's own 4-zone
/// pattern exactly, track-for-track (`d64::sectors_per_track`'s own
/// table, applied modulo 35) -- confirmed via VICE's own `sector_map_d64`
/// comment ("tracks 31-35, 66-70 (D71 only)" etc., the same table reused
/// for both formats). `0` for anything outside `1..=70`.
pub fn sectors_per_track(track: u8) -> u8 {
    if !(1..=70).contains(&track) {
        return 0;
    }
    let side_relative = ((track - 1) % 35) + 1;
    match side_relative {
        1..=17 => 21,
        18..=24 => 19,
        25..=30 => 18,
        31..=35 => 17,
        _ => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidSize(usize),
    TrackOutOfRange(u8),
    SectorOutOfRange { track: u8, sector: u8 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidSize(size) => write!(
                f,
                "not a recognized .d71 size ({size} bytes -- expected {IMAGE_SIZE} or \
                 {IMAGE_SIZE_WITH_ERRORS})"
            ),
            Error::TrackOutOfRange(track) => write!(f, "track {track} out of range"),
            Error::SectorOutOfRange { track, sector } => {
                write!(f, "sector {sector} out of range on track {track}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// One parsed directory entry ("slot") -- same real layout `d64::
/// DirectoryEntry` documents, duplicated rather than shared (own shape
/// per format, matching this codebase's own established precedent, e.g.
/// `sna-snapshot`'s `PagingState` vs `z80-snapshot`'s).
#[derive(Debug, Clone, Copy)]
pub struct DirectoryEntry {
    pub file_type: u8,
    pub track: u8,
    pub sector: u8,
    /// Raw PETSCII, `0xA0`-padded.
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
    pub fn kind(&self) -> u8 {
        self.file_type & 0x0F
    }
}

/// A parsed `.d71` disk image: flat track/sector storage plus optional
/// per-sector real drive error-code bytes.
#[derive(Debug, Clone)]
pub struct D71 {
    sectors: Vec<u8>,
    errors: Option<Vec<u8>>,
}

impl D71 {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let has_errors = match bytes.len() {
            IMAGE_SIZE => false,
            IMAGE_SIZE_WITH_ERRORS => true,
            other => return Err(Error::InvalidSize(other)),
        };
        let sectors = bytes[..IMAGE_SIZE].to_vec();
        let errors = has_errors.then(|| bytes[IMAGE_SIZE..].to_vec());
        Ok(Self { sectors, errors })
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
        if !(1..=70).contains(&track) {
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

    pub fn sector_error(&self, track: u8, sector: u8) -> Result<Option<u8>, Error> {
        let index = self.sector_index(track, sector)?;
        Ok(self.errors.as_ref().map(|e| e[index]))
    }

    fn bam(&self) -> &[u8] {
        self.read_sector(BAM_TRACK, BAM_SECTOR)
            .expect("BAM track/sector is always in range for a 70-track image")
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

    /// Walks the real directory sector chain starting from the BAM's own
    /// stored link -- same guarded, circular-chain-safe walk `d64::D64::
    /// directory_entries` already implements, over this format's own
    /// wider track range.
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

    /// Reads a file's own real sector chain and returns its full content
    /// bytes -- same real last-sector-length convention (`link_byte - 1`
    /// valid bytes) `d64::D64::read_file` already documents; a DOS-level
    /// convention, not physical-format-specific, so it applies unchanged
    /// here.
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

    /// Same real CBM DOS filename-matching rules `d64::D64::find_program`
    /// documents (bare `*` = first closed PRG entry; anything else must
    /// exactly match the real, unpadded stored name).
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

/// Where side 2's per-track free-sector counts live in the first BAM sector
/// (one byte per track, tracks 36-70 at `$DD..`), per VICE's
/// `BAM_EXT_BIT_MAP_1571`. Its bitmaps are three bytes per track at the start
/// of the second BAM sector (track 53).
const SIDE2_COUNT_OFFSET: usize = 221;
const SIDE2_BITMAP_BYTES: usize = 3;

impl D71 {
    /// `(count byte location, bitmap location)` as `(track, sector, offset)`.
    fn bam_locations(&self, track: u8) -> Option<[(u8, u8, usize); 2]> {
        match track {
            1..=35 => {
                let entry = 4 + (track as usize - 1) * 4;
                Some([
                    (BAM_TRACK, BAM_SECTOR, entry),
                    (BAM_TRACK, BAM_SECTOR, entry + 1),
                ])
            }
            36..=70 => Some([
                (
                    BAM_TRACK,
                    BAM_SECTOR,
                    SIDE2_COUNT_OFFSET + (track as usize - 36),
                ),
                (
                    SECOND_BAM_TRACK,
                    SECOND_BAM_SECTOR,
                    SIDE2_BITMAP_BYTES * (track as usize - 36),
                ),
            ]),
            _ => None,
        }
    }

    /// A freshly formatted, empty double-sided disk with the given ID:
    /// everything free except the first BAM sector, the first directory
    /// sector and the whole second-side BAM track.
    pub fn blank_formatted(id: [u8; 2]) -> D71 {
        use cbm_dos::Image;
        let mut disk = D71 {
            sectors: vec![0u8; IMAGE_SIZE],
            errors: None,
        };
        let mut bam = [0u8; SECTOR_SIZE];
        bam[0] = DIR_TRACK;
        bam[1] = DIR_SECTOR;
        bam[2] = 0x41;
        bam[3] = 0x80;
        bam[0x90..0xA0].fill(PETSCII_PAD);
        bam[0xA0] = PETSCII_PAD;
        bam[0xA1] = PETSCII_PAD;
        bam[0xA2] = id[0];
        bam[0xA3] = id[1];
        bam[0xA4] = PETSCII_PAD;
        bam[0xA5] = b'2';
        bam[0xA6] = b'A';
        let _ = disk.write_sector(BAM_TRACK, BAM_SECTOR, &bam);
        let _ = disk.write_sector(SECOND_BAM_TRACK, SECOND_BAM_SECTOR, &[0u8; SECTOR_SIZE]);
        for track in 1..=70u8 {
            for sector in 0..sectors_per_track(track) {
                // Entries start "used" with count 0; free each sector.
                disk.mark(track, sector, true);
            }
        }
        for sector in 0..sectors_per_track(SECOND_BAM_TRACK) {
            disk.mark(SECOND_BAM_TRACK, sector, false);
        }
        disk.mark(BAM_TRACK, BAM_SECTOR, false);
        disk.mark(BAM_TRACK, DIR_SECTOR, false);
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

    /// Stores a PRG the way the 1571 would for `SAVE` (see `cbm-dos`).
    pub fn save_program(
        &mut self,
        name: &[u8],
        data: &[u8],
        replace: bool,
    ) -> Result<(), SaveError> {
        cbm_dos::save_program(self, name, data, replace)
    }
}

impl cbm_dos::Image for D71 {
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
        sectors_per_track(track)
    }

    fn is_free(&self, track: u8, sector: u8) -> bool {
        let Some([_, (t, s, off)]) = self.bam_locations(track) else {
            return false;
        };
        self.read_sector(t, s)
            .is_ok_and(|bam| bam[off + sector as usize / 8] & (1 << (sector % 8)) != 0)
    }

    fn mark(&mut self, track: u8, sector: u8, free: bool) {
        let Some([(ct, cs, coff), (bt, bs, boff)]) = self.bam_locations(track) else {
            return;
        };
        let Ok(bitmap) = self.read_sector(bt, bs) else {
            return;
        };
        let mut bitmap_sector = [0u8; SECTOR_SIZE];
        bitmap_sector.copy_from_slice(bitmap);
        let byte = boff + sector as usize / 8;
        let bit = 1 << (sector % 8);
        let was_free = bitmap_sector[byte] & bit != 0;
        if free == was_free {
            return;
        }
        bitmap_sector[byte] ^= bit;
        let _ = self.write_sector(bt, bs, &bitmap_sector);
        let Ok(counts) = self.read_sector(ct, cs) else {
            return;
        };
        let mut count_sector = [0u8; SECTOR_SIZE];
        count_sector.copy_from_slice(counts);
        count_sector[coff] = if free {
            count_sector[coff].saturating_add(1)
        } else {
            count_sector[coff].saturating_sub(1)
        };
        let _ = self.write_sector(ct, cs, &count_sector);
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
        // Side 1 outward from the directory track, then side 2 the same way
        // around its own BAM track (53, never used for files).
        (1..BAM_TRACK)
            .rev()
            .chain(BAM_TRACK + 1..=35)
            .chain((36..SECOND_BAM_TRACK).rev())
            .chain(SECOND_BAM_TRACK + 1..=70)
            .collect()
    }

    fn file_interleave(&self) -> u8 {
        10
    }

    fn directory_interleave(&self) -> u8 {
        3
    }

    fn system_sectors(&self) -> Vec<(u8, u8)> {
        // Side 1's BAM, and the whole of the second BAM track (track 53 is never allocatable).
        let mut out = vec![(BAM_TRACK, BAM_SECTOR)];
        out.extend((0..sectors_per_track(SECOND_BAM_TRACK)).map(|s| (SECOND_BAM_TRACK, s)));
        out
    }
}

/// Real CBM DOS PRG file-type value (`DirectoryEntry::kind()`).
pub const FILE_TYPE_PRG: u8 = 2;
/// Real CBM DOS directory-name padding byte (PETSCII shifted space).
const PETSCII_PAD: u8 = 0xA0;

#[cfg(test)]
mod tests {
    use super::*;

    fn blank_image() -> Vec<u8> {
        vec![0u8; IMAGE_SIZE]
    }

    fn sectors_per_track_repeats_the_same_zone_table_on_both_sides() {
        assert_eq!(sectors_per_track(1), 21);
        assert_eq!(sectors_per_track(17), 21);
        assert_eq!(sectors_per_track(18), 19);
        assert_eq!(sectors_per_track(35), 17);
        // Side 2 starts here -- must repeat side 1's own pattern exactly.
        assert_eq!(sectors_per_track(36), 21);
        assert_eq!(sectors_per_track(52), 21);
        assert_eq!(sectors_per_track(53), 19);
        assert_eq!(sectors_per_track(70), 17);
        assert_eq!(sectors_per_track(0), 0);
        assert_eq!(sectors_per_track(71), 0);
    }

    fn total_sectors_matches_the_real_confirmed_1366_block_count() {
        let mut total = 0usize;
        for t in 1..=70u8 {
            total += sectors_per_track(t) as usize;
        }
        assert_eq!(total, TOTAL_SECTORS);
    }
    /// Merged (every check kept): `sectors_per_track_repeats_the_same_zone_table_on_both_sides`, `total_sectors_matches_the_real_confirmed_1366_block_count`.
    #[test]
    fn geometry() {
        sectors_per_track_repeats_the_same_zone_table_on_both_sides();
        total_sectors_matches_the_real_confirmed_1366_block_count();
    }

    fn parse_recognizes_both_real_size_variants() {
        assert!(!D71::parse(&blank_image()).unwrap().has_error_info());
        let mut with_errors = blank_image();
        with_errors.extend(vec![0u8; TOTAL_SECTORS]);
        assert!(D71::parse(&with_errors).unwrap().has_error_info());
    }

    fn parse_rejects_an_unrecognized_size() {
        let err = D71::parse(&vec![0u8; 12345]).unwrap_err();
        assert_eq!(err, Error::InvalidSize(12345));
    }
    /// Merged (every check kept): `parse_recognizes_both_real_size_variants`, `parse_rejects_an_unrecognized_size`.
    #[test]
    fn parsing() {
        parse_recognizes_both_real_size_variants();
        parse_rejects_an_unrecognized_size();
    }

    fn read_write_sector_round_trips_and_reaches_the_second_side() {
        let mut disk = D71::parse(&blank_image()).unwrap();
        let mut data = [0u8; SECTOR_SIZE];
        data[0] = 0xAB;
        // Track 53 exists only because this is a real double-sided 1571
        // image -- would be out of range on a 35-track `.d64`.
        disk.write_sector(53, 0, &data).unwrap();
        assert_eq!(disk.read_sector(53, 0).unwrap()[0], 0xAB);

        assert_eq!(
            disk.read_sector(71, 0).unwrap_err(),
            Error::TrackOutOfRange(71)
        );
        assert_eq!(
            disk.read_sector(70, 17).unwrap_err(),
            Error::SectorOutOfRange {
                track: 70,
                sector: 17
            }
        );
    }

    fn to_bytes_round_trips_exactly() {
        let mut bytes = blank_image();
        bytes[0x1234] = 0x42;
        let disk = D71::parse(&bytes).unwrap();
        assert_eq!(disk.to_bytes(), bytes);
    }
    /// Merged (every check kept): `read_write_sector_round_trips_and_reaches_the_second_side`, `to_bytes_round_trips_exactly`.
    #[test]
    fn sector_io() {
        read_write_sector_round_trips_and_reaches_the_second_side();
        to_bytes_round_trips_exactly();
    }

    fn track_sector_byte_offset(track: u8, sector: u8) -> usize {
        let mut idx = 0usize;
        for t in 1..track {
            idx += sectors_per_track(t) as usize;
        }
        idx += sector as usize;
        idx * SECTOR_SIZE
    }

    fn bam_fields_read_from_the_confirmed_real_offsets() {
        let mut bytes = blank_image();
        let bam_off = track_sector_byte_offset(BAM_TRACK, BAM_SECTOR);
        let bam = &mut bytes[bam_off..bam_off + SECTOR_SIZE];
        bam[BAM_OFFSET_DIR_LINK_TRACK] = DIR_TRACK;
        bam[BAM_OFFSET_DIR_LINK_SECTOR] = DIR_SECTOR;
        bam[BAM_OFFSET_DOS_VERSION] = b'A';
        bam[BAM_OFFSET_NAME..BAM_OFFSET_NAME + 16]
            .copy_from_slice(b"MY DISK \xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0");
        bam[BAM_OFFSET_ID] = b'1';
        bam[BAM_OFFSET_ID + 1] = b'A';
        bam[BAM_OFFSET_DOS_TYPE] = b'2';
        bam[BAM_OFFSET_DOS_TYPE + 1] = b'A';

        let disk = D71::parse(&bytes).unwrap();
        assert_eq!(disk.dos_version(), b'A');
        assert_eq!(
            &disk.disk_name(),
            b"MY DISK \xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0"
        );
        assert_eq!(disk.disk_id(), [b'1', b'A']);
        assert_eq!(disk.dos_type(), [b'2', b'A']);
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

    fn directory_entries_reads_a_real_slot_from_the_second_side_capable_layout() {
        let mut bytes = blank_image();
        let bam_off = track_sector_byte_offset(BAM_TRACK, BAM_SECTOR);
        bytes[bam_off + BAM_OFFSET_DIR_LINK_TRACK] = DIR_TRACK;
        bytes[bam_off + BAM_OFFSET_DIR_LINK_SECTOR] = DIR_SECTOR;

        let dir_off = track_sector_byte_offset(DIR_TRACK, DIR_SECTOR);
        let dir = &mut bytes[dir_off..dir_off + SECTOR_SIZE];
        dir[0] = 0;
        dir[1] = 0xFF;
        // A file living on side 2 (track 40) -- only reachable if this
        // format's own wider track range is actually wired through.
        dir[SLOT_TYPE_OFFSET] = 0x80 | FILE_TYPE_PRG;
        dir[SLOT_TRACK_OFFSET] = 40;
        dir[SLOT_SECTOR_OFFSET] = 0;
        dir[SLOT_NAME_OFFSET..SLOT_NAME_OFFSET + 16]
            .copy_from_slice(b"SIDE2PRG\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0");
        dir[SLOT_SIZE_OFFSET] = 3;

        let disk = D71::parse(&bytes).unwrap();
        let entries = disk.directory_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].track, 40);
    }
    /// Merged (every check kept): `bam_fields_read_from_the_confirmed_real_offsets`, `directory_entries_reads_a_real_slot_from_the_second_side_capable_layout`.
    #[test]
    fn bam_and_directory() {
        bam_fields_read_from_the_confirmed_real_offsets();
        directory_entries_reads_a_real_slot_from_the_second_side_capable_layout();
    }

    fn read_file_reads_real_payload_bytes_from_a_side_2_track() {
        let mut bytes = blank_image();
        let off = track_sector_byte_offset(40, 0);
        bytes[off] = 0; // end of chain
        bytes[off + 1] = 5; // bytes 2..=5 inclusive are real (4 bytes)
        bytes[off + 2] = 0xAA;
        bytes[off + 3] = 0xBB;
        bytes[off + 4] = 0xCC;
        bytes[off + 5] = 0xDD;

        let disk = D71::parse(&bytes).unwrap();
        let entry = make_entry(40, 0);
        let data = disk.read_file(&entry).unwrap();
        assert_eq!(data, vec![0xAA, 0xBB, 0xCC, 0xDD]);
    }

    fn find_program_wildcard_and_exact_name_both_work() {
        let entries = vec![make_entry(40, 0)];
        assert!(D71::find_program(&entries, b"*").is_some());
        assert!(D71::find_program(&entries, b"PROGRAM").is_some());
        assert!(D71::find_program(&entries, b"OTHER").is_none());
    }
    /// Merged (every check kept): `read_file_reads_real_payload_bytes_from_a_side_2_track`, `find_program_wildcard_and_exact_name_both_work`.
    #[test]
    fn files() {
        read_file_reads_real_payload_bytes_from_a_side_2_track();
        find_program_wildcard_and_exact_name_both_work();
    }

    /// A freshly formatted double-sided disk: everything free except the
    /// BAM sectors, the first directory sector and the whole BAM track 53.
    fn formatted() -> D71 {
        use cbm_dos::Image;
        let mut disk = D71::parse(&vec![0u8; IMAGE_SIZE]).unwrap();
        let mut bam = [0u8; SECTOR_SIZE];
        bam[0] = DIR_TRACK;
        bam[1] = DIR_SECTOR;
        bam[2] = 0x41;
        bam[3] = 0x80;
        disk.write_sector(BAM_TRACK, BAM_SECTOR, &bam).unwrap();
        disk.write_sector(SECOND_BAM_TRACK, SECOND_BAM_SECTOR, &[0u8; SECTOR_SIZE])
            .unwrap();
        for track in 1..=70u8 {
            for sector in 0..sectors_per_track(track) {
                // Entries start "used" with count 0; free each sector.
                disk.mark(track, sector, true);
            }
        }
        for sector in 0..sectors_per_track(SECOND_BAM_TRACK) {
            disk.mark(SECOND_BAM_TRACK, sector, false);
        }
        disk.mark(BAM_TRACK, BAM_SECTOR, false);
        disk.mark(BAM_TRACK, DIR_SECTOR, false);
        let mut dir = [0u8; SECTOR_SIZE];
        dir[1] = 0xFF;
        disk.write_sector(DIR_TRACK, DIR_SECTOR, &dir).unwrap();
        disk
    }

    fn free_blocks(disk: &D71) -> usize {
        use cbm_dos::Image;
        (1..=70u8)
            .map(|t| {
                (0..sectors_per_track(t))
                    .filter(|&s| disk.is_free(t, s))
                    .count()
            })
            .sum()
    }

    fn counts_match_bitmaps(disk: &D71) {
        use cbm_dos::Image;
        for track in 1..=70u8 {
            let [(ct, cs, co), _] = disk.bam_locations(track).unwrap();
            let counted = disk.read_sector(ct, cs).unwrap()[co] as usize;
            let bits = (0..sectors_per_track(track))
                .filter(|&s| disk.is_free(track, s))
                .count();
            assert_eq!(counted, bits, "track {track}");
        }
    }

    #[test]
    fn a_saved_program_reads_back_and_the_bam_counts_stay_consistent() {
        let mut disk = formatted();
        let blank = free_blocks(&disk);
        assert_eq!(blank, 1366 - 2 - 19, "all but 18/0, 18/1 and track 53");
        let program: Vec<u8> = (0..900u32).map(|i| (i * 13) as u8).collect();
        disk.save_program(b"HELLO", &program, false).unwrap();
        let entries = disk.directory_entries();
        let entry = D71::find_program(&entries, b"HELLO").unwrap();
        assert_eq!(disk.read_file(entry).unwrap(), program);
        assert_eq!(blank - free_blocks(&disk), 4);
        counts_match_bitmaps(&disk);
    }

    fn a_big_file_spills_onto_side_two_and_reads_back() {
        let mut disk = formatted();
        // More than side 1 can hold (~664 blocks of 254 bytes).
        let program: Vec<u8> = (0..190_000u32).map(|i| (i % 251) as u8).collect();
        disk.save_program(b"BIG", &program, false).unwrap();
        let entries = disk.directory_entries();
        let entry = D71::find_program(&entries, b"BIG").unwrap();
        assert_eq!(disk.read_file(entry).unwrap(), program);
        let side2_used = (36..=70u8)
            .filter(|&t| t != SECOND_BAM_TRACK)
            .map(|t| {
                (0..sectors_per_track(t))
                    .filter(|&s| !cbm_dos::Image::is_free(&disk, t, s))
                    .count()
            })
            .sum::<usize>();
        assert!(side2_used > 0, "side 2 was used");
        counts_match_bitmaps(&disk);
        // Track 53 (the second BAM) never receives file data.
        assert!(
            (0..sectors_per_track(SECOND_BAM_TRACK)).all(|s| !cbm_dos::Image::is_free(
                &disk,
                SECOND_BAM_TRACK,
                s
            ))
        );
    }

    fn replacing_a_file_frees_its_old_blocks_and_a_full_disk_changes_nothing() {
        let mut disk = formatted();
        disk.save_program(b"A", &[1; 2000], false).unwrap();
        let used = free_blocks(&disk);
        assert_eq!(
            disk.save_program(b"A", &[2; 10], false),
            Err(SaveError::FileExists)
        );
        disk.save_program(b"A", &[2; 10], true).unwrap();
        assert!(free_blocks(&disk) > used);
        let before = disk.to_bytes();
        let too_big = vec![0u8; (free_blocks(&disk) + 1) * 254];
        assert_eq!(
            disk.save_program(b"B", &too_big, false),
            Err(SaveError::DiskFull)
        );
        assert_eq!(disk.to_bytes(), before);
        counts_match_bitmaps(&disk);
    }
    /// Merged (every check kept): `a_big_file_spills_onto_side_two_and_reads_back`, `replacing_a_file_frees_its_old_blocks_and_a_full_disk_changes_nothing`.
    #[test]
    fn big_and_replace() {
        a_big_file_spills_onto_side_two_and_reads_back();
        replacing_a_file_frees_its_old_blocks_and_a_full_disk_changes_nothing();
    }
}
