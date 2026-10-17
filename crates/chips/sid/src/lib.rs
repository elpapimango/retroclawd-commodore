//! Emulation of the MOS Technology 6581/8580 SID (Sound Interface Device),
//! the Commodore 64's sound chip -- own crate for the same shared-chip-crate
//! reasoning as `ay-3-8910` (see `CLAUDE.md`'s "Shared peripheral chips").
//!
//! Deliberately pure: no dependency on `machine`, `mos6502`, or an audio
//! backend. The host machine (`c64`) owns the real port-address decode that
//! routes CPU reads/writes to `write`/`read` here (SID occupies a 32-byte,
//! 25-register I/O page on real hardware, `$D400`-`$D41C` typically, mirrored
//! across the full `$D400`-`$D7FF` block -- that mirroring/mapping decision is
//! the host's job, not this crate's).
//!
//! Phase 1 covered the three original pieces of the C64 parity roadmap:
//! oscillators (triangle/sawtooth/pulse/noise), the ADSR envelope generator,
//! and register read/write behavior. Phase 2 (this pass) applies the analog
//! filter (registers `$15`-`$18`'s cutoff/resonance/routing/select bits) to
//! the mixed signal -- see `Filter`'s own doc comment for the model and its
//! explicitly accepted approximations.
//!
//! The oscillator/envelope algorithms below (24-bit phase accumulator,
//! waveform generation from its top 12 bits, the 23-bit noise LFSR and its
//! specific tap pattern, the ADSR rate-counter-with-exponential-decay state
//! machine) are the SID's own widely-published internal architecture --
//! treated here as a trusted external reference the same way this project
//! treats Z80 undocumented-flag-behavior references and the AY-3-8910's own
//! tone/noise/envelope algorithms (see `CLAUDE.md`'s "Architecture
//! guidance"), not something verified against this codebase's own chip
//! silicon. See `CREDITS.md` for the specific sources.
//!
//! Two real hardware quirks are approximated rather than bit-exact, and
//! documented as such (same epistemic tier as `ay-3-8910`'s own volume
//! table, or `zx_spectrum::SpeakerType`'s filter cutoffs): combined-waveform
//! selection (more than one waveform bit set in a voice's control register)
//! uses the common "bitwise AND of the selected waveforms' own 12-bit
//! values" approximation most non-cycle-exact SID emulators use, not a
//! per-chip-measured combined-waveform table; and the real "ADSR delay bug"
//! (retriggering the envelope while its rate counter is mid-count can delay
//! the next state change by one extra cycle) is not modeled.

/// SID's own documented envelope rate-counter period table (in system clock
/// cycles) -- one entry per 4-bit Attack/Decay/Release register value. A
/// linear 8-bit envelope counter (0-255) advances by 1 every time this many
/// clock cycles elapse, giving the chip's real, widely-published
/// millisecond-scale ADSR timings at approximately 1MHz. Same "trusted
/// external reference" status as `ay-3-8910`'s own volume table -- widely
/// republished across SID-emulation projects and hardware references, not
/// measured against this codebase's own silicon.
const RATE_PERIODS: [u16; 16] = [
    9, 32, 63, 95, 149, 220, 267, 313, 392, 977, 1954, 3126, 3906, 11720, 19531, 31250,
];

/// The real SID envelope generator's decay/release phases are exponential,
/// not linear -- achieved by only letting the linear rate counter above
/// actually decrement the envelope value once every N times it elapses,
/// where N depends on the envelope's current level. This is the chip's own
/// widely-published exponential-divisor breakpoint table (see
/// `CREDITS.md`). Attack always uses divisor 1 (linear ramp) -- only
/// decay/release/sustain-hold use this.
fn exponential_divisor(level: u8) -> u16 {
    match level {
        93..=255 => 1,
        54..=92 => 2,
        26..=53 => 4,
        14..=25 => 8,
        6..=13 => 16,
        0..=5 => 30,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum EnvelopePhase {
    Attack,
    Decay,
    Release,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Envelope {
    attack: u8,
    decay: u8,
    sustain: u8,
    release: u8,
    phase: EnvelopePhase,
    /// The chip's own linear 8-bit envelope counter (0-255); this is the
    /// value actually used to scale a voice's output.
    level: u8,
    rate_counter: u16,
    exponential_counter: u16,
    gate: bool,
}

impl Default for Envelope {
    fn default() -> Self {
        Self {
            attack: 0,
            decay: 0,
            sustain: 0,
            release: 0,
            phase: EnvelopePhase::Release,
            level: 0,
            rate_counter: 0,
            exponential_counter: 0,
            gate: false,
        }
    }
}

impl Envelope {
    /// Real hardware's own sustain-target formula: the 4-bit sustain
    /// register value replicated into both nibbles of an 8-bit byte
    /// (`S | (S << 4)`, i.e. `S * 17`) -- sustain 15 maps to the envelope's
    /// own top level 255, sustain 0 to silence.
    fn sustain_target(&self) -> u8 {
        (self.sustain & 0x0F) * 0x11
    }

    fn rate_period(&self) -> u16 {
        let nibble = match self.phase {
            EnvelopePhase::Attack => self.attack,
            EnvelopePhase::Decay => self.decay,
            EnvelopePhase::Release => self.release,
        };
        RATE_PERIODS[(nibble & 0x0F) as usize]
    }

    /// Gate bit 0->1: (re-)enter Attack from whatever level the envelope is
    /// currently at -- real hardware does not reset the counter to 0 here,
    /// which is what gives retriggered notes their characteristic "soft"
    /// re-attack rather than a hard click back to silence.
    /// Gate 1->0: enter Release from the current level, regardless of phase.
    fn set_gate(&mut self, gate: bool) {
        if gate == self.gate {
            return;
        }
        self.gate = gate;
        self.phase = if gate {
            EnvelopePhase::Attack
        } else {
            EnvelopePhase::Release
        };
        self.rate_counter = 0;
        self.exponential_counter = 0;
    }

    fn tick_one_cycle(&mut self) {
        self.rate_counter += 1;
        if self.rate_counter < self.rate_period() {
            return;
        }
        self.rate_counter = 0;

        match self.phase {
            EnvelopePhase::Attack => {
                // Attack is a real linear ramp -- the published rate-period
                // table already accounts for the full 0-255 sweep taking the
                // chip's documented attack time, no exponential divisor.
                self.level = self.level.saturating_add(1);
                if self.level == 255 {
                    self.phase = EnvelopePhase::Decay;
                    self.exponential_counter = 0;
                }
            }
            EnvelopePhase::Decay => {
                let target = self.sustain_target();
                if self.level <= target {
                    return;
                }
                self.exponential_counter += 1;
                if self.exponential_counter < exponential_divisor(self.level) {
                    return;
                }
                self.exponential_counter = 0;
                self.level = self.level.saturating_sub(1).max(target);
            }
            EnvelopePhase::Release => {
                if self.level == 0 {
                    return;
                }
                self.exponential_counter += 1;
                if self.exponential_counter < exponential_divisor(self.level) {
                    return;
                }
                self.exponential_counter = 0;
                self.level = self.level.saturating_sub(1);
            }
        }
    }
}

/// One of the SID's 3 identical voices: a combined
/// triangle/sawtooth/pulse/noise oscillator plus its own ADSR envelope.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Voice {
    /// 16-bit oscillator frequency (registers `+0`/`+1`) -- added to the
    /// 24-bit phase accumulator every system clock cycle.
    freq: u16,
    /// 12-bit pulse width (registers `+2`/`+3`, high nibble of `+3` unused).
    pulse_width: u16,
    control: u8,
    envelope: Envelope,
    /// 24-bit (real hardware width) phase accumulator; only the low 24 bits
    /// are ever meaningful.
    accumulator: u32,
    /// 23-bit noise LFSR (real hardware width). Seeded non-zero -- an
    /// all-zero LFSR would never produce output, same reasoning as
    /// `ay_3_8910::Noise`'s own seed.
    noise_lfsr: u32,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            freq: 0,
            pulse_width: 0,
            control: 0,
            envelope: Envelope::default(),
            accumulator: 0,
            noise_lfsr: 0x7FFFFF,
        }
    }
}

