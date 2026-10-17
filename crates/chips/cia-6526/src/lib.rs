//! MOS 6526 CIA (Complex Interface Adapter) -- full chip model: Timer A
//! (the original milestone -- CIA1's own IRQ drives the KERNAL's jiffy
//! clock), **Timer B**, the **Time-of-Day (TOD) clock** (with its real
//! alarm-compare and read/write latch mechanism), the **serial shift
//! register** (output/CNT-clocked mode only -- see its own doc comment),
//! and full port A/B storage (a real key-matrix/joystick/VIC-bank-select
//! consumer reads/writes these through this same generic register file,
//! same "chip stays generic, host wires the chip-specific bits" split
//! `c64::bus::C64Bus` already established for Port B/keyboard).
//!
//! Shared chip crate (`cia6526`): extracted from `c64::cia` once a second
//! machine needed it -- the 1570/1571/1581 drives carry a 6526 of their own
//! (fast-serial shift register, 1581 port lines), and the C128 reuses it.
//!
//! Timer/ICR semantics (start/stop bit, one-shot vs continuous run mode,
//! force-load as a self-clearing strobe bit, the latch-vs-live-counter
//! split between `TA_HI`/`TB_HI` writes while running vs stopped, and the
//! ICR's set/clear-mask write protocol) are the 6526's own well-documented,
//! widely-published register behavior -- same "how the chip works" trusted-
//! reference epistemic tier as `ay-3-8910`'s own entry in `CREDITS.md`, not
//! independently verified against a datasheet scan in this session.
//!
//! **Explicitly not modeled**: the CNT and SP pins (external clock/serial-
//! data lines) have no real host wiring in this project at all yet -- no
//! real IEC serial-bus clock, no RS-232 line driver -- so every CNT-gated
//! timer input mode (`INMODE` values that count CNT edges rather than
//! phi2 cycles or Timer A underflows) and the shift register's own
//! CNT-clocked *input* mode are inert: storage-only, consistent with this
//! file's original scope-cut, not silently approximated as "always
//! ticking". The output-mode shift register *is* modeled (see its own doc
//! comment below), since its real clock source is Timer A, which this
//! chip already ticks.
//!
//! **The FLAG pin is now wired** (`set_flag`, below) -- CIA1's own FLAG
//! input is the real cassette READ line on actual hardware (see
//! `c64::bus::C64Bus`'s own cassette wiring), confirmed against VICE's
//! own real `ciacore.c`/`cia.h` this session (`ciacore_set_flag`/
//! `CIA_IM_FLG = 0x10`, see `CREDITS.md`): a real, dedicated,
//! always-negative-edge-triggered interrupt input, unlike `via6522::Via`'s
//! own configurable-edge `CA1`/`CB1` -- there is no real bit anywhere on
//! the 6526 that selects FLAG's edge polarity.

const ICR_TIMER_A: u8 = 0x01;
const ICR_TIMER_B: u8 = 0x02;
const ICR_TOD_ALARM: u8 = 0x04;
const ICR_SP: u8 = 0x08;
/// Real `CIA_IM_FLG` value (confirmed against VICE's own `cia.h` this
/// session, see `CREDITS.md`) -- the FLAG pin's own ICR bit.
const ICR_FLAG: u8 = 0x10;
const ICR_ALL_SOURCES: u8 = 0x1F;

const CRA_START: u8 = 0x01;
const CRA_ONE_SHOT: u8 = 0x08;
const CRA_FORCE_LOAD: u8 = 0x10;
const CRA_SP_OUTPUT: u8 = 0x40;

const CRB_START: u8 = 0x01;
const CRB_ONE_SHOT: u8 = 0x08;
const CRB_FORCE_LOAD: u8 = 0x10;
/// `CRB` bits 6:5 -- Timer B's own input-mode select (a 2-bit field,
/// unlike Timer A's single CNT/phi2 bit, since Timer B can additionally
/// count Timer A's own underflows -- real hardware's way of chaining the
/// two timers into an effective 32-bit timer).
const CRB_TB_INMODE_MASK: u8 = 0x60;
const CRB_TB_INMODE_PHI2: u8 = 0x00;
const CRB_TB_INMODE_TIMER_A: u8 = 0x40;
const CRB_ALARM_SELECT: u8 = 0x80;

