//! The 1581's 3.5" drive: an 80-cylinder, double-sided MFM floppy turning at
//! 300 RPM under a head, modelled as the raw byte stream of the current
//! track (6250 bytes, one per 32 us) that [`wd1770::Wd1770`] reads and
//! writes.
//!
//! A `.d81` image is a list of 256-byte DOS sectors; the drive stores 512-byte
//! MFM sectors, 10 per side per cylinder. Following VICE's `fdd.c` (reference
//! only): the head select line that reaches the WD1770 is the inverse of CIA
//! Port A bit 0, and the ID field's head byte and the half of the cylinder
//! the data comes from are both that value inverted again, so **Port A bit 0
//! selects the cylinder's first (0) or second (1) half of the image**. A
//! physical sector with ID `R` (1-10) holds the two DOS sectors starting at
//! `(cylinder * 2 + half) * 20 + 2 * (R - 1)`.
//!
//! The track is synthesized from the image when the head arrives (gap 4a,
//! then per sector: 12 zero bytes, three `A1` sync bytes, `FE`, C H R N, CRC,
//! gap 2, 12 zeros, three `A1` sync bytes, `FB`, 512 data bytes, CRC, gap 3)
//! and decoded back into the image when the head leaves it after the
//! controller wrote to it.

use wd1770::{CRC_AFTER_DAM, CRC_AFTER_IDAM, Floppy, crc_byte};

/// Bytes in one revolution: 250 kbit/s at 300 RPM.
pub(crate) const TRACK_BYTES: usize = 6250;
const SECTORS_PER_SIDE: u8 = 10;
const SECTOR_CODE: u8 = 2; // 512 bytes
const CYLINDERS: u8 = d81::TRACK_COUNT;
/// How many bytes after the index the hole sensor stays active.
const INDEX_LEN: usize = 16;
const MAX_CYLINDER: u8 = 85;

#[derive(Clone)]
pub(crate) struct Floppy81 {
    disk: Option<d81::D81>,
    motor: bool,
    cylinder: u8,
    /// The head line as the WD1770 sees it (inverse of CIA Port A bit 0).
    head: u8,
    raw: Vec<u8>,
    sync: Vec<bool>,
    pos: usize,
    index_count: u32,
    /// `(cylinder, head)` the raw buffer holds.
    built_for: Option<(u8, u8)>,
    dirty: bool,
    /// The disk-change line: set on insert/eject, cleared by the first step.
    disk_change: bool,
    modified: bool,
}

/// What survives a save state (the media stays external).
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(crate) struct Floppy81State {
    motor: bool,
    cylinder: u8,
    head: u8,
    pos: usize,
    index_count: u32,
    disk_change: bool,
}

impl Default for Floppy81 {
    fn default() -> Self {
        Self {
            disk: None,
            motor: false,
            cylinder: 0,
            head: 0,
            raw: vec![0x4E; TRACK_BYTES],
            sync: vec![false; TRACK_BYTES],
            pos: 0,
            index_count: 0,
            built_for: None,
            dirty: false,
            disk_change: true,
            modified: false,
        }
    }
}

impl Floppy81 {
    pub(crate) fn attach(&mut self, disk: d81::D81) {
        self.flush();
        self.disk = Some(disk);
        self.built_for = None;
        self.disk_change = true;
    }

    pub(crate) fn detach(&mut self) {
        self.flush();
        self.disk = None;
        self.built_for = None;
        self.disk_change = true;
    }

    pub(crate) fn has_disk(&self) -> bool {
        self.disk.is_some()
    }

    pub(crate) fn disk_change(&self) -> bool {
        self.disk_change
    }

    pub(crate) fn set_motor(&mut self, on: bool) {
        self.motor = on;
    }

    /// `head` as the WD1770 sees it.
    pub(crate) fn set_head(&mut self, head: u8) {
        self.head = head & 1;
    }

