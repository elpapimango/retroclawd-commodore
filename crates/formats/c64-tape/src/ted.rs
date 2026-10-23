//! C16/Plus4 TAP version 2: exact half-wave lengths in the TED's slow timer
//! clock, not the CPU's switchable clock. This API deliberately does not feed
//! C16 half-waves to the C64 player.
//!
//! Reference: VICE manual, "17.1 The raw tape image format",
//! <https://vice-emu.sourceforge.io/vice_17.html#SEC403>.
//! Parsing is strict about version, platform, video standard, reserved byte,
//! declared length and positive durations. Payloads are limited to 32 MiB
//! (at most 128 MiB of decoded durations). Unlike the legacy C64 parser,
//! inconsistent length fields are not repaired.

use super::{TAP_HEADER_SIZE, decode_tap_pulses};
use std::fmt;

const MAGIC: &[u8; 12] = b"C16-TAPE-RAW";
const MAX_DATA_BYTES: usize = 32 * 1024 * 1024;
const MAX_CYCLES: u32 = 0x00ff_ffff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoStandard {
    Pal,
    Ntsc,
}

impl VideoStandard {
    pub fn clock_hz(self) -> u32 {
        match self {
            Self::Pal => 886_724,
            Self::Ntsc => 894_886,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Pal => "PAL",
            Self::Ntsc => "NTSC",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    TooShort(usize),
    BadMagic,
    UnsupportedVersion(u8),
    UnsupportedPlatform(u8),
    UnsupportedVideo(u8),
    ReservedByte(u8),
    SizeMismatch { declared: u32, actual: usize },
    DataTooLarge,
    InvalidDuration(u32),
    Pulse(super::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort(len) => write!(f, "C16 TAP header needs 20 bytes, got {len}"),
            Self::BadMagic => write!(f, "not a C16 TAP (expected C16-TAPE-RAW)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported C16 TAP version {v}; expected 2"),
            Self::UnsupportedPlatform(p) => {
                write!(f, "unsupported C16 TAP platform {p}; expected 2")
            }
            Self::UnsupportedVideo(v) => write!(f, "unsupported C16 TAP video standard {v}"),
            Self::ReservedByte(v) => write!(f, "unsupported C16 TAP reserved byte {v}"),
            Self::SizeMismatch { declared, actual } => {
                write!(
                    f,
                    "C16 TAP data size is {declared}, but {actual} bytes follow"
                )
            }
            Self::DataTooLarge => write!(f, "C16 TAP data exceeds the 32 MiB limit"),
            Self::InvalidDuration(n) => {
                write!(f, "C16 TAP half-wave duration {n} is outside 1..=16777215")
            }
            Self::Pulse(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Pulse(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TedTape {
    video: VideoStandard,
    halfwaves: Vec<u32>,
}

impl TedTape {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < TAP_HEADER_SIZE {
            return Err(Error::TooShort(bytes.len()));
        }
        if &bytes[..12] != MAGIC {
            return Err(Error::BadMagic);
        }
        if bytes[12] != 2 {
            return Err(Error::UnsupportedVersion(bytes[12]));
        }
        if bytes[13] != 2 {
            return Err(Error::UnsupportedPlatform(bytes[13]));
        }
        let video = match bytes[14] {
            0 => VideoStandard::Pal,
            1 => VideoStandard::Ntsc,
            value => return Err(Error::UnsupportedVideo(value)),
        };
        if bytes[15] != 0 {
            return Err(Error::ReservedByte(bytes[15]));
        }
        let declared = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let payload = &bytes[TAP_HEADER_SIZE..];
        if u64::from(declared) > MAX_DATA_BYTES as u64 || payload.len() > MAX_DATA_BYTES {
            return Err(Error::DataTooLarge);
        }
        if u64::from(declared) != payload.len() as u64 {
            return Err(Error::SizeMismatch {
                declared,
                actual: payload.len(),
            });
        }
        let halfwaves = decode_tap_pulses(payload, 2).map_err(Error::Pulse)?;
        Self::from_halfwaves(video, halfwaves)
    }

    /// Constructs a tape without quantizing, clipping or joining half-waves.
    pub fn from_halfwaves(video: VideoStandard, halfwaves: Vec<u32>) -> Result<Self, Error> {
        encoded_len(&halfwaves)?;
        Ok(Self { video, halfwaves })
    }

    pub fn video(&self) -> VideoStandard {
        self.video
    }

    /// Durations in slow TED timer clocks. The first half-wave begins with a
    /// low-to-high transition; consecutive durations alternate the signal level.
    pub fn halfwaves(&self) -> &[u32] {
        &self.halfwaves
    }

    /// Writes a canonical version-2 file, preserving exact duration and order.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let size = encoded_len(&self.halfwaves)?;
        let mut out = Vec::with_capacity(TAP_HEADER_SIZE + size);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&[
            2,
            2,
            match self.video {
                VideoStandard::Pal => 0,
                VideoStandard::Ntsc => 1,
            },
            0,
        ]);
        out.extend_from_slice(&(size as u32).to_le_bytes());
        super::append_exact_pulses(&mut out, &self.halfwaves);
        Ok(out)
    }
}

fn encoded_len(halfwaves: &[u32]) -> Result<usize, Error> {
    let mut size: usize = 0;
    for &cycles in halfwaves {
        if cycles == 0 || cycles > MAX_CYCLES {
            return Err(Error::InvalidDuration(cycles));
        }
        let width = if cycles.is_multiple_of(8) && (8..=2040).contains(&cycles) {
            1
        } else {
            4
        };
        size = size
            .checked_add(width)
            .filter(|&n| n <= MAX_DATA_BYTES)
            .ok_or(Error::DataTooLarge)?;
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(payload: &[u8]) -> Vec<u8> {
        let mut bytes = b"C16-TAPE-RAW".to_vec();
        bytes.extend_from_slice(&[2, 2, 0, 0]);
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn halfwaves_keep_exact_cycles_and_ted_video_metadata() {
        for video in [VideoStandard::Pal, VideoStandard::Ntsc] {
            let cycles = vec![1, 7, 8, 9, 2040, 2041, MAX_CYCLES];
            let tape = TedTape::from_halfwaves(video, cycles).unwrap();
            let bytes = tape.to_bytes().unwrap();
            assert_eq!(TedTape::parse(&bytes).unwrap(), tape);
            assert!(super::super::parse_tap(&bytes).is_err());
        }
        let tape = TedTape::parse(&image(&[1, 0, 1, 2, 3, 255])).unwrap();
        assert_eq!(tape.halfwaves(), [8, 0x03_0201, 2040]);
    }

    #[test]
    fn every_truncated_prefix_and_damaged_header_is_rejected() {
        let bytes = image(&[0, 1, 2, 3]);
        for end in 0..bytes.len() {
            assert!(TedTape::parse(&bytes[..end]).is_err(), "prefix {end}");
        }
        for (offset, value) in [(0, 0), (12, 1), (13, 0), (14, 2), (15, 1), (16, 3)] {
            let mut damaged = bytes.clone();
            damaged[offset] = value;
            assert!(TedTape::parse(&damaged).is_err(), "header {offset}");
        }
        let mut trailing = bytes.clone();
        trailing.push(1);
        assert!(matches!(
            TedTape::parse(&trailing),
            Err(Error::SizeMismatch { .. })
        ));
    }

    #[test]
    fn malformed_pulses_and_unrepresentable_output_are_errors() {
        for payload in [&[0][..], &[0, 1], &[0, 1, 2], &[0, 0, 0, 0]] {
            assert!(TedTape::parse(&image(payload)).is_err());
        }
        for cycles in [0, MAX_CYCLES + 1, u32::MAX] {
            assert_eq!(
                TedTape::from_halfwaves(VideoStandard::Pal, vec![cycles]).unwrap_err(),
                Error::InvalidDuration(cycles)
            );
        }
        let mut oversized = image(&[]);
        oversized[16..20].copy_from_slice(&((MAX_DATA_BYTES as u32) + 1).to_le_bytes());
        assert_eq!(TedTape::parse(&oversized).unwrap_err(), Error::DataTooLarge);
    }
}
