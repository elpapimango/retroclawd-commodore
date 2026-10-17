//! Parses Commodore `.g64` (GCR-encoded raw track) disk images and decodes
//! their standard-format sectors into a plain [`d64::D64`].
//!
//! Layout (read from VICE's `fsimage-probe.c`/`fsimage-gcr.c`): 8 bytes
//! `"GCR-1541"`, a version byte (0), the number of half-track entries
//! (usually 84), the maximum track size (`u16` LE), then a table of `u32` LE
//! file offsets, one per half-track starting at track 1 (index `2 * (track
//! - 1)` is the full track), then a table of speed-zone words; a track's data
//! is a `u16` LE length followed by that many bytes of GCR bitstream. An
//!   offset of 0 means the half-track holds nothing.
//!
//! The machine side consumes sectors (the fast loader answers LOAD from
//! logical contents), so [`G64::to_d64`] walks each full track's bitstream
//! the way the 1541 does: find a SYNC (ten or more 1 bits), read the header
//! block (`$08`, checksum, sector, track, ID, ID) and then the data block
//! (`$07`, 256 bytes, checksum). Copy-protection tricks (weak bits, long
//! tracks, half tracks, non-standard sector layouts) do not survive the
//! decode; a sector that cannot be found or fails its checksum is left
//! zeroed and counted in [`Decoded::bad_sectors`].

use d64::D64;
use std::fmt;

const MAGIC: &[u8; 8] = b"GCR-1541";
const HEADER_LEN: usize = 12;
/// SYNC is at least this many consecutive 1 bits.
const SYNC_BITS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    TooShort(usize),
    BadMagic,
    UnsupportedVersion(u8),
    BadTrackTable,
    Decode(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooShort(n) => write!(f, ".g64 too short ({n} bytes)"),
            Error::BadMagic => write!(f, "not a .g64 (missing \"GCR-1541\")"),
            Error::UnsupportedVersion(v) => write!(f, "unsupported .g64 version {v}"),
            Error::BadTrackTable => write!(f, ".g64 track table points outside the file"),
            Error::Decode(e) => write!(f, "cannot build a sector image: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// A parsed `.g64`: the raw GCR bytes of each full track.
#[derive(Debug, Clone)]
pub struct G64 {
    /// `tracks[0]` is track 1; empty when the image has no data for it.
    tracks: Vec<Vec<u8>>,
}

/// The sector image decoded from a `.g64`, plus what could not be read.
#[derive(Debug)]
pub struct Decoded {
    pub disk: D64,
    /// Sectors (in tracks that have data) that were not found or failed
    /// their checksum.
    pub bad_sectors: usize,
}

fn le16(b: &[u8], at: usize) -> Option<usize> {
    Some(usize::from(*b.get(at)?) | usize::from(*b.get(at + 1)?) << 8)
}

fn le32(b: &[u8], at: usize) -> Option<usize> {
    Some(le16(b, at)? | le16(b, at + 2)? << 16)
}

pub fn parse(bytes: &[u8]) -> Result<G64, Error> {
    if bytes.len() < HEADER_LEN {
        return Err(Error::TooShort(bytes.len()));
    }
    if &bytes[..8] != MAGIC {
        return Err(Error::BadMagic);
    }
    if bytes[8] != 0 {
        return Err(Error::UnsupportedVersion(bytes[8]));
    }
    let half_tracks = usize::from(bytes[9]);
    let mut tracks = Vec::new();
    for index in (0..half_tracks).step_by(2) {
        let offset = le32(bytes, HEADER_LEN + index * 4).ok_or(Error::BadTrackTable)?;
        if offset == 0 {
            tracks.push(Vec::new());
            continue;
        }
        let length = le16(bytes, offset).ok_or(Error::BadTrackTable)?;
        let data = bytes
            .get(offset + 2..offset + 2 + length)
            .ok_or(Error::BadTrackTable)?;
        tracks.push(data.to_vec());
    }
    Ok(G64 { tracks })
}

impl G64 {
    /// Raw GCR bytes of a full track (1-based), if the image has any.
    pub fn track(&self, track: u8) -> Option<&[u8]> {
        self.tracks
            .get(usize::from(track).checked_sub(1)?)
            .map(Vec::as_slice)
            .filter(|t| !t.is_empty())
    }

    /// Number of full tracks that hold data.
    pub fn track_count(&self) -> usize {
        self.tracks.iter().filter(|t| !t.is_empty()).count()
    }

    /// Decodes every standard sector into a `.d64` (35 tracks, or 40 when
    /// tracks above 35 hold data).
    pub fn to_d64(&self) -> Result<Decoded, Error> {
        let tracks: u8 = if (36..=40).any(|t| self.track(t).is_some()) {
            40
        } else {
            35
        };
        let total: usize = (1..=tracks)
            .map(|t| usize::from(d64::sectors_per_track(t)))
            .sum();
        let mut image = vec![0u8; total * 256];
        let mut bad = 0usize;
        let mut base = 0usize;
        for track in 1..=tracks {
            let sectors = usize::from(d64::sectors_per_track(track));
            if let Some(gcr) = self.track(track) {
                let found = decode_track(gcr, track, sectors);
                for (sector, data) in found.into_iter().enumerate() {
                    match data {
                        Some(bytes) => {
                            let at = (base + sector) * 256;
                            image[at..at + 256].copy_from_slice(&bytes);
                        }
                        None => bad += 1,
                    }
                }
            }
            base += sectors;
        }
        let disk = D64::parse(&image).map_err(|e| Error::Decode(e.to_string()))?;
        Ok(Decoded {
            disk,
            bad_sectors: bad,
        })
    }
}

/// The 5-bit group to nibble table (and `None` for invalid groups).
fn gcr_nibble(group: u8) -> Option<u8> {
    Some(match group {
        0x0A => 0x0,
        0x0B => 0x1,
        0x12 => 0x2,
        0x13 => 0x3,
        0x0E => 0x4,
        0x0F => 0x5,
        0x16 => 0x6,
        0x17 => 0x7,
        0x09 => 0x8,
        0x19 => 0x9,
        0x1A => 0xA,
        0x1B => 0xB,
        0x0D => 0xC,
        0x1D => 0xD,
        0x1E => 0xE,
        0x15 => 0xF,
        _ => return None,
    })
}

/// A circular view of a track as a bit sequence, MSB first.
struct Bits<'a> {
    data: &'a [u8],
    len: usize,
}

impl Bits<'_> {
    fn bit(&self, at: usize) -> bool {
        let at = at % self.len;
        self.data[at / 8] >> (7 - at % 8) & 1 != 0
    }

    /// Reads `count` GCR-decoded bytes starting at bit `at`
    /// (`count * 10` bits).
    fn decode_bytes(&self, at: usize, count: usize) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(count);
        let mut pos = at;
        for _ in 0..count {
            let mut hi = 0u8;
            let mut lo = 0u8;
            for _ in 0..5 {
                hi = hi << 1 | u8::from(self.bit(pos));
                pos += 1;
            }
            for _ in 0..5 {
                lo = lo << 1 | u8::from(self.bit(pos));
                pos += 1;
            }
            out.push(gcr_nibble(hi)? << 4 | gcr_nibble(lo)?);
        }
        Some(out)
    }
}