    pub(crate) fn cylinder(&self) -> u8 {
        self.cylinder
    }

    pub(crate) fn disk_bytes(&self) -> Option<Vec<u8>> {
        self.disk.as_ref().map(d81::D81::to_bytes)
    }

    pub(crate) fn modified(&self) -> bool {
        self.modified
    }

    /// Writes whatever the controller laid on the current track back into
    /// the image.
    pub(crate) fn commit(&mut self) {
        self.flush();
    }

    pub(crate) fn mark_saved(&mut self) {
        self.modified = false;
    }

    pub(crate) fn dump_state(&self) -> Floppy81State {
        Floppy81State {
            motor: self.motor,
            cylinder: self.cylinder,
            head: self.head,
            pos: self.pos,
            index_count: self.index_count,
            disk_change: self.disk_change,
        }
    }

    pub(crate) fn restore_state(&mut self, state: Floppy81State) {
        self.flush();
        self.motor = state.motor;
        self.cylinder = state.cylinder;
        self.head = state.head;
        self.pos = state.pos % TRACK_BYTES;
        self.index_count = state.index_count;
        self.disk_change = state.disk_change;
        self.built_for = None;
    }

    /// First DOS (256-byte) sector of the cylinder half the data of this
    /// head comes from, as a linear index.
    fn first_linear_sector(cylinder: u8, head: u8) -> usize {
        (usize::from(cylinder) * 2 + usize::from(head ^ 1)) * usize::from(SECTORS_PER_SIDE) * 2
    }

    fn put(raw: &mut [u8], sync: &mut [bool], p: &mut usize, byte: u8, is_sync: bool) {
        raw[*p] = byte;
        sync[*p] = is_sync;
        *p = (*p + 1) % raw.len();
    }

    /// Builds the raw track for `(cylinder, head)` from the image.
    fn build(&mut self) {
        let (cylinder, head) = (self.cylinder, self.head);
        self.built_for = Some((cylinder, head));
        self.raw.fill(0x4E);
        self.sync.fill(false);
        let Some(disk) = &self.disk else {
            return;
        };
        if cylinder >= CYLINDERS {
            return;
        }
        let mut p = 32usize;
        let base = Self::first_linear_sector(cylinder, head);
        for s in 0..SECTORS_PER_SIDE {
            for _ in 0..12 {
                Self::put(&mut self.raw, &mut self.sync, &mut p, 0x00, false);
            }
            for _ in 0..3 {
                Self::put(&mut self.raw, &mut self.sync, &mut p, 0xA1, true);
            }
            Self::put(&mut self.raw, &mut self.sync, &mut p, 0xFE, false);
            let id = [cylinder, head ^ 1, s + 1, SECTOR_CODE];
            let mut crc = CRC_AFTER_IDAM;
            for byte in id {
                Self::put(&mut self.raw, &mut self.sync, &mut p, byte, false);
                crc = crc_byte(crc, byte);
            }
            Self::put(
                &mut self.raw,
                &mut self.sync,
                &mut p,
                (crc >> 8) as u8,
                false,
            );
            Self::put(&mut self.raw, &mut self.sync, &mut p, crc as u8, false);
            for _ in 0..22 {
                Self::put(&mut self.raw, &mut self.sync, &mut p, 0x4E, false);
            }
            for _ in 0..12 {
                Self::put(&mut self.raw, &mut self.sync, &mut p, 0x00, false);
            }
            for _ in 0..3 {
                Self::put(&mut self.raw, &mut self.sync, &mut p, 0xA1, true);
            }
            Self::put(&mut self.raw, &mut self.sync, &mut p, 0xFB, false);
            let mut crc = CRC_AFTER_DAM;
            for half in 0..2usize {
                let linear = base + 2 * usize::from(s) + half;
                let (track, sector) = ((linear / 40 + 1) as u8, (linear % 40) as u8);
                let data = disk.read_sector(track, sector).unwrap_or(&[0u8; 256]);
                for &byte in data {
                    Self::put(&mut self.raw, &mut self.sync, &mut p, byte, false);
                    crc = crc_byte(crc, byte);
                }
            }
            Self::put(
                &mut self.raw,
                &mut self.sync,
                &mut p,
                (crc >> 8) as u8,
                false,
            );
            Self::put(&mut self.raw, &mut self.sync, &mut p, crc as u8, false);
            for _ in 0..35 {
                Self::put(&mut self.raw, &mut self.sync, &mut p, 0x4E, false);
            }
        }
    }

