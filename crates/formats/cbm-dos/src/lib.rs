//! Writing a program to a Commodore DOS disk image, independent of the image
//! format. `.d64`, `.d71` and `.d81` differ only in geometry and where the
//! block availability map (BAM) lives; the file system on top -- directory
//! chain of 32-byte slots, files as chains of 254-byte sectors, the last
//! sector's second link byte counting its valid bytes -- is the same. A
//! format crate implements [`Image`] (sector access plus BAM queries) and
//! gets [`save_program`], the work the 1541's `SAVE` does.
//!
//! The image is left exactly as it was when a save fails.

use std::collections::{HashMap, HashSet};
use std::fmt;

mod ops;
pub use ops::*;

pub const SECTOR_SIZE: usize = 256;
const PAYLOAD: usize = SECTOR_SIZE - 2;
const SLOT_SIZE: usize = 32;
const SLOTS_PER_SECTOR: usize = SECTOR_SIZE / SLOT_SIZE;
const SLOT_TYPE: usize = 2;
const SLOT_TRACK: usize = 3;
const SLOT_SECTOR: usize = 4;
const SLOT_NAME: usize = 5;
pub const NAME_LEN: usize = 16;
const SLOT_BLOCKS: usize = 30;
const PETSCII_PAD: u8 = 0xA0;

/// One disk image as the file system sees it.
pub trait Image: Clone {
    /// Sector contents, `None` outside the geometry.
    fn read(&self, track: u8, sector: u8) -> Option<[u8; SECTOR_SIZE]>;
    /// Overwrites a sector; `false` outside the geometry.
    fn write(&mut self, track: u8, sector: u8, data: &[u8; SECTOR_SIZE]) -> bool;
    fn sectors_per_track(&self, track: u8) -> u8;
    /// Whether the BAM lists the sector as free.
    fn is_free(&self, track: u8, sector: u8) -> bool;
    /// Marks a sector free or used in the BAM, keeping the per-track count.
    fn mark(&mut self, track: u8, sector: u8, free: bool);
    /// First sector of the directory chain.
    fn directory_start(&self) -> (u8, u8);
    /// The track directory sectors are added on.
    fn directory_track(&self) -> u8;
    /// Tracks file data is allocated from, best first (and never the
    /// directory/BAM tracks).
    fn allocation_order(&self) -> Vec<u8>;
    /// Sector skip between consecutive sectors of one file on a track.
    fn file_interleave(&self) -> u8;
    /// Sector skip when adding a directory sector.
    fn directory_interleave(&self) -> u8;
    /// Sectors the file system itself owns besides the directory chain: the header and the BAM
    /// sectors. Used by [`validate`] to tell them from orphaned sectors.
    fn system_sectors(&self) -> Vec<(u8, u8)> {
        Vec::new()
    }
}

/// Why a program could not be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveError {
    /// Empty, or longer than the 16 characters a directory slot holds.
    InvalidName,
    /// A file of that name exists and replacing was not requested ("63,
    /// FILE EXISTS").
    FileExists,
    /// Not enough free sectors ("72, DISK FULL").
    DiskFull,
    /// The directory track has no room for another directory sector.
    DirectoryFull,
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SaveError::InvalidName => "file name must be 1 to 16 characters",
            SaveError::FileExists => "file exists",
            SaveError::DiskFull => "disk full",
            SaveError::DirectoryFull => "directory full",
        })
    }
}

impl std::error::Error for SaveError {}

/// Stores `data` (a PRG: the two load-address bytes followed by the
/// program) as a closed PRG called `name`. With `replace` an existing file
/// of the same name is scratched first (the `@0:` form).
pub fn save_program<I: Image>(
    image: &mut I,
    name: &[u8],
    data: &[u8],
    replace: bool,
) -> Result<(), SaveError> {
    save_file(image, name, FileKind::Prg, data, replace)
}

/// Like [`save_program`] for any plain file type (`SEQ`, `PRG`, `USR`): `data` is stored as given.
pub fn save_file<I: Image>(
    image: &mut I,
    name: &[u8],
    kind: FileKind,
    data: &[u8],
    replace: bool,
) -> Result<(), SaveError> {
    if name.is_empty() || name.len() > NAME_LEN {
        return Err(SaveError::InvalidName);
    }
    let mut padded = [PETSCII_PAD; NAME_LEN];
    padded[..name.len()].copy_from_slice(name);

    let backup = image.clone();
    let result = save_inner(image, &padded, kind, data, replace);
    if result.is_err() {
        *image = backup;
    }
    result
}

fn find_entry<I: Image>(image: &I, padded: &[u8; NAME_LEN]) -> Option<(u8, u8)> {
    let (mut track, mut sector) = image.directory_start();
    let mut seen = HashSet::new();
    while track != 0 && seen.insert((track, sector)) {
        let data = image.read(track, sector)?;
        for slot in 0..SLOTS_PER_SECTOR {
            let base = slot * SLOT_SIZE;
            if data[base + SLOT_TYPE] != 0
                && &data[base + SLOT_NAME..base + SLOT_NAME + NAME_LEN] == padded
            {
                return Some((data[base + SLOT_TRACK], data[base + SLOT_SECTOR]));
            }
        }
        (track, sector) = (data[0], data[1]);
    }
    None
}

