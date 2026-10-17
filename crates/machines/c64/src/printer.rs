//! A text printer on the serial bus (device 4 or 5), reached through the
//! KERNAL's own serial entry points (`LISTEN`/`SECOND`/`CIOUT`/`UNLSN` in the
//! jump table). Nothing models the IEC wire: with the trap on, `OPEN 1,4` /
//! `PRINT#1` / `CLOSE 1` succeed and the bytes are captured here, which the
//! host exports as text -- the same job VICE's "text printer" device does.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Printer {
    bytes: Vec<u8>,
    listening: bool,
    /// Secondary address of the open channel (7 = lower/upper case).
    secondary: u8,
}

/// PETSCII -> ASCII for text output. `lower` is the printer's lower/upper case
/// mode (secondary 7 or a `$0E` byte); graphics and unknown codes become `?`.
pub fn petscii_to_ascii(byte: u8, lower: bool) -> Option<char> {
    Some(match byte {
        0x0D | 0x8D => '\n',
        0x20..=0x40 => byte as char,
        0x41..=0x5A => {
            if lower {
                (byte + 0x20) as char
            } else {
                byte as char
            }
        }
        0x5B | 0x5D => byte as char,
        0xC1..=0xDA if lower => (byte - 0x80) as char,
        0x0E | 0x8E | 0x11 | 0x91 | 0x13 | 0x93 | 0x00 => return None,
        _ => '?',
    })
}

impl Printer {
    pub fn listen(&mut self) {
        self.listening = true;
    }

    pub fn listening(&self) -> bool {
        self.listening
    }

    pub fn unlisten(&mut self) {
        self.listening = false;
    }

    pub fn set_secondary(&mut self, sa: u8) {
        self.secondary = sa & 0x0F;
    }

    pub fn push(&mut self, byte: u8) {
        if self.bytes.len() < 4 * 1024 * 1024 {
            self.bytes.push(byte);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Everything printed so far as ASCII text. Secondary 7 or a `$0E` byte
    /// switches to lower/upper case, `$8E` back.
    pub fn text(&self) -> String {
        let mut lower = self.secondary == 7;
        let mut out = String::new();
        for &b in &self.bytes {
            match b {
                0x0E => lower = true,
                0x8E => lower = false,
                _ => {}
            }
            if let Some(c) = petscii_to_ascii(b, lower) {
                out.push(c);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upper_case_mode_keeps_letters_and_return_becomes_a_newline() {
        let mut p = Printer::default();
        for b in b"HELLO\r1+1\r" {
            p.push(*b);
        }
        assert_eq!(p.text(), "HELLO\n1+1\n");
    }

    fn lower_case_mode_swaps_the_letter_ranges() {
        let mut p = Printer::default();
        p.set_secondary(7);
        for b in [0x48, 0x49, 0xC8, 0xC9] {
            p.push(b);
        }
        assert_eq!(p.text(), "hiHI");
    }

    fn mode_bytes_and_unknown_codes() {
        let mut p = Printer::default();
        for b in [0x0E, 0x41, 0x8E, 0x41, 0xA0] {
            p.push(b);
        }
        assert_eq!(p.text(), "aA?");
    }
    /// Merged (every check kept): `upper_case_mode_keeps_letters_and_return_becomes_a_newline`, `lower_case_mode_swaps_the_letter_ranges`, `mode_bytes_and_unknown_codes`.
    #[test]
    fn printer_modes() {
        upper_case_mode_keeps_letters_and_return_becomes_a_newline();
        lower_case_mode_swaps_the_letter_ranges();
        mode_bytes_and_unknown_codes();
    }
}