/// Decodes one track's sectors; index = sector number.
fn decode_track(gcr: &[u8], track: u8, sectors: usize) -> Vec<Option<[u8; 256]>> {
    let mut found: Vec<Option<[u8; 256]>> = vec![None; sectors];
    let bits = Bits {
        data: gcr,
        len: gcr.len() * 8,
    };
    if bits.len < 64 {
        return found;
    }
    // Walk the bitstream twice so a block that wraps the index hole is seen.
    let mut pos = 0usize;
    let end = bits.len * 2;
    let mut pending_header: Option<usize> = None;
    while pos < end {
        // Find the end of a SYNC run: >= SYNC_BITS ones followed by a zero.
        let mut ones = 0usize;
        while pos < end && ones < SYNC_BITS {
            if bits.bit(pos) {
                ones += 1;
            } else {
                ones = 0;
            }
            pos += 1;
        }
        if ones < SYNC_BITS {
            break;
        }
        while pos < end && bits.bit(pos) {
            pos += 1;
        }
        // `pos` is now at the first bit after the SYNC.
        let Some(first) = bits.decode_bytes(pos, 1) else {
            continue;
        };
        match first[0] {
            0x08 => {
                if let Some(h) = bits.decode_bytes(pos, 8) {
                    let checksum_ok = h[1] == h[2] ^ h[3] ^ h[4] ^ h[5];
                    let sector = usize::from(h[2]);
                    pending_header =
                        (checksum_ok && h[3] == track && sector < sectors).then_some(sector);
                }
            }
            0x07 => {
                if let (Some(sector), Some(block)) =
                    (pending_header.take(), bits.decode_bytes(pos, 258))
                {
                    let payload: [u8; 256] = block[1..257].try_into().expect("256 bytes");
                    let checksum = payload.iter().fold(0u8, |a, b| a ^ b);
                    if checksum == block[257] && found[sector].is_none() {
                        found[sector] = Some(payload);
                    }
                }
            }
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROUPS: [u8; 16] = [
        0x0A, 0x0B, 0x12, 0x13, 0x0E, 0x0F, 0x16, 0x17, 0x09, 0x19, 0x1A, 0x1B, 0x0D, 0x1D, 0x1E,
        0x15,
    ];

    /// Packs bits (MSB first) into bytes.
    struct BitWriter {
        bytes: Vec<u8>,
        used: usize,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                used: 0,
            }
        }
        fn bit(&mut self, b: bool) {
            if self.used.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if b {
                *self.bytes.last_mut().unwrap() |= 0x80 >> (self.used % 8);
            }
            self.used += 1;
        }
        fn sync(&mut self) {
            for _ in 0..12 {
                self.bit(true);
            }
        }
        fn gcr_byte(&mut self, value: u8) {
            for group in [
                GROUPS[usize::from(value >> 4)],
                GROUPS[usize::from(value & 15)],
            ] {
                for i in (0..5).rev() {
                    self.bit(group >> i & 1 != 0);
                }
            }
        }
    }

    /// A track of `sectors` blocks whose sector `s` holds `fill(track, s)`.
    fn encode_track(track: u8, sectors: usize, fill: impl Fn(u8, usize) -> u8) -> Vec<u8> {
        let id = [0x41u8, 0x42];
        let mut w = BitWriter::new();
        for s in 0..sectors {
            w.sync();
            let chk = s as u8 ^ track ^ id[0] ^ id[1];
            for b in [0x08, chk, s as u8, track, id[1], id[0], 0x0F, 0x0F] {
                w.gcr_byte(b);
            }
            for _ in 0..9 {
                w.bit(false); // header gap (the real one is 0x55 bytes)
            }
            w.sync();
            w.gcr_byte(0x07);
            let data = [fill(track, s); 256];
            let mut sum = 0u8;
            for b in data {
                w.gcr_byte(b);
                sum ^= b;
            }
            w.gcr_byte(sum);
            w.gcr_byte(0);
            w.gcr_byte(0);
            for _ in 0..8 {
                w.bit(false);
            }
        }
        w.bytes
    }

    fn g64(tracks: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let half = 84usize;
        let mut file = Vec::new();
        file.extend_from_slice(MAGIC);
        file.push(0);
        file.push(half as u8);
        file.extend_from_slice(&7928u16.to_le_bytes());
        let table_end = HEADER_LEN + half * 8;
        let mut offsets = vec![0u32; half];
        let mut body = Vec::new();
        for (track, data) in tracks {
            offsets[(usize::from(*track) - 1) * 2] = (table_end + body.len()) as u32;
            body.extend_from_slice(&(data.len() as u16).to_le_bytes());
            body.extend_from_slice(data);
        }
        for o in offsets {
            file.extend_from_slice(&o.to_le_bytes());
        }
        file.extend(std::iter::repeat_n(0u8, half * 4)); // speed table
        file.extend_from_slice(&body);
        file
    }

    #[test]
    fn standard_sectors_decode_into_a_d64() {
        let tracks: Vec<(u8, Vec<u8>)> = (1..=35u8)
            .map(|t| {
                (
                    t,
                    encode_track(t, usize::from(d64::sectors_per_track(t)), |t, s| {
                        t.wrapping_mul(7).wrapping_add(s as u8)
                    }),
                )
            })
            .collect();
        let image = parse(&g64(&tracks)).unwrap();
        assert_eq!(image.track_count(), 35);
        let decoded = image.to_d64().unwrap();
        assert_eq!(decoded.bad_sectors, 0);
        for t in [1u8, 17, 18, 35] {
            for s in 0..d64::sectors_per_track(t) {
                let sector = decoded.disk.read_sector(t, s).unwrap();
                assert_eq!(
                    sector[0],
                    t.wrapping_mul(7).wrapping_add(s),
                    "track {t} sector {s}"
                );
                assert!(sector.iter().all(|&b| b == sector[0]));
            }
        }
    }

    fn a_missing_or_damaged_sector_is_counted_and_left_zero() {
        let mut t1 = encode_track(1, 21, |_, _| 0x55);
        // Damage the middle of the track: corrupt many bytes.
        let mid = t1.len() / 2;
        for b in &mut t1[mid..mid + 400] {
            *b ^= 0xFF;
        }
        let mut tracks = vec![(1u8, t1)];
        tracks.push((2, encode_track(2, 21, |_, _| 0x11)));
        let decoded = parse(&g64(&tracks)).unwrap().to_d64().unwrap();
        assert!(decoded.bad_sectors > 0);
        assert_eq!(decoded.disk.read_sector(2, 3).unwrap()[0], 0x11);
    }

    fn header_checks_reject_a_non_g64_and_a_truncated_table() {
        assert_eq!(parse(&[0; 4]).unwrap_err(), Error::TooShort(4));
        assert_eq!(parse(&[0; 20]).unwrap_err(), Error::BadMagic);
        let mut bad = g64(&[(1, vec![0xFF; 100])]);
        bad.truncate(HEADER_LEN + 16);
        assert_eq!(parse(&bad).unwrap_err(), Error::BadTrackTable);
        let mut v = g64(&[]);
        v[8] = 1;
        assert_eq!(parse(&v).unwrap_err(), Error::UnsupportedVersion(1));
    }
    /// Merged (every check kept): `header_checks_reject_a_non_g64_and_a_truncated_table`, `a_missing_or_damaged_sector_is_counted_and_left_zero`.
    #[test]
    fn damaged_files() {
        header_checks_reject_a_non_g64_and_a_truncated_table();
        a_missing_or_damaged_sector_is_counted_and_left_zero();
    }

    #[test]
    fn tracks_above_35_make_a_40_track_image() {
        let tracks = vec![
            (1u8, encode_track(1, 21, |_, _| 1)),
            (36, encode_track(36, 17, |_, _| 2)),
        ];
        let decoded = parse(&g64(&tracks)).unwrap().to_d64().unwrap();
        assert_eq!(decoded.disk.variant(), d64::Variant::Track40);
        assert_eq!(decoded.disk.read_sector(36, 4).unwrap()[10], 2);
    }
}
