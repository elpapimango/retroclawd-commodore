//! The C128 keyboard: the C64's 8x8 matrix (CIA1 Port A selects columns,
//! Port B reads rows) plus three extra columns, K0-K2, selected by the low
//! bits of the VIC-IIe's `$D02F` and read through the same Port B. The
//! positions of the extra keys were read off the KERNAL's own decode table
//! (`$FA80`, 11 columns of 8 keys), not recalled; CAPS LOCK is sensed on the
//! 8502 port and the 40/80 DISPLAY key through the MMU's MCR.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum C128Key {
    InstDel,
    Return,
    CursorRight,
    F7,
    F1,
    F3,
    F5,
    CursorDown,
    Digit3,
    W,
    A,
    Digit4,
    Z,
    S,
    E,
    LeftShift,
    Digit5,
    R,
    D,
    Digit6,
    C,
    F,
    T,
    X,
    Digit7,
    Y,
    G,
    Digit8,
    B,
    H,
    U,
    V,
    Digit9,
    I,
    J,
    Digit0,
    M,
    K,
    O,
    N,
    Plus,
    P,
    L,
    Minus,
    Period,
    Colon,
    At,
    Comma,
    Pound,
    Asterisk,
    Semicolon,
    ClrHome,
    RightShift,
    Equals,
    UpArrow,
    Slash,
    Digit1,
    LeftArrow,
    Control,
    Digit2,
    Space,
    Commodore,
    Q,
    RunStop,
    // The C128's extra keys, scanned on three more lines (K0-K2).
    Help,
    Tab,
    Keypad1,
    Keypad2,
    Keypad3,
    Keypad4,
    Keypad5,
    Keypad6,
    Keypad7,
    Keypad8,
    Keypad9,
    Keypad0,
    KeypadPlus,
    KeypadMinus,
    KeypadEnter,
    KeypadPeriod,
    Escape,
    LineFeed,
    Alt,
    NoScroll,
    Up,
    Down,
    Left,
    Right,
    /// Locking key sensed through the 8502 port (bit 6), not the matrix.
    CapsLock,
    /// The 40/80 DISPLAY key: sensed through the MMU's mode register.
    Display4080,
}

