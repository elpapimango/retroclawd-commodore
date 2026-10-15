//! The Commodore KERNAL's standard cassette format, as it appears in a
//! `.tap` pulse stream. Everything here was read off a recording of the real
//! KERNAL's own SAVE (see `crates/machines/c64/tests/tape_roundtrip.rs`),
//! not recalled:
//!
//! * a pulse is *short*, *medium* or *long*; in recordings they measure
//!   about 362, 510 and 680 cycles, in third-party `.tap` files `0x30`,
//!   `0x42`, `0x56` units (384, 528, 688), so the classifier splits at 436
//!   and 596 cycles, midway between both families;
//! * a byte is a *long, medium* marker followed by nine bit-pairs, least
//!   significant bit first: bit 0 is *short, medium*, bit 1 is *medium,
//!   short*; the ninth bit is the odd-parity bit;
//! * a block is a leader of short pulses, then the countdown bytes
//!   `$89..$81` (first copy) or `$09..$01` (the repeat), the payload, an XOR
//!   checksum of the payload, and for the first copy an end marker
//!   (*long, short*) followed by 79 more short pulses;
//! * every block is written twice, the second copy being a fallback when the
//!   first one is damaged;
//! * a *header* block's payload is 192 bytes: the file type (1 relocatable
//!   BASIC program, 3 absolute program, 4 sequential-data header, 5 end of
//!   tape), start address, end address (exclusive), the 16-character name
//!   and `$20` padding; the *data* block that follows carries
//!   `end - start` bytes. Leaders measured: 27136 short pulses before a
//!   header, 5377 before data.

use crate::Tape;

/// Standard pulse lengths used when writing (`0x30`, `0x42`, `0x56` units).
pub const SHORT: u32 = 0x30 * 8;
pub const MEDIUM: u32 = 0x42 * 8;
pub const LONG: u32 = 0x56 * 8;

const SHORT_MEDIUM_SPLIT: u32 = 436;
const MEDIUM_LONG_SPLIT: u32 = 596;
const HEADER_LEADER: usize = 27_136;
const DATA_LEADER: usize = 5_377;
const GAP: usize = 79;
const HEADER_PAYLOAD: usize = 192;

/// One of the three pulse classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sym {
    S,
    M,
    L,
}

fn classify(cycles: u32) -> Sym {
    if cycles < SHORT_MEDIUM_SPLIT {
        Sym::S
    } else if cycles < MEDIUM_LONG_SPLIT {
        Sym::M
    } else {
        Sym::L
    }
}

/// A program stored in the standard format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RomFile {
    /// Header type: 1 relocatable BASIC program, 3 absolute program.
    pub kind: u8,
    /// The 16 name bytes as stored (PETSCII, space padded).
    pub name: [u8; 16],
    pub start: u16,
    /// One past the last byte.
    pub end: u16,
    pub data: Vec<u8>,
}

impl RomFile {
    /// The name without its trailing padding.
    pub fn trimmed_name(&self) -> &[u8] {
        let len = self
            .name
            .iter()
            .rposition(|&b| b != 0x20 && b != 0xA0 && b != 0)
            .map_or(0, |p| p + 1);
        &self.name[..len]
    }
}

struct Block {
    /// `0x89` for the first copy, `0x09` for the repeat.
    copy: u8,
    payload: Vec<u8>,
    checksum_ok: bool,
}

fn read_byte(syms: &[Sym], pos: usize) -> Option<(u8, usize)> {
    if syms.get(pos) != Some(&Sym::L) || syms.get(pos + 1) != Some(&Sym::M) {
        return None;
    }
    let mut p = pos + 2;
    let mut value = 0u8;
    let mut ones = 0u32;
    for bit in 0..9 {
        let bit_value = match (syms.get(p)?, syms.get(p + 1)?) {
            (Sym::S, Sym::M) => 0,
            (Sym::M, Sym::S) => 1,
            _ => return None,
        };
        p += 2;
        ones += bit_value;
        if bit < 8 {
            value |= (bit_value as u8) << bit;
        }
    }
    // Odd parity over all nine bits.
    (ones % 2 == 1).then_some((value, p))
}

fn read_block(syms: &[Sym], pos: usize) -> Option<(Block, usize)> {
    let (first, mut p) = read_byte(syms, pos)?;
    if first != 0x89 && first != 0x09 {
        return None;
    }
    for step in 1..9u8 {
        let (value, next) = read_byte(syms, p)?;
        if value != first - step {
            return None;
        }
        p = next;
    }
    let mut bytes = Vec::new();
    while let Some((value, next)) = read_byte(syms, p) {
        bytes.push(value);
        p = next;
    }
    let checksum = bytes.pop()?;
    let checksum_ok = bytes.iter().fold(0u8, |acc, b| acc ^ b) == checksum;
    Some((
        Block {
            copy: first,
            payload: bytes,
            checksum_ok,
        },
        p,
    ))
}

/// Finds every block (both copies) in the stream.
fn scan_blocks(syms: &[Sym]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut i = 0;
    while i + 1 < syms.len() {
        if syms[i] == Sym::L
            && syms[i + 1] == Sym::M
            && let Some((block, next)) = read_block(syms, i)
        {
            blocks.push(block);
            i = next;
            continue;
        }
        i += 1;
    }
    blocks
}

/// Pairs each first copy with its repeat and keeps whichever copy has a
/// good checksum (the first when both do). A block whose both copies are
/// damaged is dropped.
fn good_payloads(blocks: Vec<Block>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut iter = blocks.into_iter().peekable();
    while let Some(block) = iter.next() {
        if block.copy != 0x89 {
            continue; // a stray repeat
        }
        let repeat = if iter.peek().is_some_and(|b| b.copy == 0x09) {
            iter.next()
        } else {
            None
        };
        if block.checksum_ok {
            out.push(block.payload);
        } else if let Some(repeat) = repeat.filter(|b| b.checksum_ok) {
            out.push(repeat.payload);
        }
    }
    out
}