/// This chip runs off the same phi2 clock as the CPU on real hardware
/// (mirrors `c64::bus::PAL_CLOCK_HZ`, redeclared here since this module
/// has no path dependency on `bus`). Used only to derive the TOD clock's
/// real-time tenths-of-a-second tick -- see `tod_tick`'s own doc comment
/// for why the CRA `TODIN` (50/60Hz) bit doesn't otherwise factor in.
const CIA_CLOCK_HZ: u32 = 985_248;
/// Rounded `CIA_CLOCK_HZ / 10`. A real 6526 derives its 10Hz TOD tick from
/// an external 50Hz/60Hz AC-mains reference (selected by CRA's `TODIN`
/// bit) divided down internally -- this emulation has no AC-mains signal
/// to divide at all, so it derives the same nominal 10Hz rate directly
/// from the CPU's own already-modeled clock instead. The `8` discarded by
/// integer rounding (`985_248 / 10 == 98_524.8`) is a deliberately
/// accepted, tiny long-run drift source, same epistemic tier as this
/// project's other documented timing approximations (e.g.
/// `zx_spectrum::SpeakerType`'s filter cutoffs).
const TOD_TENTH_CYCLES: u32 = CIA_CLOCK_HZ / 10;
/// The same for the NTSC machine's 1 022 727 Hz clock.
const NTSC_TOD_TENTH_CYCLES: u32 = 1_022_727 / 10;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Cia {
    /// NTSC machine clock (1.022727 MHz) for the TOD tenths divider.
    #[serde(default)]
    ntsc: bool,
    pub port_a: u8,
    pub port_b: u8,
    pub ddr_a: u8,
    pub ddr_b: u8,

    timer_a_latch: u16,
    timer_a_counter: u16,
    cra: u8,

    timer_b_latch: u16,
    timer_b_counter: u16,
    crb: u8,

    /// Enabled interrupt sources (written via the ICR's set/clear-mask
    /// protocol, bits `TA`/`TB`/`TOD alarm`/`SP`/`FLG`).
    icr_mask: u8,
    /// Interrupt sources that have actually occurred since the ICR was
    /// last read (reading clears this, real hardware behavior).
    icr_data: u8,

    tod_tenths: u8,
    tod_seconds: u8,
    tod_minutes: u8,
    /// Bit 7 = PM, bits 4:0 = BCD hour `1..=12`.
    tod_hours: u8,
    tod_alarm_tenths: u8,
    tod_alarm_seconds: u8,
    tod_alarm_minutes: u8,
    tod_alarm_hours: u8,
    /// Real hardware: writing the hour register (clock, not alarm) halts
    /// TOD counting until tenths is written back -- guarantees software
    /// reading/writing all 4 registers can't race a live rollover
    /// mid-write. See `write`'s own `0x0B`/`0x08` arms.
    tod_halted: bool,
    /// Real hardware: reading the hour register (clock) latches all 4
    /// TOD registers so a multi-byte read can't race a live rollover
    /// either; reading tenths (the last register in the real read order)
    /// releases the latch. `None` = live (unlatched) registers.
    tod_latch: Option<[u8; 4]>,
    tod_cycle_accumulator: u32,

    sdr: u8,
    /// Real hardware, output mode only: writing `SDR` loads this with `8`
    /// and clocks one bit out per Timer A underflow; reaching `0` sets the
    /// ICR's SP bit (byte fully shifted). `0` = idle, nothing queued.
    sdr_shift_count: u8,

    /// The FLAG pin's own last known external level, for edge detection in
    /// `set_flag` -- mirrors `via6522::Via`'s own `ca1_level` field
    /// exactly, just fixed to negative-edge-only instead of `PCR`-selected.
    flag_level: bool,
}

impl Cia {
    pub fn new() -> Self {
        Self::default()
    }

    fn timer_a_running(&self) -> bool {
        self.cra & CRA_START != 0
    }
    fn timer_a_one_shot(&self) -> bool {
        self.cra & CRA_ONE_SHOT != 0
    }
    fn timer_b_running(&self) -> bool {
        self.crb & CRB_START != 0
    }
    fn timer_b_one_shot(&self) -> bool {
        self.crb & CRB_ONE_SHOT != 0
    }