    fn update(&mut self) {
        if self.built_for != Some((self.cylinder, self.head)) {
            self.flush();
            self.build();
        }
    }

    /// Decodes what the controller wrote back into the image.
    fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some((cylinder, head)) = self.built_for else {
            return;
        };
        if cylinder >= CYLINDERS || self.disk.is_none() {
            return;
        }
        let n = self.raw.len();
        let at = |i: usize| (self.raw[i % n], self.sync[i % n]);
        let mut found: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut i = 0;
        while i < n {
            // Three sync bytes then an ID mark.
            if !(at(i).1 && at(i).0 == 0xA1) {
                i += 1;
                continue;
            }
            let mut j = i;
            while at(j).1 && at(j).0 == 0xA1 {
                j += 1;
            }
            if j - i < 2 || at(j).0 != 0xFE {
                i = j.max(i + 1);
                continue;
            }
            let id: Vec<u8> = (1..=6).map(|k| at(j + k).0).collect();
            let mut crc = CRC_AFTER_IDAM;
            for &byte in &id {
                crc = crc_byte(crc, byte);
            }
            let (c, h, r, size) = (id[0], id[1], id[2], id[3]);
            i = j + 7;
            if crc != 0
                || c != cylinder
                || h != (head ^ 1)
                || size != SECTOR_CODE
                || !(1..=SECTORS_PER_SIDE).contains(&r)
            {
                continue;
            }
            // The data field follows within the next few dozen bytes.
            let mut k = i;
            let limit = i + 60;
            let mut data_at = None;
            while k < limit {
                if at(k).1 && at(k).0 == 0xA1 {
                    let mut m = k;
                    while at(m).1 && at(m).0 == 0xA1 {
                        m += 1;
                    }
                    if m - k >= 2 && matches!(at(m).0, 0xFB | 0xF8) {
                        data_at = Some(m + 1);
                    }
                    break;
                }
                k += 1;
            }
            let Some(start) = data_at else {
                continue;
            };
            let data: Vec<u8> = (0..512).map(|k| at(start + k).0).collect();
            let mut crc = CRC_AFTER_DAM;
            for &byte in &data {
                crc = crc_byte(crc, byte);
            }
            let stored = (u16::from(at(start + 512).0) << 8) | u16::from(at(start + 513).0);
            if crc != stored {
                continue;
            }
            found.push((r, data));
            i = start + 514;
        }
        let base = Self::first_linear_sector(cylinder, head);
        let Some(disk) = &mut self.disk else {
            return;
        };
        for (r, data) in found {
            for half in 0..2usize {
                let linear = base + 2 * usize::from(r - 1) + half;
                let (track, sector) = ((linear / 40 + 1) as u8, (linear % 40) as u8);
                let chunk: [u8; 256] = data[half * 256..half * 256 + 256]
                    .try_into()
                    .expect("256-byte slice");
                if disk
                    .read_sector(track, sector)
                    .is_ok_and(|old| old != chunk)
                    && disk.write_sector(track, sector, &chunk).is_ok()
                {
                    self.modified = true;
                }
            }
        }
    }
}

impl Floppy for Floppy81 {
    fn index(&self) -> bool {
        self.pos < INDEX_LEN
    }

    fn index_count(&self) -> u32 {
        self.index_count
    }