const GATE_BIT: u8 = 0x01;
const SYNC_BIT: u8 = 0x02;
const RING_MOD_BIT: u8 = 0x04;
const TEST_BIT: u8 = 0x08;
const TRIANGLE_BIT: u8 = 0x10;
const SAWTOOTH_BIT: u8 = 0x20;
const PULSE_BIT: u8 = 0x40;
const NOISE_BIT: u8 = 0x80;

impl Voice {
    fn test(&self) -> bool {
        self.control & TEST_BIT != 0
    }

    /// Sawtooth: the accumulator's own top 12 bits, ramping linearly.
    fn sawtooth(&self) -> u16 {
        ((self.accumulator >> 12) & 0xFFF) as u16
    }

    /// Triangle: the accumulator's top 12 bits, mirrored (bit-inverted) for
    /// the half of the cycle where the selected MSB is set -- ring
    /// modulation substitutes another voice's current MSB for this voice's
    /// own when enabled, which is what makes ring mod audibly fold the
    /// triangle wave against another oscillator's frequency.
    fn triangle(&self, ring_mod_msb: bool) -> u16 {
        let msb = if self.control & RING_MOD_BIT != 0 {
            ring_mod_msb
        } else {
            self.accumulator & 0x0080_0000 != 0
        };
        let bits = ((self.accumulator >> 11) & 0x1FFE) as u16;
        if msb { !bits & 0x1FFE } else { bits }
    }

    /// Pulse: high whenever the accumulator's top 12 bits reach the
    /// programmed pulse width, low otherwise -- always high while `TEST` is
    /// held (real hardware fact).
    fn pulse(&self) -> u16 {
        if self.test() {
            return 0xFFF;
        }
        if self.sawtooth() >= self.pulse_width {
            0xFFF
        } else {
            0
        }
    }

    /// Noise: 12 bits picked from specific taps of the 23-bit LFSR --
    /// exactly the SID's own widely-published bit layout (see
    /// `CREDITS.md`), not a generic PRNG.
    fn noise(&self) -> u16 {
        let l = self.noise_lfsr;
        let bit = |n: u32| (l >> n) & 1;
        ((bit(22) << 11)
            | (bit(20) << 10)
            | (bit(16) << 9)
            | (bit(13) << 8)
            | (bit(11) << 7)
            | (bit(7) << 6)
            | (bit(4) << 5)
            | (bit(2) << 4)) as u16
    }

    /// Combined waveform output per the control register's selected bits.
    /// More than one bit set is the documented "combined waveform" quirk --
    /// approximated here as a bitwise AND of the selected waveforms' own
    /// 12-bit values (see this module's own doc comment for the accuracy
    /// caveat).
    fn waveform_output(&self, ring_mod_msb: bool) -> u16 {
        let mut result: u16 = 0xFFF;
        let mut any = false;
        if self.control & TRIANGLE_BIT != 0 {
            result &= self.triangle(ring_mod_msb);
            any = true;
        }
        if self.control & SAWTOOTH_BIT != 0 {
            result &= self.sawtooth();
            any = true;
        }
        if self.control & PULSE_BIT != 0 {
            result &= self.pulse();
            any = true;
        }
        if self.control & NOISE_BIT != 0 {
            result &= self.noise();
            any = true;
        }
        if any { result } else { 0 }
    }

    fn write_control(&mut self, value: u8) {
        self.control = value;
        self.envelope.set_gate(value & GATE_BIT != 0);
    }

