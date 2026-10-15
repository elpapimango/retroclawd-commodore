//! RR-Net (and the TFE/64NIC family) ethernet cartridge: a CS8900A behind the
//! `$DE00`/`$DF00` I/O window. In RR-Net mode ports 0-1 are not decoded and the
//! rest are wired with address bit 3 inverted (VICE's `ethernetcart.c`).
//!
//! There is no real network behind it. With `loopback` on, every frame the
//! program transmits is received back, which is enough to run software that
//! detects the card and exercises its driver; without it frames are dropped.

use cs8900a::Cs8900a;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RrNet {
    pub chip: Cs8900a,
    pub loopback: bool,
    /// `0xDE` or `0xDF`.
    pub page: u8,
    /// Frames the program transmitted (kept for the host/tests to inspect).
    pub sent: u64,
}

impl RrNet {
    pub fn new(page: u8, loopback: bool) -> Self {
        RrNet {
            chip: Cs8900a::new(),
            loopback,
            page,
            sent: 0,
        }
    }

    fn port(offset: u8) -> Option<u8> {
        (offset >= 2).then_some((offset & 0x0F) ^ 8)
    }

    pub fn read(&mut self, offset: u8) -> u8 {
        Self::port(offset).map_or(0, |p| self.chip.read(p))
    }

    pub fn peek(&self, offset: u8) -> u8 {
        Self::port(offset).map_or(0, |p| self.chip.peek(p))
    }

    pub fn write(&mut self, offset: u8, value: u8) {
        if let Some(p) = Self::port(offset) {
            self.chip.write(p, value);
        }
    }

    /// Moves transmitted frames to the receive side when looping back.
    pub fn pump(&mut self) {
        while let Some(frame) = self.chip.take_tx_frame() {
            self.sent += 1;
            if self.loopback {
                self.chip.push_rx_frame(frame);
            }
        }
    }
}
