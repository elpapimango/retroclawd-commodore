//! Parses Commodore `.tap`/`.t64` cassette tape files. No dependency on
//! `machine`, any CPU crate, or `c64` -- same "format crate only parses
//! bytes into a plain struct" precedent as `z80-snapshot`/`zx81-snapshot`/
//! `zx-tape`/`d64`: this is a fact about the file format, not about how a
//! machine consumes it.
//!
//! **`.tap` and ZX Spectrum's own `.tap` share only a file extension, not
//! a format** -- Commodore's real `.tap` ("C64-TAPE-RAW") is a completely
//! different byte layout from Sinclair's own `.tap`/`.tzx` family (see
//! `zx-tape`'s own doc comment); the two never share code, only the
//! generic "load a tape by extension, dispatch to whichever machine
//! plugin is selected" mechanism `retroclawd`'s own `apply_media`
//! already uses for every other extension-overloaded case.
//!
//! **`.tap` is a real, raw pulse-capture format** (unlike `.t64` below):
//! after a 20-byte header (`"C64-TAPE-RAW"` magic, version, platform and
//! video-standard bytes, 1 reserved byte, a `u32` LE data-size field at
//! offset 16 -- the header was wrongly taken as 24 bytes before 2026-10-12,
//! which dropped the first 4 pulses of every real file; real files are
//! routinely inconsistent with this last field, so `parse_tap` reads
//! until EOF instead of trusting it, same permissive stance real players
//! already take), the rest of the file is a flat sequence of pulse-length
//! bytes: a nonzero byte `N` means a pulse `N * 8` cycles long; a `0x00`
//! byte (version 1 only) escapes to the next 3 bytes, a little-endian
//! 24-bit *exact* cycle count (no further `* 8` scaling) -- confirmed
//! directly against VICE's own real `datasette.c` (`datasette_read_gap`,
//! fetched live this session, not recalled) rather than `tape/tap.c`'s
//! own *separate* `tap_get_pulse` helper, whose own `>> 3` scaling turned
//! out to belong to a different internal use (an import/pilot-detection
//! aid), not real playback -- see `CREDITS.md` for the full story of how
//! that mismatch was noticed and resolved by trusting the actual
//! real-time player over the parsing-side helper. A `0x00` byte in a
//! **version 0** file has no real, spec-defined meaning at all --
//! `datasette.c`'s own real code falls back to an arbitrary internal
//! constant for it (`datasette_zero_gap_delay`, VICE's own engineering
//! choice, not a documented format value), so this crate errors clearly
//! (`Error::ZeroByteInVersion0`) rather than inventing its own guess at
//! that same constant. **Version 2** ("half-wave", real but only ever
//! used for the C16/Plus4, per VICE's own source) is out of scope for
//! the C64 parser -- `Error::UnsupportedVersion`. The separate [`ted`]
//! API handles C16/Plus4 version-2 half-waves without changing C64 playback.
//!
//! There is no separate "level" encoded anywhere in the file -- each
//! pulse is a real half-wave: the READ line toggles once per pulse,
//! starting from a fixed level, confirmed against `datasette.c`'s own
//! `fullwave[port] ^= 1` toggle. `Tape`/`TapePlayer` therefore store only
//! pulse *lengths*; `TapePlayer::read_bit`'s current level is derived
//! from parity of how many pulses have completed, not stored per-pulse.
//!
//! **`.t64` is not a real bitstream at all** -- a directory-based
//! container (invented by the C64S emulator for convenience) that a real
//! Commodore Datasette never produced; no real hardware ever read one
//! back as an actual cassette signal. Treating it as a genuine tape
//! format to bit-bang would mean inventing timing that was never real --
//! this crate instead exposes exactly what the format really is: a
//! directory of stored programs, each a raw load address plus data,
//! ready to be poked directly into memory the same way `.prg` already is
//! (see `retroclawd`'s own `resolve_program_load`) -- no motor/FLAG/
//! real-time simulation applies to it, which is the honest, not the
//! lazy, choice here. Header (64 bytes: 32-byte description string,
//! `u16` LE version, `u16` LE max/used entry counts, 2 reserved bytes,
//! 24-byte tape name) and each 32-byte directory record (entry type,
//! C1541 file type, `u16` LE start/end address, 4-byte LE absolute file
//! offset to the data, 16-byte PETSCII name) are confirmed directly
//! against VICE's own real `tape/t64.h`/`tape/t64.c` this session -- see
//! `CREDITS.md`. Real T64 files are famously loose about their own
//! header (the "magic" 32-byte description varies across the tools that
//! historically wrote them) -- `parse_t64` doesn't validate it at all,
//! same deliberate leniency real players already need. A record whose
//! `end_addr` doesn't exceed `start_addr` (some real files get this
//! wrong) falls back to "read until EOF" for that entry's size, a
//! reasonable, documented correction, not a bit-for-bit port of VICE's
//! own (not fully citable from source alone) equivalent fix-up.