/// Every program (header types 1 and 3) stored in the standard format.
pub fn decode_files(tape: &Tape) -> Vec<RomFile> {
    let syms: Vec<Sym> = tape.pulses().iter().map(|&c| classify(c)).collect();
    let payloads = good_payloads(scan_blocks(&syms));
    let mut files = Vec::new();
    let mut i = 0;
    while i < payloads.len() {
        let header = &payloads[i];
        i += 1;
        if header.len() != HEADER_PAYLOAD || !matches!(header[0], 1 | 3) {
            continue;
        }
        let start = u16::from_le_bytes([header[1], header[2]]);
        let end = u16::from_le_bytes([header[3], header[4]]);
        let Some(data) = payloads.get(i) else { break };
        if data.len() != usize::from(end.wrapping_sub(start)) {
            continue;
        }
        i += 1;
        let mut name = [0x20u8; 16];
        name.copy_from_slice(&header[5..21]);
        files.push(RomFile {
            kind: header[0],
            name,
            start,
            end,
            data: data.clone(),
        });
    }
    files
}

fn push_byte(out: &mut Vec<u32>, value: u8) {
    out.extend([LONG, MEDIUM]);
    let mut ones = 0u32;
    for bit in 0..8 {
        let b = value >> bit & 1;
        ones += u32::from(b);
        out.extend(if b == 0 {
            [SHORT, MEDIUM]
        } else {
            [MEDIUM, SHORT]
        });
    }
    let parity = u8::from(ones.is_multiple_of(2));
    out.extend(if parity == 0 {
        [SHORT, MEDIUM]
    } else {
        [MEDIUM, SHORT]
    });
}

fn push_block(out: &mut Vec<u32>, payload: &[u8], repeat: bool) {
    let base = if repeat { 0x09 } else { 0x89 };
    for step in 0..9u8 {
        push_byte(out, base - step);
    }
    for &b in payload {
        push_byte(out, b);
    }
    push_byte(out, payload.iter().fold(0u8, |acc, b| acc ^ b));
}

fn push_pair(out: &mut Vec<u32>, leader: usize, payload: &[u8]) {
    out.extend(std::iter::repeat_n(SHORT, leader));
    push_block(out, payload, false);
    out.extend([LONG, SHORT]);
    out.extend(std::iter::repeat_n(SHORT, GAP));
    push_block(out, payload, true);
}

/// A program as the KERNAL's SAVE would put it on tape: header block twice,
/// then the data block twice.
pub fn encode_program(kind: u8, name: &[u8], start: u16, data: &[u8]) -> Vec<u32> {
    let end = start.wrapping_add(data.len() as u16);
    let mut header = vec![0x20u8; HEADER_PAYLOAD];
    header[0] = kind;
    header[1..3].copy_from_slice(&start.to_le_bytes());
    header[3..5].copy_from_slice(&end.to_le_bytes());
    for (slot, &b) in header[5..21].iter_mut().zip(name) {
        *slot = b;
    }
    let mut out = Vec::new();
    push_pair(&mut out, HEADER_LEADER, &header);
    push_pair(&mut out, DATA_LEADER, data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tape(pulses: Vec<u32>) -> Tape {
        Tape::from_pulses(pulses)
    }

    #[test]
    fn a_saved_program_decodes_back_to_the_same_name_address_and_bytes() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
        let t = tape(encode_program(1, b"HELLO", 0x0801, &data));
        let files = decode_files(&t);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!((f.kind, f.start, f.end), (1, 0x0801, 0x0801 + 300));
        assert_eq!(f.trimmed_name(), b"HELLO");
        assert_eq!(f.data, data);
    }

    #[test]
    fn several_programs_on_one_tape_come_back_in_order() {
        let mut pulses = encode_program(3, b"ONE", 0xC000, &[1, 2, 3]);
        pulses.extend(encode_program(1, b"TWO", 0x0801, &[9; 40]));
        let files = decode_files(&tape(pulses));
        let names: Vec<&[u8]> = files.iter().map(|f| f.trimmed_name()).collect();
        assert_eq!(names, vec![&b"ONE"[..], &b"TWO"[..]]);
    }

    #[test]
    fn a_damaged_first_copy_falls_back_to_the_repeat() {
        let data = vec![0x55u8; 20];
        let mut pulses = encode_program(3, b"X", 0x4000, &data);
        // Break the byte marker of the 13th byte of the data block's first
        // copy: that copy can no longer be read to its checksum.
        let data_pair = DATA_LEADER + 30 * 20 + 2 + GAP + 30 * 20;
        let data_block_start = pulses.len() - data_pair + DATA_LEADER;
        let target = data_block_start + 12 * 20;
        assert_eq!(pulses[target], LONG);
        pulses[target] = SHORT;
        let files = decode_files(&tape(pulses));
        assert_eq!(files.len(), 1, "the repeat copy rescued the file");
        assert_eq!(files[0].data, data);
    }

    #[test]
    fn recorded_style_pulse_widths_classify_like_the_standard_ones() {
        for (cycles, sym) in [
            (362, Sym::S),
            (384, Sym::S),
            (510, Sym::M),
            (528, Sym::M),
            (680, Sym::L),
            (688, Sym::L),
        ] {
            assert_eq!(classify(cycles), sym, "{cycles}");
        }
    }

    #[test]
    fn noise_and_non_standard_tapes_decode_to_nothing() {
        assert!(decode_files(&tape(vec![300; 1000])).is_empty());
        assert!(decode_files(&tape(vec![])).is_empty());
    }
}
