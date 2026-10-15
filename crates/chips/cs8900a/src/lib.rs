//! Crystal CS8900A ethernet controller in I/O-space mode, as used by the
//! C64 "ethernet cartridges" (TFE, RR-Net, 64NIC).
//!
//! Host-agnostic: frames the program transmits are queued for the host
//! ([`Cs8900a::take_tx_frame`]); frames to receive are fed in with
//! [`Cs8900a::push_rx_frame`]. There is no network backend here -- the
//! machine crate offers a loopback (a transmitted frame is received back) and
//! that is all this project wires up.
//!
//! The 16 I/O ports and the 4 KB PacketPage memory follow the datasheet as
//! VICE's `cs8900.c` models them (read as a reference, see `CREDITS.md`):
//! RxTxData at +0, TxCMD +4, TxLength +6, ISQ +8, PacketPage pointer +A,
//! PacketPage data +C. Receive filtering supports individual, broadcast,
//! multicast-by-hash, hash and promiscuous modes.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const PP_SIZE: usize = 0x1000;
const PP_PRODUCT_ID: usize = 0x0000;
const PP_CC_RXCFG: usize = 0x0102;
const PP_CC_RXCTL: usize = 0x0104;
const PP_CC_TXCMD: usize = 0x0108;
const PP_CC_LINECTL: usize = 0x0112;
const PP_CC_SELFCTL: usize = 0x0114;
const PP_SE_RXEVENT: usize = 0x0124;
const PP_SE_BUSST: usize = 0x0138;
const PP_TXCMD: usize = 0x0144;
const PP_TXLENGTH: usize = 0x0146;
const PP_LOG_ADDR_FILTER: usize = 0x0150;
const PP_MAC_ADDR: usize = 0x0158;
const PP_RXSTATUS: usize = 0x0400;
const PP_RXLENGTH: usize = 0x0402;
const PP_RX_FRAMELOC: usize = 0x0404;
const PP_TX_FRAMELOC: usize = 0x0A00;

const MAX_TXLENGTH: usize = 1518;
const MIN_TXLENGTH: usize = 4;
const MAX_RXLENGTH: usize = 1518;
const MIN_RXLENGTH: usize = 64;
const RX_QUEUE_LIMIT: usize = 64;

