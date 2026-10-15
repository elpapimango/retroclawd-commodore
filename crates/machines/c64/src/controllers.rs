//! The analogue side of the two control ports: paddles and the 1351 mouse.
//!
//! Both are read through the SID's POTX/POTY registers (`$D419`/`$D41A`);
//! which port's pots the SID sees is chosen by bits 7:6 of CIA1 Port A
//! (VICE's `set_joyport_pot_mask((b >> 6) & 3)`: value 1 selects control
//! port 1, 2 selects port 2, 3 both at once, 0 neither -> `$FF`). Fire
//! buttons arrive on the digital joystick lines (paddle A/B on Left/Right;
//! on the 1351, left button on Fire and right button on Up), pulled low
//! while pressed -- the same lines `C64Bus` already ANDs into CIA1 reads.
//! Value conventions follow VICE's `mouse_1351.c` (`(counter & 0x7F) +
//! 0x40`) and `mouse_paddle.c` (host motion halved, clamped to 0..255,
//! inverted); see `CREDITS.md`.

use serde::{Deserialize, Serialize};

const LEFT: u8 = 0x04;
const RIGHT: u8 = 0x08;
const FIRE: u8 = 0x10;
const UP: u8 = 0x01;

/// What the host mouse drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseMode {
    Off,
    /// A 1351 proportional mouse in control port 1 or 2.
    Mouse1351(u8),
    /// The mouse turns a pair of paddles in control port 1 or 2.
    Paddle(u8),
    /// The mouse points a light pen at the picture (its switch is control
    /// port 1's fire line, and the same pin triggers the VIC-II latch).
    LightPen,
}

/// A pair of paddle keys' direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaddleDirection {
    Decrease,
    Increase,
}

/// Cycles of key-driven paddle travel per pot step (255 steps ~ 0.5 s).
const CYCLES_PER_PADDLE_STEP: u32 = 2_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Controllers {
    mode: MouseMode,
    /// 1351 position counters (wrap freely, only the low 7 bits are seen).
    counter: [i32; 2],
    /// Mouse-driven paddle positions before inversion (0..=255).
    mouse_paddle: [i32; 2],
    buttons: [bool; 2],
    /// Light pen position in pixels of the 320x200 display area.
    pen: (i32, i32),
    /// Key-driven paddles: pot value per [port][paddle], and which have been
    /// touched (an untouched port reads `$FF`, an open pot).
    paddle: [[u8; 2]; 2],
    paddle_attached: [bool; 2],
    paddle_velocity: [[i8; 2]; 2],
    paddle_fire: [[bool; 2]; 2],
    cycle_debt: u32,
}

impl Default for Controllers {
    fn default() -> Self {
        Self {
            mode: MouseMode::Off,
            counter: [0; 2],
            mouse_paddle: [128; 2],
            buttons: [false; 2],
            pen: (160, 100),
            paddle: [[128; 2]; 2],
            paddle_attached: [false; 2],
            paddle_velocity: [[0; 2]; 2],
            paddle_fire: [[false; 2]; 2],
            cycle_debt: 0,
        }
    }
}

impl Controllers {
    pub fn set_mode(&mut self, mode: MouseMode) {
        self.mode = mode;
    }

    /// Host mouse motion (screen coordinates: `dy` grows downwards).
    pub fn mouse_motion(&mut self, dx: i32, dy: i32) {
        match self.mode {
            MouseMode::Off => {}
            MouseMode::Mouse1351(_) => {
                self.counter[0] = self.counter[0].wrapping_add(dx);
                // Moving the mouse away from you raises POTY.
                self.counter[1] = self.counter[1].wrapping_sub(dy);
            }
            MouseMode::LightPen => {
                self.pen.0 = (self.pen.0 + dx).clamp(0, 319);
                self.pen.1 = (self.pen.1 + dy).clamp(0, 199);
            }
            MouseMode::Paddle(_) => {
                self.mouse_paddle[0] = (self.mouse_paddle[0] + dx / 2).clamp(0, 255);
                self.mouse_paddle[1] = (self.mouse_paddle[1] + dy / 2).clamp(0, 255);
            }
        }
    }

    /// Where the light pen is pointing, when the mouse is one.
    pub fn pen_position(&self) -> Option<(u16, u16)> {
        (self.mode == MouseMode::LightPen).then_some((self.pen.0 as u16, self.pen.1 as u16))
    }

