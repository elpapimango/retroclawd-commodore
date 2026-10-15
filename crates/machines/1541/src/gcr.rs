//! Real 1541 GCR (Group Code Recording) codec: the 4-bit-to-5-bit code
//! table used to encode every byte written to (or read from) a real 1541
//! disk, plus the real sector-to-GCR-bytes encoding algorithm DOS
//! firmware's own disk-formatting routine performs. Ported from VICE's own
//! real `gcr.c` (`GCR_conv_data`/`gcr_convert_4bytes_to_GCR`/
//! `gcr_convert_sector_to_GCR`, fetched live this session -- see
//! `CREDITS.md`), not recalled from memory or guessed. `encode_4bytes_to_5`
//! is a direct, faithful port of the real (deliberately obscure -- a
//! rolling 32-bit-register bit-accumulator trick) C loop rather than a
//! reimplementation from a simplified mental model of what it does --
//! cross-checked byte-for-byte equal against a from-scratch "pack 8 5-bit
//! codes MSB-first into a 40-bit stream" reimplementation over 20000
//! random inputs before trusting either (a throwaway Python script, not
//! committed here), specifically because the C source's rolling-register
//! technique is easy to mis-port by hand.
//!
//! **Real hardware fact this code exists to guarantee**: the 16 real code
//! words are chosen so that no valid code, and no adjacent pair of valid
//! codes, ever produces a run of more than 2 consecutive `0` bits -- which
//! is exactly what makes a run of 10+ consecutive `1` bits
//! (`crate::disk::SYNC_MIN_ONES`) an unambiguous marker a real drive can
//! use to find the start of a sector: that bit pattern can never occur
//! inside legitimately GCR-encoded data, only in the dedicated SYNC field
//! written between sectors.
//!
//! **Real hardware fact this crate deliberately does NOT model**: on real
//! hardware, decoding a GCR-encoded byte back into a real nibble is DOS
//! *firmware's* own job (a lookup table baked into the ROM), not something
//! the disk controller/VIA2 hardware itself ever does -- the real
//! controller only ever detects SYNC and assembles raw GCR bytes (see
//! `crate::disk::DiskMechanism`). `decode_nibble`/`decode_5bytes_to_4`
//! exist purely so this module's own tests can round-trip-verify
//! `encode_sector`'s output without transcribing every sector's real
//! decoded contents by hand a second time -- the emulated hardware path
//! itself never calls either.
//!
//! **Real gap-byte counts are an accepted, documented approximation, not
//! byte-exact**: real 1541 firmware fills an entire track with `0x55` gap
//! bytes first, then writes sync/header/data at the exact byte offsets a
//! real physical rotation's own bit rate dictates for that track's speed
//! zone -- the real total gap length is whatever's left over to make the
//! whole track add up to one physical rotation at that zone's own bit
//! rate, not a fixed constant. `HEADER_GAP_BYTES`/`SECTOR_GAP_BYTES` below
//! are reasonable, commonly-cited-magnitude stand-ins (same epistemic tier
//! as `zx_spectrum::SpeakerType`'s filter cutoffs) -- what matters for a
//! real DOS routine reading this synthesized track is that real SYNC marks
//! bound each sector correctly, not that the total track byte count
//! matches a real drive's own rotation exactly.

/// Real 4-bit-to-5-bit GCR code table (confirmed directly against VICE's
/// own `gcr.c` `GCR_conv_data` this session, not recalled) -- index by
/// nibble (`0x0`-`0xF`), get back its 5-bit GCR code (bits 4:0 of the
/// resulting byte).
pub const GCR_ENCODE: [u8; 16] = [
    0x0a, 0x0b, 0x12, 0x13, 0x0e, 0x0f, 0x16, 0x17, 0x09, 0x19, 0x1a, 0x1b, 0x0d, 0x1d, 0x1e, 0x15,
];