const PTR_AUTO_INCR: u16 = 0x8000;
const PTR_ADDR_MASK: u16 = 0x0FFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Tx {
    Idle,
    GotCmd,
    GotLen,
    ReadBusSt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cs8900a {
    io: [u8; 16],
    pp: Vec<u8>,
    pp_ptr: u16,
    mac: [u8; 6],
    hash_mask: [u32; 2],
    recv_control: u16,
    tx_state: Tx,
    rx_has_frame: bool,
    tx_enabled: bool,
    rx_enabled: bool,
    tx_buffer: usize,
    rx_buffer: usize,
    tx_count: usize,
    rx_count: usize,
    tx_length: usize,
    rx_length: usize,
    rxevent_read_mask: u8,
    rx_queue: VecDeque<Vec<u8>>,
    tx_frames: VecDeque<Vec<u8>>,
}

impl Default for Cs8900a {
    fn default() -> Self {
        Self::new()
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

impl Cs8900a {
    pub fn new() -> Self {
        let mut c = Cs8900a {
            io: [0; 16],
            pp: vec![0; PP_SIZE],
            pp_ptr: 0,
            mac: [0; 6],
            hash_mask: [0; 2],
            recv_control: 0,
            tx_state: Tx::Idle,
            rx_has_frame: false,
            tx_enabled: false,
            rx_enabled: false,
            tx_buffer: PP_TX_FRAMELOC,
            rx_buffer: PP_RXSTATUS,
            tx_count: 0,
            rx_count: 0,
            tx_length: 0,
            rx_length: 0,
            rxevent_read_mask: 3,
            rx_queue: VecDeque::new(),
            tx_frames: VecDeque::new(),
        };
        c.reset();
        c
    }

    fn get16(&self, at: usize) -> u16 {
        u16::from(self.pp[at]) | (u16::from(self.pp[at + 1]) << 8)
    }

    fn set16(&mut self, at: usize, v: u16) {
        self.pp[at] = v as u8;
        self.pp[at + 1] = (v >> 8) as u8;
    }

    /// Chip reset (also the SelfCTL reset bit). The MAC address is kept, as
    /// on real hardware.
    pub fn reset(&mut self) {
        self.io = [0; 16];
        self.pp.fill(0);
        // Product ID: CS8900A rev. D reads 0E630009 as two words.
        self.pp[PP_PRODUCT_ID..PP_PRODUCT_ID + 4].copy_from_slice(&0x0900_630Eu32.to_le_bytes());
        self.set16(0x20, 0x0300);
        self.set16(0x22, 0x0004);
        self.set16(0x24, 0x0003);
        for (at, v) in [
            (PP_CC_RXCFG, 0x0003u16),
            (PP_CC_RXCTL, 0x0005),
            (0x0106, 0x0007),
            (PP_CC_TXCMD, 0x0009),
            (0x010A, 0x000B),
            (PP_CC_LINECTL, 0x0013),
            (PP_CC_SELFCTL, 0x0015),
            (0x0116, 0x0017),
            (0x0118, 0x0019),
            (0x0120, 0x0000),
            (PP_SE_RXEVENT, 0x0004),
            (0x0128, 0x0008),
            (0x012C, 0x000C),
            (0x0130, 0x0010),
            (0x0132, 0x0012),
            // Link OK, 10Base-T, polarity OK, as software expects "up".
            (0x0134, 0x1294),
            // INITD set: the device is ready.
            (0x0136, 0x0896),
            (PP_SE_BUSST, 0x0018),
            (0x013C, 0x001C),
            (PP_TXCMD, 0x0009),
        ] {
            self.set16(at, v);
        }
        self.recv_control = self.get16(PP_CC_RXCTL);
        for i in 0..6 {
            self.pp[PP_MAC_ADDR + i] = self.mac[i];
        }
        self.set_transmitter(false);
        self.set_receiver(false);
        self.tx_state = Tx::Idle;
        self.rx_has_frame = false;
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// A frame arrives from the wire. Dropped when the receiver is off or the
    /// queue is full; filtering happens when the program asks for it.
    pub fn push_rx_frame(&mut self, frame: Vec<u8>) {
        if self.rx_enabled && self.rx_queue.len() < RX_QUEUE_LIMIT && frame.len() >= 6 {
            self.rx_queue.push_back(frame);
        }
    }

    /// The oldest frame the program has transmitted.
    pub fn take_tx_frame(&mut self) -> Option<Vec<u8>> {
        self.tx_frames.pop_front()
    }

    fn set_tx_status(&mut self, ready: bool, error: bool) {
        let mut s = self.get16(PP_SE_BUSST) & !0x180;
        if ready {
            s |= 0x100;
        }
        if error {
            s |= 0x080;
        }
        self.set16(PP_SE_BUSST, s);
    }

    fn set_receiver(&mut self, enabled: bool) {
        self.rx_enabled = enabled;
        self.rx_has_frame = false;
        self.rxevent_read_mask = 3;
        if !enabled {
            self.rx_queue.clear();
        }
    }

    fn set_transmitter(&mut self, enabled: bool) {
        self.tx_enabled = enabled;
        self.tx_state = Tx::Idle;
        self.set_tx_status(false, false);
    }

    /// (accepted, hashed, hash_index, correct_mac, broadcast, multicast)
    fn should_accept(&self, f: &[u8]) -> (bool, bool, u32, bool, bool, bool) {
        let rc = self.recv_control;
        let (bcast_ok, ia_ok, multi_ok, promisc, hash_ok) = (
            rc & 0x0800 != 0,
            rc & 0x0400 != 0,
            rc & 0x0200 != 0,
            rc & 0x0080 != 0,
            rc & 0x0040 != 0,
        );
        let correct = f[..6] == self.mac;
        if correct && (ia_ok || promisc) {
            return (true, false, 0, true, false, false);
        }
        if f[..6] == [0xFF; 6] {
            return (bcast_ok || promisc, false, 0, correct, true, false);
        }
        let index = (!crc32(&f[..6]) >> 26) & 0x3F;
        let bit = self.hash_mask[usize::from(index >= 32)] & (1 << (index & 0x1F)) != 0;
        if bit {
            if f[0] & 0x80 != 0 {
                return (multi_ok || promisc, false, index, correct, false, true);
            }
            return (hash_ok || promisc, true, index, correct, false, false);
        }
        (promisc, false, 0, correct, false, false)
    }

    /// Looks for an acceptable queued frame; returns the RxEvent word.
    fn receive(&mut self) -> u16 {
        let mut ret = 0x0004u16;
        while let Some(mut frame) = self.rx_queue.pop_front() {
            let (ok, hashed, index, correct, broadcast, multicast) = self.should_accept(&frame);
            if !ok {
                continue;
            }
            if frame.len() % 2 == 1 {
                frame.push(0);
            }
            let len = frame.len().min(MAX_RXLENGTH);
            ret |= 0x0100; // RxOK
            if multicast {
                ret |= 0x0200;
            }
            if hashed {
                ret |= 0x0040 | ((index as u16) << 9);
            } else {
                if correct {
                    ret |= 0x0400;
                }
                if broadcast {
                    ret |= 0x0800;
                }
            }
            if len < MIN_RXLENGTH {
                ret |= 0x2000; // runt
            }
            self.set16(PP_RXLENGTH, len as u16);
            self.pp[PP_RX_FRAMELOC..PP_RX_FRAMELOC + len].copy_from_slice(&frame[..len]);
            self.rx_buffer = PP_RXSTATUS;
            self.rx_length = len;
            self.rx_count = 0;
            self.rx_has_frame = true;
            break;
        }
        ret
    }

    fn write_tx(&mut self, value: u8, odd: bool) {
        if self.tx_state != Tx::ReadBusSt {
            self.set_tx_status(false, false);
            return;
        }
        let mut addr = self.tx_buffer;
        if odd {
            addr += 1;
            self.tx_buffer += 2;
        }
        self.tx_count += 1;
        if addr < PP_SIZE {
            self.pp[addr] = value;
        }
        if self.tx_count == self.tx_length {
            if self.tx_enabled {
                let frame = self.pp[PP_TX_FRAMELOC..PP_TX_FRAMELOC + self.tx_length].to_vec();
                self.tx_frames.push_back(frame);
            }
            self.tx_state = Tx::Idle;
            self.set_tx_status(false, false);
        }
    }

    fn read_rx(&mut self, odd: bool) -> u8 {
        if !self.rx_has_frame {
            return 0;
        }
        let mut addr = usize::from(odd);
        let value;
        if self.rx_count < 4 {
            addr += self.rx_buffer;
            value = self.pp[addr];
            self.rx_count += 1;
            if !odd {
                self.rx_buffer += 2;
            }
        } else {
            if self.rx_count >= 6 && !odd {
                self.rx_buffer += 2;
            }
            addr += self.rx_buffer;
            value = self.pp.get(addr).copied().unwrap_or(0);
            self.rx_count += 1;
        }
        if self.rx_count >= self.rx_length + 4 {
            self.rx_has_frame = false;
        }
        value
    }

    fn read_register(&self, at: usize) -> u16 {
        let value = self.get16(at);
        match at {
            0x0004..=0x001F => 0x0300,
            0x0100..=0x011F => {
                let reg = ((at - 0x100) & !1) + 1;
                if reg == 0x01 || reg == 0x11 || reg > 0x19 {
                    0x0300
                } else {
                    value
                }
            }
            0x0120..=0x013F => {
                let reg = (at - 0x120) & !1;
                if matches!(reg, 0x02 | 0x06 | 0x0A | 0x0E | 0x1A | 0x1E) {
                    0x0300
                } else {
                    value
                }
            }
            0x0140..=0x014F => {
                if at == PP_TXCMD || at == PP_TXLENGTH {
                    value
                } else {
                    0x0300
                }
            }
            0x0150..=0x015D => value,
            0x015E..=0x03FF => 0x0300,
            0x0400..=0x09FF | 0x0A00.. => 0x0000,
            _ => value,
        }
    }

    fn write_register(&mut self, at: usize, mut value: u16) {
        match at {
            0x0000..=0x00FF => {
                if at < 0x20 || (0x26..0x2C).contains(&at) || at == 0x38 || at >= 0x44 {
                    return;
                }
            }
            0x0100..=0x011F => {
                let reg = (((at - 0x100) & !1) + 1) as u16;
                value = (value & !0x3F) | reg;
                if reg == 0x01 || reg == 0x11 || reg > 0x19 {
                    return;
                }
            }
            0x0120..=0x013F => return,
            0x0140..=0x014F => {
                if at == PP_TXCMD {
                    value = (value & !0x3F) | 0x09;
                    value &= 0x33FF;
                } else if at == PP_TXLENGTH {
                    value &= 0x0FFF;
                } else if !(0x144..=0x147).contains(&at) {
                    return;
                }
            }
            0x0150..=0x015D => {}
            _ => return,
        }
        self.set16(at, value);
    }

    fn side_effects_write(&mut self, at: usize, odd: bool) {
        let content = self.get16(at);
        match at {
            PP_CC_RXCFG => {
                if content & 0x40 != 0 {
                    self.tx_state = Tx::Idle;
                    let en = self.tx_enabled;
                    self.set_transmitter(en);
                    self.set16(at, content & !0x40);
                }
            }
            PP_CC_RXCTL => self.recv_control = content,
            PP_CC_LINECTL => {
                let (tx, rx) = (content & 0x80 != 0, content & 0x40 != 0);
                if tx != self.tx_enabled || rx != self.rx_enabled {
                    self.set_transmitter(tx);
                    self.set_receiver(rx);
                }
            }
            PP_CC_SELFCTL => {
                if content & 0x40 != 0 {
                    self.reset();
                }
            }
            PP_TXCMD => {
                if odd {
                    let cmd = self.get16(PP_TXCMD);
                    self.set16(PP_CC_TXCMD, cmd);
                    self.tx_state = Tx::GotCmd;
                    self.set_tx_status(false, false);
                }
            }
            PP_TXLENGTH => {
                if odd && self.tx_state == Tx::GotCmd {
                    let len = usize::from(self.get16(PP_TXLENGTH));
                    let cmd = self.get16(PP_CC_TXCMD);
                    if len < MIN_TXLENGTH {
                        self.tx_state = Tx::Idle;
                        self.set_tx_status(true, false);
                    } else if len > MAX_TXLENGTH || (len > MAX_TXLENGTH - 4 && cmd & 0x1000 == 0) {
                        self.tx_state = Tx::Idle;
                        self.set_tx_status(false, true);
                    } else {
                        self.tx_buffer = PP_TX_FRAMELOC;
                        self.tx_count = 0;
                        self.tx_length = len;
                        self.tx_state = Tx::GotLen;
                        self.set_tx_status(true, false);
                    }
                }
            }
            PP_LOG_ADDR_FILTER..=0x0156 => {
                let pos = 8 * (at - PP_LOG_ADDR_FILTER + usize::from(odd));
                let word = usize::from(pos >= 32);
                let shift = pos % 32;
                self.hash_mask[word] &= !(0xFFu32 << shift);
                self.hash_mask[word] |= u32::from(self.pp[at + usize::from(odd)]) << shift;
            }
            PP_MAC_ADDR..=0x015C => {
                let i = at - PP_MAC_ADDR + usize::from(odd);
                self.mac[i] = self.pp[at + usize::from(odd)];
            }
            _ => {}
        }
    }

    fn side_effects_read(&mut self, at: usize, odd: bool) {
        match at {
            PP_SE_RXEVENT => {
                let mask = if odd { 1 } else { 2 };
                if mask & self.rxevent_read_mask != 0 {
                    if self.rx_enabled {
                        let v = self.receive();
                        self.set16(PP_RXSTATUS, v);
                        self.set16(PP_SE_RXEVENT, v);
                    }
                    self.rxevent_read_mask = mask;
                } else {
                    self.rxevent_read_mask |= mask;
                }
            }
            PP_SE_BUSST
                if odd && self.tx_state == Tx::GotLen && self.get16(PP_SE_BUSST) & 0x100 != 0 =>
            {
                self.tx_state = Tx::ReadBusSt;
            }
            _ => {}
        }
    }

    fn auto_incr(&mut self) {
        if self.pp_ptr & PTR_AUTO_INCR != 0 {
            let ptr = (self.pp_ptr & PTR_ADDR_MASK).wrapping_add(1) & PTR_ADDR_MASK;
            self.pp_ptr = ptr | (self.pp_ptr & 0xF000);
        }
    }

    fn pp_address_for(&mut self, base: u8, advance: bool) -> usize {
        match base {
            0x0C | 0x0E => {
                let at = usize::from(self.pp_ptr & PTR_ADDR_MASK) & !1;
                if advance {
                    self.auto_incr();
                }
                at
            }
            0x08 => 0x0120,
            0x04 => PP_TXCMD,
            _ => PP_TXLENGTH,
        }
    }

    /// I/O read at `port` (0-15) with side effects.
    pub fn read(&mut self, port: u8) -> u8 {
        let port = port & 0x0F;
        let base = port & !1;
        if base == 0x00 || base == 0x02 {
            return self.read_rx(port & 1 != 0);
        }
        let word = if base == 0x0A {
            self.pp_ptr
        } else {
            let at = self.pp_address_for(base, true);
            self.side_effects_read(at, port & 1 != 0);
            self.read_register(at)
        };
        self.io[usize::from(base)] = word as u8;
        self.io[usize::from(base) + 1] = (word >> 8) as u8;
        if port & 1 == 0 {
            word as u8
        } else {
            (word >> 8) as u8
        }
    }

    /// Same value without side effects (the receive data port reads 0).
    pub fn peek(&self, port: u8) -> u8 {
        let port = port & 0x0F;
        let base = port & !1;
        if base == 0x00 || base == 0x02 {
            return 0;
        }
        let word = match base {
            0x0A => self.pp_ptr,
            0x0C | 0x0E => self.read_register(usize::from(self.pp_ptr & PTR_ADDR_MASK) & !1),
            0x08 => self.read_register(0x0120),
            0x04 => self.read_register(PP_TXCMD),
            _ => self.read_register(PP_TXLENGTH),
        };
        if port & 1 == 0 {
            word as u8
        } else {
            (word >> 8) as u8
        }
    }

    /// I/O write at `port` (0-15).
    pub fn write(&mut self, port: u8, byte: u8) {
        let port = port & 0x0F;
        let base = port & !1;
        let odd = port & 1 != 0;
        if base == 0x00 || base == 0x02 {
            self.write_tx(byte, odd);
            return;
        }
        let word = if odd {
            u16::from(self.io[usize::from(base)]) | (u16::from(byte) << 8)
        } else {
            u16::from(byte) | (u16::from(self.io[usize::from(base) + 1]) << 8)
        };
        let word = if base == 0x0A {
            self.pp_ptr = word | 0x3000;
            self.pp_ptr
        } else {
            let at = self.pp_address_for(base, true);
            let at = if base == 0x0C || base == 0x0E {
                at & (PP_SIZE - 1)
            } else {
                at
            };
            self.write_register(at, word);
            self.side_effects_write(at, odd);
            self.get16(at)
        };
        self.io[usize::from(base)] = word as u8;
        self.io[usize::from(base) + 1] = (word >> 8) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write16(c: &mut Cs8900a, port: u8, v: u16) {
        c.write(port, v as u8);
        c.write(port + 1, (v >> 8) as u8);
    }

    fn read16(c: &mut Cs8900a, port: u8) -> u16 {
        u16::from(c.read(port)) | (u16::from(c.read(port + 1)) << 8)
    }

    fn pp_write(c: &mut Cs8900a, addr: u16, v: u16) {
        write16(c, 0x0A, addr);
        write16(c, 0x0C, v);
    }

    fn pp_read(c: &mut Cs8900a, addr: u16) -> u16 {
        write16(c, 0x0A, addr);
        read16(c, 0x0C)
    }

    const MAC: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];

    fn ready() -> Cs8900a {
        let mut c = Cs8900a::new();
        for (i, pair) in MAC.chunks(2).enumerate() {
            pp_write(
                &mut c,
                0x158 + 2 * i as u16,
                u16::from(pair[0]) | (u16::from(pair[1]) << 8),
            );
        }
        pp_write(&mut c, 0x112, 0x00C3); // LineCTL: tx + rx on
        pp_write(&mut c, 0x104, 0x0D05); // RxCTL: broadcast + IA + correct
        c
    }

    fn frame(dest: [u8; 6], len: usize) -> Vec<u8> {
        let mut f = dest.to_vec();
        f.extend((6..len).map(|i| i as u8));
        f
    }

    fn transmit(c: &mut Cs8900a, f: &[u8]) {
        write16(c, 0x04, 0x00C9); // TxCMD
        write16(c, 0x06, f.len() as u16);
        // Rdy4TxNOW must be read from BusST before the data is accepted.
        assert_ne!(pp_read(c, 0x138) & 0x100, 0, "ready to transmit");
        for pair in f.chunks(2) {
            c.write(0x00, pair[0]);
            c.write(0x01, *pair.get(1).unwrap_or(&0));
        }
    }

    #[test]
    fn product_id_and_default_registers_read_back() {
        let mut c = Cs8900a::new();
        assert_eq!(pp_read(&mut c, 0), 0x630E);
        assert_eq!(pp_read(&mut c, 2), 0x0900);
        assert_eq!(pp_read(&mut c, 0x134) & 0x80, 0x80, "link up");
        assert_eq!(pp_read(&mut c, 0x136) & 0x80, 0x80, "INITD");
        assert_eq!(pp_read(&mut c, 0x300), 0x0300, "reserved range");
    }

    #[test]
    fn mac_address_and_auto_increment_pointer() {
        let mut c = ready();
        assert_eq!(c.mac(), MAC);
        write16(&mut c, 0x0A, 0x8158 | 0x3000); // auto-increment from the MAC words
        assert_eq!(read16(&mut c, 0x0C), 0x1102);
        assert_eq!(read16(&mut c, 0x0C), 0x3322);
        // The pointer advances once per byte access.
        assert_eq!(c.pp_ptr & PTR_ADDR_MASK, 0x15C);
    }

    #[test]
    fn a_transmitted_frame_is_queued_for_the_host() {
        let mut c = ready();
        let f = frame(MAC, 60);
        transmit(&mut c, &f);
        assert_eq!(c.take_tx_frame(), Some(f));
        assert_eq!(c.take_tx_frame(), None);
    }

    #[test]
    fn too_short_or_too_long_lengths_are_refused() {
        let mut c = ready();
        write16(&mut c, 0x04, 0x00C9);
        write16(&mut c, 0x06, 2);
        assert_eq!(
            pp_read(&mut c, 0x138) & 0x180,
            0x100,
            "short: still ready, nothing taken"
        );
        write16(&mut c, 0x04, 0x00C9);
        write16(&mut c, 0x06, 1600);
        assert_eq!(pp_read(&mut c, 0x138) & 0x180, 0x080, "TxBidErr");
    }

    #[test]
    fn a_received_frame_shows_in_rxevent_and_reads_out_status_length_data() {
        let mut c = ready();
        let f = frame(MAC, 64);
        c.push_rx_frame(f.clone());
        let event = pp_read(&mut c, 0x124);
        assert_eq!(event & 0x0104, 0x0104, "RxOK + marker bit");
        assert_ne!(event & 0x0400, 0, "individual address match");
        // RxStatus and RxLength are read high byte first, then the frame
        // low byte first.
        let hl = |c: &mut Cs8900a| {
            let hi = c.read(0x01);
            let lo = c.read(0x00);
            u16::from(lo) | (u16::from(hi) << 8)
        };
        let status = hl(&mut c);
        let length = hl(&mut c);
        assert_eq!(status, event);
        assert_eq!(length, 64);
        let mut got = Vec::new();
        for _ in 0..32 {
            let w = read16(&mut c, 0x00);
            got.push(w as u8);
            got.push((w >> 8) as u8);
        }
        assert_eq!(got, f);
        assert_eq!(pp_read(&mut c, 0x124) & 0x0100, 0, "no further frame");
    }

    #[test]
    fn the_receive_filter_drops_foreign_unicast_and_accepts_broadcast() {
        let mut c = ready();
        c.push_rx_frame(frame([2, 9, 9, 9, 9, 9], 64));
        assert_eq!(pp_read(&mut c, 0x124) & 0x0100, 0);
        c.push_rx_frame(frame([0xFF; 6], 64));
        let e = pp_read(&mut c, 0x124);
        assert_ne!(e & 0x0800, 0, "broadcast flag");
        assert_ne!(e & 0x0100, 0);
    }

    #[test]
    fn promiscuous_mode_takes_everything_and_a_disabled_receiver_nothing() {
        let mut c = ready();
        pp_write(&mut c, 0x104, 0x0085);
        c.push_rx_frame(frame([2, 9, 9, 9, 9, 9], 64));
        assert_ne!(pp_read(&mut c, 0x124) & 0x0100, 0);
        pp_write(&mut c, 0x112, 0x0000);
        c.push_rx_frame(frame(MAC, 64));
        assert_eq!(c.rx_queue.len(), 0);
    }

    #[test]
    fn the_self_control_reset_bit_resets_but_keeps_the_mac() {
        let mut c = ready();
        pp_write(&mut c, 0x114, 0x0055);
        assert_eq!(c.mac(), MAC);
        assert_eq!(pp_read(&mut c, 0x112) & 0xC0, 0, "line control back to off");
    }
}