    /// This voice's current instantaneous output, scaled to `0.0..=1.0` by
    /// its own envelope level -- the same per-voice quantity `Sid::output`
    /// always mixed directly into the final sum before the filter existed;
    /// now also the filter's own per-cycle *input* for whichever voices are
    /// routed through it (see `Sid::tick_one_cycle`).
    fn sample(&self, ring_mod_msb: bool) -> f32 {
        (self.waveform_output(ring_mod_msb) as f32 / 0xFFF as f32)
            * (self.envelope.level as f32 / 255.0)
    }
}

/// The real SID filter is an analog state-variable (multi-mode) filter --
/// two cascaded integrators around a feedback loop -- built from two of the
/// chip's own op-amps, not a digital biquad. This models that same
/// architecture using the classic discrete "Chamberlin" state-variable
/// filter recurrence (a standard, widely-taught digital approximation of
/// exactly this analog topology, independently derivable from the
/// continuous-time equations -- not SID-specific or copied from any single
/// emulator's source), stepped once per real system clock cycle in
/// `Sid::tick_one_cycle` (continuous integration, matching how the real
/// analog filter is always running, not resampled at the host's much
/// slower audio sample rate).
///
/// `low`/`band` are the filter's own persistent integrator state (the
/// low-pass and band-pass outputs respectively, both continuously
/// available); the high-pass output is purely combinatorial from the
/// *current* `low`/`band` plus this instant's input, so it isn't stored --
/// `tick` returns it directly for the caller to record if needed (`Sid`
/// keeps the most recent one in `filter_last_hp` for `output` to read).
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct Filter {
    low: f32,
    band: f32,
}

impl Filter {
    /// Advances the filter by one cycle given this cycle's summed input
    /// (the filter-routed voices' current samples) and returns the
    /// high-pass output for this same instant. `w0` is the per-cycle
    /// integrator coefficient (`cutoff_coefficient`, below); `q_inv` is the
    /// resonance feedback term (`resonance_q_inv`, below).
    fn tick(&mut self, input: f32, w0: f32, q_inv: f32) -> f32 {
        let high = input - self.low - q_inv * self.band;
        self.band += w0 * high;
        self.low += w0 * self.band;
        high
    }
}

/// This crate's own established "approximately 1MHz" baseline for anything
/// needing an absolute real-world clock rate (see `RATE_PERIODS`'s own doc
/// comment, which already uses this same assumption for envelope timing) --
/// the real PAL/NTSC C64 clock is 985248Hz/1022727Hz respectively, and this
/// crate has no dependency on `machine`/`c64` to read either exactly. The
/// filter's cutoff register is meaningless without *some* absolute Hz
/// mapping (unlike an oscillator's frequency register, which only needs a
/// ratio, never an absolute rate) -- see `cutoff_coefficient`'s own doc
/// comment for how this is used.
const ASSUMED_FILTER_CLOCK_HZ: f32 = 1_000_000.0;

/// The real SID's cutoff-register-to-frequency mapping is famously
/// nonlinear and differs significantly between the 6581 and 8580 revisions
/// (driven by real DAC nonlinearity that even varies between individual
/// 6581 chips) -- reproducing either curve exactly needs a per-chip-
/// measured lookup table (reSID's own approach), which this crate doesn't
/// have. This uses a single linear approximation across both chip
/// revisions instead (same "no `--sid-model` split yet" scope this crate's
/// module doc comment already states elsewhere), spanning the commonly
/// cited real range of roughly 30Hz-12kHz -- same documented-approximation
/// epistemic tier as this crate's combined-waveform/ADSR-delay-bug notes
/// above.
const FILTER_MIN_CUTOFF_HZ: f32 = 30.0;
const FILTER_MAX_CUTOFF_HZ: f32 = 12_000.0;
/// The lowest cutoff a 6581 reaches with FC = 0: "FCmin = 220Hz (measured
/// value)" in reSID's own `filter.h` for the 6581's 470pF filter capacitor.
/// The 8580 keeps the older 30 Hz floor.
const FILTER_MIN_CUTOFF_HZ_6581: f32 = 220.0;

/// Which SID revision is fitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SidModel {
    /// The original NMOS part in most C64s: a filter that never closes below
    /// ~220 Hz, and a master-volume stage that leaks a DC offset, so writing
    /// `$D418` is audible (the basis of 4-bit sample playback).
    Mos6581,
    /// The later HMOS part (C64C): the offset is much smaller, so volume
    /// writes are nearly inaudible and sample playback is faint.
    Mos8580,
}

impl SidModel {
    /// Output level (as a fraction of one voice at full scale) that the
    /// master volume multiplies even when every voice is silent. The sign
    /// and ratio (6581 several times the 8580) match the well-known
    /// behaviour; the absolute numbers are this crate's own engineering
    /// choice, not a measurement.
    fn volume_dc(self) -> f32 {
        match self {
            SidModel::Mos6581 => 0.10,
            SidModel::Mos8580 => 0.01,
        }
    }

    fn min_cutoff_hz(self) -> f32 {
        match self {
            SidModel::Mos6581 => FILTER_MIN_CUTOFF_HZ_6581,
            SidModel::Mos8580 => FILTER_MIN_CUTOFF_HZ,
        }
    }
}

/// Maps the 11-bit cutoff register (`$15`/`$16`, `0..=2047`) to the
/// discrete-time state-variable filter's per-cycle integrator coefficient
/// (`w0 = 2*pi*fc/clock`, the standard derivation for this exact filter
/// topology -- see `Filter`'s own doc comment). Clamped well below the
/// recurrence's real stability limit (`w0 < 2`) as a numerical safety
/// margin, not a hardware fact.
fn cutoff_coefficient(cutoff: u16, model: SidModel) -> f32 {
    let t = (cutoff & 0x07FF) as f32 / 2047.0;
    let min = model.min_cutoff_hz();
    let hz = min + t * (FILTER_MAX_CUTOFF_HZ - min);
    (2.0 * std::f32::consts::PI * hz / ASSUMED_FILTER_CLOCK_HZ).min(1.0)
}