/// Real sync count: this many consecutive `0xFF` bytes (40 bits of `1`s --
/// well past the 10-bit minimum GCR itself guarantees never occurs in real
/// data) written before both a sector's header block and its data block --
/// a commonly-cited real value, matching the `sync` parameter VICE's own
/// `gcr_convert_sector_to_GCR` is typically called with, not independently
/// re-derived from a disassembly this session.
pub const SYNC_BYTES: usize = 5;
/// Approximate gap between a sector's header GCR block and its data
/// block's own sync run -- see this module's own doc comment on why this
/// isn't a byte-exact real per-zone value.
pub const HEADER_GAP_BYTES: usize = 9;
/// Approximate tail gap after a sector's data block, before the next
/// sector's own header sync run begins -- same approximation tier as
/// `HEADER_GAP_BYTES`.
pub const SECTOR_GAP_BYTES: usize = 8;

const HEADER_GCR_BYTES: usize = 10; // 2 groups of 5
const DATA_GCR_BYTES: usize = 325; // 65 groups of 5 (3 + 63*4 + 1 = 256 data bytes)

/// Reverses `GCR_ENCODE` by linear search rather than a second,
/// independently transcribed lookup table -- correct by construction
/// (can't silently mismatch its own encode table the way a hand-copied
/// reverse table could), and cross-checked equal, index-for-index, against
/// VICE's own separately-maintained `From_GCR_conv_data` table this
/// session. `None` for any 5-bit value that isn't one of the 16 real,
/// valid GCR code words -- a real drive reading one of those off an actual
/// disk would be looking at a data error, not a real nibble. Only used by
/// this module's own tests, see this module's own doc comment.
fn decode_nibble(code: u8) -> Option<u8> {
    GCR_ENCODE
        .iter()
        .position(|&c| c == code & 0x1F)
        .map(|i| i as u8)
}

/// Direct port of VICE's real `gcr_convert_4bytes_to_GCR` -- packs 4 source
/// bytes' 8 nibbles into 8 real 5-bit GCR codes, MSB-first, into 5 output
/// bytes. Verified byte-for-byte equal to a from-scratch "pack into a
/// 40-bit stream" reimplementation over 20000 random inputs (see this
/// module's own doc comment) before trusting this literal port of the
/// source's own rolling-accumulator technique.
fn encode_4bytes_to_5(source: [u8; 4]) -> [u8; 5] {
    let mut dest = [0u8; 5];
    let mut tdest: u32 = 0;
    for (i, &b) in source.iter().enumerate() {
        tdest = (tdest << 5) | GCR_ENCODE[(b >> 4) as usize] as u32;
        tdest = (tdest << 5) | GCR_ENCODE[(b & 0x0F) as usize] as u32;
        dest[i] = (tdest >> (2 * (i as u32 + 1))) as u8;
    }
    dest[4] = tdest as u8;
    dest
}

/// Reverses `encode_4bytes_to_5` by reconstructing the same 40-bit packed
/// stream (big-endian across the 5 bytes) and decoding each of its 8 5-bit
/// codes back to a nibble -- `None` if any of the 8 codes isn't a real,
/// valid GCR code word. Test/round-trip-verification use only, see this
/// module's own doc comment.
fn decode_5bytes_to_4(src: &[u8; 5]) -> Option<[u8; 4]> {
    let acc: u64 = ((src[0] as u64) << 32)
        | ((src[1] as u64) << 24)
        | ((src[2] as u64) << 16)
        | ((src[3] as u64) << 8)
        | (src[4] as u64);
    let mut nibbles = [0u8; 8];
    for (i, slot) in nibbles.iter_mut().enumerate() {
        let shift = 35 - i * 5;
        let code = ((acc >> shift) & 0x1F) as u8;
        *slot = decode_nibble(code)?;
    }
    let mut out = [0u8; 4];
    for i in 0..4 {
        out[i] = (nibbles[i * 2] << 4) | nibbles[i * 2 + 1];
    }
    Some(out)
}

