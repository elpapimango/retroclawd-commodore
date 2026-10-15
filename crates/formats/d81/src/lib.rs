//! Parses a Commodore `.d81` disk image -- the standard 1581 3.5" disk
//! format. No dependency on `machine`, any CPU crate, or `c1541`/`c64` --
//! same "format crate only parses bytes into a plain struct" precedent as
//! the sibling `d64`/`d71` crates.
//!
//! **Real hardware fact this format encodes, and why its own layout is
//! genuinely different from `d64`/`d71`'s shared lineage**: the 1541/1571
//! write MFM^H^HGCR at a constant *linear* bit density on a constant-
//! angular-velocity spindle, so outer tracks hold more sectors than inner
//! ones (the 4-zone table `d64`/`d71` both document). The 1581 is a real,
//! different physical drive -- true MFM encoding (like a PC floppy) at a
//! *uniform* sector count on every track, no speed zones at all. This is
//! directly visible in VICE's own source: unlike `d64`'s `sector_map_d64`
//! table, there is no `sector_map_d81` table, only a single
//! `NUM_SECTORS_1581 = 40` constant -- confirmed via VICE's own
//! `diskconstants.h` this session, not guessed (see `CREDITS.md`).
//!
//! Header/BAM layout is also a real, different shape from `d64`/`d71`'s
//! shared 1541-lineage BAM (not simply "the same struct at a different
//! track") -- confirmed against VICE's own `diskconstants.h` and
//! `vdrive-bam.c` this session:
//!
//! - Track 40, sector 0 is the disk **header** sector (`HEADER_TRACK`/
//!   `HEADER_SECTOR`, matching VICE's `BAM_TRACK_1581`/`BAM_SECTOR_1581`):
//!   offset 0-1 = the real first directory sector link (`DIR_TRACK_1581`/
//!   `DIR_SECTOR_1581` = 40/3, confirmed directly), offset 2 = DOS version
//!   type byte, offset 4 = the 16-byte disk name (`BAM_NAME_1581`,
//!   confirmed directly -- genuinely not 144 the way `d64`/`d71` use,
//!   since this is a different header sector shape entirely), offset 22 =
//!   the 2-byte disk ID (`BAM_ID_1581`, confirmed directly), offset 25 =
//!   the 2-byte DOS type (derived the same padding-consistency way `d64`'s
//!   own `BAM_OFFSET_DOS_TYPE` was, but cross-checked against a real,
//!   literal confirmation: VICE's own disk-formatting code writes the
//!   literal bytes `51, 68` there, i.e. ASCII `"3D"` -- the well-known real
//!   1581 DOS type).
//! - Tracks 40/sector 1 and 40/sector 2 are two separate **BAM** sectors
//!   (distinct from the header sector, unlike `d64`/`d71`'s single
//!   combined header+BAM sector) -- confirmed via VICE's own
//!   `vdrive-bam.c`: each covers 40 tracks' worth of real 6-byte-per-track
//!   groups (1 free-count byte + 5 bitmap bytes, since 40 sectors/track
//!   need exactly 5 bytes = 40 bits to cover), sector 1 for tracks 1-40,
//!   sector 2 for tracks 41-80.
//!
//! **Deliberately out of scope this pass, same as `d71`**: the exact
//! starting byte offset of each BAM sector's own per-track group table
//! isn't decoded here -- confirmed the group *size* (6 bytes/track) and
//! *sector assignment* (which BAM sector covers which 40 tracks), but not
//! independently reverified the literal starting-offset constant this
//! session. Attaching and *reading* a `.d81` (directory listing, file
//! extraction) needs only the real track/sector chain-walking this crate
//! does implement -- nothing here allocates space or writes new files, so
//! the free-space bitmap was never load-bearing for that.
//!
//! Two known-real size variants are supported, same "detect by length"
//! convention as `d64`/`d71`: the standard 80-track/40-sector image
//! (819200 bytes, the well-known real "800K" `.d81` size), and the same
//! optional per-sector real drive error-code byte channel.

use std::fmt;

pub const SECTOR_SIZE: usize = 256;

