//! Read-only VIC-20 generic ROM cartridges, not C64 CRT containers.
//!
//! Layouts follow VICE's pinned `vic20-generic.c`; see `vic20.intake.toml`.
//! Parsing does not reset a Machine or decide RAM-expansion conflicts.

const BASES: [u16; 4] = [0x2000, 0x4000, 0x6000, 0xa000];

#[derive(Debug, Clone)]
pub struct Cartridge {
    blocks: [Option<Vec<u8>>; 4],
}

impl Cartridge {
    pub fn blocks(&self) -> impl Iterator<Item = (u16, &[u8])> {
        BASES
            .into_iter()
            .zip(&self.blocks)
            .filter_map(|(base, data)| data.as_deref().map(|data| (base, data)))
    }

    pub fn read(&self, address: u16) -> Option<u8> {
        let index = BASES.iter().position(|&base| address & 0xe000 == base)?;
        self.blocks[index]
            .as_ref()
            .map(|data| data[usize::from(address & 0x1fff)])
    }
}

/// Raw block extensions select an address. PRG/BIN/ROM require a load address.
/// No signature guessing, truncation, padding of malformed files or bank switching.
pub fn parse(extension: &str, bytes: &[u8]) -> Result<Cartridge, String> {
    let extension = extension.to_ascii_lowercase();
    let (address, data, mirror) = match extension.as_str() {
        "20" | "40" | "60" | "a0" | "b0" => {
            let address = match extension.as_str() {
                "20" => 0x2000,
                "40" => 0x4000,
                "60" => 0x6000,
                "a0" => 0xa000,
                _ => 0xb000,
            };
            let valid = match address {
                0x2000 => matches!(bytes.len(), 4096 | 8192 | 16384 | 32768),
                0x4000 | 0x6000 => matches!(bytes.len(), 4096 | 8192 | 16384),
                0xa000 => matches!(bytes.len(), 4096 | 8192),
                _ => matches!(bytes.len(), 2048 | 4096),
            };
            if !valid {
                return Err(format!(
                    "invalid VIC-20 .{extension} cartridge size {}",
                    bytes.len()
                ));
            }
            (address, bytes, bytes.len() < 8192)
        }
        "prg" | "bin" | "rom" => {
            let header = bytes
                .get(..2)
                .ok_or("VIC-20 cartridge is missing its load address")?;
            (
                u16::from_le_bytes([header[0], header[1]]),
                &bytes[2..],
                false,
            )
        }
        _ => {
            return Err(format!(
                "unsupported VIC-20 cartridge extension .{extension}; expected .20/.40/.60/.a0/.b0 or addressed .prg/.bin/.rom"
            ));
        }
    };
    let valid = match data.len() {
        2048 => address == 0xb000,
        4096 => matches!(
            address,
            0x2000 | 0x3000 | 0x4000 | 0x5000 | 0x6000 | 0x7000 | 0xa000 | 0xb000
        ),
        8192 => BASES.contains(&address),
        16384 => matches!(address, 0x2000 | 0x4000 | 0x6000),
        32768 => address == 0x2000,
        _ => false,
    };
    if !valid {
        return Err(format!(
            "unsupported VIC-20 cartridge layout: {} bytes at ${address:04x}",
            data.len()
        ));
    }
    let mut cartridge = Cartridge {
        blocks: std::array::from_fn(|_| None),
    };
    if mirror || address == 0xb000 {
        let base = address & 0xe000;
        let index = BASES
            .iter()
            .position(|&value| value == base)
            .expect("validated block");
        cartridge.blocks[index] = Some(data.iter().copied().cycle().take(8192).collect());
    } else {
        for (offset, &value) in data.iter().enumerate() {
            // Cartridge address order skips the character ROM and I/O at $8000-$9fff.
            let linear = usize::from(address) + offset;
            let mapped = if linear >= 0x8000 && address < 0x8000 {
                linear + 0x2000
            } else {
                linear
            };
            let index = BASES
                .iter()
                .position(|&base| mapped & 0xe000 == usize::from(base))
                .expect("validated layout");
            let block = cartridge.blocks[index].get_or_insert_with(|| vec![0xff; 8192]);
            block[mapped & 0x1fff] = value;
        }
    }
    Ok(cartridge)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addressed(address: u16, data: &[u8]) -> Vec<u8> {
        [address.to_le_bytes().as_slice(), data].concat()
    }

    #[test]
    fn raw_half_blocks_mirror_but_addressed_halves_leave_the_other_half_empty() {
        for extension in ["20", "40", "60", "A0", "b0"] {
            let cart = parse(extension, &vec![0x5a; 4096]).unwrap();
            let (base, data) = cart.blocks().next().unwrap();
            assert_eq!(data, vec![0x5a; 8192]);
            assert_eq!(cart.read(base + 0x1fff), Some(0x5a));
            assert_eq!(cart.read(0x8000), None);
        }
        for address in [0x2000, 0x3000, 0x4000, 0x5000, 0x6000, 0x7000, 0xa000] {
            let cart = parse("prg", &addressed(address, &vec![0x67; 4096])).unwrap();
            assert_eq!(cart.read(address), Some(0x67));
            assert_eq!(cart.read(address ^ 0x1000), Some(0xff));
        }
        for size in [2048, 4096] {
            let data: Vec<u8> = (0..size).map(|n| (n / 256) as u8).collect();
            let cart = parse("bin", &addressed(0xb000, &data)).unwrap();
            for offset in 0..8192 {
                assert_eq!(cart.read(0xa000 + offset as u16), Some(data[offset % size]));
            }
        }
    }

    #[test]
    fn multi_block_layouts_skip_io_and_preserve_rom_order() {
        for (address, size, bases) in [
            (0x2000, 16384, vec![0x2000, 0x4000]),
            (0x4000, 16384, vec![0x4000, 0x6000]),
            (0x6000, 16384, vec![0x6000, 0xa000]),
            (0x2000, 32768, vec![0x2000, 0x4000, 0x6000, 0xa000]),
        ] {
            let data: Vec<u8> = (0..size).map(|n| (n / 8192 + 1) as u8).collect();
            let cart = parse("rom", &addressed(address, &data)).unwrap();
            assert_eq!(
                cart.blocks().map(|(base, _)| base).collect::<Vec<_>>(),
                bases
            );
            for (i, base) in bases.into_iter().enumerate() {
                assert_eq!(cart.read(base), Some(i as u8 + 1));
                assert_eq!(cart.read(base + 8191), Some(i as u8 + 1));
            }
            assert_eq!(cart.read(0x9000), None);
        }
    }

    #[test]
    fn malformed_and_ambiguous_layouts_are_rejected() {
        for (extension, bytes) in [
            ("crt", vec![0; 8192]),
            ("bin", vec![0; 8192]),
            ("prg", vec![0]),
            ("a0", vec![0; 8194]),
            ("b0", vec![0; 8192]),
            ("40", vec![0; 32768]),
            ("20", vec![0; 4095]),
            ("prg", addressed(0x8000, &vec![0; 8192])),
            ("prg", addressed(0xa000, &vec![0; 16384])),
            ("prg", addressed(0x4000, &vec![0; 32768])),
        ] {
            assert!(
                parse(extension, &bytes).is_err(),
                "{extension}: {}",
                bytes.len()
            );
        }
    }
}