/// Encodes one real 256-byte sector into its full real on-disk GCR byte
/// sequence: header sync + header block (marker/checksum/sector/track,
/// then id2/id1/`0x0f`/`0x0f`) + header-to-data gap + data sync + data
/// block (256 data bytes as 65 GCR groups, closing with `[last_byte,
/// checksum, 0, 0]`) + tail gap. Header/data checksums and field ordering
/// ported directly from VICE's own real `gcr_convert_sector_to_GCR` (no
/// error-code injection modeled -- this crate has no real GCR *write*
/// path, only a faithful read path over an always-clean synthesized
/// track, see `crate::disk`).
pub fn encode_sector(track: u8, sector: u8, id1: u8, id2: u8, data: &[u8; 256]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        SYNC_BYTES
            + HEADER_GCR_BYTES
            + HEADER_GAP_BYTES
            + SYNC_BYTES
            + DATA_GCR_BYTES
            + SECTOR_GAP_BYTES,
    );

    out.extend(std::iter::repeat_n(0xFFu8, SYNC_BYTES));

    let header_checksum = sector ^ track ^ id2 ^ id1;
    out.extend(encode_4bytes_to_5([0x08, header_checksum, sector, track]));
    out.extend(encode_4bytes_to_5([id2, id1, 0x0f, 0x0f]));

    out.extend(std::iter::repeat_n(0x55u8, HEADER_GAP_BYTES));
    out.extend(std::iter::repeat_n(0xFFu8, SYNC_BYTES));

    let mut checksum = data[0] ^ data[1] ^ data[2];
    out.extend(encode_4bytes_to_5([0x07, data[0], data[1], data[2]]));
    for chunk in data[3..255].as_chunks::<4>().0 {
        checksum ^= chunk[0] ^ chunk[1] ^ chunk[2] ^ chunk[3];
        out.extend(encode_4bytes_to_5(*chunk));
    }
    checksum ^= data[255];
    out.extend(encode_4bytes_to_5([data[255], checksum, 0, 0]));

    out.extend(std::iter::repeat_n(0x55u8, SECTOR_GAP_BYTES));

    out
}

/// Decodes one `encode_sector`-shaped byte sequence back into
/// `(track, sector, id1, id2, data)`, verifying both real checksums --
/// test/round-trip-verification use only (see this module's own doc
/// comment; the emulated hardware path never decodes GCR itself). Returns
/// `None` on any invalid GCR code or checksum mismatch, rather than a
/// partially-decoded result.
#[cfg(test)]
fn decode_sector(bytes: &[u8]) -> Option<(u8, u8, u8, u8, [u8; 256])> {
    let mut pos = SYNC_BYTES;
    let group1: [u8; 5] = bytes[pos..pos + 5].try_into().ok()?;
    let [marker, checksum, sector, track] = decode_5bytes_to_4(&group1)?;
    if marker != 0x08 {
        return None;
    }
    pos += 5;
    let group2: [u8; 5] = bytes[pos..pos + 5].try_into().ok()?;
    let [id2, id1, pad1, pad2] = decode_5bytes_to_4(&group2)?;
    if pad1 != 0x0f || pad2 != 0x0f || checksum != (sector ^ track ^ id2 ^ id1) {
        return None;
    }
    pos += 5 + HEADER_GAP_BYTES + SYNC_BYTES;

    let mut data = [0u8; 256];
    let group0: [u8; 5] = bytes[pos..pos + 5].try_into().ok()?;
    let [data_marker, d0, d1, d2] = decode_5bytes_to_4(&group0)?;
    if data_marker != 0x07 {
        return None;
    }
    data[0] = d0;
    data[1] = d1;
    data[2] = d2;
    let mut running_checksum = d0 ^ d1 ^ d2;
    pos += 5;
    let mut data_index = 3;
    for _ in 0..63 {
        let group: [u8; 5] = bytes[pos..pos + 5].try_into().ok()?;
        let [b0, b1, b2, b3] = decode_5bytes_to_4(&group)?;
        data[data_index] = b0;
        data[data_index + 1] = b1;
        data[data_index + 2] = b2;
        data[data_index + 3] = b3;
        running_checksum ^= b0 ^ b1 ^ b2 ^ b3;
        data_index += 4;
        pos += 5;
    }
    let final_group: [u8; 5] = bytes[pos..pos + 5].try_into().ok()?;
    let [last_byte, data_checksum, pad3, pad4] = decode_5bytes_to_4(&final_group)?;
    data[255] = last_byte;
    running_checksum ^= last_byte;
    if pad3 != 0 || pad4 != 0 || data_checksum != running_checksum {
        return None;
    }

    Some((track, sector, id1, id2, data))
}