/// Real hardware fact: uniform sector count on every track -- see the
/// module doc comment for why this format has no per-zone table at all,
/// unlike `d64`/`d71`.
pub const SECTORS_PER_TRACK: u8 = 40;
pub const TRACK_COUNT: u8 = 80;

pub const HEADER_TRACK: u8 = 40;
pub const HEADER_SECTOR: u8 = 0;
pub const DIR_TRACK: u8 = 40;
pub const DIR_SECTOR: u8 = 3;
/// The two real, separate BAM sectors -- see module doc comment. Not
/// consumed by this crate this pass (no bitmap decode implemented yet).
pub const BAM_SECTOR_1: (u8, u8) = (40, 1);
pub const BAM_SECTOR_2: (u8, u8) = (40, 2);

// Header sector (track 40, sector 0) byte offsets.
const HEADER_OFFSET_DIR_LINK_TRACK: usize = 0;
const HEADER_OFFSET_DIR_LINK_SECTOR: usize = 1;
const HEADER_OFFSET_DOS_VERSION: usize = 2;
const HEADER_OFFSET_NAME: usize = 4;
const HEADER_OFFSET_ID: usize = 22;
const HEADER_OFFSET_DOS_TYPE: usize = 25;

// Directory entry ("slot") layout -- identical 32-byte CBM DOS format
// `d64`/`d71` already document, unchanged across the whole DOS family.
const DIR_SLOT_SIZE: usize = 32;
const DIR_SLOTS_PER_SECTOR: usize = SECTOR_SIZE / DIR_SLOT_SIZE;
const SLOT_TYPE_OFFSET: usize = 2;
const SLOT_TRACK_OFFSET: usize = 3;
const SLOT_SECTOR_OFFSET: usize = 4;
const SLOT_NAME_OFFSET: usize = 5;
const SLOT_NAME_LEN: usize = 16;
const SLOT_SIZE_OFFSET: usize = 30;

const TOTAL_SECTORS: usize = TRACK_COUNT as usize * SECTORS_PER_TRACK as usize;
const IMAGE_SIZE: usize = TOTAL_SECTORS * SECTOR_SIZE;
const IMAGE_SIZE_WITH_ERRORS: usize = IMAGE_SIZE + TOTAL_SECTORS;

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
                "not a recognized .d81 size ({size} bytes -- expected {IMAGE_SIZE} or \
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
/// DirectoryEntry`/`d71::DirectoryEntry` document, duplicated rather than
/// shared (own shape per format, matching this codebase's own established
/// precedent).
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

/// A parsed `.d81` disk image: flat track/sector storage plus optional
/// per-sector real drive error-code bytes.
#[derive(Debug, Clone)]
pub struct D81 {
    sectors: Vec<u8>,
    errors: Option<Vec<u8>>,
}

impl D81 {
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

    /// A freshly formatted, empty disk with the given ID: everything free
    /// except the header, both BAM sectors and the first directory sector.
    pub fn blank_formatted(id: [u8; 2]) -> D81 {
        use cbm_dos::Image;
        let mut disk = D81 {
            sectors: vec![0u8; IMAGE_SIZE],
            errors: None,
        };
        let mut header = [0u8; SECTOR_SIZE];
        header[0] = DIR_TRACK;
        header[1] = DIR_SECTOR;
        header[2] = b'D';
        header[4..20].fill(PETSCII_PAD);
        header[20] = PETSCII_PAD;
        header[21] = PETSCII_PAD;
        header[22] = id[0];
        header[23] = id[1];
        header[24] = PETSCII_PAD;
        header[25] = b'3';
        header[26] = b'D';
        let _ = disk.write_sector(HEADER_TRACK, HEADER_SECTOR, &header);
        for (n, (t, s)) in [BAM_SECTOR_1, BAM_SECTOR_2].into_iter().enumerate() {
            let mut bam = [0u8; SECTOR_SIZE];
            if n == 0 {
                bam[0] = BAM_SECTOR_2.0;
                bam[1] = BAM_SECTOR_2.1;
            } else {
                bam[1] = 0xFF;
            }
            bam[2] = b'D';
            bam[3] = !b'D';
            bam[4] = id[0];
            bam[5] = id[1];
            bam[6] = 0xC0;
            let _ = disk.write_sector(t, s, &bam);
        }
        for track in 1..=TRACK_COUNT {
            for sector in 0..SECTORS_PER_TRACK {
                disk.mark(track, sector, true);
            }
        }
        for sector in 0..=DIR_SECTOR {
            disk.mark(HEADER_TRACK, sector, false);
        }
        let mut dir = [0u8; SECTOR_SIZE];
        dir[1] = 0xFF;
        let _ = disk.write_sector(DIR_TRACK, DIR_SECTOR, &dir);
        disk
    }