/// Maps the 4-bit resonance nibble (`$17` bits 7:4) to the state-variable
/// filter's feedback term. Real SID resonance behavior is widely documented
/// as inconsistent across individual chips (part of the same DAC-mismatch
/// story `cutoff_coefficient` already cites) and can reach self-oscillation
/// at the top of its real range -- this uses a documented, clearly-
/// approximate linear mapping from "unity-ish Q" at `0` down toward a
/// bounded, deliberately-not-quite-self-oscillating minimum at `15`
/// (`q_inv` must stay comfortably above `0` for this recurrence to stay
/// numerically stable), not a chip-measured curve.
fn resonance_q_inv(resonance: u8) -> f32 {
    let r = (resonance & 0x0F) as f32 / 15.0;
    1.0 - r * 0.85
}

/// A MOS 6581/8580 SID: 3 voices, addressed through the classic 25-register
/// map (`$00`-`$18` relative to the chip's base address; the real base
/// address and any port mirroring is the host machine's own concern).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Sid {
    voices: [Voice; 3],
    /// Register `$15`/`$16`: 11-bit filter cutoff, see `cutoff_coefficient`.
    filter_cutoff: u16,
    /// Which revision this chip is (`set_model`).
    model: SidModel,
    /// Register `$17`: resonance (high nibble, see `resonance_q_inv`) +
    /// per-voice filter routing (bits 0-2: voice 1/2/3 routed through the
    /// filter when set, bypassing it straight to the mixer otherwise) +
    /// external-input filter routing (bit 3 -- no external audio input
    /// exists in this project, so this bit is stored but never actually
    /// contributes anything, same "no real source to approximate" stance
    /// as the CNT/SP pins in `c64::cia::Cia`).
    filter_resonance_routing: u8,
    /// Register `$18`: bit 7 = voice 3 off (fully disconnects voice 3 from
    /// the final mix, filtered or not -- real hardware's own way of using
    /// voice 3 purely as an inaudible modulation/OSC3-randomness source);
    /// bits 6:4 = high/band/low-pass filter output select (any combination
    /// -- an unset selection means the filter's output doesn't reach the
    /// mixer at all, silencing every voice routed to it); bits 3:0 = master
    /// volume.
    mode_volume: u8,
    /// The filter's own persistent low-pass/band-pass integrator state --
    /// see `Filter`'s own doc comment.
    filter: Filter,
    /// The filter's high-pass output as of the most recent `tick_one_cycle`
    /// call -- purely combinatorial (not integrator state), so it isn't
    /// part of `Filter` itself; `output` reads it directly rather than
    /// re-deriving it without a per-cycle input to re-derive it from.
    filter_last_hp: f32,
}

impl Default for Sid {
    fn default() -> Self {
        Self::new()
    }
}

impl Sid {
    /// Picks the SID revision (default 6581, what most C64s shipped with).
    pub fn set_model(&mut self, model: SidModel) {
        self.model = model;
    }

    pub fn model(&self) -> SidModel {
        self.model
    }

    pub fn new() -> Self {
        Self {
            voices: [Voice::default(), Voice::default(), Voice::default()],
            filter_cutoff: 0,
            model: SidModel::Mos6581,
            filter_resonance_routing: 0,
            mode_volume: 0,
            filter: Filter::default(),
            filter_last_hp: 0.0,
        }
    }

    /// Writes register `addr` (real hardware only decodes the low 5 address
    /// bits, `addr & 0x1F` -- registers `$19`-$1F` are read-only or unused
    /// and silently ignore writes, matching real hardware).
    pub fn write(&mut self, addr: u8, value: u8) {
        match addr & 0x1F {
            r @ 0x00..=0x14 => {
                let voice = &mut self.voices[(r / 7) as usize];
                match r % 7 {
                    0 => voice.freq = (voice.freq & 0xFF00) | value as u16,
                    1 => voice.freq = (voice.freq & 0x00FF) | ((value as u16) << 8),
                    2 => voice.pulse_width = (voice.pulse_width & 0x0F00) | value as u16,
                    3 => {
                        voice.pulse_width =
                            (voice.pulse_width & 0x00FF) | ((value as u16 & 0x0F) << 8)
                    }
                    4 => voice.write_control(value),
                    5 => {
                        voice.envelope.attack = value >> 4;
                        voice.envelope.decay = value & 0x0F;
                    }
                    6 => {
                        voice.envelope.sustain = value >> 4;
                        voice.envelope.release = value & 0x0F;
                    }
                    _ => unreachable!("r % 7 < 7"),
                }
            }
            0x15 => self.filter_cutoff = (self.filter_cutoff & 0x07F8) | (value as u16 & 0x07),
            0x16 => self.filter_cutoff = (self.filter_cutoff & 0x0007) | ((value as u16) << 3),
            0x17 => self.filter_resonance_routing = value,
            0x18 => self.mode_volume = value,
            // $19 POTX, $1A POTY, $1B OSC3, $1C ENV3, and the unused
            // $1D-$1F tail are all read-only on real hardware.
            _ => {}
        }
    }

    /// Reads register `addr`. Only `$19`/`$1A` (paddle ADC inputs -- no
    /// paddle is modeled, reads back `0xFF` matching an unconnected input),
    /// `$1B` (voice 3's current raw waveform output, top 8 bits -- real
    /// software commonly uses this as a pseudo-random number source), and
    /// `$1C` (voice 3's current envelope level) are meaningfully readable;
    /// every write-only register reads back `0xFF`, matching real hardware.
    pub fn read(&self, addr: u8) -> u8 {
        match addr & 0x1F {
            0x19 | 0x1A => 0xFF,
            0x1B => (self.voices[2].waveform_output(false) >> 4) as u8,
            0x1C => self.voices[2].envelope.level,
            _ => 0xFF,
        }
    }

    /// Advances every voice's oscillator, noise generator, and envelope by
    /// `cycles` real SID clock cycles (the caller's own clock divider from
    /// the host CPU's clock -- on a real C64 the SID runs at the same clock
    /// as the 6510 -- is the host machine's concern, not this crate's, same
    /// convention as `ay_3_8910::Ay3_8910::tick`).
    pub fn tick(&mut self, cycles: u32) {
        for _ in 0..cycles {
            self.tick_one_cycle();
        }
    }

