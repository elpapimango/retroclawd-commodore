//! GeoRAM (Berkeley Softworks / BBG): a banked RAM cartridge. A 256-byte
//! window at `$DE00-$DEFF` shows one page of the RAM; `$DFFF` selects the
//! 16K block and `$DFFE` the 256-byte page inside it (0-63). Out-of-range
//! values wrap modulo the block count / 64, as VICE's `georam.c` does.

use serde::{Deserialize, Serialize};

const BLOCK_SIZE: usize = 16 * 1024;
const PAGE_SIZE: usize = 256;

#[derive(Debug, Clone)]
pub struct GeoRam {
    ram: Vec<u8>,
    page: u8,
    block: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoRamState {
    ram: Vec<u8>,
    page: u8,
    block: u8,
}

impl GeoRam {
    /// 512, 1024, 2048 or 4096 KB (the sizes real GeoRAMs came in).
    pub fn new(kilobytes: usize) -> Result<Self, String> {
        if ![512, 1024, 2048, 4096].contains(&kilobytes) {
            return Err(format!(
                "GeoRAM size must be 512, 1024, 2048 or 4096 KB, not {kilobytes}"
            ));
        }
        Ok(Self {
            ram: vec![0; kilobytes * 1024],
            page: 0,
            block: 0,
        })
    }

    pub fn kilobytes(&self) -> usize {
        self.ram.len() / 1024
    }

    fn blocks(&self) -> usize {
        self.ram.len() / BLOCK_SIZE
    }

    fn offset(&self, addr: u16) -> usize {
        usize::from(self.block) * BLOCK_SIZE
            + usize::from(self.page) * PAGE_SIZE
            + usize::from(addr & 0xFF)
    }

    /// `$DE00-$DEFF` read.
    pub fn read_window(&self, addr: u16) -> u8 {
        self.ram[self.offset(addr)]
    }

    /// `$DE00-$DEFF` write.
    pub fn write_window(&mut self, addr: u16, value: u8) {
        let at = self.offset(addr);
        self.ram[at] = value;
    }

    /// `$DF00-$DFFF` write: only odd/even addresses `$DFFE`/`$DFFF` mean
    /// anything on the real card; like VICE, any odd address selects the
    /// block and any even one the page.
    pub fn write_register(&mut self, addr: u16, value: u8) {
        if addr & 1 == 1 {
            self.block = (usize::from(value) % self.blocks()) as u8;
        } else {
            self.page = value % 64;
        }
    }

    /// `$DF00-$DFFF` peek: the registers read back only at `$DFFE/$DFFF`.
    pub fn read_register(&self, addr: u16) -> u8 {
        match addr & 0xFF {
            0xFE => self.page,
            0xFF => self.block,
            _ => 0xFF,
        }
    }

    pub fn state(&self) -> GeoRamState {
        GeoRamState {
            ram: self.ram.clone(),
            page: self.page,
            block: self.block,
        }
    }

    pub fn restore_state(&mut self, state: GeoRamState) {
        if state.ram.len() == self.ram.len() {
            self.ram = state.ram;
            self.page = state.page;
            self.block = state.block;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_follows_block_and_page_registers() {
        let mut g = GeoRam::new(512).unwrap();
        g.write_register(0xDFFF, 3);
        g.write_register(0xDFFE, 5);
        g.write_window(0xDE10, 0xAB);
        assert_eq!(g.ram[3 * 16384 + 5 * 256 + 0x10], 0xAB);
        assert_eq!(g.read_window(0xDE10), 0xAB);
        g.write_register(0xDFFE, 6);
        assert_eq!(g.read_window(0xDE10), 0);
        g.write_register(0xDFFE, 5);
        assert_eq!(g.read_window(0xDE10), 0xAB);
    }

    #[test]
    fn out_of_range_values_wrap() {
        let mut g = GeoRam::new(512).unwrap(); // 32 blocks
        g.write_register(0xDFFF, 33);
        g.write_register(0xDFFE, 65);
        assert_eq!(g.read_register(0xDFFF), 1);
        assert_eq!(g.read_register(0xDFFE), 1);
    }

    #[test]
    fn only_real_sizes_are_accepted_and_state_round_trips() {
        assert!(GeoRam::new(256).is_err());
        let mut g = GeoRam::new(1024).unwrap();
        g.write_register(0xDFFF, 40);
        g.write_window(0xDE00, 7);
        let saved = g.state();
        let mut h = GeoRam::new(1024).unwrap();
        h.restore_state(saved);
        assert_eq!(h.read_window(0xDE00), 7);
        assert_eq!(h.read_register(0xDFFF), 40);
    }
}