fn free_chain<I: Image>(image: &mut I, mut track: u8, mut sector: u8) {
    let mut seen = HashSet::new();
    while track != 0 && seen.insert((track, sector)) {
        let Some(data) = image.read(track, sector) else {
            break;
        };
        image.mark(track, sector, true);
        (track, sector) = (data[0], data[1]);
    }
}

fn allocate<I: Image>(image: &mut I, previous: Option<(u8, u8)>) -> Option<(u8, u8)> {
    for track in image.allocation_order() {
        let per_track = image.sectors_per_track(track);
        if per_track == 0 {
            continue;
        }
        let start = match previous {
            Some((t, s)) if t == track => (s + image.file_interleave()) % per_track,
            _ => 0,
        };
        for step in 0..per_track {
            let sector = (start + step) % per_track;
            if image.is_free(track, sector) {
                image.mark(track, sector, false);
                return Some((track, sector));
            }
        }
    }
    None
}

fn save_inner<I: Image>(
    image: &mut I,
    padded: &[u8; NAME_LEN],
    kind: FileKind,
    data: &[u8],
    replace: bool,
) -> Result<(), SaveError> {
    if let Some((track, sector)) = find_entry(image, padded) {
        if !replace {
            return Err(SaveError::FileExists);
        }
        free_chain(image, track, sector);
    }

    // Data sectors.
    let blocks = data.len().div_ceil(PAYLOAD).max(1);
    let mut chain = Vec::with_capacity(blocks);
    let mut previous = None;
    for _ in 0..blocks {
        let next = allocate(image, previous).ok_or(SaveError::DiskFull)?;
        chain.push(next);
        previous = Some(next);
    }
    for (index, &(track, sector)) in chain.iter().enumerate() {
        let mut buffer = [0u8; SECTOR_SIZE];
        let from = index * PAYLOAD;
        let to = (from + PAYLOAD).min(data.len());
        let payload = &data[from..to];
        buffer[2..2 + payload.len()].copy_from_slice(payload);
        match chain.get(index + 1) {
            Some(&(next_track, next_sector)) => {
                buffer[0] = next_track;
                buffer[1] = next_sector;
            }
            None => {
                buffer[0] = 0;
                // Index of the last valid byte (readers take `link - 1`).
                buffer[1] = (payload.len() + 1) as u8;
            }
        }
        if !image.write(track, sector, &buffer) {
            return Err(SaveError::DiskFull);
        }
    }

    // The directory slot: the scratched file's, else the first unused one,
    // else a new directory sector.
    let (dir_track, dir_sector, slot) = find_slot(image, padded)?;
    let mut sector_data = image
        .read(dir_track, dir_sector)
        .ok_or(SaveError::DirectoryFull)?;
    let base = slot * SLOT_SIZE;
    sector_data[base + SLOT_TYPE..base + SLOT_SIZE].fill(0);
    sector_data[base + SLOT_TYPE] = 0x80 | kind.code();
    sector_data[base + SLOT_TRACK] = chain[0].0;
    sector_data[base + SLOT_SECTOR] = chain[0].1;
    sector_data[base + SLOT_NAME..base + SLOT_NAME + NAME_LEN].copy_from_slice(padded);
    sector_data[base + SLOT_BLOCKS..base + SLOT_BLOCKS + 2]
        .copy_from_slice(&(blocks as u16).to_le_bytes());
    if !image.write(dir_track, dir_sector, &sector_data) {
        return Err(SaveError::DirectoryFull);
    }
    Ok(())
}

/// `(track, sector, slot)` for the new entry.
fn find_slot<I: Image>(
    image: &mut I,
    padded: &[u8; NAME_LEN],
) -> Result<(u8, u8, usize), SaveError> {
    let (mut track, mut sector) = image.directory_start();
    let mut seen = HashSet::new();
    let mut free_slot = None;
    let mut last = (track, sector);
    while track != 0 && seen.insert((track, sector)) {
        let Some(data) = image.read(track, sector) else {
            break;
        };
        for slot in 0..SLOTS_PER_SECTOR {
            let base = slot * SLOT_SIZE;
            if data[base + SLOT_TYPE] == 0 {
                free_slot.get_or_insert((track, sector, slot));
            } else if &data[base + SLOT_NAME..base + SLOT_NAME + NAME_LEN] == padded {
                return Ok((track, sector, slot));
            }
        }
        last = (track, sector);
        (track, sector) = (data[0], data[1]);
    }
    if let Some(found) = free_slot {
        return Ok(found);
    }

    // Chain a new directory sector on the directory track.
    let dir_track = image.directory_track();
    let per_track = image.sectors_per_track(dir_track);
    if per_track == 0 {
        return Err(SaveError::DirectoryFull);
    }
    let start = (last.1 + image.directory_interleave()) % per_track;
    let new_sector = (0..per_track)
        .map(|step| (start + step) % per_track)
        .find(|&candidate| image.is_free(dir_track, candidate))
        .ok_or(SaveError::DirectoryFull)?;
    image.mark(dir_track, new_sector, false);
    let mut link = image.read(last.0, last.1).ok_or(SaveError::DirectoryFull)?;
    link[0] = dir_track;
    link[1] = new_sector;
    if !image.write(last.0, last.1, &link) {
        return Err(SaveError::DirectoryFull);
    }
    let mut fresh = [0u8; SECTOR_SIZE];
    fresh[1] = 0xFF;
    if !image.write(dir_track, new_sector, &fresh) {
        return Err(SaveError::DirectoryFull);
    }
    Ok((dir_track, new_sector, 0))
}
