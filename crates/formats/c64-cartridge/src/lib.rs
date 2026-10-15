//! Parser for the Commodore 64 `.crt` cartridge container.
//!
//! A format crate: bytes in, plain data out, no dependency on `machine` or
//! the C64 (same split as `d64`/`c64-tape`). What a cartridge *does* once
//! plugged in -- bank switching, the EXROM/GAME lines -- is `c64`'s job.
//!
//! Layout (VICE manual's "CRT file format", Peter Schepers' `CRT.TXT`, and
//! VICE's `crt.c`; see `CREDITS.md`):
//!
//! ```text
//! $00-$0F  "C64 CARTRIDGE   "
//! $10-$13  header length (big-endian, normally $40)
//! $14-$15  version (big-endian, $0100 or $0101)
//! $16-$17  hardware type (big-endian)
//! $18      EXROM line level (0 = low = active)
//! $19      GAME  line level (0 = low = active)
//! $1A-$1F  reserved (1.1: subtype at $1A)
//! $20-$3F  name, zero padded
//! then CHIP packets:
//!   "CHIP", total packet length (BE u32, header included),
//!   chip type (BE u16: 0 ROM, 1 RAM, 2 flash), bank (BE u16),
//!   load address (BE u16), image size (BE u16), then the image bytes.
//! ```

const SIGNATURE: &[u8; 16] = b"C64 CARTRIDGE   ";
const CHIP_SIGNATURE: &[u8; 4] = b"CHIP";
const CHIP_HEADER_LEN: usize = 16;
const MIN_HEADER_LEN: usize = 0x40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipKind {
    Rom,
    Ram,
    Flash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub kind: ChipKind,
    pub bank: u16,
    pub load_address: u16,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crt {
    pub version: u16,
    /// The `.crt` hardware type id (0 = normal, 5 = Ocean, 19 = Magic Desk...).
    pub hardware_type: u16,
    /// EXROM line level in the header: `false` = low = **active**.
    pub exrom_level: bool,
    /// GAME line level in the header: `false` = low = **active**.
    pub game_level: bool,
    pub name: String,
    pub chips: Vec<Chip>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    TooShort,
    BadSignature,
    BadHeaderLength(u32),
    BadChipSignature { offset: usize },
    TruncatedChip { offset: usize },
    BadChipLength { offset: usize },
    UnknownChipType(u16),
    NoChips,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "file is too short to be a .crt cartridge"),
            Error::BadSignature => write!(f, "missing the \"C64 CARTRIDGE\" signature"),
            Error::BadHeaderLength(n) => write!(f, "unusable .crt header length {n}"),
            Error::BadChipSignature { offset } => {
                write!(f, "expected a CHIP packet at offset ${offset:X}")
            }
            Error::TruncatedChip { offset } => {
                write!(
                    f,
                    "CHIP packet at offset ${offset:X} runs past the end of the file"
                )
            }
            Error::BadChipLength { offset } => {
                write!(
                    f,
                    "CHIP packet at offset ${offset:X} has an inconsistent length"
                )
            }
            Error::UnknownChipType(t) => write!(f, "unknown CHIP type {t}"),
            Error::NoChips => write!(f, "the cartridge image contains no CHIP packets"),
        }
    }
}

impl std::error::Error for Error {}

fn be16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