    fn index_count_reset(&mut self) {
        self.index_count = 0;
    }

    fn rotate(&mut self, bytes: u64) {
        if !self.motor || self.disk.is_none() {
            return;
        }
        let total = self.pos as u64 + bytes;
        self.index_count = self
            .index_count
            .saturating_add((total / TRACK_BYTES as u64).min(u64::from(u32::MAX)) as u32);
        self.pos = (total % TRACK_BYTES as u64) as usize;
    }

    fn track0(&self) -> bool {
        self.cylinder == 0
    }

    fn write_protect(&self) -> bool {
        false
    }

    fn seek_pulse(&mut self, inward: bool) {
        self.flush();
        if self.motor {
            self.cylinder = if inward {
                (self.cylinder + 1).min(MAX_CYLINDER)
            } else {
                self.cylinder.saturating_sub(1)
            };
        }
        if self.disk.is_some() {
            self.disk_change = false;
        }
    }

    fn read(&mut self) -> u16 {
        if !self.motor {
            return 0;
        }
        self.update();
        let value = u16::from(self.raw[self.pos]) | if self.sync[self.pos] { 0x100 } else { 0 };
        self.pos += 1;
        if self.pos >= TRACK_BYTES {
            self.pos = 0;
            self.index_count += 1;
        }
        value
    }