    /// The real *effective* Port A value: bits driven as output (`ddr_a`
    /// bit set) reflect whatever was written to `port_a`; bits left as
    /// input float high (`1`) -- same "undriven pin reads 1" convention
    /// `C64Bus::port01`/`keyboard_port_b` already use for the 6510's own
    /// I/O port and CIA1's own Port A read-back. Public so the host bus
    /// can decode CIA2's Port A for real VIC bank selection (`$DD00` bits
    /// 0-1) without reaching into this struct's private fields.
    pub fn effective_port_a(&self) -> u8 {
        self.port_a | !self.ddr_a
    }

    /// Reflects a real external level change on the FLAG pin -- sets
    /// `ICR_FLAG` only on a genuine negative (high-to-low) edge, real
    /// hardware's own fixed, non-configurable polarity for this pin (see
    /// this module's own top-level doc comment). The host (`c64::bus::
    /// C64Bus`, wiring the real cassette READ line to CIA1's FLAG this
    /// way) is expected to seed this with the tape's own idle level once,
    /// same "construction-time seed, not a per-call default" precedent
    /// `c1541::bus::C1541Bus::new`'s own `via1.set_ca1(true)` already
    /// established for an analogous idle-high assumption.
    pub fn set_flag(&mut self, level: bool) {
        if self.flag_level && !level {
            self.icr_data |= ICR_FLAG;
        }
        self.flag_level = level;
    }

    fn bcd_inc_60(v: u8) -> (u8, bool) {
        let mut ones = (v & 0x0F) + 1;
        let mut tens = (v >> 4) & 0x0F;
        if ones > 9 {
            ones = 0;
            tens += 1;
        }
        if tens > 5 {
            (0, true)
        } else {
            ((tens << 4) | ones, false)
        }
    }

    /// Real 6526 hour register: BCD `1..=12` in bits 4:0, PM flag in bit
    /// 7. AM/PM toggles exactly at the 11->12 rollover (in either
    /// direction -- both "11:59:59 AM -> 12:00:00 PM" and "11:59:59 PM ->
    /// 12:00:00 AM" pass through this same transition). The real,
    /// documented MOS6526 erratum around this exact rollover (a handful of
    /// silicon revisions handle the 12:00:00 boundary inconsistently) is
    /// not reproduced -- this always does the straightforwardly-intended
    /// thing, same "trusted reference, not a claim of one universal real-
    /// chip result" tier as `mos6502`'s own unstable-illegal-opcode group.
    fn tod_inc_hour(&mut self) {
        let pm = self.tod_hours & 0x80;
        let hour = ((self.tod_hours >> 4) & 0x01) * 10 + (self.tod_hours & 0x0F);
        let next_hour = if hour >= 12 { 1 } else { hour + 1 };
        let next_pm = if next_hour == 12 { pm ^ 0x80 } else { pm };
        self.tod_hours = next_pm | ((next_hour / 10) << 4) | (next_hour % 10);
    }

    fn tod_tick(&mut self) {
        if self.tod_halted {
            return;
        }
        let mut tenths = self.tod_tenths + 1;
        let mut carry = false;
        if tenths > 9 {
            tenths = 0;
            carry = true;
        }
        self.tod_tenths = tenths;
        if carry {
            let (secs, carry_min) = Self::bcd_inc_60(self.tod_seconds);
            self.tod_seconds = secs;
            if carry_min {
                let (mins, carry_hr) = Self::bcd_inc_60(self.tod_minutes);
                self.tod_minutes = mins;
                if carry_hr {
                    self.tod_inc_hour();
                }
            }
        }
        if self.tod_tenths == self.tod_alarm_tenths
            && self.tod_seconds == self.tod_alarm_seconds
            && self.tod_minutes == self.tod_alarm_minutes
            && self.tod_hours == self.tod_alarm_hours
        {
            self.icr_data |= ICR_TOD_ALARM;
        }
    }

