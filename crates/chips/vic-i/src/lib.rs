//! MOS 6560/6561 VIC-I video and sound, independent of CPU and Machine wiring.
//!
//! Sources: Commodore's VIC-20 Programmer's Reference Guide, pp. 174-177,
//! 212-216; VICE 3.10.0 `vic-mem.c`, `vic20.h`, and `vic20sound.c`.
//! See the Family CREDITS.md for pinned revisions.
//!
//! One tick is one CPU clock, or four video dots. The display fetch model
//! handles character addressing, color, and raster-visible writes, but not
//! VIC-I's sub-clock register delays or display flip-flop tricks. Interlace,
//! light-pen latching and analogue paddle conversion are not implemented.
//! Sound uses the documented divider/shift-register circuits, VICE's measured
//! nonlinear output-voltage calibration and RC filters.

use serde::{Deserialize, Serialize};

pub const SAMPLE_RATE: u32 = 44_100;

// Breakpoints of VICE 3.10.0's VIC-I voltage calibration (GPL-2.0-or-later;
// Rami Rasanen and Ville-Matias Heikkila). Linear interpolation reproduces
// all 436 published entries within 0.02 PCM16 units. See CREDITS.md.
const MIXER_CURVE: [(f32, f32); 39] = [
    (0.0, 0.0),
    (2.0, 296.55),
    (3.0, 735.97),
    (4.0, 914.88),
    (5.0, 1126.89),
    (6.0, 1321.86),
    (7.0, 1503.07),
    (8.0, 1603.50),
    (9.0, 1758.00),
    (10.0, 1913.98),
    (11.0, 2070.94),
    (12.0, 2220.36),
    (13.0, 2342.91),
    (14.0, 2488.07),
    (15.0, 3188.98),
    (24.0, 4059.96),
    (30.0, 5188.81),
    (32.0, 5197.00),
    (40.0, 6329.14),
    (45.0, 7729.97),
    (56.0, 8603.24),
    (60.0, 9814.02),
    (72.0, 10040.12),
    (75.0, 12281.45),
    (88.0, 12327.32),
    (90.0, 13898.78),
    (104.0, 14064.03),
    (105.0, 16926.31),
    (120.0, 17837.26),
    (132.0, 20288.31),
    (135.0, 20676.61),
    (150.0, 22141.34),
    (165.0, 23_206.3),
    (174.0, 23648.67),
    (180.0, 24369.56),
    (195.0, 25604.92),
    (210.0, 26414.28),
    (225.0, 27718.45),
    (435.0, 29_491.2),
];

fn mixer_voltage(position: f32) -> f32 {
    let upper = MIXER_CURVE.partition_point(|&(index, _)| index < position);
    if upper == 0 {
        return 0.0;
    }
    let (left, low) = MIXER_CURVE[upper - 1];
    let (right, high) = MIXER_CURVE[upper];
    low + (high - low) * (position - left) / (right - left)
}

/// VICE's original VIC-I RGB calibration, not the C64 VIC-II palette.
pub const PALETTE: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0xff, 0xff, 0xff],
    [0xf0, 0x00, 0x00],
    [0x00, 0xf0, 0xf0],
    [0x60, 0x00, 0x60],
    [0x00, 0xa0, 0x00],
    [0x00, 0x00, 0xf0],
    [0xd0, 0xd0, 0x00],
    [0xc0, 0xa0, 0x00],
    [0xff, 0xa0, 0x00],
    [0xf0, 0x80, 0x80],
    [0x00, 0xff, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0x00],
    [0x00, 0xa0, 0xff],
    [0xff, 0xff, 0x00],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Standard {
    Pal,
    Ntsc,
}

impl Standard {
    pub fn clock_hz(self) -> u32 {
        match self {
            Self::Pal => 1_108_405,
            Self::Ntsc => 1_022_727,
        }
    }

    pub fn cycles_per_line(self) -> u16 {
        match self {
            Self::Pal => 71,
            Self::Ntsc => 65,
        }
    }

    pub fn cycles_per_frame(self) -> u32 {
        match self {
            Self::Pal => 22_152,
            Self::Ntsc => 16_965,
        }
    }

    pub fn height(self) -> u16 {
        match self {
            Self::Pal => 312,
            Self::Ntsc => 262,
        }
    }

