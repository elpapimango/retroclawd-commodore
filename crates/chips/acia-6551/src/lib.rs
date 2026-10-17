//! MOS/Rockwell 6551 ACIA (asynchronous communications interface adapter)
//! as wired on the C64 SwiftLink and Turbo232 cartridges.
//!
//! Host-agnostic like `ay-3-8910`/`sid`: no machine, CPU or network
//! dependency. The chip exchanges bytes with the outside world through two
//! queues -- [`Acia::take_tx`] drains what the program transmitted,
//! [`Acia::push_rx`] feeds received bytes -- and the host (a TCP socket, a
//! test) moves them. Character timing follows the programmed baud rate,
//! frame length and the host clock.
//!
//! Behaviour follows VICE's `aciacore.c` (read as a reference, see
//! `CREDITS.md`): register map, status bits, the programmed reset on a
//! status-register write, transmit/receive interrupts, SwiftLink doubling the
//! baud table and the Turbo232 extension register at offset 7.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Which board the chip sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// A bare 6551: baud table as programmed.
    Normal,
    /// SwiftLink: crystal runs the table twice as fast.
    SwiftLink,
    /// Turbo232: like SwiftLink plus the extended control register at +7
    /// giving 230400/115200/57600/28800 bps.
    Turbo232,
}

pub const SR_PARITY_ERROR: u8 = 0x01;
pub const SR_FRAMING_ERROR: u8 = 0x02;
pub const SR_OVERRUN: u8 = 0x04;
pub const SR_RECEIVE_FULL: u8 = 0x08;
pub const SR_TRANSMIT_EMPTY: u8 = 0x10;
/// Low = carrier present.
pub const SR_DCD: u8 = 0x20;
/// Low = data set ready.
pub const SR_DSR: u8 = 0x40;
pub const SR_IRQ: u8 = 0x80;

const CMD_DTR_ENABLE: u8 = 0x01;
const CMD_RX_IRQ_DISABLE: u8 = 0x02;
const CMD_TX_MASK: u8 = 0x0C;
const CMD_TX_WITH_IRQ: u8 = 0x04;
const CMD_PARITY_ENABLED: u8 = 0x20;
const CMD_PARITY_KEEP: u8 = 0xE0;

const CTRL_BAUD_MASK: u8 = 0x0F;
const CTRL_BAUD_EXTERNAL: u8 = 0x00;
const CTRL_WORD_MASK: u8 = 0x60;
const CTRL_2_STOP: u8 = 0x80;

const ECTRL_ACTIVE: u8 = 0x80;
const ECTRL_BAUD_MASK: u8 = 0x03;

const BAUD_TABLE: [f64; 16] = [
    10.0, 50.0, 75.0, 109.92, 134.58, 150.0, 300.0, 600.0, 1200.0, 1800.0, 2400.0, 3600.0, 4800.0,
    7200.0, 9600.0, 19200.0,
];
const TURBO_BAUD_TABLE: [f64; 4] = [230400.0, 115200.0, 57600.0, 28800.0];

/// Queued received bytes beyond this are dropped (a runaway sender must not
/// grow memory without bound).
const RX_QUEUE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acia {
    mode: Mode,
    txdata: u8,
    rxdata: u8,
    status: u8,
    cmd: u8,
    ctrl: u8,
    ectrl: u8,
    /// A data-register write is waiting for its character time to pass.
    tx_pending: bool,
    irq: bool,
    /// Cycles until the transmitter / receiver finishes the current character.
    tx_countdown: u32,
    rx_countdown: u32,
    /// Cycles one character takes at the programmed rate.
    char_cycles: u32,
    clock_hz: u32,
    datamask: u8,
    carrier: bool,
    tx_out: Vec<u8>,
    rx_in: VecDeque<u8>,
}