    /// Advances Timer A, Timer B, the output-mode serial shift register,
    /// and the TOD clock by `cycles` real clock cycles. Returns `true` if
    /// the CIA's IRQ line is *currently* asserted (any enabled source has
    /// an unacknowledged pending flag in `icr_data`) -- real hardware's
    /// IRQ/NMI output is level-triggered and stays asserted until the ICR
    /// is read, so this (correctly) keeps reporting `true` on every later
    /// call too, not just the exact cycle a new source fires; the caller
    /// (`C64::step`) turning a `true` return into a fresh
    /// `request_irq()`/`request_nmi()` call every time is harmless, same
    /// "request now, take at the next instruction boundary if not already
    /// pending" shape `mos6502::Cpu`'s own pending-flag already has.
    ///
    /// Decrements one cycle at a time rather than computing underflow
    /// counts algebraically -- simple and correct for any `cycles` value
    /// up to a full instruction's cost, not cycle-exact to the real
    /// chip's internal phase relationship with the CPU clock, which this
    /// milestone doesn't need (same accepted tier as the original Timer-
    /// A-only `tick` already documented).
    pub fn tick(&mut self, cycles: u8) -> bool {
        for _ in 0..cycles {
            let mut ta_underflowed = false;
            if self.timer_a_running() {
                if self.timer_a_counter == 0 {
                    ta_underflowed = true;
                    self.icr_data |= ICR_TIMER_A;
                    self.timer_a_counter = self.timer_a_latch;
                    if self.timer_a_one_shot() {
                        self.cra &= !CRA_START;
                    }
                } else {
                    self.timer_a_counter -= 1;
                }
            }

            if self.timer_b_running() {
                let should_decrement = match self.crb & CRB_TB_INMODE_MASK {
                    CRB_TB_INMODE_PHI2 => true,
                    CRB_TB_INMODE_TIMER_A => ta_underflowed,
                    // The two CNT-gated modes (`0x20`/`0x60`) need a real
                    // CNT pin this project doesn't wire to either CIA yet
                    // -- see this module's own top-level doc comment.
                    _ => false,
                };
                if should_decrement {
                    if self.timer_b_counter == 0 {
                        self.icr_data |= ICR_TIMER_B;
                        self.timer_b_counter = self.timer_b_latch;
                        if self.timer_b_one_shot() {
                            self.crb &= !CRB_START;
                        }
                    } else {
                        self.timer_b_counter -= 1;
                    }
                }
            }

            if self.cra & CRA_SP_OUTPUT != 0 && self.sdr_shift_count > 0 && ta_underflowed {
                self.sdr_shift_count -= 1;
                if self.sdr_shift_count == 0 {
                    self.icr_data |= ICR_SP;
                }
            }

            self.tod_cycle_accumulator += 1;
            let tenth = if self.ntsc {
                NTSC_TOD_TENTH_CYCLES
            } else {
                TOD_TENTH_CYCLES
            };
            if self.tod_cycle_accumulator >= tenth {
                self.tod_cycle_accumulator -= tenth;
                self.tod_tick();
            }
        }
        self.icr_data & self.icr_mask != 0
    }

    /// Whether the interrupt output is currently asserted: any enabled
    /// source is pending in the ICR. A level, held until the CPU reads the
    /// ICR (`tick` returns the same value).
    pub fn interrupt_asserted(&self) -> bool {
        self.icr_data & self.icr_mask != 0
    }

    /// Selects the NTSC machine clock for the TOD clock's 10 Hz divider.
    pub fn set_ntsc(&mut self, ntsc: bool) {
        self.ntsc = ntsc;
    }

    /// `reg` already decoded to `0..0x10` by the host bus (CIA1/CIA2's
    /// real register block mirrors across its whole page, same division
    /// of labor as `vic_ii::Vic::read`/`write`). Reading the ICR (`$0D`)
    /// clears its pending flags; reading the hour register (`$0B`)
    /// latches all 4 TOD registers; reading tenths (`$08`) releases that
    /// latch -- all real, load-bearing side effects.
    pub fn read(&mut self, reg: u8) -> u8 {
        let val = self.peek(reg);
        match reg & 0x0F {
            0x0D => self.icr_data = 0,
            0x0B => {
                if self.tod_latch.is_none() {
                    self.tod_latch = Some([
                        self.tod_tenths,
                        self.tod_seconds,
                        self.tod_minutes,
                        self.tod_hours,
                    ]);
                }
            }
            0x08 => self.tod_latch = None,
            _ => {}
        }
        val
    }