    pub fn mouse_button(&mut self, right: bool, pressed: bool) {
        self.buttons[usize::from(right)] = pressed;
    }

    /// Holds paddle `paddle` (0 = A on POTX, 1 = B on POTY) of control port
    /// `port` (1 or 2) turning one way, or `None` to let go.
    pub fn paddle_turn(&mut self, port: u8, paddle: u8, direction: Option<PaddleDirection>) {
        let (Some(p), Some(a)) = (
            usize::from(port).checked_sub(1).filter(|p| *p < 2),
            Some(usize::from(paddle)).filter(|a| *a < 2),
        ) else {
            return;
        };
        self.paddle_attached[p] = true;
        self.paddle_velocity[p][a] = match direction {
            None => 0,
            Some(PaddleDirection::Decrease) => -1,
            Some(PaddleDirection::Increase) => 1,
        };
    }

    pub fn paddle_fire(&mut self, port: u8, paddle: u8, pressed: bool) {
        if let Some(slot) = usize::from(port)
            .checked_sub(1)
            .and_then(|p| self.paddle_fire.get_mut(p))
            .and_then(|f| f.get_mut(usize::from(paddle)))
        {
            *slot = pressed;
            self.paddle_attached[usize::from(port) - 1] = true;
        }
    }

    /// Advances key-driven paddle motion.
    pub fn tick(&mut self, cycles: u32) {
        if self.paddle_velocity == [[0; 2]; 2] {
            self.cycle_debt = 0;
            return;
        }
        self.cycle_debt += cycles;
        let steps = self.cycle_debt / CYCLES_PER_PADDLE_STEP;
        self.cycle_debt %= CYCLES_PER_PADDLE_STEP;
        for p in 0..2 {
            for a in 0..2 {
                let moved = i32::from(self.paddle[p][a])
                    + i32::from(self.paddle_velocity[p][a]) * steps as i32;
                self.paddle[p][a] = moved.clamp(0, 255) as u8;
            }
        }
    }

    /// Joystick-line bits (active high = pressed) this port's devices press.
    pub fn lines(&self, port: u8) -> u8 {
        let Some(p) = usize::from(port).checked_sub(1).filter(|p| *p < 2) else {
            return 0;
        };
        let mut bits = 0;
        if self.paddle_fire[p][0] {
            bits |= LEFT;
        }
        if self.paddle_fire[p][1] {
            bits |= RIGHT;
        }
        match self.mode {
            MouseMode::Mouse1351(m) if usize::from(m) == p + 1 => {
                if self.buttons[0] {
                    bits |= FIRE;
                }
                if self.buttons[1] {
                    bits |= UP;
                }
            }
            MouseMode::LightPen if p == 0 => {
                if self.buttons[0] {
                    bits |= FIRE;
                }
            }
            MouseMode::Paddle(m) if usize::from(m) == p + 1 => {
                if self.buttons[0] {
                    bits |= LEFT;
                }
                if self.buttons[1] {
                    bits |= RIGHT;
                }
            }
            _ => {}
        }
        bits
    }

    /// POT value (`axis` 0 = X, 1 = Y) on one port.
    fn port_pot(&self, p: usize, axis: usize) -> u8 {
        match self.mode {
            MouseMode::Mouse1351(m) if usize::from(m) == p + 1 => {
                ((self.counter[axis] & 0x7F) as u8).wrapping_add(0x40)
            }
            MouseMode::Paddle(m) if usize::from(m) == p + 1 => {
                (255 - self.mouse_paddle[axis]).clamp(0, 255) as u8
            }
            _ if self.paddle_attached[p] => self.paddle[p][axis],
            _ => 0xFF,
        }
    }

    /// What the SID reads on POTX/POTY with CIA1 Port A bits 7:6 = `select`.
    pub fn pot(&self, select: u8, axis: usize) -> u8 {
        match select & 3 {
            1 => self.port_pot(0, axis),
            2 => self.port_pot(1, axis),
            3 => parallel(self.port_pot(0, axis), self.port_pot(1, axis)),
            _ => 0xFF,
        }
    }
}