impl C128Key {
    pub(crate) fn position(self) -> (u8, u8) {
        use C128Key::*;
        match self {
            InstDel => (0, 0),
            Return => (0, 1),
            CursorRight => (0, 2),
            F7 => (0, 3),
            F1 => (0, 4),
            F3 => (0, 5),
            F5 => (0, 6),
            CursorDown => (0, 7),
            Digit3 => (1, 0),
            W => (1, 1),
            A => (1, 2),
            Digit4 => (1, 3),
            Z => (1, 4),
            S => (1, 5),
            E => (1, 6),
            LeftShift => (1, 7),
            Digit5 => (2, 0),
            R => (2, 1),
            D => (2, 2),
            Digit6 => (2, 3),
            C => (2, 4),
            F => (2, 5),
            T => (2, 6),
            X => (2, 7),
            Digit7 => (3, 0),
            Y => (3, 1),
            G => (3, 2),
            Digit8 => (3, 3),
            B => (3, 4),
            H => (3, 5),
            U => (3, 6),
            V => (3, 7),
            Digit9 => (4, 0),
            I => (4, 1),
            J => (4, 2),
            Digit0 => (4, 3),
            M => (4, 4),
            K => (4, 5),
            O => (4, 6),
            N => (4, 7),
            Plus => (5, 0),
            P => (5, 1),
            L => (5, 2),
            Minus => (5, 3),
            Period => (5, 4),
            Colon => (5, 5),
            At => (5, 6),
            Comma => (5, 7),
            Pound => (6, 0),
            Asterisk => (6, 1),
            Semicolon => (6, 2),
            ClrHome => (6, 3),
            RightShift => (6, 4),
            Equals => (6, 5),
            UpArrow => (6, 6),
            Slash => (6, 7),
            Digit1 => (7, 0),
            LeftArrow => (7, 1),
            Control => (7, 2),
            Digit2 => (7, 3),
            Space => (7, 4),
            Commodore => (7, 5),
            Q => (7, 6),
            RunStop => (7, 7),
            // K0 (column 8)
            Help => (8, 0),
            Keypad8 => (8, 1),
            Keypad5 => (8, 2),
            Tab => (8, 3),
            Keypad2 => (8, 4),
            Keypad4 => (8, 5),
            Keypad7 => (8, 6),
            Keypad1 => (8, 7),
            // K1 (column 9)
            Escape => (9, 0),
            KeypadPlus => (9, 1),
            KeypadMinus => (9, 2),
            LineFeed => (9, 3),
            KeypadEnter => (9, 4),
            Keypad6 => (9, 5),
            Keypad9 => (9, 6),
            Keypad3 => (9, 7),
            // K2 (column 10)
            Alt => (10, 0),
            Keypad0 => (10, 1),
            KeypadPeriod => (10, 2),
            Up => (10, 3),
            Down => (10, 4),
            Left => (10, 5),
            Right => (10, 6),
            NoScroll => (10, 7),
            // Not in the matrix.
            CapsLock | Display4080 => (11, 0),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct KeyMatrix {
    /// One bit per row, indexed by column (0-7 the C64 lines, 8-10 K0-K2);
    /// `1` = pressed.
    pressed: [u8; 11],
    /// CAPS LOCK is latched down.
    pub caps_lock: bool,
    /// The 40/80 key is down (80 columns on a cold boot).
    pub display_4080_down: bool,
}

impl KeyMatrix {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn press(&mut self, key: C128Key) {
        match key {
            C128Key::CapsLock => self.caps_lock = true,
            C128Key::Display4080 => self.display_4080_down = true,
            _ => {
                let (col, row) = key.position();
                self.pressed[col as usize] |= 1 << row;
            }
        }
    }

    pub fn release(&mut self, key: C128Key) {
        match key {
            C128Key::CapsLock => self.caps_lock = false,
            C128Key::Display4080 => self.display_4080_down = false,
            _ => {
                let (col, row) = key.position();
                self.pressed[col as usize] &= !(1 << row);
            }
        }
    }

    /// `pa`: the byte on Port A (a `0` selects that column); `k`: the low
    /// three bits of `$D02F` (a `0` selects K0-K2). Returns Port B (a `0`
    /// bit = a key connects that row to a selected column).
    pub fn read(&self, pa: u8, k: u8) -> u8 {
        let mut pulled_low = 0u8;
        for col in 0..8u8 {
            if pa & (1 << col) == 0 {
                pulled_low |= self.pressed[col as usize];
            }
        }
        for line in 0..3u8 {
            if k & (1 << line) == 0 {
                pulled_low |= self.pressed[8 + line as usize];
            }
        }
        !pulled_low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c64_keys_scan_exactly_as_on_the_c64() {
        let mut matrix = KeyMatrix::new();
        matrix.press(C128Key::Q);
        assert_eq!(matrix.read(0b0111_1111, 0xFF) & (1 << 6), 0);
        assert_eq!(matrix.read(0xFF, 0xFF), 0xFF);
    }

    fn the_extra_columns_are_selected_by_the_low_bits_of_d02f() {
        let mut matrix = KeyMatrix::new();
        matrix.press(C128Key::Escape); // K1 row 0
        assert_eq!(matrix.read(0xFF, 0b111), 0xFF, "no K line selected");
        assert_eq!(matrix.read(0xFF, 0b101) & 1, 0, "K1 selected");
        assert_eq!(
            matrix.read(0xFF, 0b110),
            0xFF,
            "K0 selected: another column"
        );
    }

    fn caps_lock_and_the_40_80_key_are_not_in_the_matrix() {
        let mut matrix = KeyMatrix::new();
        matrix.press(C128Key::CapsLock);
        matrix.press(C128Key::Display4080);
        assert!(matrix.caps_lock && matrix.display_4080_down);
        assert_eq!(matrix.read(0x00, 0x00), 0xFF);
        matrix.release(C128Key::CapsLock);
        assert!(!matrix.caps_lock);
    }
    /// Merged (every check kept): `c64_keys_scan_exactly_as_on_the_c64`, `the_extra_columns_are_selected_by_the_low_bits_of_d02f`, `caps_lock_and_the_40_80_key_are_not_in_the_matrix`.
    #[test]
    fn keyboard_matrix() {
        c64_keys_scan_exactly_as_on_the_c64();
        the_extra_columns_are_selected_by_the_low_bits_of_d02f();
        caps_lock_and_the_40_80_key_are_not_in_the_matrix();
    }
}