    /// Pure read with no side effects -- doesn't clear the ICR's pending
    /// flags or touch the TOD latch. For `Machine::read_memory`'s "for
    /// display purposes" contract, same role `C64Bus::peek` plays at the
    /// bus level.
    pub fn peek(&self, reg: u8) -> u8 {
        match reg & 0x0F {
            0x00 => self.port_a,
            0x01 => self.port_b,
            0x02 => self.ddr_a,
            0x03 => self.ddr_b,
            0x04 => self.timer_a_counter as u8,
            0x05 => (self.timer_a_counter >> 8) as u8,
            0x06 => self.timer_b_counter as u8,
            0x07 => (self.timer_b_counter >> 8) as u8,
            0x08 => self.tod_latch.map_or(self.tod_tenths, |l| l[0]),
            0x09 => self.tod_latch.map_or(self.tod_seconds, |l| l[1]),
            0x0A => self.tod_latch.map_or(self.tod_minutes, |l| l[2]),
            0x0B => self.tod_latch.map_or(self.tod_hours, |l| l[3]),
            0x0C => self.sdr,
            0x0D => {
                let active = self.icr_data & self.icr_mask;
                self.icr_data | if active != 0 { 0x80 } else { 0 }
            }
            0x0E => self.cra,
            0x0F => self.crb,
            _ => 0,
        }
    }