/// Two pots on one input in parallel: 0 wins, an open pot (255) gives way,
/// otherwise the resistances (taken proportional to `v / (255 - v)`) add in
/// parallel. VICE does the same in spirit (`calc_parallel_paddle_value`);
/// the exact curve is this crate's own approximation.
fn parallel(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    if a == 255 {
        return b;
    }
    if b == 255 {
        return a;
    }
    let ra = f64::from(a) / f64::from(255 - a);
    let rb = f64::from(b) / f64::from(255 - b);
    let r = ra * rb / (ra + rb);
    (255.0 * r / (1.0 + r)).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_attached_reads_open_pots() {
        let c = Controllers::default();
        assert_eq!(c.pot(1, 0), 0xFF);
        assert_eq!(c.pot(2, 1), 0xFF);
        assert_eq!(c.pot(0, 0), 0xFF);
    }

    #[test]
    fn the_1351_counts_motion_into_the_low_seven_bits_plus_0x40() {
        let mut c = Controllers::default();
        c.set_mode(MouseMode::Mouse1351(1));
        assert_eq!(c.pot(1, 0), 0x40);
        c.mouse_motion(5, 0);
        assert_eq!(c.pot(1, 0), 0x45);
        c.mouse_motion(-10, 0);
        assert_eq!(c.pot(1, 0), 0x40 + 0x7B, "wraps within 7 bits");
        // Y: moving the mouse up (negative host dy) raises POTY.
        c.mouse_motion(0, -3);
        assert_eq!(c.pot(1, 1), 0x43);
        // Port 2 is not selected, so it stays open.
        assert_eq!(c.pot(2, 0), 0xFF);
    }

    #[test]
    fn the_1351_buttons_are_fire_and_up() {
        let mut c = Controllers::default();
        c.set_mode(MouseMode::Mouse1351(2));
        c.mouse_button(false, true);
        assert_eq!(c.lines(2), FIRE);
        assert_eq!(c.lines(1), 0);
        c.mouse_button(true, true);
        assert_eq!(c.lines(2), FIRE | UP);
    }

    #[test]
    fn a_light_pen_follows_the_mouse_inside_the_picture_and_presses_port_1_fire() {
        let mut c = Controllers::default();
        assert_eq!(c.pen_position(), None);
        c.set_mode(MouseMode::LightPen);
        assert_eq!(c.pen_position(), Some((160, 100)));
        c.mouse_motion(-1000, 5000);
        assert_eq!(c.pen_position(), Some((0, 199)));
        c.mouse_button(false, true);
        assert_eq!(c.lines(1), FIRE);
        assert_eq!(c.lines(2), 0);
        assert_eq!(c.pot(1, 0), 0xFF, "no pots on a pen");
    }

    #[test]
    fn a_mouse_driven_paddle_is_halved_clamped_and_inverted() {
        let mut c = Controllers::default();
        c.set_mode(MouseMode::Paddle(2));
        assert_eq!(c.pot(2, 0), 127, "centred at 128 -> 255 - 128");
        c.mouse_motion(20, 0);
        assert_eq!(c.pot(2, 0), 255 - 138);
        c.mouse_motion(10_000, 0);
        assert_eq!(c.pot(2, 0), 0);
        c.mouse_button(false, true);
        c.mouse_button(true, true);
        assert_eq!(c.lines(2), LEFT | RIGHT);
    }

    #[test]
    fn key_driven_paddles_move_with_time_and_fire_on_left_right() {
        let mut c = Controllers::default();
        c.paddle_turn(1, 0, Some(PaddleDirection::Increase));
        assert_eq!(c.pot(1, 0), 128, "attached paddles start centred");
        c.tick(CYCLES_PER_PADDLE_STEP * 10);
        assert_eq!(c.pot(1, 0), 138);
        c.paddle_turn(1, 0, None);
        c.tick(CYCLES_PER_PADDLE_STEP * 10);
        assert_eq!(c.pot(1, 0), 138, "released paddles stay put");
        c.paddle_turn(1, 0, Some(PaddleDirection::Decrease));
        c.tick(CYCLES_PER_PADDLE_STEP * 500);
        assert_eq!(c.pot(1, 0), 0, "clamped");
        c.paddle_fire(1, 1, true);
        assert_eq!(c.lines(1), RIGHT);
    }

    #[test]
    fn both_ports_selected_combine_in_parallel() {
        assert_eq!(parallel(0, 200), 0);
        assert_eq!(parallel(255, 90), 90);
        assert_eq!(parallel(90, 255), 90);
        let both = parallel(128, 128);
        assert!(both < 128 && both > 0, "{both}");
    }
}