impl Acia {
    pub fn new(mode: Mode, clock_hz: u32) -> Self {
        let mut a = Acia {
            mode,
            txdata: 0,
            rxdata: 0,
            status: 0,
            cmd: 0,
            ctrl: 0,
            ectrl: 0,
            tx_pending: false,
            irq: false,
            tx_countdown: 0,
            rx_countdown: 0,
            char_cycles: 1,
            clock_hz,
            datamask: 0xFF,
            carrier: false,
            tx_out: Vec::new(),
            rx_in: VecDeque::new(),
        };
        a.reset();
        a
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Hardware reset: registers back to power-on, queues kept (a reset line
    /// does not flush a socket).
    pub fn reset(&mut self) {
        self.cmd = 0;
        self.ctrl = 0;
        self.ectrl = 0;
        self.status = SR_TRANSMIT_EMPTY;
        self.irq = false;
        self.tx_pending = false;
        self.tx_countdown = 0;
        self.rx_countdown = 0;
        self.update_timing();
    }

    /// Host clock changed (PAL/NTSC).
    pub fn set_clock(&mut self, clock_hz: u32) {
        self.clock_hz = clock_hz;
        self.update_timing();
    }

    /// The modem carrier line (DCD/DSR): `true` while a peer is connected.
    pub fn set_carrier(&mut self, present: bool) {
        self.carrier = present;
    }

    /// Bytes the program has transmitted since the last call.
    pub fn take_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx_out)
    }

    /// A byte arriving from the line.
    pub fn push_rx(&mut self, byte: u8) {
        if self.rx_in.len() < RX_QUEUE_LIMIT {
            self.rx_in.push_back(byte);
        }
    }

    /// Bytes still waiting to be received by the chip.
    pub fn rx_backlog(&self) -> usize {
        self.rx_in.len()
    }

    fn baud(&self) -> f64 {
        let table = BAUD_TABLE[(self.ctrl & CTRL_BAUD_MASK) as usize];
        match self.mode {
            Mode::Normal => table,
            Mode::SwiftLink => table * 2.0,
            Mode::Turbo232 => {
                if self.ctrl & CTRL_BAUD_MASK == CTRL_BAUD_EXTERNAL {
                    TURBO_BAUD_TABLE[(self.ectrl & ECTRL_BAUD_MASK) as usize]
                } else {
                    table * 2.0
                }
            }
        }
    }

    /// The programmed bits per second.
    pub fn bps(&self) -> u32 {
        self.baud() as u32
    }

    fn update_timing(&mut self) {
        let data_bits = match self.ctrl & CTRL_WORD_MASK {
            0x00 => 8,
            0x20 => 7,
            0x40 => 6,
            _ => 5,
        };
        self.datamask = 0xFFu8 >> (8 - data_bits);
        let bits = data_bits
            + 1
            + u32::from(self.cmd & CMD_PARITY_ENABLED != 0)
            + if self.ctrl & CTRL_2_STOP != 0 { 2 } else { 1 };
        self.char_cycles = ((f64::from(self.clock_hz) / self.baud()) * f64::from(bits))
            .round()
            .max(1.0) as u32;
    }

    fn status_now(&self) -> u8 {
        let mut s = self.status & !(SR_DCD | SR_DSR);
        if !self.carrier {
            s |= SR_DCD | SR_DSR;
        }
        s
    }

    /// Register read. `offset` is the low address bits (0-3, 0-7 on the
    /// Turbo232). Reading the status clears the interrupt flag.
    pub fn read(&mut self, offset: u8) -> u8 {
        let mask = if self.mode == Mode::Turbo232 { 7 } else { 3 };
        match offset & mask {
            0 => {
                self.status &= !(SR_OVERRUN | SR_PARITY_ERROR | SR_FRAMING_ERROR | SR_RECEIVE_FULL);
                self.rxdata
            }
            1 => {
                let v = self.status_now() | if self.irq { SR_IRQ } else { 0 };
                self.irq = false;
                v
            }
            2 => self.cmd,
            3 => self.ctrl,
            7 => {
                self.ectrl
                    | if self.ctrl & CTRL_BAUD_MASK == CTRL_BAUD_EXTERNAL {
                        ECTRL_ACTIVE
                    } else {
                        0
                    }
            }
            _ => 0xFF,
        }
    }

    /// Same value without side effects.
    pub fn peek(&self, offset: u8) -> u8 {
        match offset & 3 {
            0 => self.rxdata,
            1 => self.status_now() | if self.irq { SR_IRQ } else { 0 },
            2 => self.cmd,
            _ => self.ctrl,
        }
    }