    fn write(&mut self, byte: u16) {
        if !self.motor {
            return;
        }
        self.update();
        self.raw[self.pos] = byte as u8;
        self.sync[self.pos] = byte & 0x100 != 0;
        self.dirty = true;
        self.pos += 1;
        if self.pos >= TRACK_BYTES {
            self.pos = 0;
            self.index_count += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk() -> d81::D81 {
        let mut d = d81::D81::parse(&vec![0u8; 819_200]).unwrap();
        for t in 1..=80u8 {
            for s in 0..40u8 {
                let fill = t.wrapping_mul(7).wrapping_add(s);
                d.write_sector(t, s, &[fill; 256]).unwrap();
            }
        }
        d
    }

    fn spinning() -> Floppy81 {
        let mut f = Floppy81::default();
        f.attach(disk());
        f.set_motor(true);
        f
    }

    /// Reads bytes until the three-sync + `mark` pattern passes.
    fn find_mark(f: &mut Floppy81, mark: u8) {
        let mut run = 0;
        for _ in 0..TRACK_BYTES * 2 {
            let w = f.read();
            if w == 0x1A1 {
                run += 1;
            } else if run >= 3 && w == u16::from(mark) {
                return;
            } else {
                run = 0;
            }
        }
        panic!("mark {mark:02X} not found");
    }

    fn the_first_id_field_is_cylinder_head_sector_1_with_a_good_crc() {
        let mut f = spinning();
        f.set_head(1); // CIA Port A bit 0 = 0: the image's first half
        find_mark(&mut f, 0xFE);
        let id: Vec<u16> = (0..6).map(|_| f.read()).collect();
        assert_eq!(&id[..4], &[0, 0, 1, 2], "C H R N");
        let mut crc = CRC_AFTER_IDAM;
        for w in &id {
            crc = crc_byte(crc, *w as u8);
        }
        assert_eq!(crc, 0);
    }

    fn a_data_field_carries_the_512_bytes_of_two_dos_sectors() {
        let mut f = spinning();
        f.set_head(1);
        find_mark(&mut f, 0xFB);
        let data: Vec<u8> = (0..512).map(|_| f.read() as u8).collect();
        // Cylinder 0, first half, sector R=1: DOS track 1 sectors 0 and 1.
        assert!(data[..256].iter().all(|&b| b == 7));
        assert!(data[256..].iter().all(|&b| b == 8));
    }

    fn the_other_head_serves_the_second_half_of_the_cylinder() {
        let mut f = spinning();
        f.set_head(0);
        find_mark(&mut f, 0xFB);
        let data: Vec<u8> = (0..512).map(|_| f.read() as u8).collect();
        // Linear sector 20 -> DOS track 1 sector 20.
        assert!(data[..256].iter().all(|&b| b == 7 + 20));
    }
    /// Merged (every check kept): `the_first_id_field_is_cylinder_head_sector_1_with_a_good_crc`, `a_data_field_carries_the_512_bytes_of_two_dos_sectors`, `the_other_head_serves_the_second_half_of_the_cylinder`.
    #[test]
    fn id_and_data_fields() {
        the_first_id_field_is_cylinder_head_sector_1_with_a_good_crc();
        a_data_field_carries_the_512_bytes_of_two_dos_sectors();
        the_other_head_serves_the_second_half_of_the_cylinder();
    }

    #[test]
    fn writing_a_data_field_and_leaving_the_track_stores_it_in_the_image() {
        let mut f = spinning();
        f.set_head(1);
        find_mark(&mut f, 0xFB);
        let start = f.pos;
        let payload: Vec<u8> = (0..512).map(|i| (i % 253) as u8).collect();
        let mut crc = CRC_AFTER_DAM;
        for &b in &payload {
            f.write(u16::from(b));
            crc = crc_byte(crc, b);
        }
        f.write(u16::from((crc >> 8) as u8));
        f.write(u16::from(crc as u8));
        assert!(f.pos > start);
        f.seek_pulse(true); // leaves the track: the write is flushed
        let disk = f.disk.as_ref().unwrap();
        assert_eq!(disk.read_sector(1, 0).unwrap(), &payload[..256]);
        assert_eq!(disk.read_sector(1, 1).unwrap(), &payload[256..]);
        assert!(f.modified());
    }

    fn the_disk_change_line_clears_on_the_first_step_and_returns_on_eject() {
        let mut f = Floppy81::default();
        assert!(f.disk_change());
        f.attach(disk());
        f.set_motor(true);
        assert!(f.disk_change());
        f.seek_pulse(true);
        assert!(!f.disk_change());
        f.detach();
        assert!(f.disk_change());
    }

    fn a_stopped_motor_reads_nothing_and_does_not_turn() {
        let mut f = Floppy81::default();
        f.attach(disk());
        f.rotate(100);
        assert_eq!(f.pos, 0);
        assert_eq!(f.read(), 0);
    }
    /// Merged (every check kept): `the_disk_change_line_clears_on_the_first_step_and_returns_on_eject`, `a_stopped_motor_reads_nothing_and_does_not_turn`.
    #[test]
    fn disk_change_and_motor() {
        the_disk_change_line_clears_on_the_first_step_and_returns_on_eject();
        a_stopped_motor_reads_nothing_and_does_not_turn();
    }
}

#[cfg(test)]
mod controller_tests {
    use super::*;
    use wd1770::Wd1770;

    fn controller() -> Wd1770<Floppy81> {
        let mut disk = d81::D81::parse(&vec![0u8; 819_200]).unwrap();
        for t in 1..=80u8 {
            for s in 0..40u8 {
                disk.write_sector(t, s, &[t.wrapping_mul(7).wrapping_add(s); 256])
                    .unwrap();
            }
        }
        let mut wd = Wd1770::new(Floppy81::default(), 2, false);
        wd.floppy.attach(disk);
        wd.floppy.set_motor(true);
        wd.floppy.set_head(1);
        wd
    }

    /// Ticks until BUSY drops (or `limit` cycles pass), collecting every byte
    /// the CPU would have taken from the data register on DRQ.
    fn run(wd: &mut Wd1770<Floppy81>, limit: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        for _ in 0..limit {
            wd.tick(2);
            if wd.drq() {
                bytes.push(wd.read(3));
            }
            if wd.peek(0) & 1 == 0 && wd.intrq() {
                break;
            }
        }
        bytes
    }

    fn restore_finds_track_zero_and_the_status_reports_it() {
        let mut wd = controller();
        wd.floppy.seek_pulse(true);
        wd.floppy.seek_pulse(true);
        wd.write(0, 0x08); // restore, head loaded
        run(&mut wd, 3_000_000);
        assert_eq!(wd.peek(1), 0, "track register");
        assert_eq!(wd.floppy.cylinder(), 0);
        assert_ne!(wd.peek(0) & 0x04, 0, "TR00 in the status");
    }

    fn read_address_returns_the_next_id_field() {
        let mut wd = controller();
        wd.write(0, 0xC8);
        let id = run(&mut wd, 3_000_000);
        assert_eq!(id.len(), 6, "{id:?}");
        assert_eq!(&id[..4], &[0, 0, 1, 2], "C H R N");
        assert_eq!(wd.peek(0) & 0x18, 0, "no RNF, no CRC error");
    }

    fn read_sector_streams_512_bytes() {
        let mut wd = controller();
        wd.write(1, 0); // track register
        wd.write(2, 3); // sector 3 -> DOS sectors 4 and 5 on cylinder 0
        wd.write(0, 0x88);
        let data = run(&mut wd, 3_000_000);
        assert_eq!(data.len(), 512);
        assert!(data[..256].iter().all(|&b| b == 7 + 4));
        assert!(data[256..].iter().all(|&b| b == 7 + 5));
        assert_eq!(wd.peek(0) & 0x1C, 0, "status clean: {:02x}", wd.peek(0));
    }

    fn a_missing_sector_gives_record_not_found() {
        let mut wd = controller();
        wd.write(2, 99);
        wd.write(0, 0x88);
        run(&mut wd, 30_000_000);
        assert_ne!(wd.peek(0) & 0x10, 0, "RNF");
    }
    /// Merged (every check kept): `read_address_returns_the_next_id_field`, `read_sector_streams_512_bytes`, `a_missing_sector_gives_record_not_found`.
    #[test]
    fn read_commands() {
        read_address_returns_the_next_id_field();
        read_sector_streams_512_bytes();
        a_missing_sector_gives_record_not_found();
    }

    fn seek_moves_the_head_and_verifies_the_track() {
        let mut wd = controller();
        wd.write(1, 0);
        wd.write(3, 5); // seek target
        wd.write(0, 0x1C); // seek with verify
        run(&mut wd, 30_000_000);
        assert_eq!(wd.floppy.cylinder(), 5);
        assert_eq!(wd.peek(1), 5);
        assert_eq!(wd.peek(0) & 0x18, 0);
    }
    /// Merged (every check kept): `restore_finds_track_zero_and_the_status_reports_it`, `seek_moves_the_head_and_verifies_the_track`.
    #[test]
    fn restore_and_seek() {
        restore_finds_track_zero_and_the_status_reports_it();
        seek_moves_the_head_and_verifies_the_track();
    }

    #[test]
    fn write_sector_stores_the_bytes_in_the_image() {
        let mut wd = controller();
        wd.write(1, 0);
        wd.write(2, 2);
        wd.write(0, 0xA8);
        let mut sent = 0usize;
        for _ in 0..3_000_000u32 {
            wd.tick(2);
            if wd.drq() && sent < 512 {
                wd.write(3, (sent % 251) as u8);
                sent += 1;
            }
            if wd.peek(0) & 1 == 0 && wd.intrq() {
                break;
            }
        }
        assert_eq!(sent, 512);
        wd.floppy.commit();
        assert!(wd.floppy.modified());
        let disk = wd.floppy.disk_bytes().unwrap();
        // Sector R=2 -> DOS sectors 2,3 of track 1.
        let at = (2 * 256) as usize;
        let expect: Vec<u8> = (0..512).map(|i| (i % 251) as u8).collect();
        assert_eq!(&disk[at..at + 512], &expect[..]);
    }
}
