//! Emulation of the MOS Technology 6522 VIA (Versatile Interface Adapter) --
//! two 8-bit parallel ports (each with its own data-direction register and
//! a pair of handshake control lines), two 16-bit timers, and an 8-bit
//! shift register, all addressed through the real 16-register map (`$0`-
//! `$F` relative to the chip's base address; the real base-address decode
//! and any port mirroring is each host machine's own concern, same split
//! `ay-3-8910`/`sid`/`c64::cia::Cia` already use). Own crate for the same
//! shared-chip-crate reasoning as those: the 6522 wasn't just the
//! Commodore 1541's own I/O chip -- it's also in the VIC-20, the C64's own
//! user port, the BBC Micro, and many other 6502-family machines, so it's
//! reusable exactly like `ay-3-8910` was once CPC needed the same chip ZX
//! Spectrum 128K already had.
//!
//! No dependency on `machine`, any CPU crate, or a specific host's wiring
//! -- `read`/`write`/`peek` are the CPU-facing register interface,
//! `tick(cycles)` advances both timers (and the shift register, when
//! clocked by one of them) by that many real clock cycles, and
//! `set_ca1`/`set_ca2_input`/`set_cb1`/`set_cb2_input`/`set_external_port_a`/
//! `set_external_port_b`/`pulse_pb6` are how a future host reflects its own
//! real external line changes in -- none of them are called by anything in
//! this codebase yet (no 1541/IEC host exists this pass), same "chip built
//! and tested standalone, wired into a machine in a later pass" precedent
//! `sid`'s own phase 1/phase 2 split already established.
//!
//! **A genuine, distinctive hardware difference from `c64::cia::Cia`**
//! (the MOS 6526 CIA, this project's other 6502-family timer/port chip):
//! the 6522 has no start/stop control bit for either timer at all -- both
//! timers are *always* counting down every real phi2 cycle (Timer 2 only
//! in its one-shot/timed-interrupt sub-mode; in pulse-counting mode it
//! instead only decrements on a real negative edge on PB6, via
//! `pulse_pb6`) from the moment the chip exists. "Starting" a timer is
//! simply the act of writing its high-order counter register, which loads
//! the counter from the latch and clears that timer's own interrupt flag
//! -- there's no separate enable/disable concept the way CIA's own
//! `CRA`/`CRB` start bits work. This is a real datasheet fact, not a
//! simplification: `tick` therefore unconditionally decrements Timer 1
//! every cycle in both its modes, unlike `Cia::tick`'s own
//! `timer_a_running()` gate.
//!
//! Register/mode semantics (T1's continuous-vs-one-shot split and its
//! optional PB7 square-wave output, T2's one-shot-vs-pulse-counting split,
//! the CA1/CA2/CB1/CB2 peripheral-control edge/handshake/pulse/manual
//! modes, the real -- and easy to get wrong -- distinction between
//! register `$01` "ORA with handshake" and register `$0F` "ORA with no
//! side effects", and the IER's real set/clear-mask write protocol) are
//! the chip's own widely-published, widely-republished internal
//! architecture -- same "trusted external reference, not verified against
//! this project's own silicon" epistemic tier as this project's other
//! chip crates (`ay-3-8910`/`sid`/`c64::cia::Cia`), not attributed to one
//! specific source.
//!
//! **Explicitly simplified, documented tier**: the shift register's real 8
//! distinct modes (disabled; shift-in/shift-out under T2, under phi2, or
//! under an external CB1 pulse; plus T2 free-running shift-out) are
//! collapsed here into "disabled", "clocked by an already-ticked internal
//! source (T2 or phi2)" (which this crate *can* model faithfully, since
//! both are already ticked), and "clocked by an external CB1 pulse" (left
//! inert -- no real external serial-data device is wired to anything in
//! this project yet, same "no real source to approximate" stance
//! `c64::cia::Cia`'s own unwired CNT/SP pins and `sid::Sid`'s unwired EXT
//! IN already take). The actual bit-by-bit data movement into/out of the
//! shift register over a real `CB2` serial line isn't modeled either, only
//! the completion timing (`IFR` bit 2 after 8 clock pulses) -- exactly the
//! same scope cut `Cia`'s own output-mode serial shift register already
//! made for the identical reason.

const IFR_CA2: u8 = 0x01;
const IFR_CA1: u8 = 0x02;
const IFR_SR: u8 = 0x04;
const IFR_CB2: u8 = 0x08;
const IFR_CB1: u8 = 0x10;
const IFR_T2: u8 = 0x20;
const IFR_T1: u8 = 0x40;
const IFR_ALL_SOURCES: u8 = 0x7F;