    pub fn width(self) -> u16 {
        self.cycles_per_line() * 4
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Voice {
    counter: u16,
    shift: u8,
    output: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vic {
    standard: Standard,
    registers: [u8; 16],
    cycle: u16,
    raster: u16,
    frames: u64,
    bus_value: u8,
    drawing: Vec<u8>,
    completed: Vec<u8>,
    voices: [Voice; 4],
    noise: u16,
    previous_noise_bit: bool,
    sample_phase: u32,
    sample_sum: u32,
    sample_clocks: u32,
    low_pass: f32,
    high_pass: f32,
}

impl Vic {
    pub fn new(standard: Standard) -> Self {
        let size = usize::from(standard.width()) * usize::from(standard.height());
        let mut registers = [0; 16];
        registers[8] = 0xff;
        registers[9] = 0xff;
        Self {
            standard,
            registers,
            cycle: 0,
            raster: 0,
            frames: 0,
            bus_value: 0,
            drawing: vec![0; size],
            completed: vec![0; size],
            voices: std::array::from_fn(|_| Voice::default()),
            noise: 0,
            previous_noise_bit: false,
            sample_phase: 0,
            sample_sum: 0,
            sample_clocks: 0,
            low_pass: 0.0,
            high_pass: 0.0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.standard);
    }

    pub fn standard(&self) -> Standard {
        self.standard
    }
    pub fn raster(&self) -> u16 {
        self.raster
    }
    pub fn cycle(&self) -> u16 {
        self.cycle
    }
    pub fn frames(&self) -> u64 {
        self.frames
    }
    pub fn bus_value(&self) -> u8 {
        self.bus_value
    }
    pub fn drive_bus(&mut self, value: u8) {
        self.bus_value = value;
    }
    pub fn interlace_requested(&self) -> bool {
        self.registers[0] & 0x80 != 0
    }

    pub fn read(&self, register: u8) -> u8 {
        match register & 15 {
            3 => (self.registers[3] & 0x7f) | ((self.raster as u8 & 1) << 7),
            4 => (self.raster >> 1) as u8,
            index => self.registers[usize::from(index)],
        }
    }

    pub fn write(&mut self, register: u8, value: u8) {
        match register & 15 {
            4 | 6..=9 => {}
            index => self.registers[usize::from(index)] = value,
        }
    }

    /// `fetch` receives a VIC-side 14-bit address, never a CPU address.
    /// Return the eight data pins and four color pins for that address.
    /// The Machine owns the two address spaces' different wiring.
    pub fn tick(&mut self, mut fetch: impl FnMut(u16) -> (u8, u8)) -> Option<f32> {
        let width = self.standard.width();
        let x = self.cycle * 4;
        for dot in x..x + 4 {
            let color = self.pixel(dot, self.raster, &mut fetch);
            self.drawing[usize::from(self.raster) * usize::from(width) + usize::from(dot)] = color;
        }
        self.cycle += 1;
        let line_cycles = match (self.standard, self.raster) {
            (Standard::Ntsc, 0) => 32,
            (Standard::Ntsc, 261) => 33,
            _ => self.standard.cycles_per_line(),
        };
        if self.cycle == line_cycles {
            self.cycle = 0;
            self.raster += 1;
            if self.raster == self.standard.height() {
                self.raster = 0;
                self.frames = self.frames.wrapping_add(1);
                std::mem::swap(&mut self.drawing, &mut self.completed);
                self.drawing.fill(self.registers[15] & 7);
            }
        }
        self.clock_sound()
    }

    fn pixel(&mut self, x: u16, y: u16, fetch: &mut impl FnMut(u16) -> (u8, u8)) -> u8 {
        let border = self.registers[15] & 7;
        let left = u16::from(self.registers[0] & 0x7f) * 4;
        let top = u16::from(self.registers[1]) * 2;
        let columns = u16::from(self.registers[2] & 0x7f);
        let rows = u16::from((self.registers[3] >> 1) & 0x3f);
        let char_height = if self.registers[3] & 1 == 0 { 8 } else { 16 };
        if x < left || y < top || x - left >= columns * 8 || y - top >= rows * char_height {
            return border;
        }
        let cell = (y - top) / char_height * columns + (x - left) / 8;
        let matrix =
            (u16::from(self.registers[5] & 0xf0) << 6) | (u16::from(self.registers[2] & 0x80) << 2);
        let (character, color) = fetch((matrix + cell) & 0x3fff);
        let character_base = u16::from(self.registers[5] & 15) << 10;
        let glyph_address =
            character_base + u16::from(character) * char_height + (y - top) % char_height;
        let (glyph, _) = fetch(glyph_address & 0x3fff);
        self.bus_value = glyph;
        let bit = ((x - left) & 7) as u8;
        let background = self.registers[15] >> 4;
        if color & 8 == 0 {
            let set = (glyph >> (7 - bit)) & 1 != 0;
            let normal = self.registers[15] & 8 != 0;
            if set == normal { color & 7 } else { background }
        } else {
            match (glyph >> (6 - (bit & 6))) & 3 {
                0 => background,
                1 => border,
                2 => color & 7,
                _ => self.registers[14] >> 4,
            }
        }
    }

    fn clock_sound(&mut self) -> Option<f32> {
        for channel in 0..4 {
            let voice = &mut self.voices[channel];
            if voice.counter > 1 {
                voice.counter -= 1;
                continue;
            }
            let value = self.registers[10 + channel];
            let divider = u16::from((!value) & 0x7f);
            voice.counter = if divider == 0 { 128 } else { divider } << (4 - channel);
            let enabled = value & 0x80 != 0;
            let noise_bit = self.noise & 1 != 0;
            if channel < 3 || (noise_bit && !self.previous_noise_bit) {
                let input = enabled && voice.shift & 0x80 == 0;
                voice.shift = (voice.shift << 1) | u8::from(input);
            }
            if channel == 3 {
                let parity = ((self.noise >> 3)
                    ^ (self.noise >> 12)
                    ^ (self.noise >> 14)
                    ^ (self.noise >> 15))
                    & 1;
                let feedback = if enabled { parity } else { 1 };
                self.previous_noise_bit = noise_bit;
                self.noise = (self.noise << 1) | feedback;
            }
            voice.output = voice.shift & 1 != 0 && (channel < 3 || enabled);
        }
        self.sample_sum += self.voices.iter().filter(|v| v.output).count() as u32
            * u32::from(self.registers[14] & 15);
        self.sample_clocks += 1;
        self.sample_phase += SAMPLE_RATE;
        if self.sample_phase < self.standard.clock_hz() {
            return None;
        }
        self.sample_phase -= self.standard.clock_hz();
        let position = 7.0 * self.sample_sum as f32 / self.sample_clocks as f32
            + f32::from(self.registers[14] & 15);
        let level = mixer_voltage(position) / 32767.0;
        self.sample_sum = 0;
        self.sample_clocks = 0;
        let dt = 1.0 / SAMPLE_RATE as f32;
        let sample = self.low_pass - self.high_pass;
        self.high_pass += dt / (dt + 0.001) * (self.low_pass - self.high_pass);
        self.low_pass += dt / (dt + 0.0001) * (level - self.low_pass);
        Some(sample)
    }

    pub fn frame_rgb(&self) -> (u32, u32, Vec<u8>) {
        let pixels = self
            .completed
            .iter()
            .flat_map(|&index| PALETTE[usize::from(index)])
            .collect();
        (
            u32::from(self.standard.width()),
            u32::from(self.standard.height()),
            pixels,
        )
    }

    /// Validate deserialized state before it becomes live Chip state.
    pub fn validate(&self) -> Result<(), String> {
        let size = usize::from(self.standard.width()) * usize::from(self.standard.height());
        let line_cycles = match (self.standard, self.raster) {
            (Standard::Ntsc, 0) => 32,
            (Standard::Ntsc, 261) => 33,
            _ => self.standard.cycles_per_line(),
        };
        if self.raster >= self.standard.height()
            || self.cycle >= line_cycles
            || self.drawing.len() != size
            || self.completed.len() != size
            || self.drawing.iter().chain(&self.completed).any(|&v| v > 15)
            || self.sample_phase >= self.standard.clock_hz()
            || self.sample_clocks > 26
            || self.sample_sum > self.sample_clocks * 60
            || self.voices.iter().any(|voice| voice.counter > 2048)
            || !self.low_pass.is_finite()
            || !self.high_pass.is_finite()
        {
            return Err("invalid VIC-I state".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(vic: &mut Vic, cycles: u32) -> Vec<f32> {
        (0..cycles).filter_map(|_| vic.tick(|_| (0, 0))).collect()
    }

    #[test]
    fn exact_frames_include_the_two_partial_ntsc_lines() {
        for standard in [Standard::Pal, Standard::Ntsc] {
            let mut vic = Vic::new(standard);
            let count = standard.cycles_per_frame();
            let samples = clock(&mut vic, count);
            assert_eq!((vic.raster(), vic.cycle(), vic.frames()), (0, 0, 1));
            assert_eq!(
                samples.len(),
                (u64::from(count) * u64::from(SAMPLE_RATE) / u64::from(standard.clock_hz()))
                    as usize
            );
            vic.validate().unwrap();
        }
    }

    #[test]
    fn raster_registers_split_the_counter_and_ignore_writes() {
        let mut vic = Vic::new(Standard::Pal);
        vic.write(3, 0xa3);
        vic.write(4, 0xff);
        clock(&mut vic, 71 * 257);
        assert_eq!(vic.read(3), 0xa3);
        assert_eq!(vic.read(4), 128);
        clock(&mut vic, 71);
        assert_eq!(vic.read(3), 0x23);
        assert_eq!(vic.read(4), 129);
    }

    fn picture(color: u8, glyph: u8, reverse: bool) -> Vec<u8> {
        let mut vic = Vic::new(Standard::Pal);
        vic.write(2, 1);
        vic.write(3, 2);
        vic.write(5, 0x80);
        vic.write(14, 0xa0);
        vic.write(15, 0x13 | if reverse { 0 } else { 8 });
        for _ in 0..22_152 {
            vic.tick(|address| {
                if address == 0x2000 {
                    (1, color)
                } else {
                    (glyph, 0)
                }
            });
        }
        vic.completed[..8].to_vec()
    }

    #[test]
    fn hires_reverse_and_all_four_multicolor_sources_render() {
        assert_eq!(picture(2, 0xa5, false), [2, 1, 2, 1, 1, 2, 1, 2]);
        assert_eq!(picture(2, 0xa5, true), [1, 2, 1, 2, 2, 1, 2, 1]);
        assert_eq!(picture(10, 0x1b, false), [1, 1, 3, 3, 2, 2, 10, 10]);
    }

    #[test]
    fn sound_and_picture_continue_identically_after_serialization() {
        let mut vic = Vic::new(Standard::Pal);
        vic.write(10, 0xe0);
        vic.write(11, 0xc3);
        vic.write(13, 0xb7);
        vic.write(14, 15);
        clock(&mut vic, 3911);
        let config = bincode::config::standard();
        let bytes = bincode::serde::encode_to_vec(&vic, config).unwrap();
        let (mut restored, _): (Vic, usize) =
            bincode::serde::decode_from_slice(&bytes, config).unwrap();
        restored.validate().unwrap();
        assert_eq!(clock(&mut vic, 30_000), clock(&mut restored, 30_000));
        assert_eq!(vic.frame_rgb(), restored.frame_rgb());
        vic.reset();
        assert_eq!(clock(&mut vic, 2000), vec![0.0; 79]);
    }

    #[test]
    fn invalid_frame_and_audio_state_is_rejected() {
        let mut vic = Vic::new(Standard::Pal);
        vic.completed.pop();
        assert!(vic.validate().is_err());
        let mut vic = Vic::new(Standard::Ntsc);
        vic.cycle = 32;
        assert!(vic.validate().is_err());
        let mut vic = Vic::new(Standard::Pal);
        vic.low_pass = f32::NAN;
        assert!(vic.validate().is_err());
    }

    #[test]
    fn tone_dividers_and_disabled_noise_keep_their_clock_phase() {
        let mut vic = Vic::new(Standard::Pal);
        vic.write(10, 0xfe);
        let outputs: Vec<bool> = (0..256)
            .map(|_| {
                vic.clock_sound();
                vic.voices[0].output
            })
            .collect();
        assert!(outputs[..128].iter().all(|&output| output));
        assert!(outputs[128..].iter().all(|&output| !output));
        assert_ne!(vic.noise, 0);
        vic.write(13, 0xfe);
        let before = vic.noise;
        clock(&mut vic, 2048);
        assert_ne!(vic.noise, before);
    }

    #[test]
    fn volume_writes_integrate_at_each_clock_not_at_sample_boundaries() {
        let mut vic = Vic::new(Standard::Pal);
        vic.voices[0].output = true;
        vic.voices[0].counter = 100;
        for _ in 0..10 {
            assert!(vic.clock_sound().is_none());
        }
        vic.write(14, 15);
        for _ in 0..10 {
            assert!(vic.clock_sound().is_none());
        }
        assert_eq!(vic.sample_sum, 150);
        vic.write(14, 0);
        let sample = (0..6).find_map(|_| vic.clock_sound()).unwrap();
        assert_eq!(sample, 0.0);
        let sample = (0..26).find_map(|_| vic.clock_sound()).unwrap();
        assert!(sample > 0.0);
    }

    #[test]
    fn tone_volume_curve_matches_reference_rms() {
        for (standard, volume, expected) in [
            (Standard::Pal, 15, 0.155869),
            (Standard::Ntsc, 15, 0.151126),
            (Standard::Pal, 7, 0.075602),
        ] {
            let mut vic = Vic::new(standard);
            vic.write(10, 0xe0);
            vic.write(14, volume);
            let samples = clock(&mut vic, standard.clock_hz() / 2);
            let settled = &samples[SAMPLE_RATE as usize / 10..];
            let mean = settled.iter().map(|&v| f64::from(v)).sum::<f64>() / settled.len() as f64;
            let rms = (settled
                .iter()
                .map(|&v| (f64::from(v) - mean).powi(2))
                .sum::<f64>()
                / settled.len() as f64)
                .sqrt();
            assert!((rms / expected - 1.0).abs() < 0.01, "{standard:?}: {rms}");
        }
    }
}
