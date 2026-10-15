//! Real C64 8x8 keyboard matrix, scanned through CIA1's Port A (column
//! select, driven low) / Port B (row read, pressed keys read low) --
//! genuinely different shape from `sinclair-keyboard`'s Z80-`IN`-port
//! decode (a *different* mechanism, not just a different key layout), so
//! kept local here rather than forcing a shared crate from a second data
//! point -- same "don't extract from one example" principle this
//! codebase already applies elsewhere (see `CLAUDE.md`'s `sinclair-
//! keyboard` notes: it was only extracted once ZX Spectrum needed the
//! *identical* mechanism ZX81 already had).
//!
//! Matrix layout (column, row) per key -- the standard, widely-published
//! C64 keyboard matrix table (commonly cited across independent
//! references, e.g. VICE's own documentation; not cross-checked against
//! a real KERNAL ROM disassembly or datasheet in this session -- same
//! "no outbound network access" caveat as this project's other
//! not-yet-verified entries, see `TODO.md`). `CursorRight`/`CursorDown`
//! are real hardware's *only* cursor matrix positions -- Cursor Left/Up
//! are Shift+CursorRight/Shift+CursorDown on real hardware, not separate
//! keys, so this enum has no separate variants for them (same "arrow
//! keys map to their digit 1:1, Shift handled once combined" pattern
//! `any_machine::zx81_key`/`spectrum_key` already use).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum C64Key {
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
}

impl C64Key {
    fn position(self) -> (u8, u8) {
        use C64Key::*;
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
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct KeyMatrix {
    /// One bit per row, indexed by column; `1` = pressed. Mirrors
    /// `sinclair_keyboard::KeyboardMatrix`'s own "plain row-state array,
    /// addressed by (row, bit)-shaped position" convention, just with
    /// column/row swapped to match this hardware's own wiring.
    pressed: [u8; 8],
}

impl KeyMatrix {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn press(&mut self, key: C64Key) {
        let (col, row) = key.position();
        self.pressed[col as usize] |= 1 << row;
    }

    pub fn release(&mut self, key: C64Key) {
        let (col, row) = key.position();
        self.pressed[col as usize] &= !(1 << row);
    }

    /// `pa`: the byte currently driven onto Port A (active low -- a `0`
    /// bit selects that column; a `1` bit, or a column not driven as
    /// output at all, selects nothing). Returns the resulting Port B
    /// byte (active low -- a `0` bit means some pressed key connects
    /// that row to a currently-selected column; several columns
    /// selected at once naturally OR their rows together, exactly like
    /// `sinclair_keyboard::KeyboardMatrix::read`'s own multi-row-select
    /// behavior).
    pub fn read(&self, pa: u8) -> u8 {
        let mut rows_pulled_low = 0u8;
        for col in 0..8u8 {
            if pa & (1 << col) == 0 {
                rows_pulled_low |= self.pressed[col as usize];
            }
        }
        !rows_pulled_low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressing_a_key_pulls_its_row_low_only_when_its_column_is_selected() {
        let mut matrix = KeyMatrix::new();
        matrix.press(C64Key::Q); // column 7, row 6

        // Column 7 selected (bit 7 low), every other column high (not selected).
        let pb = matrix.read(0b0111_1111);
        assert_eq!(pb & (1 << 6), 0, "Q's row should read low");
        assert_eq!(pb & !(1 << 6), !(1 << 6), "every other row stays high");

        // No column selected at all -> nothing reads low, even though Q is
        // still held.
        assert_eq!(matrix.read(0xFF), 0xFF);
    }

    #[test]
    fn release_clears_the_key() {
        let mut matrix = KeyMatrix::new();
        matrix.press(C64Key::A);
        matrix.release(C64Key::A);
        let (col, _) = C64Key::A.position();
        assert_eq!(matrix.read(!(1 << col)), 0xFF);
    }

    #[test]
    fn two_keys_in_different_columns_both_show_up_when_both_columns_selected() {
        let mut matrix = KeyMatrix::new();
        matrix.press(C64Key::Return); // column 0, row 1
        matrix.press(C64Key::Space); // column 7, row 4

        let pb = matrix.read(0b0111_1110); // columns 0 and 7 both selected
        assert_eq!(pb & (1 << 1), 0, "Return's row");
        assert_eq!(pb & (1 << 4), 0, "Space's row");
    }
}