    pub fn write(&mut self, reg: u8, val: u8) {
        match reg & 0x0F {
            0x00 => self.port_a = val,
            0x01 => self.port_b = val,
            0x02 => self.ddr_a = val,
            0x03 => self.ddr_b = val,
            0x04 => self.timer_a_latch = (self.timer_a_latch & 0xFF00) | val as u16,
            0x05 => {
                self.timer_a_latch = (self.timer_a_latch & 0x00FF) | ((val as u16) << 8);
                if !self.timer_a_running() {
                    self.timer_a_counter = self.timer_a_latch;
                }
            }
            0x06 => self.timer_b_latch = (self.timer_b_latch & 0xFF00) | val as u16,
            0x07 => {
                self.timer_b_latch = (self.timer_b_latch & 0x00FF) | ((val as u16) << 8);
                if !self.timer_b_running() {
                    self.timer_b_counter = self.timer_b_latch;
                }
            }
            0x08 => {
                if self.crb & CRB_ALARM_SELECT != 0 {
                    self.tod_alarm_tenths = val & 0x0F;
                } else {
                    self.tod_tenths = val & 0x0F;
                    self.tod_halted = false;
                }
            }
            0x09 => {
                if self.crb & CRB_ALARM_SELECT != 0 {
                    self.tod_alarm_seconds = val & 0x7F;
                } else {
                    self.tod_seconds = val & 0x7F;
                }
            }
            0x0A => {
                if self.crb & CRB_ALARM_SELECT != 0 {
                    self.tod_alarm_minutes = val & 0x7F;
                } else {
                    self.tod_minutes = val & 0x7F;
                }
            }
            0x0B => {
                if self.crb & CRB_ALARM_SELECT != 0 {
                    self.tod_alarm_hours = val & 0x9F;
                } else {
                    self.tod_hours = val & 0x9F;
                    self.tod_halted = true;
                }
            }
            0x0C => {
                self.sdr = val;
                if self.cra & CRA_SP_OUTPUT != 0 {
                    self.sdr_shift_count = 8;
                }
            }
            0x0D => {
                let bits = val & ICR_ALL_SOURCES;
                if val & 0x80 != 0 {
                    self.icr_mask |= bits;
                } else {
                    self.icr_mask &= !bits;
                }
            }
            0x0E => {
                self.cra = val & !CRA_FORCE_LOAD;
                if val & CRA_FORCE_LOAD != 0 {
                    self.timer_a_counter = self.timer_a_latch;
                }
            }
            0x0F => {
                self.crb = val & !CRB_FORCE_LOAD;
                if val & CRB_FORCE_LOAD != 0 {
                    self.timer_b_counter = self.timer_b_latch;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer_a_underflows_reloads_and_reports_irq_when_enabled() {
        let mut cia = Cia::new();
        cia.write(0x04, 0x02); // latch low = 2
        cia.write(0x05, 0x00); // latch high = 0 -> latch = 2, counter loads since stopped
        cia.write(0x0D, 0x81); // enable Timer A interrupt (set bit0, bit7=set)
        cia.write(0x0E, 0x01); // start, continuous mode

        assert!(!cia.tick(1), "counter 2 -> 1, no underflow yet");
        assert!(!cia.tick(1), "counter 1 -> 0, no underflow yet");
        assert!(
            cia.tick(1),
            "counter was 0, this cycle underflows and reloads"
        );
    }

    fn timer_a_irq_only_reported_when_enabled_in_icr_mask() {
        let mut cia = Cia::new();
        cia.write(0x04, 0x01);
        cia.write(0x05, 0x00);
        cia.write(0x0E, 0x01); // start, but ICR mask never enabled
        assert!(!cia.tick(1)); // counter 1 -> 0
        assert!(
            !cia.tick(1),
            "underflow happens but isn't reported without the mask bit"
        );
    }

    fn one_shot_mode_stops_after_first_underflow() {
        let mut cia = Cia::new();
        cia.write(0x04, 0x01);
        cia.write(0x05, 0x00);
        cia.write(0x0E, 0x01 | 0x08); // start, one-shot
        cia.tick(1); // -> 0
        cia.tick(1); // underflow, one-shot stops
        assert!(!cia.tick(1), "timer should have stopped itself");
    }
    /// Merged (every check kept): `timer_a_underflows_reloads_and_reports_irq_when_enabled`, `timer_a_irq_only_reported_when_enabled_in_icr_mask`, `one_shot_mode_stops_after_first_underflow`.
    #[test]
    fn timer_a_cases() {
        timer_a_underflows_reloads_and_reports_irq_when_enabled();
        timer_a_irq_only_reported_when_enabled_in_icr_mask();
        one_shot_mode_stops_after_first_underflow();
    }

    #[test]
    fn reading_icr_clears_the_pending_flags() {
        let mut cia = Cia::new();
        cia.write(0x04, 0x01);
        cia.write(0x05, 0x00);
        cia.write(0x0D, 0x81);
        cia.write(0x0E, 0x01);
        cia.tick(1);
        cia.tick(1); // underflow, sets icr_data bit0
        let first = cia.read(0x0D);
        assert_eq!(first & 0x81, 0x81, "bit0 set, bit7 (IRQ summary) set");
        let second = cia.read(0x0D);
        assert_eq!(second, 0x00, "reading should have cleared the flags");
    }

    #[test]
    fn force_load_is_a_strobe_that_never_reads_back_set() {
        let mut cia = Cia::new();
        cia.write(0x04, 0x05);
        cia.write(0x05, 0x00); // latch = 5, counter loads since stopped
        cia.write(0x0E, 0x10); // force-load strobe, timer not started
        assert_eq!(
            cia.peek(0x04),
            5,
            "force-load should have reloaded from the latch"
        );
        assert_eq!(
            cia.peek(0x0E) & 0x10,
            0,
            "the strobe bit itself must never read back set"
        );
    }

    fn timer_b_counts_phi2_cycles_independently_of_timer_a() {
        let mut cia = Cia::new();
        cia.write(0x06, 0x02);
        cia.write(0x07, 0x00); // TB latch = 2
        cia.write(0x0D, 0x82); // enable Timer B interrupt
        cia.write(0x0F, 0x01); // start Timer B, phi2 mode (inmode bits clear)

        assert!(!cia.tick(1));
        assert!(!cia.tick(1));
        assert!(cia.tick(1), "Timer B should underflow and report its IRQ");
    }

    fn timer_b_linked_mode_counts_timer_a_underflows_not_cycles() {
        let mut cia = Cia::new();
        cia.write(0x04, 0x01);
        cia.write(0x05, 0x00); // TA latch = 1 -> underflows every other cycle
        cia.write(0x0E, 0x01); // start Timer A, phi2 mode

        cia.write(0x06, 0x02);
        cia.write(0x07, 0x00); // TB latch = 2 -> needs 3 TA-underflow pulses to
        // first underflow itself (same "latch+1 pulses"
        // rule Timer A's own phi2-counting already
        // follows, just clocked by TA underflows instead
        // of raw cycles)
        cia.write(0x0D, 0x82); // enable Timer B interrupt
        cia.write(0x0F, 0x01 | 0x40); // start Timer B, INMODE = count TA underflows

        // TA (latch=1) underflows on ticks 2, 4, 6 -- each one is a single
        // TB clock pulse. TB (latch=2) needs 3 such pulses before its own
        // counter (2 -> 1 -> 0 -> underflow-on-next-pulse) rolls over.
        assert!(!cia.tick(1), "TA: 1->0, no underflow yet");
        assert!(!cia.tick(1), "TA underflow #1 (TB pulse) -> TB: 2->1");
        assert!(!cia.tick(1), "TA: 1->0, no underflow yet");
        assert!(
            !cia.tick(1),
            "TA underflow #2 (TB pulse) -> TB: 1->0, no TB underflow yet"
        );
        assert!(!cia.tick(1), "TA: 1->0, no underflow yet");
        assert!(
            cia.tick(1),
            "TA underflow #3 (TB pulse) -> TB underflows from 0, reports its IRQ"
        );
    }

    fn timer_b_cnt_gated_modes_never_advance_without_a_real_cnt_pin() {
        let mut cia = Cia::new();
        cia.write(0x06, 0x01);
        cia.write(0x07, 0x00);
        cia.write(0x0D, 0x82);
        cia.write(0x0F, 0x01 | 0x20); // start, INMODE = count CNT edges

        for _ in 0..100 {
            assert!(
                !cia.tick(1),
                "no CNT pin is wired here, Timer B must stay inert"
            );
        }
        assert_eq!(
            cia.peek(0x06),
            1,
            "counter should still read its untouched latch value"
        );
    }
    /// Merged (every check kept): `timer_b_counts_phi2_cycles_independently_of_timer_a`, `timer_b_linked_mode_counts_timer_a_underflows_not_cycles`, `timer_b_cnt_gated_modes_never_advance_without_a_real_cnt_pin`.
    #[test]
    fn timer_b_cases() {
        timer_b_counts_phi2_cycles_independently_of_timer_a();
        timer_b_linked_mode_counts_timer_a_underflows_not_cycles();
        timer_b_cnt_gated_modes_never_advance_without_a_real_cnt_pin();
    }

    fn tod_tenths_advance_in_real_time_and_roll_into_seconds() {
        let mut cia = Cia::new();
        cia.write(0x08, 0x09); // clock tenths = 9 (also un-halts TOD)
        for _ in 0..(TOD_TENTH_CYCLES - 1) {
            cia.tick(1);
        }
        assert_eq!(cia.peek(0x08), 9, "not quite a full tenth-second yet");
        cia.tick(1);
        assert_eq!(cia.peek(0x08), 0, "tenths rolled over");
        assert_eq!(cia.peek(0x09), 0x01, "carried into seconds (BCD 01)");
    }

    fn writing_the_hour_register_halts_tod_until_tenths_is_written() {
        let mut cia = Cia::new();
        cia.write(0x0B, 0x12); // write clock hours -> halts TOD
        for _ in 0..(TOD_TENTH_CYCLES * 3) {
            cia.tick(1);
        }
        assert_eq!(
            cia.peek(0x08),
            0,
            "TOD must stay frozen with no tenths written yet"
        );
        cia.write(0x08, 0x00); // writing tenths releases the halt
        for _ in 0..TOD_TENTH_CYCLES {
            cia.tick(1);
        }
        assert_eq!(cia.peek(0x08), 1, "TOD resumes advancing once released");
    }

    fn reading_the_hour_register_latches_all_four_tod_registers() {
        let mut cia = Cia::new();
        cia.write(0x08, 0x00); // un-halt, start from 00:00:00.0
        let hours_snapshot = cia.read(0x0B); // latches
        for _ in 0..(TOD_TENTH_CYCLES * 15) {
            cia.tick(1); // real clock keeps advancing underneath the latch
        }
        assert_eq!(
            cia.read(0x0A),
            0,
            "minutes read through the latch should still show the snapshot, not live time"
        );
        assert_eq!(cia.peek(0x0B), hours_snapshot);
        // The tenths read that releases the latch still returns the frozen
        // snapshot value, not live time -- that's the whole point of the
        // latch (a guaranteed-atomic 4-byte read), not merely "the last
        // register happens to update last".
        let unlatched_tenths = cia.read(0x08);
        assert_eq!(
            unlatched_tenths, 0,
            "still the frozen snapshot, not live time"
        );
        assert_eq!(
            cia.peek(0x08),
            5,
            "now unlatched, a later read sees live time"
        );
    }

    fn tod_alarm_match_sets_the_icr_bit_when_enabled() {
        let mut cia = Cia::new();
        cia.write(0x0D, 0x84); // enable TOD alarm interrupt
        cia.write(0x0F, 0x80); // CRB ALARM select -> $08-$0B now target the alarm regs
        cia.write(0x08, 0x01); // alarm tenths = 1 (seconds/minutes/hours stay 0, matching clock's own start)
        cia.write(0x0F, 0x00); // back to clock registers

        for _ in 0..(TOD_TENTH_CYCLES - 1) {
            assert!(!cia.tick(1), "no match yet");
        }
        assert!(
            cia.tick(1),
            "clock tenths just reached 1, matching the alarm -> IRQ"
        );
    }
    /// Merged (every check kept): `writing_the_hour_register_halts_tod_until_tenths_is_written`, `reading_the_hour_register_latches_all_four_tod_registers`, `tod_alarm_match_sets_the_icr_bit_when_enabled`.
    #[test]
    fn tod_registers() {
        writing_the_hour_register_halts_tod_until_tenths_is_written();
        reading_the_hour_register_latches_all_four_tod_registers();
        tod_alarm_match_sets_the_icr_bit_when_enabled();
    }

    fn hour_rollover_toggles_am_pm_at_the_eleven_to_twelve_transition() {
        let mut cia = Cia::new();
        cia.write(0x0B, 0x11); // clock hour = 11 AM
        cia.write(0x08, 0x00); // release the write-halt
        cia.tod_inc_hour();
        assert_eq!(cia.peek(0x0B), 0x80 | 0x12, "11 AM -> 12 PM, PM bit set");
        cia.tod_inc_hour();
        assert_eq!(
            cia.peek(0x0B),
            0x80 | 0x01,
            "12 PM -> 1 PM, PM bit unchanged"
        );
    }
    /// Merged (every check kept): `tod_tenths_advance_in_real_time_and_roll_into_seconds`, `hour_rollover_toggles_am_pm_at_the_eleven_to_twelve_transition`.
    #[test]
    fn tod_counting() {
        tod_tenths_advance_in_real_time_and_roll_into_seconds();
        hour_rollover_toggles_am_pm_at_the_eleven_to_twelve_transition();
    }

    #[test]
    fn flag_pin_fires_only_on_a_real_negative_edge_when_enabled() {
        let mut cia = Cia::new();
        cia.write(0x0D, 0x80 | ICR_FLAG); // enable FLAG interrupt

        cia.set_flag(true); // idle-high seed, no edge yet (flag_level starts false)
        assert_eq!(
            cia.peek(0x0D) & ICR_FLAG,
            0,
            "the very first seed to high is not itself a falling edge"
        );

        cia.set_flag(true); // no change at all
        assert_eq!(cia.peek(0x0D) & ICR_FLAG, 0, "no edge on a repeated level");

        cia.set_flag(false); // real negative edge
        assert_eq!(
            cia.peek(0x0D) & ICR_FLAG,
            ICR_FLAG,
            "high-to-low transition must set the FLAG interrupt"
        );

        cia.read(0x0D); // ack
        cia.set_flag(true); // positive edge -- FLAG is negative-edge-only
        assert_eq!(
            cia.peek(0x0D) & ICR_FLAG,
            0,
            "a rising edge must never set FLAG on real hardware"
        );
    }

    #[test]
    fn serial_output_mode_shifts_a_byte_out_over_eight_timer_a_underflows() {
        let mut cia = Cia::new();
        cia.write(0x04, 0x01);
        cia.write(0x05, 0x00); // TA latch = 1 -> underflows every other tick
        cia.write(0x0D, 0x88); // enable SP interrupt
        cia.write(0x0E, 0x01 | 0x40); // start Timer A, SP output mode
        cia.write(0x0C, 0xAA); // write SDR -> queues an 8-bit shift

        let mut fired = false;
        for _ in 0..32 {
            if cia.tick(1) {
                fired = true;
                break;
            }
        }
        assert!(fired, "SDR should report its IRQ once fully shifted out");
    }
}
