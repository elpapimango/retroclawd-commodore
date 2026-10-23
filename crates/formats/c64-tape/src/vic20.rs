//! Strict VIC-20 TAP v0/v1 metadata and exact full-pulse durations.
//! Platform 1 is required; legacy platform-0 recordings need explicit conversion.
//! Timing follows the file's region, not whichever machine happens to load it.

use super::{Error, TAP_HEADER_SIZE, TAP_MAGIC, Tape, decode_tap_pulses};

const MAX_DATA_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoStandard {
    Pal,
    Ntsc,
}

impl VideoStandard {
    pub fn clock_hz(self) -> u32 {
        match self {
            Self::Pal => 1_108_405,
            Self::Ntsc => 1_022_727,
        }
    }
}

pub fn parse(bytes: &[u8]) -> Result<(VideoStandard, Tape), Error> {
    if bytes.len() < TAP_HEADER_SIZE {
        return Err(Error::TooShort(bytes.len()));
    }
    if &bytes[..12] != TAP_MAGIC {
        return Err(Error::BadTapMagic);
    }
    if bytes[12] > 1 {
        return Err(Error::UnsupportedVersion(bytes[12]));
    }
    if bytes[13] != 1 {
        return Err(Error::UnsupportedPlatform(bytes[13]));
    }
    let video = match bytes[14] {
        0 => VideoStandard::Pal,
        1 => VideoStandard::Ntsc,
        n => return Err(Error::UnsupportedVideo(n)),
    };
    if bytes[15] != 0 {
        return Err(Error::ReservedByte(bytes[15]));
    }
    let size = u32::from_le_bytes(bytes[16..20].try_into().expect("checked header"));
    let payload = &bytes[20..];
    if u64::from(size) > MAX_DATA_BYTES as u64 || payload.len() > MAX_DATA_BYTES {
        return Err(Error::DataTooLarge);
    }
    if u64::from(size) != payload.len() as u64 {
        return Err(Error::SizeMismatch);
    }
    let pulses = decode_tap_pulses(payload, bytes[12])?;
    if pulses.contains(&0) {
        return Err(Error::InvalidDuration(0));
    }
    Ok((video, Tape::from_pulses(pulses)))
}

/// Canonical version 1, without quantizing or clipping pulse durations.
pub fn write(video: VideoStandard, tape: &Tape) -> Result<Vec<u8>, Error> {
    let mut size = 0usize;
    for &cycles in tape.pulses() {
        if !(1..=0x00ff_ffff).contains(&cycles) {
            return Err(Error::InvalidDuration(cycles));
        }
        size += if cycles.is_multiple_of(8) && cycles <= 2040 {
            1
        } else {
            4
        };
        if size > MAX_DATA_BYTES {
            return Err(Error::DataTooLarge);
        }
    }
    let mut out = Vec::with_capacity(TAP_HEADER_SIZE + size);
    out.extend_from_slice(TAP_MAGIC);
    out.extend_from_slice(&[1, 1, u8::from(video == VideoStandard::Ntsc), 0]);
    out.extend_from_slice(&(size as u32).to_le_bytes());
    super::append_exact_pulses(&mut out, tape.pulses());
    Ok(out)
}

/// Uses the existing Commodore KERNAL protocol, expressed in VIC timer clocks.
pub fn encode_program(
    video: VideoStandard,
    name: &[u8],
    start: u16,
    data: &[u8],
) -> Result<Vec<u8>, Error> {
    if data.is_empty()
        || data.len() > usize::from(u16::MAX - start)
        || name.is_empty()
        || name.len() > 16
    {
        return Err(Error::InvalidProgram);
    }
    let pulses = super::rom_format::encode_program(3, name, start, data)
        .into_iter()
        .map(|cycles| {
            ((u64::from(cycles) * u64::from(video.clock_hz()) + 492_624) / 985_248) as u32
        })
        .collect();
    write(video, &Tape::from_pulses(pulses))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_pulses_and_metadata_round_trip_and_bad_headers_fail() {
        for video in [VideoStandard::Pal, VideoStandard::Ntsc] {
            let tape = Tape::from_pulses(vec![1, 8, 431, 2040, 2048, 0x00ff_ffff]);
            let bytes = write(video, &tape).unwrap();
            let (actual, parsed) = parse(&bytes).unwrap();
            assert_eq!(actual, video);
            assert_eq!(parsed.pulses(), tape.pulses());
            for (index, value) in [(12, 2), (13, 0), (14, 2), (15, 1), (16, 0)] {
                let mut bad = bytes.clone();
                bad[index] = value;
                assert!(parse(&bad).is_err(), "header byte {index}");
            }
            assert!(parse(&bytes[..bytes.len() - 1]).is_err());
        }
        assert!(write(VideoStandard::Pal, &Tape::from_pulses(vec![0])).is_err());
        assert!(write(VideoStandard::Pal, &Tape::from_pulses(vec![0x0100_0000])).is_err());
        let mut bytes = write(VideoStandard::Pal, &Tape::from_pulses(vec![1])).unwrap();
        bytes[21..24].fill(0);
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn strict_position_validation_rejects_corruption_without_mutation() {
        let mut player = super::super::TapePlayer::new(Tape::from_pulses(vec![10, 20]));
        player.play();
        player.tick(7);
        let valid = player.dump_state();
        assert!(player.validate_state(&valid).is_ok());
        for bad in [
            super::super::TapePosition {
                position: 3,
                ..valid
            },
            super::super::TapePosition {
                cycles_into_current: 10,
                ..valid
            },
            super::super::TapePosition {
                speed_multiplier: 0,
                ..valid
            },
            super::super::TapePosition {
                level: false,
                ..valid
            },
        ] {
            assert!(player.validate_state(&bad).is_err());
        }
        assert_eq!(player.tick(3), 1);
    }
}