const ACR_PA_LATCH: u8 = 0x01;
const ACR_PB_LATCH: u8 = 0x02;
const ACR_SR_MODE_MASK: u8 = 0x1C;
const ACR_T2_PULSE_COUNT: u8 = 0x20;
const ACR_T1_CONTINUOUS: u8 = 0x40;
const ACR_T1_PB7_ENABLE: u8 = 0x80;

const PCR_CA1_POSITIVE_EDGE: u8 = 0x01;
const PCR_CB1_POSITIVE_EDGE: u8 = 0x10;

/// A `CA2`/`CB2` peripheral-control line's decoded 3-bit mode (`PCR` bits
/// 3:1 for `CA2`, bits 7:5 for `CB2`) -- see this module's own doc comment
/// for the citation tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlLineMode {
    InputNegativeEdge { independent: bool },
    InputPositiveEdge { independent: bool },
    HandshakeOutput,
    PulseOutput,
    ManualOutput(bool),
}

fn decode_control_mode(bits3: u8) -> ControlLineMode {
    match bits3 & 0x07 {
        0b000 => ControlLineMode::InputNegativeEdge { independent: false },
        0b001 => ControlLineMode::InputNegativeEdge { independent: true },
        0b010 => ControlLineMode::InputPositiveEdge { independent: false },
        0b011 => ControlLineMode::InputPositiveEdge { independent: true },
        0b100 => ControlLineMode::HandshakeOutput,
        0b101 => ControlLineMode::PulseOutput,
        0b110 => ControlLineMode::ManualOutput(false),
        _ => ControlLineMode::ManualOutput(true),
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Via {
    /// Output Register A/B -- the CPU-written side of each port. Read back
    /// (for driven bits) via `effective_port_a`/`effective_port_b`, not
    /// directly, since an input-configured bit reflects the outside world
    /// instead (see `set_external_port_a`/`set_external_port_b`).
    pub port_a: u8,
    pub port_b: u8,
    pub ddr_a: u8,
    pub ddr_b: u8,

    /// The current external logic level of every Port A/B pin, as last
    /// reported by a (future) host via `set_external_port_a`/
    /// `set_external_port_b` -- only the bits this VIA has configured as
    /// *input* (`ddr_a`/`ddr_b` bit clear) ever actually read from here.
    /// Defaults to `0xFF` (undriven-floats-high), same convention
    /// `c64::bus::C64Bus::port01`/`keyboard_port_b` already use for the
    /// 6510 port and CIA1's own Port A.
    external_a: u8,
    external_b: u8,

    /// The most recent input snapshot latched on Port A/B's own control
    /// line (`CA1`/`CB1`) active edge -- only actually consulted by a read
    /// when `ACR_PA_LATCH`/`ACR_PB_LATCH` is enabled; otherwise a read
    /// always sees the live pin state instead.
    latched_a: u8,
    latched_b: u8,

    ca1_level: bool,
    cb1_level: bool,
    /// `CA2`/`CB2`'s current level -- meaningful as a live *input* level
    /// while that line is configured as an input, or as the level this
    /// chip is currently *driving out* while configured as an output (see
    /// `ca2_output_level`/`cb2_output_level`).
    ca2_level: bool,
    cb2_level: bool,
    /// Pulse-output mode (`ControlLineMode::PulseOutput`) drives the line
    /// low for exactly one real clock cycle after an `ORA`/`ORB`
    /// handshake access, then releases it -- tracked here so `tick` knows
    /// to bring it back high on the very next cycle.
    ca2_pulse_pending: bool,
    cb2_pulse_pending: bool,

    t1_latch: u16,
    t1_counter: u16,
    /// Timer 1's own dedicated `PB7` square-wave output level, active only
    /// while `ACR_T1_PB7_ENABLE` is set -- overrides `port_b`/`ddr_b` bit 7
    /// entirely while active, a real hardware fact (`effective_port_b`
    /// reflects this).
    t1_pb7_output: bool,

    /// Timer 2 has only one real hardware latch (for its low byte) --
    /// unlike Timer 1's independent latch/counter pair, writing the high
    /// byte always loads the live counter directly, there's no separate
    /// `T2L-H` register in the real map at all.
    t2_latch_low: u8,
    t2_counter: u16,
    /// For edge detection in pulse-counting mode (`pulse_pb6`).
    pb6_level: bool,

    acr: u8,
    pcr: u8,
    ifr: u8,
    ier: u8,

    sr: u8,
    /// Bits remaining in the current 8-bit shift register transfer, under
    /// this crate's simplified "clocked by an internal source" model --
    /// see this module's own doc comment.
    sr_shift_count: u8,
}

impl Via {
    pub fn new() -> Self {
        Self::default()
    }

    fn ca2_mode(&self) -> ControlLineMode {
        decode_control_mode(self.pcr >> 1)
    }
    fn cb2_mode(&self) -> ControlLineMode {
        decode_control_mode(self.pcr >> 5)
    }

    /// Port A's real *effective* value: bits driven as output (`ddr_a` bit
    /// set) reflect `port_a`; bits left as input reflect the last external
    /// level reported via `set_external_port_a` -- same "driven vs.
    /// floating" split `c64::bus::C64Bus::port01` already established for
    /// the 6510's own I/O port. Public so a host can read what this VIA is
    /// currently driving out on Port A (e.g. onto a real IEC bus line)
    /// without reaching into private fields.
    pub fn effective_port_a(&self) -> u8 {
        (self.port_a & self.ddr_a) | (self.external_a & !self.ddr_a)
    }

    /// Same as `effective_port_a`, but for Port B -- also folds in Timer
    /// 1's own `PB7` square-wave override when `ACR_T1_PB7_ENABLE` is set,
    /// a real hardware fact: that bit stops being ordinary Port B logic
    /// entirely and becomes the timer's own dedicated output pin.
    pub fn effective_port_b(&self) -> u8 {
        let base = (self.port_b & self.ddr_b) | (self.external_b & !self.ddr_b);
        if self.acr & ACR_T1_PB7_ENABLE != 0 {
            (base & 0x7F) | ((self.t1_pb7_output as u8) << 7)
        } else {
            base
        }
    }

    fn read_port_a(&self) -> u8 {
        if self.acr & ACR_PA_LATCH != 0 {
            self.latched_a
        } else {
            self.effective_port_a()
        }
    }

    fn read_port_b(&self) -> u8 {
        if self.acr & ACR_PB_LATCH != 0 {
            self.latched_b
        } else {
            self.effective_port_b()
        }
    }

    /// A CPU access (read *or* write) to the handshake-side Port A
    /// register (`$01`) -- real hardware clears `CA1`'s own pending flag
    /// unconditionally, and additionally drives `CA2` (when it's in an
    /// input mode wired as "not independent" of `ORA` access, or in
    /// handshake/pulse output mode) exactly the way a real handshake
    /// protocol needs. Register `$0F` (the same data, no side effects at
    /// all) deliberately never calls this -- see this module's own doc
    /// comment.
    fn on_ora_handshake_access(&mut self) {
        self.ifr &= !IFR_CA1;
        match self.ca2_mode() {
            ControlLineMode::InputNegativeEdge { independent: false }
            | ControlLineMode::InputPositiveEdge { independent: false } => {
                self.ifr &= !IFR_CA2;
            }
            ControlLineMode::HandshakeOutput => self.ca2_level = false,
            ControlLineMode::PulseOutput => {
                self.ca2_level = false;
                self.ca2_pulse_pending = true;
            }
            _ => {}
        }
    }

    fn on_orb_handshake_access(&mut self) {
        self.ifr &= !IFR_CB1;
        match self.cb2_mode() {
            ControlLineMode::InputNegativeEdge { independent: false }
            | ControlLineMode::InputPositiveEdge { independent: false } => {
                self.ifr &= !IFR_CB2;
            }
            ControlLineMode::HandshakeOutput => self.cb2_level = false,
            ControlLineMode::PulseOutput => {
                self.cb2_level = false;
                self.cb2_pulse_pending = true;
            }
            _ => {}
        }
    }

    fn apply_manual_output_levels(&mut self) {
        if let ControlLineMode::ManualOutput(level) = self.ca2_mode() {
            self.ca2_level = level;
        }
        if let ControlLineMode::ManualOutput(level) = self.cb2_mode() {
            self.cb2_level = level;
        }
    }

    /// Reflects a real external level change on the `CA1` input pin --
    /// edge-triggers `IFR_CA1` per `PCR`'s own edge-select bit, latches
    /// Port A's live input (if `ACR_PA_LATCH` is set), and releases a
    /// `CA2` handshake-output line back high (the real handshake
    /// protocol's other half from `on_ora_handshake_access`).
    pub fn set_ca1(&mut self, level: bool) {
        let positive = self.pcr & PCR_CA1_POSITIVE_EDGE != 0;
        let active_edge = if positive {
            level && !self.ca1_level
        } else {
            !level && self.ca1_level
        };
        self.ca1_level = level;
        if active_edge {
            self.ifr |= IFR_CA1;
            if self.acr & ACR_PA_LATCH != 0 {
                self.latched_a = self.effective_port_a();
            }
            if self.ca2_mode() == ControlLineMode::HandshakeOutput {
                self.ca2_level = true;
            }
        }
    }

    pub fn set_cb1(&mut self, level: bool) {
        let positive = self.pcr & PCR_CB1_POSITIVE_EDGE != 0;
        let active_edge = if positive {
            level && !self.cb1_level
        } else {
            !level && self.cb1_level
        };
        self.cb1_level = level;
        if active_edge {
            self.ifr |= IFR_CB1;
            if self.acr & ACR_PB_LATCH != 0 {
                self.latched_b = self.effective_port_b();
            }
            if self.cb2_mode() == ControlLineMode::HandshakeOutput {
                self.cb2_level = true;
            }
        }
    }

    /// Reflects a real external level change on the `CA2` pin -- only
    /// meaningful while `CA2` is configured as an input; a no-op (matching
    /// real hardware, an output pin ignores its own input latch logic)
    /// while it's configured as any output mode.
    pub fn set_ca2_input(&mut self, level: bool) {
        match self.ca2_mode() {
            ControlLineMode::InputNegativeEdge { .. } => {
                if !level && self.ca2_level {
                    self.ifr |= IFR_CA2;
                }
            }
            ControlLineMode::InputPositiveEdge { .. } => {
                if level && !self.ca2_level {
                    self.ifr |= IFR_CA2;
                }
            }
            _ => return,
        }
        self.ca2_level = level;
    }

    pub fn set_cb2_input(&mut self, level: bool) {
        match self.cb2_mode() {
            ControlLineMode::InputNegativeEdge { .. } => {
                if !level && self.cb2_level {
                    self.ifr |= IFR_CB2;
                }
            }
            ControlLineMode::InputPositiveEdge { .. } => {
                if level && !self.cb2_level {
                    self.ifr |= IFR_CB2;
                }
            }
            _ => return,
        }
        self.cb2_level = level;
    }

    /// `CA2`'s current driven level, or `None` while it's configured as an
    /// input (nothing to read as an output in that case -- a host wiring
    /// this VIA onto a real bus line should treat `None` as "not driving
    /// this line", same convention a tri-state bus pin needs).
    pub fn ca2_output_level(&self) -> Option<bool> {
        match self.ca2_mode() {
            ControlLineMode::InputNegativeEdge { .. }
            | ControlLineMode::InputPositiveEdge { .. } => None,
            _ => Some(self.ca2_level),
        }
    }

    pub fn cb2_output_level(&self) -> Option<bool> {
        match self.cb2_mode() {
            ControlLineMode::InputNegativeEdge { .. }
            | ControlLineMode::InputPositiveEdge { .. } => None,
            _ => Some(self.cb2_level),
        }
    }

    /// Sets Port A's current external input levels (the bits this VIA has
    /// configured as input read from here; driven-output bits are
    /// unaffected). Mirrors `c64::cia::Cia`'s own port-storage shape, just
    /// split into a dedicated external-state field since a VIA's ports are
    /// far more often genuinely driven by another real chip/bus than
    /// CIA1's keyboard-scan Port B ever needs to be.
    pub fn set_external_port_a(&mut self, value: u8) {
        self.external_a = value;
    }

    pub fn set_external_port_b(&mut self, value: u8) {
        self.external_b = value;
    }

    /// Real hardware: in pulse-counting mode (`ACR_T2_PULSE_COUNT`),
    /// Timer 2 decrements once per real *negative* edge on `PB6`, not off
    /// phi2 at all -- a no-op in the (default) one-shot timed-interrupt
    /// mode.
    pub fn pulse_pb6(&mut self, level: bool) {
        let falling = !level && self.pb6_level;
        self.pb6_level = level;
        if falling && self.acr & ACR_T2_PULSE_COUNT != 0 {
            self.underflow_or_decrement_t2();
        }
    }

    fn underflow_or_decrement_t2(&mut self) -> bool {
        if self.t2_counter == 0 {
            self.ifr |= IFR_T2;
            self.t2_counter = 0xFFFF;
            true
        } else {
            self.t2_counter -= 1;
            false
        }
    }

    /// Advances both timers (Timer 2 only when not in pulse-counting
    /// mode -- see `pulse_pb6`) and the shift register (when clocked by
    /// one of the two internal sources this crate can model, see this
    /// module's own doc comment) by `cycles` real phi2 clock cycles.
    /// Returns `true` if this VIA's IRQ line is *currently* asserted (any
    /// enabled, unacknowledged interrupt source) -- real hardware's IRQ
    /// output is level-triggered and stays asserted until `IFR` is
    /// cleared, matching `c64::cia::Cia::tick`'s own return-value
    /// convention exactly.
    pub fn tick(&mut self, cycles: u8) -> bool {
        for _ in 0..cycles {
            // Timer 1 always counts phi2 cycles in both modes -- no
            // start/stop bit exists on real hardware, see this module's
            // own doc comment.
            let t1_underflowed = if self.t1_counter == 0 {
                self.ifr |= IFR_T1;
                if self.acr & ACR_T1_PB7_ENABLE != 0 {
                    self.t1_pb7_output = !self.t1_pb7_output;
                }
                self.t1_counter = if self.acr & ACR_T1_CONTINUOUS != 0 {
                    self.t1_latch
                } else {
                    0xFFFF
                };
                true
            } else {
                self.t1_counter -= 1;
                false
            };

            let t2_underflowed = if self.acr & ACR_T2_PULSE_COUNT == 0 {
                self.underflow_or_decrement_t2()
            } else {
                false
            };

            let sr_mode = (self.acr & ACR_SR_MODE_MASK) >> 2;
            let sr_pulses_this_cycle = match sr_mode {
                0b010 | 0b110 => true,                   // clocked by phi2
                0b001 | 0b100 | 0b101 => t2_underflowed, // clocked by T2
                _ => false, // disabled, or CB1-external-clocked (unwired, see doc comment)
            };
            if sr_pulses_this_cycle && self.sr_shift_count > 0 {
                self.sr_shift_count -= 1;
                if self.sr_shift_count == 0 {
                    self.ifr |= IFR_SR;
                    if sr_mode == 0b100 {
                        self.sr_shift_count = 8; // free-running shift-out
                    }
                }
            }

            if self.ca2_pulse_pending {
                self.ca2_level = true;
                self.ca2_pulse_pending = false;
            }
            if self.cb2_pulse_pending {
                self.cb2_level = true;
                self.cb2_pulse_pending = false;
            }

            let _ = t1_underflowed; // only the IFR side effect matters here
        }
        self.ifr & self.ier & IFR_ALL_SOURCES != 0
    }

    pub fn peek(&self, reg: u8) -> u8 {
        match reg & 0x0F {
            0x00 => self.read_port_b(),
            0x01 => self.read_port_a(),
            0x02 => self.ddr_b,
            0x03 => self.ddr_a,
            0x04 => (self.t1_counter & 0xFF) as u8,
            0x05 => (self.t1_counter >> 8) as u8,
            0x06 => (self.t1_latch & 0xFF) as u8,
            0x07 => (self.t1_latch >> 8) as u8,
            0x08 => (self.t2_counter & 0xFF) as u8,
            0x09 => (self.t2_counter >> 8) as u8,
            0x0A => self.sr,
            0x0B => self.acr,
            0x0C => self.pcr,
            0x0D => {
                self.ifr
                    | if self.ifr & self.ier & IFR_ALL_SOURCES != 0 {
                        0x80
                    } else {
                        0
                    }
            }
            // Real hardware: reading IER always reads bit 7 back as 1,
            // a documented quirk (bit 7 has no storage of its own -- it's
            // only ever a write-side set/clear-mask selector).
            0x0E => self.ier | 0x80,
            0x0F => self.read_port_a(),
            _ => 0,
        }
    }

    pub fn read(&mut self, reg: u8) -> u8 {
        let val = self.peek(reg);
        match reg & 0x0F {
            0x00 => self.on_orb_handshake_access(),
            0x01 => self.on_ora_handshake_access(),
            0x04 => self.ifr &= !IFR_T1,
            0x08 => self.ifr &= !IFR_T2,
            0x0A => self.sr_shift_count = 8,
            // $0F (ORA, no handshake) deliberately triggers none of the
            // $01 access's side effects -- the real, distinctive point of
            // that register existing at all.
            _ => {}
        }
        val
    }

    pub fn write(&mut self, reg: u8, val: u8) {
        match reg & 0x0F {
            0x00 => {
                self.port_b = val;
                self.on_orb_handshake_access();
            }
            0x01 => {
                self.port_a = val;
                self.on_ora_handshake_access();
            }
            0x02 => self.ddr_b = val,
            0x03 => self.ddr_a = val,
            0x04 => self.t1_latch = (self.t1_latch & 0xFF00) | val as u16,
            0x05 => {
                self.t1_latch = (self.t1_latch & 0x00FF) | ((val as u16) << 8);
                self.t1_counter = self.t1_latch;
                self.ifr &= !IFR_T1;
                if self.acr & ACR_T1_PB7_ENABLE != 0 {
                    self.t1_pb7_output = false;
                }
            }
            0x06 => self.t1_latch = (self.t1_latch & 0xFF00) | val as u16,
            // T1L-H: latch only, deliberately no counter transfer or IFR
            // clear -- real hardware's whole reason this register is
            // distinct from T1C-H, so a continuous timer's *next* reload
            // value can be changed without disturbing the current
            // countdown.
            0x07 => self.t1_latch = (self.t1_latch & 0x00FF) | ((val as u16) << 8),
            0x08 => self.t2_latch_low = val,
            0x09 => {
                self.t2_counter = ((val as u16) << 8) | self.t2_latch_low as u16;
                self.ifr &= !IFR_T2;
            }
            0x0A => {
                self.sr = val;
                self.sr_shift_count = 8;
            }
            0x0B => self.acr = val,
            0x0C => {
                self.pcr = val;
                self.apply_manual_output_levels();
            }
            // Real hardware: writing a 1 to any IFR bit clears that flag;
            // bit 7 is ignored on write (it's a derived summary, not
            // storage).
            0x0D => self.ifr &= !(val & IFR_ALL_SOURCES),
            0x0E => {
                let bits = val & IFR_ALL_SOURCES;
                if val & 0x80 != 0 {
                    self.ier |= bits;
                } else {
                    self.ier &= !bits;
                }
            }
            0x0F => self.port_a = val,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddr_splits_each_port_into_driven_and_floating_bits() {
        let mut via = Via::new();
        via.write(0x03, 0x0F); // DDRA: low nibble output, high nibble input
        via.write(0x01, 0xAA); // ORA
        via.set_external_port_a(0x50);
        // Driven bits (low nibble) come from ORA; floating bits (high
        // nibble) come from the external input state.
        assert_eq!(via.peek(0x01), 0x5A);
    }

    #[test]
    fn timer1_continuous_mode_reloads_and_fires_repeatedly() {
        let mut via = Via::new();
        via.write(0x06, 0x02); // T1L-L = 2
        via.write(0x07, 0x00); // T1L-H = 0 (latch only, no start yet)
        via.write(0x0E, 0x80 | IFR_T1); // enable T1 interrupt
        via.write(0x0B, ACR_T1_CONTINUOUS); // continuous mode
        via.write(0x05, 0x00); // T1C-H: loads counter from latch, starts counting

        assert!(!via.tick(1), "counter 2 -> 1");
        assert!(!via.tick(1), "counter 1 -> 0");
        assert!(via.tick(1), "counter was 0, underflows and reloads");
        via.read(0x04); // real ack: reading T1C-L clears IFR_T1
        assert!(!via.tick(1), "just reloaded to 2 -> 1, no underflow yet");
        assert!(!via.tick(1), "2 -> 1 -> 0");
        assert!(
            via.tick(1),
            "underflows again -- continuous mode keeps firing"
        );
    }

    #[test]
    fn timer1_one_shot_mode_does_not_reload_from_the_latch() {
        let mut via = Via::new();
        via.write(0x06, 0x01);
        via.write(0x07, 0x00);
        via.write(0x05, 0x00); // start, one-shot (ACR default has continuous bit clear)
        via.tick(1); // 1 -> 0
        via.tick(1); // underflow -> reload to 0xFFFF, not back to the small latch value
        assert_eq!(
            via.peek(0x04),
            0xFF,
            "one-shot reload is 0xFFFF, not the latch"
        );
        assert_eq!(via.peek(0x05), 0xFF);
    }

    #[test]
    fn timer1_pb7_square_wave_toggles_every_underflow_and_overrides_port_b() {
        let mut via = Via::new();
        via.write(0x06, 0x01);
        via.write(0x07, 0x00);
        via.write(0x0B, ACR_T1_CONTINUOUS | ACR_T1_PB7_ENABLE);
        via.write(0x02, 0xFF); // DDRB all-output (irrelevant once PB7 override kicks in)
        via.write(0x00, 0xFF); // ORB all-1s
        via.write(0x05, 0x00); // start -- forces PB7 low immediately

        assert_eq!(
            via.effective_port_b() & 0x80,
            0,
            "PB7 forced low on timer load"
        );
        via.tick(1); // 1 -> 0
        via.tick(1); // underflow -> toggles PB7 high
        assert_eq!(
            via.effective_port_b() & 0x80,
            0x80,
            "PB7 toggled high on underflow"
        );
    }

    #[test]
    fn timer2_one_shot_fires_once_and_needs_rewriting_to_fire_again() {
        let mut via = Via::new();
        via.write(0x08, 0x02); // T2C-L latch = 2
        via.write(0x0E, 0x80 | IFR_T2);
        via.write(0x09, 0x00); // T2C-H: loads counter, starts

        assert!(!via.tick(1));
        assert!(!via.tick(1));
        assert!(via.tick(1), "underflows once");
        via.read(0x08); // ack
        for _ in 0..500 {
            assert!(
                !via.tick(1),
                "no start bit exists, but nothing re-arms IFR2 for ~64K cycles"
            );
        }
    }

    #[test]
    fn timer2_pulse_counting_mode_ignores_phi2_and_only_counts_pb6_edges() {
        let mut via = Via::new();
        via.write(0x08, 0x02);
        via.write(0x0B, ACR_T2_PULSE_COUNT);
        via.write(0x09, 0x00);

        for _ in 0..1000 {
            assert!(
                !via.tick(1),
                "phi2 cycles alone must never decrement T2 in pulse mode"
            );
        }
        via.write(0x0E, 0x80 | IFR_T2);
        via.pulse_pb6(true);
        via.pulse_pb6(false); // falling edge #1: 2 -> 1
        via.pulse_pb6(true);
        via.pulse_pb6(false); // falling edge #2: 1 -> 0
        assert_eq!(via.peek(0x08), 0);
        via.pulse_pb6(true);
        assert!(!via.tick(0)); // sanity: tick(0) is a pure no-op
        let fired = {
            via.pulse_pb6(false); // falling edge #3: underflow
            via.ifr & via.ier & IFR_ALL_SOURCES != 0
        };
        assert!(
            fired,
            "third falling edge should underflow and report the IRQ"
        );
    }

    #[test]
    fn ora_handshake_register_clears_ca1_but_the_no_handshake_alias_does_not() {
        let mut via = Via::new();
        via.set_ca1(true); // arm a falling-edge baseline
        via.set_ca1(false); // active (negative) edge -> sets IFR_CA1
        assert_eq!(via.peek(0x0D) & IFR_CA1, IFR_CA1);

        via.read(0x0F); // no-handshake alias: must NOT clear CA1
        assert_eq!(
            via.peek(0x0D) & IFR_CA1,
            IFR_CA1,
            "reading $0F must not clear CA1"
        );

        via.read(0x01); // handshake register: must clear CA1
        assert_eq!(via.peek(0x0D) & IFR_CA1, 0, "reading $01 must clear CA1");
    }

    #[test]
    fn ca2_handshake_output_goes_low_on_ora_access_and_high_on_ca1_edge() {
        let mut via = Via::new();
        // CA2 bits (PCR bits 3:1) = 100 -> handshake output.
        via.write(0x0C, 0b100 << 1);

        via.write(0x01, 0x00); // ORA access -> CA2 goes low
        assert_eq!(via.ca2_output_level(), Some(false));

        via.set_ca1(true);
        via.set_ca1(false); // active (negative, default) edge on CA1 -> releases CA2 high
        assert_eq!(via.ca2_output_level(), Some(true));
    }

    #[test]
    fn ca2_pulse_output_goes_low_for_exactly_one_cycle() {
        let mut via = Via::new();
        via.write(0x0C, 0b101 << 1); // CA2 = pulse output
        via.write(0x01, 0x00); // ORA access -> CA2 pulses low
        assert_eq!(via.ca2_output_level(), Some(false));
        via.tick(1);
        assert_eq!(
            via.ca2_output_level(),
            Some(true),
            "pulse releases after one cycle"
        );
    }

    #[test]
    fn ca2_manual_output_is_driven_directly_by_pcr_and_ignores_ora_access() {
        let mut via = Via::new();
        via.write(0x0C, 0b111 << 1); // CA2 = manual output, high
        assert_eq!(via.ca2_output_level(), Some(true));
        via.write(0x01, 0x00); // ORA access must NOT affect a manual-output CA2
        assert_eq!(via.ca2_output_level(), Some(true));

        via.write(0x0C, 0b110 << 1); // manual output, low
        assert_eq!(via.ca2_output_level(), Some(false));
    }

    #[test]
    fn ca2_independent_input_mode_is_not_cleared_by_ora_access() {
        let mut via = Via::new();
        via.write(0x0C, 0b001 << 1); // CA2 = input, negative edge, independent
        via.set_ca2_input(true);
        via.set_ca2_input(false); // active edge -> sets IFR_CA2
        assert_eq!(via.peek(0x0D) & IFR_CA2, IFR_CA2);

        via.read(0x01); // independent mode: ORA access must NOT clear it
        assert_eq!(via.peek(0x0D) & IFR_CA2, IFR_CA2);

        via.write(0x0D, IFR_CA2); // only an explicit IFR write-1-to-clear works
        assert_eq!(via.peek(0x0D) & IFR_CA2, 0);
    }

    #[test]
    fn ca2_non_independent_input_mode_is_cleared_by_ora_access() {
        let mut via = Via::new();
        via.write(0x0C, 0b000 << 1); // CA2 = input, negative edge, NOT independent
        via.set_ca2_input(true);
        via.set_ca2_input(false);
        assert_eq!(via.peek(0x0D) & IFR_CA2, IFR_CA2);

        via.read(0x01); // non-independent: ORA access clears it too
        assert_eq!(via.peek(0x0D) & IFR_CA2, 0);
    }

    #[test]
    fn input_latching_freezes_a_port_read_until_the_next_active_edge() {
        let mut via = Via::new();
        via.write(0x0B, ACR_PA_LATCH);
        via.set_external_port_a(0x11);
        via.set_ca1(true);
        via.set_ca1(false); // active edge -> latches 0x11
        via.set_external_port_a(0x99); // live pin changes...
        assert_eq!(
            via.peek(0x01),
            0x11,
            "read must still see the latched snapshot"
        );

        via.set_ca1(true);
        via.set_ca1(false); // a new active edge re-latches the now-current value
        assert_eq!(via.peek(0x01), 0x99);
    }

    #[test]
    fn ier_set_clear_mask_protocol_and_the_bit7_readback_quirk() {
        let mut via = Via::new();
        via.write(0x0E, 0x80 | IFR_T1 | IFR_T2); // set bits
        assert_eq!(
            via.peek(0x0E),
            0x80 | IFR_T1 | IFR_T2,
            "bit7 always reads back set"
        );

        via.write(0x0E, IFR_T1); // clear-mask (bit7 clear): clears just IFR_T1's enable
        assert_eq!(via.peek(0x0E), 0x80 | IFR_T2);
    }

    #[test]
    fn irq_summary_bit_reflects_enabled_and_pending_sources_only() {
        let mut via = Via::new();
        via.write(0x06, 0x01);
        via.write(0x05, 0x00);
        via.tick(2); // underflows, sets IFR_T1, but IER never enabled it
        assert_eq!(
            via.peek(0x0D) & 0x80,
            0,
            "no summary bit without an enabled source"
        );

        via.write(0x0E, 0x80 | IFR_T1);
        assert_eq!(
            via.peek(0x0D) & 0x80,
            0x80,
            "now enabled, summary bit reflects it"
        );
    }

    #[test]
    fn shift_register_clocked_by_phi2_completes_after_eight_pulses() {
        let mut via = Via::new();
        via.write(0x0E, 0x80 | IFR_SR);
        via.write(0x0B, 0b110 << 2); // SR mode = shift out, clocked by phi2
        via.write(0x0A, 0x55); // arms an 8-bit transfer

        for _ in 0..7 {
            assert!(!via.tick(1), "not yet 8 pulses");
        }
        assert!(via.tick(1), "8th phi2 pulse completes the transfer");
    }

    #[test]
    fn shift_register_clocked_by_cb1_stays_inert_with_no_real_host_wiring() {
        let mut via = Via::new();
        via.write(0x0E, 0x80 | IFR_SR);
        via.write(0x0B, 0b011 << 2); // SR mode = shift in, clocked by CB1 (unwired)
        via.write(0x0A, 0x00);
        for _ in 0..10_000 {
            assert!(!via.tick(1), "no phi2/T2 clock source applies to this mode");
        }
    }
}