    /// Register write.
    pub fn write(&mut self, offset: u8, value: u8) {
        let mask = if self.mode == Mode::Turbo232 { 7 } else { 3 };
        match offset & mask {
            0 => {
                self.txdata = value;
                if self.cmd & CMD_DTR_ENABLE != 0 {
                    self.status &= !SR_TRANSMIT_EMPTY;
                    self.tx_pending = true;
                    if self.tx_countdown == 0 {
                        self.tx_countdown = 1;
                    }
                }
            }
            1 => {
                // Programmed reset: clears overrun, keeps only the parity
                // bits of the command register, stops the transmitter.
                self.status &= !SR_OVERRUN;
                self.cmd &= CMD_PARITY_KEEP;
                self.tx_pending = false;
                self.tx_countdown = 0;
                self.irq = false;
                self.update_timing();
            }
            2 => {
                self.cmd = value;
                if self.cmd & CMD_DTR_ENABLE != 0 && self.rx_countdown == 0 {
                    self.rx_countdown = self.char_cycles;
                }
                self.update_timing();
            }
            3 => {
                self.ctrl = value;
                self.update_timing();
            }
            7 if self.ctrl & CTRL_BAUD_MASK == CTRL_BAUD_EXTERNAL => {
                self.ectrl = value;
                self.update_timing();
            }
            _ => {}
        }
    }