pub fn parse(bytes: &[u8]) -> Result<Crt, Error> {
    if bytes.len() < MIN_HEADER_LEN {
        return Err(Error::TooShort);
    }
    if &bytes[..16] != SIGNATURE {
        return Err(Error::BadSignature);
    }
    let header_len = be32(bytes, 0x10);
    // Some files in the wild carry $20; the name field still ends at $40, and
    // the first CHIP follows it, so a short length is clamped to $40.
    if header_len as usize > bytes.len() || header_len < 0x20 {
        return Err(Error::BadHeaderLength(header_len));
    }
    let name_bytes = &bytes[0x20..0x40];
    let name_end = name_bytes.iter().position(|&b| b == 0).unwrap_or(32);
    let name = String::from_utf8_lossy(&name_bytes[..name_end])
        .trim()
        .to_string();

    let mut chips = Vec::new();
    let mut at = (header_len as usize).max(MIN_HEADER_LEN);
    while at < bytes.len() {
        if bytes.len() - at < CHIP_HEADER_LEN {
            return Err(Error::TruncatedChip { offset: at });
        }
        if &bytes[at..at + 4] != CHIP_SIGNATURE {
            return Err(Error::BadChipSignature { offset: at });
        }
        let packet_len = be32(bytes, at + 4) as usize;
        let kind = match be16(bytes, at + 8) {
            0 => ChipKind::Rom,
            1 => ChipKind::Ram,
            2 => ChipKind::Flash,
            other => return Err(Error::UnknownChipType(other)),
        };
        let bank = be16(bytes, at + 10);
        let load_address = be16(bytes, at + 12);
        let size = be16(bytes, at + 14) as usize;
        if packet_len < CHIP_HEADER_LEN + size {
            return Err(Error::BadChipLength { offset: at });
        }
        let data_start = at + CHIP_HEADER_LEN;
        if data_start + size > bytes.len() {
            return Err(Error::TruncatedChip { offset: at });
        }
        chips.push(Chip {
            kind,
            bank,
            load_address,
            data: bytes[data_start..data_start + size].to_vec(),
        });
        // A RAM chip carries no image bytes in some files (packet length only
        // covers the header); the packet length is what advances us.
        at += packet_len.max(CHIP_HEADER_LEN + size);
    }
    if chips.is_empty() {
        return Err(Error::NoChips);
    }

    Ok(Crt {
        version: be16(bytes, 0x14),
        hardware_type: be16(bytes, 0x16),
        exrom_level: bytes[0x18] != 0,
        game_level: bytes[0x19] != 0,
        name,
        chips,
    })
}