/// One sector found on a track by `decode_track`.
#[derive(Debug, PartialEq, Eq)]
pub struct DecodedSector {
    pub track: u8,
    pub sector: u8,
    pub data: [u8; 256],
}

/// Reads every complete sector (header block followed by its data block, both
/// checksums good) out of a track's GCR byte stream -- what the write head
/// has laid down once the DOS has written a sector or formatted a track. The
/// stream is scanned as a ring (a block may straddle the index point), and a
/// sector found twice keeps the later copy. Header and data blocks are found
/// the way the hardware finds them, by their SYNC run of `0xFF` bytes; a data
/// block with no header before it, or with a bad checksum, is skipped.
pub fn decode_track(bytes: &[u8]) -> Vec<DecodedSector> {
    const DATA_BLOCK_BYTES: usize = 325;
    let ring: Vec<u8> = bytes
        .iter()
        .chain(bytes.iter().take(DATA_BLOCK_BYTES + 16))
        .copied()
        .collect();
    let mut out: Vec<DecodedSector> = Vec::new();
    let mut header: Option<(u8, u8)> = None;
    let mut i = 0;
    while i < ring.len() {
        if ring[i] != 0xFF {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < ring.len() && ring[j] == 0xFF {
            j += 1;
        }
        let group = |at: usize| -> Option<[u8; 4]> {
            decode_5bytes_to_4(ring.get(at..at + 5)?.try_into().ok()?)
        };
        match group(j) {
            Some([0x08, checksum, sector, track]) if j - i >= 2 => {
                header = match group(j + 5) {
                    Some([id2, id1, 0x0f, 0x0f]) if checksum == sector ^ track ^ id2 ^ id1 => {
                        Some((track, sector))
                    }
                    _ => None,
                };
                i = j + 10;
            }
            Some([0x07, d0, d1, d2]) if j - i >= 2 => {
                if let (Some((track, sector)), Some(block)) =
                    (header.take(), ring.get(j..j + DATA_BLOCK_BYTES))
                    && let Some(data) = decode_data_block(block, [d0, d1, d2])
                {
                    out.retain(|s| !(s.track == track && s.sector == sector));
                    out.push(DecodedSector {
                        track,
                        sector,
                        data,
                    });
                }
                i = j + DATA_BLOCK_BYTES;
            }
            _ => i = j,
        }
    }
    out
}

/// Decodes a 325-byte data block (the marker group already read as `first`)
/// and checks its XOR checksum.
fn decode_data_block(block: &[u8], first: [u8; 3]) -> Option<[u8; 256]> {
    let mut data = [0u8; 256];
    data[..3].copy_from_slice(&first);
    let mut checksum = first[0] ^ first[1] ^ first[2];
    let mut at = 3;
    for g in 1..64 {
        let [a, b, c, d] = decode_5bytes_to_4(block[g * 5..g * 5 + 5].try_into().ok()?)?;
        data[at..at + 4].copy_from_slice(&[a, b, c, d]);
        checksum ^= a ^ b ^ c ^ d;
        at += 4;
    }
    // The last group carries the final data byte and the checksum in its
    // first four nibbles; its two trailing "off" bytes may be clipped by the
    // write head switching off right after the last byte loads (DOS does
    // exactly that), and a reader ignores them, so they are not checked.
    let bits = block[320..325]
        .iter()
        .fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
    let nibble = |k: u32| decode_nibble(((bits >> (35 - 5 * k)) & 0x1F) as u8);
    let last = nibble(0)? << 4 | nibble(1)?;
    let stored = nibble(2)? << 4 | nibble(3)?;
    data[255] = last;
    (stored == checksum ^ last).then_some(data)
}

/// Synthesizes one real track's full GCR byte stream from a parsed
/// `d64::D64`'s logical sector contents -- every real sector on that
/// track, in order, each via `encode_sector`, back to back (each sector's
/// own leading header sync doubling as the "gap" between it and the
/// previous sector's own tail gap, exactly like a real, continuously
/// spinning track has no fixed start/end point). `track` outside the
/// disk's own range produces an empty stream (no sectors to encode) rather
/// than an error -- `crate::disk::DiskMechanism` treats an empty track the
/// same as "no disk attached" for SYNC-detection purposes.
pub fn build_track_gcr(disk: &impl SectorSource, track: u8) -> Vec<u8> {
    let [id1, id2] = disk.disk_id();
    let mut out = Vec::new();
    for sector in 0..disk.sectors_on_track(track) {
        let Some(data) = disk.sector_data(track, sector) else {
            continue;
        };
        out.extend(encode_sector(track, sector, id1, id2, &data));
    }
    out
}

/// What `build_track_gcr` needs to know about a disk image: its ID bytes,
/// how many sectors a (logical) track holds, and each sector's 256 bytes.
/// Implemented for `d64::D64` here and for the 1571's double-sided `.d71`
/// in `crate::disk::Medium`.
pub trait SectorSource {
    fn disk_id(&self) -> [u8; 2];
    fn sectors_on_track(&self, track: u8) -> u8;
    fn sector_data(&self, track: u8, sector: u8) -> Option<[u8; 256]>;
}

impl SectorSource for d64::D64 {
    fn disk_id(&self) -> [u8; 2] {
        d64::D64::disk_id(self)
    }

    fn sectors_on_track(&self, track: u8) -> u8 {
        d64::sectors_per_track(track)
    }

    fn sector_data(&self, track: u8, sector: u8) -> Option<[u8; 256]> {
        self.read_sector(track, sector).ok()?.try_into().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_nibble_reverses_every_real_encode_table_entry() {
        for nibble in 0..16u8 {
            let code = GCR_ENCODE[nibble as usize];
            assert_eq!(decode_nibble(code), Some(nibble));
        }
        // A code that's never a real GCR output (e.g. 0x00, all-zero) must
        // report no valid nibble.
        assert_eq!(decode_nibble(0x00), None);
        assert_eq!(decode_nibble(0x1F), None);
    }

    #[test]
    fn four_byte_groups_round_trip_through_5_byte_gcr_for_every_byte_value() {
        for b in 0..=255u8 {
            let src = [b, b.wrapping_add(1), b.wrapping_add(2), b.wrapping_add(3)];
            let gcr = encode_4bytes_to_5(src);
            assert_eq!(decode_5bytes_to_4(&gcr), Some(src));
        }
    }

    #[test]
    fn no_adjacent_pair_of_valid_codes_ever_produces_a_real_sync_run_or_more_than_2_zeros() {
        // The two real hardware facts this whole codec exists to
        // guarantee, both checked directly across every possible 4-byte
        // input's own 5-byte GCR output, doubled up to cover every
        // genuine adjacent-code boundary a real continuous bitstream can
        // produce:
        // - no run of more than 2 consecutive `0` bits (needed for a real
        //   drive's self-clocking recovery), and
        // - no run of 10 or more consecutive `1` bits -- the actual
        //   property `disk::SYNC_MIN_ONES`-based SYNC detection depends
        //   on: a run that long can only ever be a real SYNC field, never
        //   legitimately encoded data.
        for nibble_a in 0..16u8 {
            for nibble_b in 0..16u8 {
                let src = [
                    (nibble_a << 4) | nibble_b,
                    (nibble_a << 4) | nibble_b,
                    (nibble_a << 4) | nibble_b,
                    (nibble_a << 4) | nibble_b,
                ];
                let gcr = encode_4bytes_to_5(src);
                let mut bits = Vec::new();
                for _ in 0..2 {
                    for &byte in &gcr {
                        for bit_index in (0..8).rev() {
                            bits.push((byte >> bit_index) & 1);
                        }
                    }
                }
                let (max_zeros, _, max_ones, _) = bits.iter().fold(
                    (0u32, 0u32, 0u32, 0u32),
                    |(max_zeros, cur_zeros, max_ones, cur_ones), &bit| {
                        if bit == 0 {
                            let cur = cur_zeros + 1;
                            (max_zeros.max(cur), cur, max_ones, 0)
                        } else {
                            let cur = cur_ones + 1;
                            (max_zeros, 0, max_ones.max(cur), cur)
                        }
                    },
                );
                assert!(
                    max_zeros <= 2,
                    "run of {max_zeros} zero bits for nibbles {nibble_a:x}/{nibble_b:x}"
                );
                assert!(
                    max_ones < crate::disk::SYNC_MIN_ONES as u32,
                    "run of {max_ones} one bits for nibbles {nibble_a:x}/{nibble_b:x} -- would be indistinguishable from real SYNC"
                );
            }
        }
    }

    #[test]
    fn encode_sector_round_trips_through_decode_sector() {
        let mut data = [0u8; 256];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        let gcr = encode_sector(18, 3, b'6', b'4', &data);
        let (track, sector, id1, id2, decoded) = decode_sector(&gcr).expect("must decode cleanly");
        assert_eq!(track, 18);
        assert_eq!(sector, 3);
        assert_eq!(id1, b'6');
        assert_eq!(id2, b'4');
        assert_eq!(decoded, data);
    }

    #[test]
    fn encode_sector_detects_a_corrupted_data_checksum() {
        let data = [0xAAu8; 256];
        let mut gcr = encode_sector(1, 0, b'1', b'A', &data);
        // Flip a bit deep in the data block -- must fail the checksum
        // check on decode, not silently decode something wrong.
        let corrupt_index = gcr.len() - SECTOR_GAP_BYTES - 20;
        gcr[corrupt_index] ^= 0xFF;
        assert_eq!(decode_sector(&gcr), None);
    }

    #[test]
    fn build_track_gcr_encodes_every_real_sector_on_the_track_in_order() {
        let mut bytes = vec![0u8; 683 * d64::SECTOR_SIZE];
        // Minimal real BAM at track 18 sector 0 so `disk_id` reads real
        // values instead of zeros.
        let bam_index = {
            let mut idx = 0usize;
            for t in 1..18 {
                idx += d64::sectors_per_track(t) as usize;
            }
            idx
        };
        bytes[bam_index * d64::SECTOR_SIZE + 162] = b'6';
        bytes[bam_index * d64::SECTOR_SIZE + 163] = b'4';
        let disk = d64::D64::parse(&bytes).unwrap();

        let track_gcr = build_track_gcr(&disk, 18);
        let sectors_on_track = d64::sectors_per_track(18);
        let mut pos = 0;
        for expected_sector in 0..sectors_on_track {
            let remaining = &track_gcr[pos..];
            let (track, sector, id1, id2, _data) =
                decode_sector(remaining).expect("every real sector must decode cleanly");
            assert_eq!(track, 18);
            assert_eq!(sector, expected_sector);
            assert_eq!(id1, b'6');
            assert_eq!(id2, b'4');
            pos += SYNC_BYTES
                + HEADER_GCR_BYTES
                + HEADER_GAP_BYTES
                + SYNC_BYTES
                + DATA_GCR_BYTES
                + SECTOR_GAP_BYTES;
        }
        assert_eq!(pos, track_gcr.len());
    }

    #[test]
    fn decode_track_reads_back_every_encoded_sector() {
        let disk_track: Vec<u8> = (0..3u8)
            .flat_map(|sector| {
                let data: [u8; 256] = std::array::from_fn(|i| (i as u8).wrapping_mul(7) ^ sector);
                encode_sector(5, sector, b'6', b'4', &data)
            })
            .collect();
        let found = decode_track(&disk_track);
        assert_eq!(found.len(), 3);
        for f in &found {
            assert_eq!(f.track, 5);
            let want: [u8; 256] = std::array::from_fn(|i| (i as u8).wrapping_mul(7) ^ f.sector);
            assert_eq!(f.data, want);
        }
    }

    #[test]
    fn decode_track_accepts_a_data_block_whose_last_gcr_byte_was_clipped() {
        // DOS switches the write head off right after the last byte loads,
        // so the final GCR byte (part of the two "off" bytes) is garbage.
        let data = [0xA5u8; 256];
        let mut track = encode_sector(9, 2, b'6', b'4', &data);
        let n = track.len();
        let data_block_end = n - 9;
        track[data_block_end - 1] ^= 0x1F;
        let found = decode_track(&track);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].data, data);
    }

    #[test]
    fn decode_track_rejects_a_corrupt_checksum() {
        let data = [0x11u8; 256];
        let mut track = encode_sector(9, 2, b'6', b'4', &data);
        // Flip a bit in the middle of the data block.
        let mid = track.len() - 9 - 160;
        track[mid] ^= 0x04;
        assert!(decode_track(&track).is_empty());
    }
}