    /// Advances the chip. Returns `true` when it raises an interrupt on this
    /// call (an edge: the C64 wires the line to NMI).
    pub fn tick(&mut self, cycles: u32) -> bool {
        let mut fired = false;
        if self.tx_countdown > 0 {
            if cycles >= self.tx_countdown {
                self.tx_countdown = 0;
                if self.tx_pending {
                    self.tx_pending = false;
                    self.tx_out.push(self.txdata & self.datamask);
                    self.status |= SR_TRANSMIT_EMPTY;
                    if self.cmd & CMD_TX_MASK == CMD_TX_WITH_IRQ {
                        fired |= !self.irq;
                        self.irq = true;
                    }
                }
            } else {
                self.tx_countdown -= cycles;
            }
        }
        if self.cmd & CMD_DTR_ENABLE != 0 {
            let mut left = cycles;
            while left > 0 {
                if self.rx_countdown == 0 {
                    self.rx_countdown = self.char_cycles;
                }
                if left < self.rx_countdown {
                    self.rx_countdown -= left;
                    break;
                }
                left -= self.rx_countdown;
                self.rx_countdown = self.char_cycles;
                if let Some(byte) = self.rx_in.front().copied() {
                    if self.status & SR_RECEIVE_FULL != 0 {
                        // The program has not read the last byte yet.
                        self.status |= SR_OVERRUN;
                        self.rx_in.pop_front();
                        continue;
                    }
                    self.rx_in.pop_front();
                    self.rxdata = byte & self.datamask;
                    self.status |= SR_RECEIVE_FULL;
                    if self.cmd & CMD_RX_IRQ_DISABLE == 0 {
                        fired |= !self.irq;
                        self.irq = true;
                    }
                }
            }
        } else {
            self.rx_countdown = 0;
        }
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOCK: u32 = 1_000_000;

    fn run(a: &mut Acia, cycles: u32) -> bool {
        let mut fired = false;
        for _ in 0..cycles / 100 {
            fired |= a.tick(100);
        }
        fired
    }

    fn started(mode: Mode, ctrl: u8) -> Acia {
        let mut a = Acia::new(mode, CLOCK);
        a.write(3, ctrl);
        a.write(2, CMD_DTR_ENABLE | CMD_RX_IRQ_DISABLE);
        a
    }

    fn reset_state_is_transmit_empty_with_the_line_down() {
        let mut a = Acia::new(Mode::SwiftLink, CLOCK);
        assert_eq!(a.read(1), SR_TRANSMIT_EMPTY | SR_DCD | SR_DSR);
        a.set_carrier(true);
        assert_eq!(a.read(1), SR_TRANSMIT_EMPTY);
    }

    fn a_written_byte_leaves_after_one_character_time() {
        let mut a = started(Mode::Normal, 0x1E); // 9600 bps, 8N1
        a.write(0, 0x41);
        assert_eq!(a.peek(1) & SR_TRANSMIT_EMPTY, 0, "busy while sending");
        assert!(a.take_tx().is_empty());
        run(&mut a, 2_000);
        assert_eq!(a.take_tx(), [0x41]);
        assert_ne!(a.peek(1) & SR_TRANSMIT_EMPTY, 0);
    }

    fn nothing_is_sent_while_dtr_is_off() {
        let mut a = Acia::new(Mode::Normal, CLOCK);
        a.write(0, 0x41);
        run(&mut a, 20_000);
        assert!(a.take_tx().is_empty());
    }

    #[test]
    fn swiftlink_doubles_the_baud_table_and_turbo232_has_extended_rates() {
        let mut n = Acia::new(Mode::Normal, CLOCK);
        let mut s = Acia::new(Mode::SwiftLink, CLOCK);
        n.write(3, 0x1E);
        s.write(3, 0x1E);
        assert_eq!((n.bps(), s.bps()), (9600, 19200));
        let mut t = Acia::new(Mode::Turbo232, CLOCK);
        t.write(3, 0x10); // baud code 0 = external clock -> ectrl decides
        t.write(7, 1);
        assert_eq!(t.bps(), 115200);
        assert_eq!(t.read(7), 1 | ECTRL_ACTIVE);
        t.write(3, 0x1E);
        t.write(7, 3); // ignored: not in external-clock mode
        assert_eq!(t.bps(), 19200);
    }

    fn receiving_sets_the_flag_clears_on_data_read_and_interrupts_when_enabled() {
        let mut a = Acia::new(Mode::SwiftLink, CLOCK);
        a.write(3, 0x1F);
        a.write(2, CMD_DTR_ENABLE); // receiver interrupt enabled
        a.push_rx(0x5A);
        assert!(run(&mut a, 3_000), "an interrupt edge");
        assert_eq!(
            a.peek(1) & (SR_RECEIVE_FULL | SR_IRQ),
            SR_RECEIVE_FULL | SR_IRQ
        );
        assert_eq!(a.read(0), 0x5A);
        assert_eq!(a.peek(1) & SR_RECEIVE_FULL, 0);
        a.read(1);
        assert_eq!(a.peek(1) & SR_IRQ, 0, "status read acknowledges");
    }

    fn an_unread_byte_followed_by_another_overruns() {
        let mut a = started(Mode::Normal, 0x1F);
        a.push_rx(1);
        a.push_rx(2);
        run(&mut a, 3_000);
        assert_eq!(a.peek(1) & SR_OVERRUN, SR_OVERRUN);
        assert_eq!(a.read(0), 1, "the first byte is kept");
    }

    fn word_length_masks_the_data() {
        let mut a = started(Mode::Normal, 0x3E); // 7 data bits
        a.write(0, 0xFF);
        run(&mut a, 3_000);
        assert_eq!(a.take_tx(), [0x7F]);
    }
    /// Merged (every check kept): `receiving_sets_the_flag_clears_on_data_read_and_interrupts_when_enabled`, `an_unread_byte_followed_by_another_overruns`, `word_length_masks_the_data`.
    #[test]
    fn receive_cases() {
        receiving_sets_the_flag_clears_on_data_read_and_interrupts_when_enabled();
        an_unread_byte_followed_by_another_overruns();
        word_length_masks_the_data();
    }

    fn transmit_interrupt_mode_raises_an_edge_per_character() {
        let mut a = Acia::new(Mode::SwiftLink, CLOCK);
        a.write(3, 0x1F);
        a.write(2, CMD_DTR_ENABLE | CMD_RX_IRQ_DISABLE | CMD_TX_WITH_IRQ);
        a.write(0, 0x10);
        assert!(run(&mut a, 3_000));
        assert_ne!(a.read(1) & SR_IRQ, 0);
    }
    /// Merged (every check kept): `a_written_byte_leaves_after_one_character_time`, `nothing_is_sent_while_dtr_is_off`, `transmit_interrupt_mode_raises_an_edge_per_character`.
    #[test]
    fn transmit_cases() {
        a_written_byte_leaves_after_one_character_time();
        nothing_is_sent_while_dtr_is_off();
        transmit_interrupt_mode_raises_an_edge_per_character();
    }

    fn a_status_write_is_a_programmed_reset() {
        let mut a = started(Mode::Normal, 0x1E);
        a.write(2, 0xE1);
        a.write(0, 9);
        a.write(1, 0);
        assert_eq!(a.read(2), 0xE0, "only the parity bits survive");
        run(&mut a, 5_000);
        assert!(a.take_tx().is_empty(), "the pending character was dropped");
    }
    /// Merged (every check kept): `reset_state_is_transmit_empty_with_the_line_down`, `a_status_write_is_a_programmed_reset`.
    #[test]
    fn reset_cases() {
        reset_state_is_transmit_empty_with_the_line_down();
        a_status_write_is_a_programmed_reset();
    }
}