use std::fmt;

pub mod rom_format;
pub mod ted;

const TAP_MAGIC: &[u8; 12] = b"C64-TAPE-RAW";
const TAP_HEADER_SIZE: usize = 20;

const T64_HEADER_SIZE: usize = 64;
const T64_HDR_NUMENTRIES_OFFSET: usize = 34;
const T64_HDR_NUMUSED_OFFSET: usize = 36;
const T64_REC_SIZE: usize = 32;
const T64_REC_ENTRYTYPE_OFFSET: usize = 0;
const T64_REC_CBMTYPE_OFFSET: usize = 1;
const T64_REC_STARTADDR_OFFSET: usize = 2;
const T64_REC_ENDADDR_OFFSET: usize = 4;
const T64_REC_CONTENTS_OFFSET: usize = 8;
const T64_REC_NAME_OFFSET: usize = 16;
const T64_REC_NAME_LEN: usize = 16;

/// Real `T64_FILE_RECORD_NORMAL` value from VICE's own `t64.h` enum
/// (`FREE`=0, `NORMAL`=1, ...) -- the only entry type this crate treats
/// as a real, loadable program; every other value (an empty slot, a
/// C64S-snapshot pseudo-entry, etc.) is skipped by `programs()`.
const T64_FILE_RECORD_NORMAL: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TooShort(usize),
    BadTapMagic,
    UnsupportedVersion(u8),
    /// A `0x00` pulse byte in a version-0 `.tap` file -- see this
    /// module's own doc comment for why this crate errors here instead
    /// of guessing at VICE's own internal fallback constant.
    ZeroByteInVersion0,
    UnexpectedEof,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooShort(len) => {
                write!(f, "file too short to be a real tape image ({len} bytes)")
            }
            Error::BadTapMagic => write!(f, "not a real .tap file (bad \"C64-TAPE-RAW\" magic)"),
            Error::UnsupportedVersion(v) => write!(f, "unsupported .tap version {v}"),
            Error::ZeroByteInVersion0 => write!(
                f,
                "a 0x00 pulse byte in a version-0 .tap file has no real, defined meaning"
            ),
            Error::UnexpectedEof => write!(f, "unexpected end of file mid-pulse"),
        }
    }
}

impl std::error::Error for Error {}

/// A parsed `.tap` file: a flat sequence of real pulse lengths, each in
/// exact CPU cycles. See this module's own doc comment for the encoding.
#[derive(Debug, Clone)]
pub struct Tape {
    pulses: Vec<u32>,
}

impl Tape {
    pub fn pulse_count(&self) -> usize {
        self.pulses.len()
    }

    /// Builds a tape from pulse lengths in CPU cycles.
    pub fn from_pulses(pulses: Vec<u32>) -> Self {
        Self { pulses }
    }

    /// The pulse lengths in CPU cycles.
    pub fn pulses(&self) -> &[u32] {
        &self.pulses
    }
}