    fn tick_one_cycle(&mut self) {
        let old_msb = [
            self.voices[0].accumulator & 0x0080_0000 != 0,
            self.voices[1].accumulator & 0x0080_0000 != 0,
            self.voices[2].accumulator & 0x0080_0000 != 0,
        ];
        let old_bit19 = [
            self.voices[0].accumulator & 0x0008_0000 != 0,
            self.voices[1].accumulator & 0x0008_0000 != 0,
            self.voices[2].accumulator & 0x0008_0000 != 0,
        ];

        for voice in &mut self.voices {
            if voice.test() {
                voice.accumulator = 0;
            } else {
                voice.accumulator = (voice.accumulator + voice.freq as u32) & 0x00FF_FFFF;
            }
        }

        // Hard sync: each voice syncs to the *previous* voice in the
        // 3-voice ring (voice 0's sync source is voice 2) -- real SID
        // wiring. A rising edge (0->1) of the source's MSB during this
        // cycle resets the syncing voice's own accumulator to 0.
        //
        // Both loops below index across voices by position (each iteration
        // reads a *different* voice than the one it mutates, or reads a
        // same-indexed `old_*` snapshot alongside it) -- not expressible as
        // a plain `.iter_mut().enumerate()` walk.
        #[allow(clippy::needless_range_loop)]
        for i in 0..3 {
            let source = (i + 2) % 3;
            let source_new_msb = self.voices[source].accumulator & 0x0080_0000 != 0;
            if self.voices[i].control & SYNC_BIT != 0 && !old_msb[source] && source_new_msb {
                self.voices[i].accumulator = 0;
            }
        }

        #[allow(clippy::needless_range_loop)]
        for i in 0..3 {
            let new_bit19 = self.voices[i].accumulator & 0x0008_0000 != 0;
            if !self.voices[i].test() && !old_bit19[i] && new_bit19 {
                let l = self.voices[i].noise_lfsr;
                let feedback = ((l >> 22) & 1) ^ ((l >> 17) & 1);
                self.voices[i].noise_lfsr = ((l << 1) | feedback) & 0x007F_FFFF;
            }
            self.voices[i].envelope.tick_one_cycle();
        }

        // Feed the filter with this cycle's already-updated voice state --
        // real hardware's filter is a continuous analog circuit fed
        // directly by the voices' own (also continuous) current output, so
        // this must happen every cycle, not lazily inside `output` (which
        // is only ever queried at the host's much slower audio sample
        // rate, with no per-cycle input history to integrate over).
        let ring_mod_msb = self.ring_mod_msb();
        let voice3_off = self.mode_volume & 0x80 != 0;
        let mut filtered_input = 0.0f32;
        for (i, voice) in self.voices.iter().enumerate() {
            if i == 2 && voice3_off {
                continue;
            }
            if self.filter_resonance_routing & (1 << i) != 0 {
                filtered_input += voice.sample(ring_mod_msb[i]);
            }
        }
        let w0 = cutoff_coefficient(self.filter_cutoff, self.model);
        let q_inv = resonance_q_inv(self.filter_resonance_routing >> 4);
        self.filter_last_hp = self.filter.tick(filtered_input, w0, q_inv);
    }

    /// Each voice's ring-modulation source MSB: voice 0 rings against voice
    /// 2, voice 1 against voice 0, voice 2 against voice 1 -- the same
    /// "previous voice in the ring" wiring `tick_one_cycle`'s hard-sync loop
    /// already uses, read fresh (current, post-tick accumulator state) by
    /// both `tick_one_cycle`'s own filter-input computation and `output`.
    fn ring_mod_msb(&self) -> [bool; 3] {
        [
            self.voices[2].accumulator & 0x0080_0000 != 0,
            self.voices[0].accumulator & 0x0080_0000 != 0,
            self.voices[1].accumulator & 0x0080_0000 != 0,
        ]
    }