    /// Sets the disk name (at most 16 PETSCII characters, padded with `$A0`).
    pub fn set_disk_name(&mut self, name: &[u8]) {
        let mut header = [0u8; SECTOR_SIZE];
        header.copy_from_slice(self.header());
        header[HEADER_OFFSET_NAME..HEADER_OFFSET_NAME + 16].fill(PETSCII_PAD);
        let n = name.len().min(16);
        header[HEADER_OFFSET_NAME..HEADER_OFFSET_NAME + n].copy_from_slice(&name[..n]);
        let _ = self.write_sector(HEADER_TRACK, HEADER_SECTOR, &header);
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
        if !(1..=TRACK_COUNT).contains(&track) {
            return Err(Error::TrackOutOfRange(track));
        }
        if sector >= SECTORS_PER_TRACK {
            return Err(Error::SectorOutOfRange { track, sector });
        }
        Ok((track as usize - 1) * SECTORS_PER_TRACK as usize + sector as usize)
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

    fn header(&self) -> &[u8] {
        self.read_sector(HEADER_TRACK, HEADER_SECTOR)
            .expect("header track/sector is always in range for an 80-track image")
    }

    pub fn dos_version(&self) -> u8 {
        self.header()[HEADER_OFFSET_DOS_VERSION]
    }

    pub fn disk_name(&self) -> [u8; 16] {
        let header = self.header();
        let mut name = [0u8; 16];
        name.copy_from_slice(&header[HEADER_OFFSET_NAME..HEADER_OFFSET_NAME + 16]);
        name
    }

    pub fn disk_id(&self) -> [u8; 2] {
        let header = self.header();
        [header[HEADER_OFFSET_ID], header[HEADER_OFFSET_ID + 1]]
    }

    pub fn dos_type(&self) -> [u8; 2] {
        let header = self.header();
        [
            header[HEADER_OFFSET_DOS_TYPE],
            header[HEADER_OFFSET_DOS_TYPE + 1],
        ]
    }

    /// Walks the real directory sector chain starting from the header
    /// sector's own stored link -- same guarded, circular-chain-safe walk
    /// `d64::D64::directory_entries`/`d71::D71::directory_entries` already
    /// implement.
    pub fn directory_entries(&self) -> Vec<DirectoryEntry> {
        let mut out = Vec::new();
        let header = self.header();
        let mut track = header[HEADER_OFFSET_DIR_LINK_TRACK];
        let mut sector = header[HEADER_OFFSET_DIR_LINK_SECTOR];
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

    /// Reads a file's own real sector chain -- same real last-sector-
    /// length convention (`link_byte - 1` valid bytes) `d64`/`d71` already
    /// document; a DOS-level convention, not physical-format-specific.
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

    /// Same real CBM DOS filename-matching rules `d64::D64::find_program`/
    /// `d71::D71::find_program` document.
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

/// Offset of the first track entry inside each BAM sector (VICE's
/// `BAM_BIT_MAP_1581`); entries are six bytes: the free count then five
/// bitmap bytes. Sector 40/1 holds tracks 1-40, sector 40/2 tracks 41-80.
const BAM_ENTRIES_OFFSET: usize = 16;
const BAM_ENTRY_SIZE: usize = 6;

impl D81 {
    /// `(sector, entry offset)` of a track's BAM entry.
    fn bam_entry(&self, track: u8) -> Option<((u8, u8), usize)> {
        match track {
            1..=40 => Some((
                BAM_SECTOR_1,
                BAM_ENTRIES_OFFSET + (track as usize - 1) * BAM_ENTRY_SIZE,
            )),
            41..=80 => Some((
                BAM_SECTOR_2,
                BAM_ENTRIES_OFFSET + (track as usize - 41) * BAM_ENTRY_SIZE,
            )),
            _ => None,
        }
    }

    /// Stores a PRG the way the 1581 would for `SAVE` (see `cbm-dos`).
    pub fn save_program(
        &mut self,
        name: &[u8],
        data: &[u8],
        replace: bool,
    ) -> Result<(), SaveError> {
        cbm_dos::save_program(self, name, data, replace)
    }
}

impl cbm_dos::Image for D81 {
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
        if (1..=TRACK_COUNT).contains(&track) {
            SECTORS_PER_TRACK
        } else {
            0
        }
    }

    fn is_free(&self, track: u8, sector: u8) -> bool {
        let Some(((t, s), entry)) = self.bam_entry(track) else {
            return false;
        };
        self.read_sector(t, s)
            .is_ok_and(|bam| bam[entry + 1 + sector as usize / 8] & (1 << (sector % 8)) != 0)
    }

    fn mark(&mut self, track: u8, sector: u8, free: bool) {
        let Some(((t, s), entry)) = self.bam_entry(track) else {
            return;
        };
        let Ok(bam) = self.read_sector(t, s) else {
            return;
        };
        let mut bam_sector = [0u8; SECTOR_SIZE];
        bam_sector.copy_from_slice(bam);
        let byte = entry + 1 + sector as usize / 8;
        let bit = 1 << (sector % 8);
        let was_free = bam_sector[byte] & bit != 0;
        if free == was_free {
            return;
        }
        bam_sector[byte] ^= bit;
        bam_sector[entry] = if free {
            bam_sector[entry].saturating_add(1)
        } else {
            bam_sector[entry].saturating_sub(1)
        };
        let _ = self.write_sector(t, s, &bam_sector);
    }

    fn directory_start(&self) -> (u8, u8) {
        let header = self.header();
        (
            header[HEADER_OFFSET_DIR_LINK_TRACK],
            header[HEADER_OFFSET_DIR_LINK_SECTOR],
        )
    }

    fn directory_track(&self) -> u8 {
        DIR_TRACK
    }

    fn allocation_order(&self) -> Vec<u8> {
        // Outward from the header/directory track 40.
        (1..HEADER_TRACK)
            .rev()
            .chain(HEADER_TRACK + 1..=TRACK_COUNT)
            .collect()
    }

    fn file_interleave(&self) -> u8 {
        1
    }

    fn directory_interleave(&self) -> u8 {
        1
    }

    fn system_sectors(&self) -> Vec<(u8, u8)> {
        vec![(HEADER_TRACK, HEADER_SECTOR), BAM_SECTOR_1, BAM_SECTOR_2]
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

    #[test]
    fn image_size_matches_the_real_well_known_800k_d81_size() {
        assert_eq!(IMAGE_SIZE, 819_200);
        assert_eq!(IMAGE_SIZE_WITH_ERRORS, 822_400);
    }

    #[test]
    fn parse_recognizes_both_real_size_variants() {
        assert!(!D81::parse(&blank_image()).unwrap().has_error_info());
        let mut with_errors = blank_image();
        with_errors.extend(vec![0u8; TOTAL_SECTORS]);
        assert!(D81::parse(&with_errors).unwrap().has_error_info());
    }

    #[test]
    fn parse_rejects_an_unrecognized_size() {
        let err = D81::parse(&vec![0u8; 12345]).unwrap_err();
        assert_eq!(err, Error::InvalidSize(12345));
    }

    #[test]
    fn read_write_sector_round_trips_with_uniform_sector_count() {
        let mut disk = D81::parse(&blank_image()).unwrap();
        let mut data = [0u8; SECTOR_SIZE];
        data[0] = 0xAB;
        // Track 80 sector 39 is the real last valid sector -- 40 sectors
        // on every track, no speed-zone reduction unlike d64/d71.
        disk.write_sector(80, 39, &data).unwrap();
        assert_eq!(disk.read_sector(80, 39).unwrap()[0], 0xAB);

        assert_eq!(
            disk.read_sector(81, 0).unwrap_err(),
            Error::TrackOutOfRange(81)
        );
        assert_eq!(
            disk.read_sector(1, 40).unwrap_err(),
            Error::SectorOutOfRange {
                track: 1,
                sector: 40
            }
        );
    }

    #[test]
    fn to_bytes_round_trips_exactly() {
        let mut bytes = blank_image();
        bytes[0x1234] = 0x42;
        let disk = D81::parse(&bytes).unwrap();
        assert_eq!(disk.to_bytes(), bytes);
    }

    fn track_sector_byte_offset(track: u8, sector: u8) -> usize {
        ((track as usize - 1) * SECTORS_PER_TRACK as usize + sector as usize) * SECTOR_SIZE
    }

    #[test]
    fn header_fields_read_from_the_confirmed_real_offsets() {
        let mut bytes = blank_image();
        let header_off = track_sector_byte_offset(HEADER_TRACK, HEADER_SECTOR);
        let header = &mut bytes[header_off..header_off + SECTOR_SIZE];
        header[HEADER_OFFSET_DIR_LINK_TRACK] = DIR_TRACK;
        header[HEADER_OFFSET_DIR_LINK_SECTOR] = DIR_SECTOR;
        header[HEADER_OFFSET_DOS_VERSION] = b'D';
        header[HEADER_OFFSET_NAME..HEADER_OFFSET_NAME + 16]
            .copy_from_slice(b"MY DISK \xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0");
        header[HEADER_OFFSET_ID] = b'1';
        header[HEADER_OFFSET_ID + 1] = b'A';
        // The real, literal DOS-type bytes VICE's own formatting code
        // writes -- ASCII "3D", the well-known real 1581 DOS type.
        header[HEADER_OFFSET_DOS_TYPE] = b'3';
        header[HEADER_OFFSET_DOS_TYPE + 1] = b'D';

        let disk = D81::parse(&bytes).unwrap();
        assert_eq!(disk.dos_version(), b'D');
        assert_eq!(
            &disk.disk_name(),
            b"MY DISK \xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0"
        );
        assert_eq!(disk.disk_id(), [b'1', b'A']);
        assert_eq!(disk.dos_type(), [b'3', b'D']);
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

    #[test]
    fn directory_entries_reads_a_real_slot() {
        let mut bytes = blank_image();
        let header_off = track_sector_byte_offset(HEADER_TRACK, HEADER_SECTOR);
        bytes[header_off + HEADER_OFFSET_DIR_LINK_TRACK] = DIR_TRACK;
        bytes[header_off + HEADER_OFFSET_DIR_LINK_SECTOR] = DIR_SECTOR;

        let dir_off = track_sector_byte_offset(DIR_TRACK, DIR_SECTOR);
        let dir = &mut bytes[dir_off..dir_off + SECTOR_SIZE];
        dir[0] = 0;
        dir[1] = 0xFF;
        dir[SLOT_TYPE_OFFSET] = 0x80 | FILE_TYPE_PRG;
        dir[SLOT_TRACK_OFFSET] = 41; // real 80-track range, past d71's own 70
        dir[SLOT_SECTOR_OFFSET] = 0;
        dir[SLOT_NAME_OFFSET..SLOT_NAME_OFFSET + 16]
            .copy_from_slice(b"BIGDISK\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0\xA0");
        dir[SLOT_SIZE_OFFSET] = 7;

        let disk = D81::parse(&bytes).unwrap();
        let entries = disk.directory_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].track, 41);
    }

    #[test]
    fn read_file_reads_real_payload_bytes() {
        let mut bytes = blank_image();
        let off = track_sector_byte_offset(41, 0);
        bytes[off] = 0;
        bytes[off + 1] = 4; // bytes 2..=4 inclusive are real (3 bytes)
        bytes[off + 2] = 0x11;
        bytes[off + 3] = 0x22;
        bytes[off + 4] = 0x33;

        let disk = D81::parse(&bytes).unwrap();
        let entry = make_entry(41, 0);
        let data = disk.read_file(&entry).unwrap();
        assert_eq!(data, vec![0x11, 0x22, 0x33]);
    }

    #[test]
    fn find_program_wildcard_and_exact_name_both_work() {
        let entries = vec![make_entry(41, 0)];
        assert!(D81::find_program(&entries, b"*").is_some());
        assert!(D81::find_program(&entries, b"PROGRAM").is_some());
        assert!(D81::find_program(&entries, b"OTHER").is_none());
    }

    /// A freshly formatted disk: everything free except the header, both BAM
    /// sectors and the first directory sector.
    fn formatted() -> D81 {
        use cbm_dos::Image;
        let mut disk = D81::parse(&vec![0u8; IMAGE_SIZE]).unwrap();
        let mut header = [0u8; SECTOR_SIZE];
        header[0] = DIR_TRACK;
        header[1] = DIR_SECTOR;
        header[2] = b'D';
        disk.write_sector(HEADER_TRACK, HEADER_SECTOR, &header)
            .unwrap();
        for (t, s) in [BAM_SECTOR_1, BAM_SECTOR_2] {
            disk.write_sector(t, s, &[0u8; SECTOR_SIZE]).unwrap();
        }
        for track in 1..=TRACK_COUNT {
            for sector in 0..SECTORS_PER_TRACK {
                disk.mark(track, sector, true);
            }
        }
        for sector in 0..=DIR_SECTOR {
            disk.mark(HEADER_TRACK, sector, false);
        }
        let mut dir = [0u8; SECTOR_SIZE];
        dir[1] = 0xFF;
        disk.write_sector(DIR_TRACK, DIR_SECTOR, &dir).unwrap();
        disk
    }

    fn counts_match_bitmaps(disk: &D81) {
        use cbm_dos::Image;
        for track in 1..=TRACK_COUNT {
            let ((t, s), entry) = disk.bam_entry(track).unwrap();
            let counted = disk.read_sector(t, s).unwrap()[entry] as usize;
            let bits = (0..SECTORS_PER_TRACK)
                .filter(|&sec| disk.is_free(track, sec))
                .count();
            assert_eq!(counted, bits, "track {track}");
        }
    }

    #[test]
    fn a_saved_program_reads_back_and_the_bam_counts_stay_consistent() {
        let mut disk = formatted();
        let program: Vec<u8> = (0..5000u32).map(|i| (i * 7) as u8).collect();
        disk.save_program(b"HELLO", &program, false).unwrap();
        let entries = disk.directory_entries();
        let entry = D81::find_program(&entries, b"HELLO").unwrap();
        assert_eq!(disk.read_file(entry).unwrap(), program);
        assert_eq!(entry.size_blocks, 20);
        counts_match_bitmaps(&disk);
    }

    #[test]
    fn a_big_file_reaches_the_second_bam_sector_and_directories_grow() {
        let mut disk = formatted();
        // Tracks 39..1 hold 39 * 40 = 1560 blocks; this needs more.
        let program: Vec<u8> = (0..450_000u32).map(|i| (i % 253) as u8).collect();
        disk.save_program(b"BIG", &program, false).unwrap();
        let entries = disk.directory_entries();
        let entry = D81::find_program(&entries, b"BIG").unwrap();
        assert_eq!(disk.read_file(entry).unwrap(), program);
        let upper_used = (41..=TRACK_COUNT)
            .flat_map(|t| (0..SECTORS_PER_TRACK).map(move |s| (t, s)))
            .filter(|&(t, s)| !cbm_dos::Image::is_free(&disk, t, s))
            .count();
        assert!(upper_used > 0, "the upper half was used");
        counts_match_bitmaps(&disk);

        for i in 0..10u8 {
            disk.save_program(&[b'F', b'0' + i], &[i], false).unwrap();
        }
        assert_eq!(disk.directory_entries().len(), 11);
    }

    #[test]
    fn replace_and_disk_full_behave_like_the_other_formats() {
        let mut disk = formatted();
        disk.save_program(b"A", &[1; 3000], false).unwrap();
        assert_eq!(
            disk.save_program(b"A", &[2; 10], false),
            Err(SaveError::FileExists)
        );
        disk.save_program(b"A", &[2; 10], true).unwrap();
        let before = disk.to_bytes();
        let too_big = vec![0u8; 800 * 1024];
        assert_eq!(
            disk.save_program(b"B", &too_big, false),
            Err(SaveError::DiskFull)
        );
        assert_eq!(disk.to_bytes(), before);
        counts_match_bitmaps(&disk);
    }
}