/// Serialises a tape as a version-1 `.tap`: lengths up to 255 units of 8
/// cycles are one byte, anything longer (or not representable) escapes to
/// the exact 24-bit cycle count.
pub fn write_tap(tape: &Tape) -> Vec<u8> {
    let mut body = Vec::with_capacity(tape.pulses.len());
    for &cycles in &tape.pulses {
        let units = (cycles + 4) / 8;
        if (1..=255).contains(&units) {
            body.push(units as u8);
        } else {
            let exact = cycles.min(0x00FF_FFFF);
            body.push(0);
            body.extend_from_slice(&exact.to_le_bytes()[..3]);
        }
    }
    let mut out = Vec::with_capacity(TAP_HEADER_SIZE + body.len());
    out.extend_from_slice(TAP_MAGIC);
    out.push(1); // version
    out.extend_from_slice(&[0, 0, 0]); // platform (C64), video standard, reserved
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Parses a real Commodore `.tap` file (versions 0 and 1 -- version 2 is
/// the C16/Plus4-only "half-wave" format, out of scope here, see this
/// module's own doc comment).
pub fn parse_tap(bytes: &[u8]) -> Result<Tape, Error> {
    if bytes.len() < TAP_HEADER_SIZE {
        return Err(Error::TooShort(bytes.len()));
    }
    if &bytes[0..12] != TAP_MAGIC {
        return Err(Error::BadTapMagic);
    }
    let version = bytes[12];
    if version > 1 {
        return Err(Error::UnsupportedVersion(version));
    }

    Ok(Tape {
        pulses: decode_tap_pulses(&bytes[TAP_HEADER_SIZE..], version)?,
    })
}

fn decode_tap_pulses(bytes: &[u8], version: u8) -> Result<Vec<u32>, Error> {
    let mut pulses = Vec::new();
    let mut bytes = bytes.iter().copied();
    while let Some(b) = bytes.next() {
        if b != 0 {
            pulses.push(b as u32 * 8);
        } else if version == 0 {
            return Err(Error::ZeroByteInVersion0);
        } else {
            let low = bytes.next().ok_or(Error::UnexpectedEof)?;
            let middle = bytes.next().ok_or(Error::UnexpectedEof)?;
            let high = bytes.next().ok_or(Error::UnexpectedEof)?;
            let cycles = u32::from_le_bytes([low, middle, high, 0]);
            pulses.push(cycles);
        }
    }
    Ok(pulses)
}

/// Real-time `.tap` playback, driven by the host's own real motor-control
/// line (see `crate::c64` -- there is no format-level "stop" event the
/// way `zx-tape`'s `.tzx` pause blocks have, since a real Datasette's
/// motor is entirely software-controlled through the 6510 port, not
/// encoded in the tape data itself).
#[derive(Debug, Clone)]
pub struct TapePlayer {
    tape: Tape,
    position: usize,
    cycles_into_current: u32,
    /// The READ line's current level -- starts `true`, toggles once per
    /// completed pulse (real hardware fact, see this module's own doc
    /// comment on `fullwave`).
    level: bool,
    playing: bool,
    speed_multiplier: u32,
}

/// A `TapePlayer`'s playback position, for save states -- deliberately
/// excludes the tape's own data (`TapePlayer::tape`), which stays
/// externally supplied, exactly like `--rom`/`--tape` already are for
/// snapshots (same shape `zx_tape::TapePosition` already established).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct TapePosition {
    position: usize,
    cycles_into_current: u32,
    level: bool,
    playing: bool,
    speed_multiplier: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapePlaybackState {
    pub position: usize,
    pub pulses: usize,
    pub playing: bool,
    pub finished: bool,
    pub speed_multiplier: u32,
}

impl TapePlayer {
    pub fn new(tape: Tape) -> Self {
        Self {
            tape,
            position: 0,
            cycles_into_current: 0,
            level: true,
            playing: false,
            speed_multiplier: 1,
        }
    }

    pub fn dump_state(&self) -> TapePosition {
        TapePosition {
            position: self.position,
            cycles_into_current: self.cycles_into_current,
            level: self.level,
            playing: self.playing,
            speed_multiplier: self.speed_multiplier,
        }
    }

    /// Restores a previously-dumped position -- `position` is clamped to
    /// this tape's own pulse count in case a different (e.g. shorter)
    /// tape ended up loaded instead, same "don't trust a stale position
    /// verbatim" precedent `zx_tape::TapePlayer::restore_state` already
    /// established.
    pub fn restore_state(&mut self, state: TapePosition) {
        self.position = state.position.min(self.tape.pulses.len());
        self.cycles_into_current = state.cycles_into_current;
        self.level = state.level;
        self.playing = state.playing;
        self.speed_multiplier = state.speed_multiplier.max(1);
    }

    /// Turbo mode: the *same* real pulse protocol, sped up -- not a
    /// different mechanism. `n` of `0` is treated as `1` (real time).
    pub fn set_speed_multiplier(&mut self, n: u32) {
        self.speed_multiplier = n.max(1);
    }

    pub fn finished(&self) -> bool {
        self.position >= self.tape.pulses.len()
    }

    /// Starts (real hardware: motor spinning) playback -- a no-op if
    /// already finished. Real motor control is entirely host-driven (the
    /// 6510 port's own bit 5, see `crate::c64`'s own bus wiring), not a
    /// format-level concept -- there is deliberately no "auto-starts on
    /// first read" heuristic here the way `zx_spectrum`'s own tape has,
    /// since the C64 has a genuine, explicit motor line real software
    /// always drives itself.
    pub fn play(&mut self) {
        if !self.finished() {
            self.playing = true;
        }
    }

    /// Stops (motor off) without losing playback position or pulse
    /// phase -- `play()` resumes from exactly the same point, matching
    /// real hardware (a stopped Datasette's tape doesn't move).
    pub fn pause(&mut self) {
        self.playing = false;
    }

    pub fn playback_state(&self) -> TapePlaybackState {
        TapePlaybackState {
            position: self.position,
            pulses: self.tape.pulses.len(),
            playing: self.playing,
            finished: self.finished(),
            speed_multiplier: self.speed_multiplier,
        }
    }

    /// The READ line's current level -- simply the last real level
    /// reached, whether or not the motor is currently running (real
    /// hardware fact: a stopped tape's read head just keeps presenting
    /// whatever level it last settled on; no real software ever polls
    /// this line while the motor is off in the first place, since the
    /// real mechanism is edge-triggered via `Cia::set_flag`, not polled,
    /// so there's no real "idle" convention to approximate here the way
    /// `zx_tape::TapePlayer::ear_bit`'s idle-high default has).
    pub fn read_bit(&self) -> bool {
        self.level
    }

    /// Advances playback by `cycles` real CPU cycles (scaled by the speed
    /// multiplier). A no-op while not playing or already finished.
    /// Returns how many real READ-line transitions occurred -- almost
    /// always `0` or `1` at real CPU-step granularity, but a loop rather
    /// than a single before/after level comparison specifically so an
    /// extreme turbo multiplier spanning multiple short pulses within one
    /// call can never silently collapse two real edges into one (the
    /// host reconstructs each intermediate level by toggling from the
    /// level it read before calling this, see `crate::c64`'s own
    /// `tick_tape`).
    pub fn tick(&mut self, cycles: u32) -> u32 {
        if !self.playing || self.finished() {
            return 0;
        }
        let mut remaining = cycles.saturating_mul(self.speed_multiplier);
        let mut transitions = 0u32;
        while remaining > 0 && !self.finished() {
            let pulse_cycles = self.tape.pulses[self.position];
            let left_in_pulse = pulse_cycles.saturating_sub(self.cycles_into_current);
            if remaining < left_in_pulse {
                self.cycles_into_current += remaining;
                remaining = 0;
            } else {
                remaining -= left_in_pulse;
                self.cycles_into_current = 0;
                self.position += 1;
                self.level = !self.level;
                transitions += 1;
            }
        }
        transitions
    }
}

/// One real `.t64` directory entry.
#[derive(Debug, Clone)]
pub struct T64Entry {
    pub entry_type: u8,
    pub c1541_type: u8,
    pub start_addr: u16,
    pub end_addr: u16,
    /// Raw PETSCII, undecoded -- same "name bytes returned raw" precedent
    /// `d64::DirectoryEntry`/`zx81_snapshot`'s own `.p81` program name
    /// already established; decoding needs the PETSCII table, not needed
    /// to load the program.
    pub name: [u8; T64_REC_NAME_LEN],
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct T64 {
    entries: Vec<T64Entry>,
}

impl T64 {
    pub fn entries(&self) -> &[T64Entry] {
        &self.entries
    }

    /// The first real, loadable program (`entry_type ==
    /// T64_FILE_RECORD_NORMAL`) in this container's own directory order --
    /// `(load_address, data)`, the exact shape `retroclawd`'s own
    /// `resolve_program_load` already uses for `.prg`, since that's
    /// genuinely all a `.t64` entry is: a `.prg`'s own two pieces of
    /// information, just stored in a directory instead of a 2-byte file
    /// header. `None` if the container has no normal entry at all.
    pub fn first_program(&self) -> Option<(u16, &[u8])> {
        self.entries
            .iter()
            .find(|e| e.entry_type == T64_FILE_RECORD_NORMAL)
            .map(|e| (e.start_addr, e.data.as_slice()))
    }
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Parses a real `.t64` tape-image container -- see this module's own
/// doc comment for why this is a directory of stored programs, not a
/// real bitstream.
pub fn parse_t64(bytes: &[u8]) -> Result<T64, Error> {
    if bytes.len() < T64_HEADER_SIZE {
        return Err(Error::TooShort(bytes.len()));
    }
    let num_entries = le_u16(&bytes[T64_HDR_NUMENTRIES_OFFSET..]) as usize;
    let num_used = le_u16(&bytes[T64_HDR_NUMUSED_OFFSET..]) as usize;
    // Real files disagree with their own counts often enough that a
    // strict "trust num_used" stance would reject legitimate tapes --
    // same permissive stance `parse_tap` already takes with its own size
    // field. Bounded by however many whole 32-byte records actually fit
    // in the file between the header and `num_entries`, whichever is
    // smaller.
    let records_that_fit = bytes.len().saturating_sub(T64_HEADER_SIZE) / T64_REC_SIZE;
    let count = num_entries
        .min(records_that_fit)
        .max(num_used.min(records_that_fit));

    let mut entries = Vec::new();
    for i in 0..count {
        let rec_start = T64_HEADER_SIZE + i * T64_REC_SIZE;
        let rec = &bytes[rec_start..rec_start + T64_REC_SIZE];
        let entry_type = rec[T64_REC_ENTRYTYPE_OFFSET];
        if entry_type == 0 {
            continue; // a real, unused directory slot -- not an error.
        }
        let c1541_type = rec[T64_REC_CBMTYPE_OFFSET];
        let start_addr = le_u16(&rec[T64_REC_STARTADDR_OFFSET..]);
        let end_addr = le_u16(&rec[T64_REC_ENDADDR_OFFSET..]);
        let contents_offset = le_u32(&rec[T64_REC_CONTENTS_OFFSET..]) as usize;
        let mut name = [0u8; T64_REC_NAME_LEN];
        name.copy_from_slice(&rec[T64_REC_NAME_OFFSET..T64_REC_NAME_OFFSET + T64_REC_NAME_LEN]);

        if entry_type != T64_FILE_RECORD_NORMAL {
            // A real but non-program record (a C64S snapshot pseudo-entry,
            // etc.) -- kept in the directory listing, but has no real
            // program data to extract here.
            entries.push(T64Entry {
                entry_type,
                c1541_type,
                start_addr,
                end_addr,
                name,
                data: Vec::new(),
            });
            continue;
        }

        let declared_size = (end_addr as usize).saturating_sub(start_addr as usize);
        let size = if declared_size == 0 {
            bytes.len().saturating_sub(contents_offset)
        } else {
            declared_size
        };
        let data = bytes
            .get(contents_offset..contents_offset + size)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();

        entries.push(T64Entry {
            entry_type,
            c1541_type,
            start_addr,
            end_addr,
            name,
            data,
        });
    }

    Ok(T64 { entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap_header(version: u8) -> Vec<u8> {
        let mut header = vec![0u8; TAP_HEADER_SIZE];
        header[0..12].copy_from_slice(TAP_MAGIC);
        header[12] = version;
        header
    }

    fn parse_tap_rejects_a_bad_magic() {
        let bytes = vec![0u8; TAP_HEADER_SIZE];
        assert_eq!(parse_tap(&bytes).unwrap_err(), Error::BadTapMagic);
    }

    fn parse_tap_rejects_too_short_a_file() {
        assert_eq!(parse_tap(&[0u8; 4]).unwrap_err(), Error::TooShort(4));
    }

    fn parse_tap_rejects_version_2() {
        let bytes = tap_header(2);
        assert_eq!(parse_tap(&bytes).unwrap_err(), Error::UnsupportedVersion(2));
    }

    fn parse_tap_version_0_decodes_nonzero_bytes_as_times_eight_cycles() {
        let mut bytes = tap_header(0);
        bytes.extend([0x10, 0x20, 0xFF]);
        let tape = parse_tap(&bytes).unwrap();
        assert_eq!(tape.pulses, vec![0x10 * 8, 0x20 * 8, 0xFF * 8]);
    }

    fn parse_tap_version_0_rejects_a_zero_byte() {
        let mut bytes = tap_header(0);
        bytes.push(0x00);
        assert_eq!(parse_tap(&bytes).unwrap_err(), Error::ZeroByteInVersion0);
    }

    fn parse_tap_version_1_decodes_the_real_extended_pulse_as_an_exact_cycle_count() {
        let mut bytes = tap_header(1);
        // 0x00 escape + 3-byte LE extended value = 0x654321 cycles exactly
        // (no further *8 scaling -- confirmed against real datasette.c).
        bytes.extend([0x00, 0x21, 0x43, 0x65]);
        let tape = parse_tap(&bytes).unwrap();
        assert_eq!(tape.pulses, vec![0x654321]);
    }
    /// Merged (every check kept): `parse_tap_version_0_decodes_nonzero_bytes_as_times_eight_cycles`, `parse_tap_version_1_decodes_the_real_extended_pulse_as_an_exact_cycle_count`.
    #[test]
    fn tap_decoding() {
        parse_tap_version_0_decodes_nonzero_bytes_as_times_eight_cycles();
        parse_tap_version_1_decodes_the_real_extended_pulse_as_an_exact_cycle_count();
    }

    fn parse_tap_version_1_errors_on_a_truncated_extended_pulse() {
        let mut bytes = tap_header(1);
        bytes.extend([0x00, 0x21]); // needs 3 bytes, only 2 given
        assert_eq!(parse_tap(&bytes).unwrap_err(), Error::UnexpectedEof);
    }
    /// Merged (every check kept): `parse_tap_rejects_a_bad_magic`, `parse_tap_rejects_too_short_a_file`, `parse_tap_rejects_version_2`, `parse_tap_version_0_rejects_a_zero_byte`, `parse_tap_version_1_errors_on_a_truncated_extended_pulse`.
    #[test]
    fn tap_rejections() {
        parse_tap_rejects_a_bad_magic();
        parse_tap_rejects_too_short_a_file();
        parse_tap_rejects_version_2();
        parse_tap_version_0_rejects_a_zero_byte();
        parse_tap_version_1_errors_on_a_truncated_extended_pulse();
    }

    fn tape_player_toggles_the_read_line_once_per_completed_pulse() {
        let tape = Tape {
            pulses: vec![10, 20, 30],
        };
        let mut player = TapePlayer::new(tape);
        player.play();
        assert!(player.read_bit(), "starts high");

        assert_eq!(player.tick(9), 0, "9 of 10 cycles -- pulse not done yet");
        assert!(player.read_bit(), "still the same level");

        assert_eq!(player.tick(1), 1, "completes the first pulse");
        assert!(!player.read_bit(), "toggled low");

        assert_eq!(player.tick(20), 1, "completes the second pulse exactly");
        assert!(player.read_bit(), "toggled back high");
    }

    fn tape_player_reports_every_transition_even_across_multiple_pulses_in_one_tick() {
        let tape = Tape {
            pulses: vec![5, 5, 5, 5],
        };
        let mut player = TapePlayer::new(tape);
        player.play();
        let transitions = player.tick(20); // spans all four pulses exactly
        assert_eq!(transitions, 4);
        assert!(player.finished());
    }
    /// Merged (every check kept): `tape_player_toggles_the_read_line_once_per_completed_pulse`, `tape_player_reports_every_transition_even_across_multiple_pulses_in_one_tick`.
    #[test]
    fn tape_player_transitions() {
        tape_player_toggles_the_read_line_once_per_completed_pulse();
        tape_player_reports_every_transition_even_across_multiple_pulses_in_one_tick();
    }

    fn tape_player_pause_freezes_position_and_resumes_from_the_same_point() {
        let tape = Tape { pulses: vec![100] };
        let mut player = TapePlayer::new(tape);
        player.play();
        player.tick(40);
        player.pause();
        assert_eq!(player.tick(1000), 0, "paused -- no advancement at all");
        player.play();
        assert_eq!(player.tick(59), 0, "resumes exactly where it left off");
        assert_eq!(player.tick(1), 1, "completes right on schedule");
    }

    fn tape_player_save_state_round_trips_and_clamps_a_stale_position() {
        let tape = Tape {
            pulses: vec![10, 10, 10],
        };
        let mut player = TapePlayer::new(tape);
        player.play();
        player.tick(15);
        let state = player.dump_state();

        let shorter_tape = Tape { pulses: vec![10] };
        let mut restored = TapePlayer::new(shorter_tape);
        restored.restore_state(state);
        assert!(
            restored.position <= 1,
            "position must be clamped to the shorter tape's own pulse count"
        );
    }
    /// Merged (every check kept): `tape_player_pause_freezes_position_and_resumes_from_the_same_point`, `tape_player_save_state_round_trips_and_clamps_a_stale_position`.
    #[test]
    fn tape_player_pause_and_state() {
        tape_player_pause_freezes_position_and_resumes_from_the_same_point();
        tape_player_save_state_round_trips_and_clamps_a_stale_position();
    }

    fn t64_header(num_entries: u16, num_used: u16) -> Vec<u8> {
        let mut header = vec![0u8; T64_HEADER_SIZE];
        header[T64_HDR_NUMENTRIES_OFFSET..T64_HDR_NUMENTRIES_OFFSET + 2]
            .copy_from_slice(&num_entries.to_le_bytes());
        header[T64_HDR_NUMUSED_OFFSET..T64_HDR_NUMUSED_OFFSET + 2]
            .copy_from_slice(&num_used.to_le_bytes());
        header
    }

    fn t64_record(
        entry_type: u8,
        c1541_type: u8,
        start_addr: u16,
        end_addr: u16,
        contents_offset: u32,
        name: &[u8],
    ) -> Vec<u8> {
        let mut rec = vec![0u8; T64_REC_SIZE];
        rec[T64_REC_ENTRYTYPE_OFFSET] = entry_type;
        rec[T64_REC_CBMTYPE_OFFSET] = c1541_type;
        rec[T64_REC_STARTADDR_OFFSET..T64_REC_STARTADDR_OFFSET + 2]
            .copy_from_slice(&start_addr.to_le_bytes());
        rec[T64_REC_ENDADDR_OFFSET..T64_REC_ENDADDR_OFFSET + 2]
            .copy_from_slice(&end_addr.to_le_bytes());
        rec[T64_REC_CONTENTS_OFFSET..T64_REC_CONTENTS_OFFSET + 4]
            .copy_from_slice(&contents_offset.to_le_bytes());
        let name_len = name.len().min(T64_REC_NAME_LEN);
        rec[T64_REC_NAME_OFFSET..T64_REC_NAME_OFFSET + name_len].copy_from_slice(&name[..name_len]);
        rec
    }

    fn parse_t64_extracts_the_first_normal_programs_load_address_and_data() {
        let mut bytes = t64_header(1, 1);
        let data_offset = (T64_HEADER_SIZE + T64_REC_SIZE) as u32;
        bytes.extend(t64_record(
            T64_FILE_RECORD_NORMAL,
            2,
            0x0801,
            0x0801 + 3,
            data_offset,
            b"PROGRAM",
        ));
        bytes.extend([0xAA, 0xBB, 0xCC]);

        let t64 = parse_t64(&bytes).unwrap();
        let (addr, data) = t64.first_program().expect("must find the normal entry");
        assert_eq!(addr, 0x0801);
        assert_eq!(data, &[0xAA, 0xBB, 0xCC]);
    }

    fn parse_t64_skips_unused_slots_and_finds_the_first_real_entry_after_them() {
        let mut bytes = t64_header(2, 1);
        bytes.extend(vec![0u8; T64_REC_SIZE]); // slot 0: entry_type 0, unused
        let data_offset = (T64_HEADER_SIZE + 2 * T64_REC_SIZE) as u32;
        bytes.extend(t64_record(
            T64_FILE_RECORD_NORMAL,
            2,
            0xC000,
            0xC000 + 2,
            data_offset,
            b"REAL",
        ));
        bytes.extend([0x11, 0x22]);

        let t64 = parse_t64(&bytes).unwrap();
        assert_eq!(t64.entries().len(), 1, "the all-zero slot must be skipped");
        let (addr, data) = t64.first_program().unwrap();
        assert_eq!(addr, 0xC000);
        assert_eq!(data, &[0x11, 0x22]);
    }

    fn parse_t64_falls_back_to_reading_until_eof_when_end_addr_does_not_exceed_start_addr() {
        let mut bytes = t64_header(1, 1);
        let data_offset = (T64_HEADER_SIZE + T64_REC_SIZE) as u32;
        // A real, malformed-but-seen-in-the-wild case: end_addr == start_addr.
        bytes.extend(t64_record(
            T64_FILE_RECORD_NORMAL,
            2,
            0x0801,
            0x0801,
            data_offset,
            b"BAD",
        ));
        bytes.extend([1, 2, 3, 4, 5]);

        let t64 = parse_t64(&bytes).unwrap();
        let (_, data) = t64.first_program().unwrap();
        assert_eq!(data, &[1, 2, 3, 4, 5], "must read every remaining byte");
    }
    /// Merged (every check kept): `parse_t64_extracts_the_first_normal_programs_load_address_and_data`, `parse_t64_skips_unused_slots_and_finds_the_first_real_entry_after_them`, `parse_t64_falls_back_to_reading_until_eof_when_end_addr_does_not_exceed_start_addr`.
    #[test]
    fn parse_t64_cases() {
        parse_t64_extracts_the_first_normal_programs_load_address_and_data();
        parse_t64_skips_unused_slots_and_finds_the_first_real_entry_after_them();
        parse_t64_falls_back_to_reading_until_eof_when_end_addr_does_not_exceed_start_addr();
    }

    #[test]
    fn parse_t64_rejects_too_short_a_file() {
        assert_eq!(parse_t64(&[0u8; 10]).unwrap_err(), Error::TooShort(10));
    }
}