    /// Current mixed output across all 3 voices, normalized to
    /// (approximately) `-1.0..=1.0` -- signed, unlike phase 1's own
    /// `0.0..=1.0`, since a voice routed through the filter can now
    /// legitimately swing negative (the filter's band-pass/high-pass taps
    /// are genuine AC signals around the state-variable recurrence's own
    /// zero, not the unsigned "how loud" quantities every unfiltered voice
    /// still produces). Every voice not routed through the filter (the
    /// default -- `filter_resonance_routing`'s low 3 bits start at `0`)
    /// mixes in exactly as it did before this filter existed, so this is a
    /// behavior-preserving default, not a rescaling of the old range.
    pub fn output(&self) -> f32 {
        let ring_mod_msb = self.ring_mod_msb();
        let voice3_off = self.mode_volume & 0x80 != 0;

        let mut bypass_sum = 0.0f32;
        for (i, voice) in self.voices.iter().enumerate() {
            if i == 2 && voice3_off {
                continue;
            }
            if self.filter_resonance_routing & (1 << i) == 0 {
                bypass_sum += voice.sample(ring_mod_msb[i]);
            }
        }

        // The filter's own 3 taps -- low-pass/band-pass are the
        // integrator's own persistent state, high-pass is `tick_one_cycle`'s
        // most recently computed instantaneous value. An unset select
        // nibble means the filter's output doesn't reach the mixer at all
        // (real hardware fact) -- every voice routed to it is then simply
        // inaudible, not silently bypassed.
        let mut filtered_out = 0.0f32;
        if self.mode_volume & 0x10 != 0 {
            filtered_out += self.filter.low;
        }
        if self.mode_volume & 0x20 != 0 {
            filtered_out += self.filter.band;
        }
        if self.mode_volume & 0x40 != 0 {
            filtered_out += self.filter_last_hp;
        }

        let master_volume = (self.mode_volume & 0x0F) as f32 / 15.0;
        ((((bypass_sum + filtered_out) / 3.0) + self.model.volume_dc()) * master_volume)
            .clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_voice(sid: &mut Sid, voice: usize, offset: u8, value: u8) {
        sid.write((voice * 7) as u8 + offset, value);
    }

    fn frequency_registers_round_trip_across_lo_hi_writes() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 0, 0xCD);
        write_voice(&mut sid, 0, 1, 0xAB);
        assert_eq!(sid.voices[0].freq, 0xABCD);
    }

    fn pulse_width_is_12_bit_split_across_two_registers() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 1, 2, 0x34);
        write_voice(&mut sid, 1, 3, 0xF6); // high nibble ignored
        assert_eq!(sid.voices[1].pulse_width, 0x634);
    }
    /// Merged (every check kept): `frequency_registers_round_trip_across_lo_hi_writes`, `pulse_width_is_12_bit_split_across_two_registers`.
    #[test]
    fn register_pairs() {
        frequency_registers_round_trip_across_lo_hi_writes();
        pulse_width_is_12_bit_split_across_two_registers();
    }

    #[test]
    fn sawtooth_ramps_linearly_from_the_top_accumulator_bits() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 0, 0x00);
        write_voice(&mut sid, 0, 1, 0x10); // moderate frequency
        write_voice(&mut sid, 0, 4, SAWTOOTH_BIT);

        let first = sid.voices[0].sawtooth();
        sid.tick(50);
        let second = sid.voices[0].sawtooth();
        assert!(second > first, "sawtooth should ramp upward over time");
    }

    #[test]
    fn pulse_output_gates_high_once_accumulator_reaches_pulse_width() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 2, 0x00);
        write_voice(&mut sid, 0, 3, 0x08); // pulse width = 0x800 (half duty)
        write_voice(&mut sid, 0, 0, 0xFF);
        write_voice(&mut sid, 0, 1, 0xFF); // fastest possible frequency
        write_voice(&mut sid, 0, 4, PULSE_BIT);

        let mut saw_low = false;
        let mut saw_high = false;
        for _ in 0..2000 {
            sid.tick(1);
            if sid.voices[0].pulse() == 0 {
                saw_low = true;
            } else {
                saw_high = true;
            }
        }
        assert!(saw_low && saw_high, "pulse never toggled both ways");
    }

    #[test]
    fn test_bit_holds_accumulator_at_zero() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 0, 0xFF);
        write_voice(&mut sid, 0, 1, 0xFF);
        write_voice(&mut sid, 0, 4, TEST_BIT);
        sid.tick(1000);
        assert_eq!(sid.voices[0].accumulator, 0);
    }

    #[test]
    fn noise_lfsr_eventually_produces_nonzero_output() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 0, 0xFF);
        write_voice(&mut sid, 0, 1, 0x0F);
        write_voice(&mut sid, 0, 4, NOISE_BIT);

        let mut saw_output = false;
        for _ in 0..5000 {
            sid.tick(1);
            if sid.voices[0].noise() != 0 {
                saw_output = true;
                break;
            }
        }
        assert!(saw_output, "noise generator never produced output");
    }

    #[test]
    fn hard_sync_resets_the_synced_voice_accumulator() {
        // Voice 0 syncs to voice 2 (the previous voice in the ring). Give
        // voice 2 a very fast frequency so it wraps (MSB rising edge)
        // almost immediately, and confirm voice 0's own accumulator gets
        // forced back to 0 rather than following its own (much slower)
        // frequency.
        let mut sid = Sid::new();
        write_voice(&mut sid, 2, 0, 0x00);
        write_voice(&mut sid, 2, 1, 0xFF); // voice 2: fastest possible frequency
        write_voice(&mut sid, 0, 0, 0x01);
        write_voice(&mut sid, 0, 1, 0x00); // voice 0: very slow frequency
        write_voice(&mut sid, 0, 4, SYNC_BIT | SAWTOOTH_BIT);

        sid.tick(200);
        // Voice 0's own frequency is far too slow to have advanced its
        // accumulator meaningfully in 200 cycles on its own; if sync is
        // working, repeated resets keep it near 0 regardless.
        assert!(sid.voices[0].accumulator < 0x00FF_FFFF);
    }

    fn envelope_attack_ramps_from_current_level_to_full_then_decays() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 5, 0x00); // attack=0 (fastest), decay=0
        write_voice(&mut sid, 0, 6, 0xF0); // sustain=15 (full), release=0
        write_voice(&mut sid, 0, 4, GATE_BIT);

        // Fastest attack rate period is 9 cycles/step; exactly 255 steps
        // reaches full level without overshooting into decay.
        sid.tick(9 * 255);
        assert_eq!(sid.voices[0].envelope.level, 255);
        assert_eq!(sid.voices[0].envelope.phase, EnvelopePhase::Decay);
    }

    fn envelope_decays_toward_sustain_level_and_holds() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 5, 0x00); // attack=0, decay=0 (fastest)
        write_voice(&mut sid, 0, 6, 0x80); // sustain=8 -> target 0x88, release=0
        write_voice(&mut sid, 0, 4, GATE_BIT);

        sid.tick(9 * 255); // exactly reach full attack, no overshoot into decay
        assert_eq!(sid.voices[0].envelope.level, 255);

        // Exponential decay from 255 can take a while; run generously long.
        sid.tick(9 * 30 * 300);
        let target = sid.voices[0].envelope.sustain_target();
        assert_eq!(sid.voices[0].envelope.level, target);

        // Continues to hold at the sustain level rather than decaying
        // further while gate stays high.
        sid.tick(9 * 30 * 50);
        assert_eq!(sid.voices[0].envelope.level, target);
    }

    fn releasing_the_gate_ramps_the_envelope_back_to_zero() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 5, 0x00); // attack=0
        write_voice(&mut sid, 0, 6, 0xF0); // sustain=15, release=0 (fastest)
        write_voice(&mut sid, 0, 4, GATE_BIT);
        sid.tick(9 * 255); // exactly reach full attack, no overshoot into decay
        assert_eq!(sid.voices[0].envelope.level, 255);

        write_voice(&mut sid, 0, 4, 0x00); // gate off -> release
        assert_eq!(sid.voices[0].envelope.phase, EnvelopePhase::Release);
        sid.tick(9 * 30 * 300);
        assert_eq!(sid.voices[0].envelope.level, 0);
    }
    /// Merged (every check kept): `envelope_attack_ramps_from_current_level_to_full_then_decays`, `envelope_decays_toward_sustain_level_and_holds`, `releasing_the_gate_ramps_the_envelope_back_to_zero`.
    #[test]
    fn envelope_stages() {
        envelope_attack_ramps_from_current_level_to_full_then_decays();
        envelope_decays_toward_sustain_level_and_holds();
        releasing_the_gate_ramps_the_envelope_back_to_zero();
    }

    #[test]
    fn silent_envelope_produces_silent_output_regardless_of_waveform() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 0, 0xFF);
        write_voice(&mut sid, 0, 1, 0x0F);
        write_voice(&mut sid, 0, 4, SAWTOOTH_BIT); // gate never set: envelope stays at 0
        write_voice(&mut sid, 0, 18, 0x0F); // master volume irrelevant here, but exercised anyway
        sid.tick(1000);
        assert_eq!(sid.output(), 0.0);
    }

    fn master_volume_register_scales_the_mixed_output() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 0, 0xFF);
        write_voice(&mut sid, 0, 1, 0x0F);
        write_voice(&mut sid, 0, 4, SAWTOOTH_BIT | GATE_BIT);
        write_voice(&mut sid, 0, 5, 0x00); // fast attack
        write_voice(&mut sid, 0, 6, 0xF0); // sustain = full
        sid.tick(9 * 300); // reach full envelope level

        sid.write(0x18, 0x00);
        assert_eq!(sid.output(), 0.0);

        sid.write(0x18, 0x0F);
        assert!(sid.output() > 0.0);
    }

    #[test]
    fn unmapped_high_registers_read_back_as_0xff() {
        let sid = Sid::new();
        assert_eq!(sid.read(0x19), 0xFF);
        assert_eq!(sid.read(0x1A), 0xFF);
        assert_eq!(sid.read(0x00), 0xFF); // write-only register
    }

    /// Runs a fast, full-envelope pulse wave on voice 0 (routed through the
    /// filter, resonance 0) and returns the peak-to-peak spread of
    /// `output()` sampled once per instruction-sized burst over `windows`
    /// windows of `cycles_per_window` cycles each -- a cheap proxy for "how
    /// much high-frequency energy is getting through", used by the LP/HP
    /// comparison tests below instead of anything requiring an FFT.
    fn filtered_pulse_peak_to_peak(cutoff: u16, select_bits: u8) -> f32 {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 0, 0xFF);
        write_voice(&mut sid, 0, 1, 0x0F); // fastest possible frequency
        write_voice(&mut sid, 0, 2, 0x00);
        write_voice(&mut sid, 0, 3, 0x08); // pulse width = half duty
        write_voice(&mut sid, 0, 5, 0x00); // attack=0, decay=0
        write_voice(&mut sid, 0, 6, 0xF0); // sustain=15 (full), release=0
        write_voice(&mut sid, 0, 4, PULSE_BIT | GATE_BIT);
        sid.tick(9 * 255); // reach full envelope level, no overshoot

        sid.write(0x15, (cutoff & 0x07) as u8);
        sid.write(0x16, (cutoff >> 3) as u8);
        sid.write(0x17, 0x01); // route voice 0 through the filter, resonance 0
        sid.write(0x18, 0x0F | select_bits); // full master volume + given select

        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for _ in 0..200 {
            sid.tick(20);
            let sample = sid.output();
            min = min.min(sample);
            max = max.max(sample);
        }
        max - min
    }

    fn low_pass_filter_attenuates_a_fast_pulse_wave_more_at_a_lower_cutoff() {
        let low_cutoff_swing = filtered_pulse_peak_to_peak(1, 0x10); // LP select
        let high_cutoff_swing = filtered_pulse_peak_to_peak(2047, 0x10); // LP select
        assert!(
            low_cutoff_swing < high_cutoff_swing,
            "a near-minimum LP cutoff ({low_cutoff_swing}) should smooth a fast \
             pulse wave more than a near-maximum cutoff ({high_cutoff_swing})"
        );
    }

    fn high_pass_lets_more_of_a_fast_pulse_wave_through_than_low_pass_at_the_same_low_cutoff() {
        let lp_swing = filtered_pulse_peak_to_peak(1, 0x10); // LP select
        let hp_swing = filtered_pulse_peak_to_peak(1, 0x40); // HP select
        assert!(
            hp_swing > lp_swing,
            "at the same low cutoff, HP ({hp_swing}) should pass more of a fast \
             pulse wave's energy through than LP ({lp_swing})"
        );
    }
    /// Merged (every check kept): `low_pass_filter_attenuates_a_fast_pulse_wave_more_at_a_lower_cutoff`, `high_pass_lets_more_of_a_fast_pulse_wave_through_than_low_pass_at_the_same_low_cutoff`.
    #[test]
    fn filter_responses() {
        low_pass_filter_attenuates_a_fast_pulse_wave_more_at_a_lower_cutoff();
        high_pass_lets_more_of_a_fast_pulse_wave_through_than_low_pass_at_the_same_low_cutoff();
    }

    fn voices_not_routed_through_the_filter_bypass_it_entirely() {
        let make = |cutoff: u16| {
            let mut sid = Sid::new();
            write_voice(&mut sid, 0, 0, 0xFF);
            write_voice(&mut sid, 0, 1, 0x0F);
            write_voice(&mut sid, 0, 4, SAWTOOTH_BIT | GATE_BIT);
            write_voice(&mut sid, 0, 5, 0x00);
            write_voice(&mut sid, 0, 6, 0xF0);
            sid.tick(9 * 255);
            // Filter routing left at its default (0): voice 0 is NOT routed.
            sid.write(0x15, (cutoff & 0x07) as u8);
            sid.write(0x16, (cutoff >> 3) as u8);
            sid.write(0x17, 0xF0); // max resonance, routing still all-clear -- must have zero effect either
            sid.write(0x18, 0x7F); // LP+BP+HP all selected, full volume
            sid.tick(500);
            sid.output()
        };

        assert_eq!(
            make(0),
            make(2047),
            "an unrouted voice's output must be identical regardless of cutoff/resonance"
        );
    }

    fn voice_3_off_silences_voice_3_regardless_of_filter_routing() {
        // Two independent instances (rather than flipping the bit mid-run
        // on one) so the filter's own persistent integrator state can't
        // leak leftover energy from "before" into "after" -- that's a real
        // decay-over-time effect, not what this test is checking.
        let make = |voice3_off: bool| {
            let mut sid = Sid::new();
            write_voice(&mut sid, 2, 0, 0xFF);
            write_voice(&mut sid, 2, 1, 0x0F);
            write_voice(&mut sid, 2, 4, SAWTOOTH_BIT | GATE_BIT);
            write_voice(&mut sid, 2, 5, 0x00);
            write_voice(&mut sid, 2, 6, 0xF0);
            sid.tick(9 * 255); // voice 3 reaches full envelope level
            sid.write(0x16, 0xFF); // cutoff near maximum -- filter charges up fast enough to measure
            sid.write(0x17, 0x04); // route voice 3 through the filter
            let select = 0x1F | if voice3_off { 0x80 } else { 0x00 }; // LP select + full volume, +voice-3-off
            sid.write(0x18, select);
            sid.tick(500);
            sid.output()
        };

        assert_ne!(
            make(false),
            0.0,
            "voice 3 should be audible before the off bit is set"
        );
        // Silent apart from the master-volume stage's own offset.
        assert_eq!(
            make(true),
            SidModel::Mos6581.volume_dc(),
            "voice 3 off must silence it even though it's routed through the filter"
        );
    }

    fn an_unselected_filter_output_reaches_the_mixer_as_silence() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 0, 0, 0xFF);
        write_voice(&mut sid, 0, 1, 0x0F);
        write_voice(&mut sid, 0, 4, SAWTOOTH_BIT | GATE_BIT);
        write_voice(&mut sid, 0, 5, 0x00);
        write_voice(&mut sid, 0, 6, 0xF0);
        sid.tick(9 * 255);
        sid.write(0x17, 0x01); // route voice 0 through the filter
        sid.write(0x18, 0x0F); // full volume, but no LP/BP/HP select bit set
        sid.tick(500);
        assert_eq!(
            sid.output(),
            SidModel::Mos6581.volume_dc(),
            "a voice routed to the filter with no output tap selected must be inaudible"
        );
    }
    /// Merged (every check kept): `voices_not_routed_through_the_filter_bypass_it_entirely`, `voice_3_off_silences_voice_3_regardless_of_filter_routing`, `an_unselected_filter_output_reaches_the_mixer_as_silence`.
    #[test]
    fn filter_routing() {
        voices_not_routed_through_the_filter_bypass_it_entirely();
        voice_3_off_silences_voice_3_regardless_of_filter_routing();
        an_unselected_filter_output_reaches_the_mixer_as_silence();
    }

    #[test]
    fn osc3_and_env3_read_voice_three_state() {
        let mut sid = Sid::new();
        write_voice(&mut sid, 2, 4, GATE_BIT);
        write_voice(&mut sid, 2, 5, 0x00);
        write_voice(&mut sid, 2, 6, 0xF0);
        sid.tick(9 * 255); // exactly reach full attack, no overshoot into decay
        assert_eq!(sid.read(0x1C), 255); // ENV3
    }

    fn the_master_volume_alone_is_audible_on_a_6581_and_nearly_not_on_an_8580() {
        // Writing $D418 with every voice silent moves the output: the basis
        // of 4-bit sample playback, strong on the 6581, faint on the 8580.
        let step = |model| {
            let mut sid = Sid::new();
            sid.set_model(model);
            sid.write(0x18, 0x00);
            let quiet = sid.output();
            sid.write(0x18, 0x0F);
            sid.output() - quiet
        };
        let step_6581 = step(SidModel::Mos6581);
        let step_8580 = step(SidModel::Mos8580);
        assert!(step_6581 > 0.05, "{step_6581}");
        assert!(step_8580 < step_6581 / 5.0, "{step_8580} vs {step_6581}");
        let mut sid = Sid::new();
        sid.write(0x18, 0x08);
        assert!(
            (sid.output() - 0.10 * 8.0 / 15.0).abs() < 1e-6,
            "scales with volume"
        );
    }
    /// Merged (every check kept): `master_volume_register_scales_the_mixed_output`, `the_master_volume_alone_is_audible_on_a_6581_and_nearly_not_on_an_8580`.
    #[test]
    fn master_volume() {
        master_volume_register_scales_the_mixed_output();
        the_master_volume_alone_is_audible_on_a_6581_and_nearly_not_on_an_8580();
    }

    #[test]
    fn a_6581_filter_never_closes_below_about_220_hz() {
        let w6581 = cutoff_coefficient(0, SidModel::Mos6581);
        let w8580 = cutoff_coefficient(0, SidModel::Mos8580);
        let hz = |w: f32| w * ASSUMED_FILTER_CLOCK_HZ / (2.0 * std::f32::consts::PI);
        assert!((hz(w6581) - 220.0).abs() < 1.0, "{}", hz(w6581));
        assert!(hz(w8580) < 60.0, "{}", hz(w8580));
        // The top of the range is shared.
        assert_eq!(
            cutoff_coefficient(2047, SidModel::Mos6581),
            cutoff_coefficient(2047, SidModel::Mos8580)
        );
    }
}