/// Serialises a [`Crt`] back to a `.crt` file (version 1.0, 64-byte header).
/// The name is truncated to 31 bytes; chip packets keep their order.
pub fn write(crt: &Crt) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(SIGNATURE);
    out.extend_from_slice(&(MIN_HEADER_LEN as u32).to_be_bytes());
    out.extend_from_slice(&0x0100u16.to_be_bytes());
    out.extend_from_slice(&crt.hardware_type.to_be_bytes());
    out.push(u8::from(crt.exrom_level));
    out.push(u8::from(crt.game_level));
    out.extend_from_slice(&[0; 6]);
    let mut name = [0u8; 32];
    let n = crt.name.len().min(31);
    name[..n].copy_from_slice(&crt.name.as_bytes()[..n]);
    out.extend_from_slice(&name);
    for chip in &crt.chips {
        out.extend_from_slice(CHIP_SIGNATURE);
        out.extend_from_slice(&((CHIP_HEADER_LEN + chip.data.len()) as u32).to_be_bytes());
        let kind: u16 = match chip.kind {
            ChipKind::Rom => 0,
            ChipKind::Ram => 1,
            ChipKind::Flash => 2,
        };
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&chip.bank.to_be_bytes());
        out.extend_from_slice(&chip.load_address.to_be_bytes());
        out.extend_from_slice(&(chip.data.len() as u16).to_be_bytes());
        out.extend_from_slice(&chip.data);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(hardware: u16, exrom: u8, game: u8, name: &str) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(SIGNATURE);
        h.extend_from_slice(&0x40u32.to_be_bytes());
        h.extend_from_slice(&0x0100u16.to_be_bytes());
        h.extend_from_slice(&hardware.to_be_bytes());
        h.push(exrom);
        h.push(game);
        h.extend_from_slice(&[0; 6]);
        let mut n = [0u8; 32];
        n[..name.len()].copy_from_slice(name.as_bytes());
        h.extend_from_slice(&n);
        h
    }

    fn chip(kind: u16, bank: u16, load: u16, data: &[u8]) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend_from_slice(CHIP_SIGNATURE);
        c.extend_from_slice(&((CHIP_HEADER_LEN + data.len()) as u32).to_be_bytes());
        c.extend_from_slice(&kind.to_be_bytes());
        c.extend_from_slice(&bank.to_be_bytes());
        c.extend_from_slice(&load.to_be_bytes());
        c.extend_from_slice(&(data.len() as u16).to_be_bytes());
        c.extend_from_slice(data);
        c
    }

    #[test]
    fn parses_header_fields_and_chips_in_order() {
        let mut file = header(5, 0, 1, "TEST CART");
        file.extend(chip(0, 0, 0x8000, &[1, 2, 3, 4]));
        file.extend(chip(0, 7, 0xA000, &[9; 8]));
        let crt = parse(&file).unwrap();
        assert_eq!(crt.hardware_type, 5);
        assert_eq!(crt.name, "TEST CART");
        assert!(
            !crt.exrom_level && crt.game_level,
            "EXROM active, GAME high"
        );
        assert_eq!(crt.chips.len(), 2);
        assert_eq!(crt.chips[0].load_address, 0x8000);
        assert_eq!(crt.chips[0].data, [1, 2, 3, 4]);
        assert_eq!((crt.chips[1].bank, crt.chips[1].load_address), (7, 0xA000));
        assert_eq!(crt.chips[1].kind, ChipKind::Rom);
    }

    #[test]
    fn write_then_parse_round_trips() {
        let mut file = header(32, 1, 0, "EASY");
        file.extend(chip(2, 0, 0x8000, &[1, 2, 3]));
        file.extend(chip(2, 5, 0xA000, &[4; 8]));
        let crt = parse(&file).unwrap();
        assert_eq!(parse(&write(&crt)).unwrap(), crt);
    }

    #[test]
    fn rejects_a_bad_signature_a_short_file_and_a_missing_chip() {
        assert_eq!(parse(&[0; 10]), Err(Error::TooShort));
        let mut file = header(0, 0, 1, "X");
        file[0] = b'X';
        file.extend(chip(0, 0, 0x8000, &[0]));
        assert_eq!(parse(&file), Err(Error::BadSignature));
        assert_eq!(parse(&header(0, 0, 1, "X")), Err(Error::NoChips));
    }

    #[test]
    fn rejects_garbage_where_a_chip_packet_should_be() {
        let mut file = header(0, 0, 1, "X");
        file.extend_from_slice(&[0xAA; 20]);
        assert!(matches!(parse(&file), Err(Error::BadChipSignature { .. })));
    }

    #[test]
    fn rejects_a_chip_that_runs_past_the_end() {
        let mut file = header(0, 0, 1, "X");
        let mut c = chip(0, 0, 0x8000, &[0; 16]);
        c.truncate(c.len() - 4);
        file.extend(c);
        assert!(matches!(parse(&file), Err(Error::TruncatedChip { .. })));
    }

    #[test]
    fn a_header_length_of_0x20_is_clamped_to_0x40() {
        let mut file = header(0, 0, 1, "X");
        file[0x10..0x14].copy_from_slice(&0x20u32.to_be_bytes());
        file.extend(chip(0, 0, 0x8000, &[5; 4]));
        assert_eq!(parse(&file).unwrap().chips[0].data, [5; 4]);
    }

    #[test]
    fn ram_and_flash_chip_types_are_recognised() {
        let mut file = header(32, 1, 0, "EF");
        file.extend(chip(2, 0, 0x8000, &[0; 4]));
        file.extend(chip(1, 1, 0xDF00, &[0; 4]));
        let kinds: Vec<_> = parse(&file).unwrap().chips.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [ChipKind::Flash, ChipKind::Ram]);
        let mut bad = header(0, 0, 1, "X");
        bad.extend(chip(9, 0, 0x8000, &[0]));
        assert_eq!(parse(&bad), Err(Error::UnknownChipType(9)));
    }
}
