//! Expansion-port cartridges: the EXROM/GAME lines, ROML/ROMH windows and
//! the bank-switching schemes of the common `.crt` hardware types.
//!
//! Behaviour follows VICE's `src/c64/cart/*.c` (read as references, not
//! copied; see `CREDITS.md`). Lines are kept in VICE's *active* sense:
//! `exrom_active` / `game_active` are `true` when the cartridge pulls that
//! line low. `(true, false)` is an 8K cartridge, `(true, true)` a 16K one,
//! `(false, true)` Ultimax and `(false, false)` "cartridge off".
//!
//! ROM contents stay external to save states (like every other medium
//! here); only the small register state ([`CartState`]) is saved.

use c64_cartridge::{ChipKind, Crt};

use crate::flash::{FLASH_SIZE, Flash040};

const BANK: usize = 0x2000;

/// The hardware types this emulates. Anything else is a clear error rather
/// than a cartridge that silently does not work.
const SUPPORTED: &str = "0 Normal, 1 Action Replay, 2 KCS Power, 3 Final Cartridge III, \
4 Simons' BASIC, 5 Ocean, 7 Fun Play, 8 Super Games, 10 Epyx Fastload, 15 System 3, 17 Dinamic, \
18 Zaxxon, 19 Magic Desk, 32 EasyFlash, 36 Retro Replay, 62 Expert, 0xC640 Unified Diagnostic Cartridge";

/// The hardware type of crystalct's C64 Unified Diagnostic Cartridge
/// (<https://github.com/crystalct/C64UnifDiagCart>): a number of this project's own, since the `.crt` format has no entry for
/// that board (the real ones are written straight to an EPROM). A `.crt` of this type holds an 8K chip at `$8000` (the standard
/// diagnostic, run as an ordinary cartridge) and an 8K chip at `$E000` (the modified dead test, run in Ultimax mode); the header's
/// EXROM/GAME levels give the mode the board starts in (the board's JPE/JPG jumpers; Ultimax by default).
pub const UNIFIED_DIAG_HARDWARE: u16 = 0xC640;

/// Cycles the Epyx Fastload capacitor needs to recharge and drop the ROM
/// out of the memory map (`EPYX_ROM_CYCLES` in VICE).
const EPYX_ROM_CYCLES: u32 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mapper {
    Normal,
    Simons,
    Ocean,
    FunPlay,
    MagicDesk,
    Dinamic,
    SuperGames,
    Zaxxon,
    Epyx,
    System3,
    EasyFlash,
    ActionReplay,
    Kcs,
    FinalIII,
    RetroReplay,
    Expert,
    /// crystalct's Unified Diagnostic Cartridge: a flip-flop on the expansion port that a write to the I/O1 window switches
    /// between Ultimax (the dead test in ROMH) and an ordinary 8K cartridge (the standard diagnostic in ROML).
    UnifiedDiag,
}

/// The register state of a cartridge -- the part a save state carries. Field
/// meaning depends on the mapper (documented where used).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CartState {
    /// Selected ROML bank (all banked types), or Super Games' bank.
    pub bank: u16,
    /// Zaxxon's ROMH bank.
    pub romh_bank: u16,
    /// Last register value written (Fun Play, Magic Desk, Super Games,
    /// EasyFlash control).
    pub register: u8,
    /// Simons' BASIC: `$A000` window enabled. Epyx: ROM currently active.
    /// Super Games: cartridge enabled.
    pub enabled: bool,
    /// Super Games: register locked until reset.
    pub latched: bool,
    /// Epyx Fastload: cycles until the capacitor drops the ROM, if counting.
    pub countdown: Option<u32>,
    /// The cartridge's own RAM: EasyFlash 256 bytes at `$DF00`, Action Replay
    /// 8K, KCS 128 bytes, Retro Replay 32K, Expert 8K.
    pub ram: Vec<u8>,
    /// Freezer cartridges: the lines as a 2-bit mode (`bit0` GAME active,
    /// `bit1` EXROM *inactive*): 0 = 8K, 1 = 16K, 2 = off, 3 = Ultimax.
    pub mode: u8,
    /// Cartridge RAM mapped over ROML (Action Replay / Retro Replay).
    pub export_ram: bool,
    /// Registers still respond (Action Replay / Retro Replay bit 2 clears it).
    pub active: bool,
    /// A freeze has happened and has not been acknowledged yet.
    pub frozen: bool,
    /// Final Cartridge III: the `$DFFF` register is still visible.
    pub reg_enabled: bool,
    /// Retro Replay: bit0 AllowBank, bit1 NoFreeze, bit2 REU mapping, bit3
    /// extended register written once, bit4 RAM at `$A000`.
    pub flags: u8,
    /// The VIC-II sees plain RAM although the CPU runs an Ultimax map (the
    /// Final Cartridge III freezer).
    pub phi1_ram: bool,
    /// Expert: switch position, 0 off, 1 prg, 2 on.
    pub expert_mode: u8,
    /// Expert: its RAM replaces `$E000-$FFFF` (and `$8000`).
    pub ramh: bool,
    /// Expert: the RAM can be written.
    pub ram_writable: bool,
    /// EasyFlash flash chips (low = ROML, high = ROMH) command state.
    pub flash: Option<[Flash040; 2]>,
    /// The EasyFlash arrays when they differ from the attached image
    /// (low chip then high chip), so a state saved after flashing keeps it.
    pub flash_data: Option<Vec<u8>>,
}

#[derive(Clone)]
pub struct Cartridge {
    mapper: Mapper,
    name: String,
    /// Header lines (Normal only), in the active sense.
    header_lines: (bool, bool),
    /// 8K banks, concatenated.
    roml: Vec<u8>,
    romh: Vec<u8>,
    state: CartState,
    /// An NMI the cartridge wants raised (freeze button, FC3 register).
    nmi: bool,
    /// EasyFlash banks were programmed or erased since attach.
    flash_dirty: bool,
}

fn bank_slice(data: &[u8], bank: usize, count: usize) -> &[u8] {
    let bank = if count == 0 { 0 } else { bank % count };
    &data[bank * BANK..bank * BANK + BANK]
}

fn put(dest: &mut Vec<u8>, bank: usize, data: &[u8]) {
    if dest.len() < (bank + 1) * BANK {
        dest.resize((bank + 1) * BANK, 0xFF);
    }
    let n = data.len().min(BANK);
    dest[bank * BANK..bank * BANK + n].copy_from_slice(&data[..n]);
    // A 4K chip is mirrored into the upper half of its 8K window.
    if n < BANK && n > 0 {
        for i in n..BANK {
            dest[bank * BANK + i] = data[i % n];
        }
    }
}

impl Cartridge {
    pub fn from_crt(crt: &Crt) -> Result<Self, String> {
        let mapper = match crt.hardware_type {
            0 => Mapper::Normal,
            4 => Mapper::Simons,
            5 => Mapper::Ocean,
            7 => Mapper::FunPlay,
            8 => Mapper::SuperGames,
            10 => Mapper::Epyx,
            15 => Mapper::System3,
            17 => Mapper::Dinamic,
            18 => Mapper::Zaxxon,
            19 => Mapper::MagicDesk,
            32 => Mapper::EasyFlash,
            1 => Mapper::ActionReplay,
            2 => Mapper::Kcs,
            3 => Mapper::FinalIII,
            36 => Mapper::RetroReplay,
            62 => Mapper::Expert,
            UNIFIED_DIAG_HARDWARE => Mapper::UnifiedDiag,
            other => {
                return Err(format!(
                    "cartridge hardware type {other} ({:?}) is not supported; supported: {SUPPORTED}",
                    crt.name
                ));
            }
        };
        let mut cart = Cartridge {
            mapper,
            name: crt.name.clone(),
            header_lines: (!crt.exrom_level, !crt.game_level),
            roml: Vec::new(),
            romh: Vec::new(),
            state: CartState::default(),
            nmi: false,
            flash_dirty: false,
        };
        if mapper == Mapper::Expert {
            cart.state.expert_mode = 1; // PRG, VICE's default switch position
        }
        cart.load_chips(crt)?;
        cart.reset();
        Ok(cart)
    }

    fn load_chips(&mut self, crt: &Crt) -> Result<(), String> {
        for chip in &crt.chips {
            if chip.kind == ChipKind::Ram && self.mapper != Mapper::Expert {
                continue;
            }
            let bank = chip.bank as usize;
            let size = chip.data.len();
            let bad = |what: &str| {
                format!(
                    "{}: CHIP bank {} at ${:04X}, {} bytes: {what}",
                    self.name, chip.bank, chip.load_address, size
                )
            };
            match self.mapper {
                Mapper::Normal => match (chip.load_address, size) {
                    (0x8000, 1..=0x2000) => put(&mut self.roml, 0, &chip.data),
                    (0x8000, 0x2001..=0x4000) => {
                        put(&mut self.roml, 0, &chip.data[..BANK]);
                        put(&mut self.romh, 0, &chip.data[BANK..]);
                    }
                    (0xA000 | 0xE000, 1..=0x2000) => put(&mut self.romh, 0, &chip.data),
                    _ => return Err(bad("not a layout a normal cartridge uses")),
                },
                Mapper::Simons => match (chip.load_address, size) {
                    (0x8000, 0x2000) => put(&mut self.roml, 0, &chip.data),
                    (0xA000, 0x2000) => put(&mut self.romh, 0, &chip.data),
                    _ => return Err(bad("Simons' BASIC wants 8K chips at $8000 and $A000")),
                },
                Mapper::Ocean | Mapper::MagicDesk | Mapper::System3 | Mapper::Dinamic => {
                    let limit = if self.mapper == Mapper::Dinamic {
                        16
                    } else {
                        128
                    };
                    if size != BANK
                        || bank >= limit
                        || !matches!(chip.load_address, 0x8000 | 0xA000)
                    {
                        return Err(bad("expected an 8K chip in a valid bank"));
                    }
                    put(&mut self.roml, bank, &chip.data);
                }
                Mapper::FunPlay => {
                    // CRT bank numbers are register encodings, not linear.
                    let linear = ((bank >> 3) & 7) | ((bank & 1) << 3);
                    if size != BANK || chip.load_address != 0x8000 || linear > 15 {
                        return Err(bad("expected 8K chips at $8000 in banks 0-15"));
                    }
                    put(&mut self.roml, linear, &chip.data);
                }
                Mapper::SuperGames => {
                    if size != 0x4000 || chip.load_address != 0x8000 || bank > 3 {
                        return Err(bad("expected 16K chips at $8000 in banks 0-3"));
                    }
                    put(&mut self.roml, bank, &chip.data[..BANK]);
                    put(&mut self.romh, bank, &chip.data[BANK..]);
                }
                Mapper::Zaxxon => match (chip.load_address, size) {
                    (0x8000, 0x1000 | 0x2000) => put(&mut self.roml, 0, &chip.data),
                    (0xA000, 0x2000) if bank < 2 => put(&mut self.romh, bank, &chip.data),
                    _ => return Err(bad("expected 4K/8K at $8000 and 8K at $A000 banks 0-1")),
                },
                Mapper::Epyx => {
                    if size != BANK {
                        return Err(bad("Epyx Fastload is a single 8K chip"));
                    }
                    put(&mut self.roml, 0, &chip.data);
                }
                Mapper::ActionReplay => {
                    if size != BANK || bank >= 4 {
                        return Err(bad("Action Replay is four 8K banks"));
                    }
                    put(&mut self.roml, bank, &chip.data);
                }
                Mapper::RetroReplay => {
                    if size != BANK || bank >= 16 {
                        return Err(bad("Retro Replay is 8K banks (up to 16)"));
                    }
                    put(&mut self.roml, bank, &chip.data);
                }
                Mapper::Kcs => match (chip.load_address, size) {
                    (0x8000, BANK) => put(&mut self.roml, 0, &chip.data),
                    (0xA000, BANK) => put(&mut self.romh, 0, &chip.data),
                    _ => return Err(bad("KCS Power is 8K chips at $8000 and $A000")),
                },
                Mapper::FinalIII => {
                    if size != 0x4000 || chip.load_address != 0x8000 || bank >= 16 {
                        return Err(bad("Final Cartridge III is 16K chips at $8000, banks 0-15"));
                    }
                    put(&mut self.roml, bank, &chip.data[..BANK]);
                    put(&mut self.romh, bank, &chip.data[BANK..]);
                }
                Mapper::Expert => {
                    if size != BANK {
                        return Err(bad("Expert holds an 8K RAM image"));
                    }
                    self.state.ram = chip.data.clone();
                }
                Mapper::UnifiedDiag => match (chip.load_address, size) {
                    (0x8000, BANK) => put(&mut self.roml, 0, &chip.data),
                    (0xE000, BANK) => put(&mut self.romh, 0, &chip.data),
                    _ => {
                        return Err(bad(
                            "the unified diagnostic cartridge is 8K chips at $8000 and $E000",
                        ));
                    }
                },
                Mapper::EasyFlash => {
                    if bank >= 64 {
                        return Err(bad("EasyFlash has 64 banks"));
                    }
                    match (chip.load_address, size) {
                        (0x8000, 0x2000) => put(&mut self.roml, bank, &chip.data),
                        (0x8000, 0x4000) => {
                            put(&mut self.roml, bank, &chip.data[..BANK]);
                            put(&mut self.romh, bank, &chip.data[BANK..]);
                        }
                        (0xA000 | 0xE000, 0x2000) => put(&mut self.romh, bank, &chip.data),
                        _ => return Err(bad("expected 8K/16K at $8000 or 8K at $A000/$E000")),
                    }
                }
            }
        }
        if self.mapper == Mapper::Expert {
            // The CRT holds the RAM contents; with none the RAM starts empty.
            self.state.ram.resize(BANK, 0xFF);
            self.roml = vec![0xFF; BANK];
            self.romh = vec![0xFF; BANK];
            return Ok(());
        }
        if self.roml.is_empty() && self.romh.is_empty() {
            return Err(format!("{}: no usable ROM chips", self.name));
        }
        if matches!(self.mapper, Mapper::ActionReplay | Mapper::RetroReplay) {
            // These boards show the same 8K bank in the ROML and ROMH windows.
            self.roml.resize(self.roml.len().max(BANK), 0xFF);
            self.romh = self.roml.clone();
        }
        if self.mapper == Mapper::EasyFlash {
            self.roml.resize(FLASH_SIZE, 0xFF);
            self.romh.resize(FLASH_SIZE, 0xFF);
        }
        // Keep both windows addressable even when a type only fills one.
        if self.roml.is_empty() {
            self.roml = vec![0xFF; BANK];
        }
        if self.romh.is_empty() {
            self.romh = vec![0xFF; BANK];
        }
        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn roml_banks(&self) -> usize {
        self.roml.len() / BANK
    }

    fn romh_banks(&self) -> usize {
        self.romh.len() / BANK
    }

    /// Power-on state (also what a reset restores, except RAM).
    pub fn reset(&mut self) {
        let ram = std::mem::take(&mut self.state.ram);
        let expert_mode = if self.mapper == Mapper::Expert {
            self.state.expert_mode
        } else {
            0
        };
        let flash = self.state.flash.take();
        self.state = CartState {
            enabled: true,
            active: true,
            reg_enabled: true,
            expert_mode,
            ..CartState::default()
        };
        self.nmi = false;
        let ram_size = match self.mapper {
            Mapper::EasyFlash => 256,
            Mapper::ActionReplay | Mapper::Expert => BANK,
            Mapper::Kcs => 128,
            Mapper::RetroReplay => 0x8000,
            _ => 0,
        };
        if ram_size > 0 {
            self.state.ram = if ram.len() == ram_size {
                ram
            } else {
                vec![0xFF; ram_size]
            };
        }
        match self.mapper {
            Mapper::Epyx => self.discharge(),
            Mapper::Kcs | Mapper::FinalIII => self.state.mode = 1,
            // Back to the mode the jumpers choose, the CRT header's lines.
            Mapper::UnifiedDiag => {
                self.state.mode = match self.header_lines {
                    (false, true) => 3,
                    _ => 0,
                }
            }
            Mapper::EasyFlash => {
                // A reset line does not reset the flash command state on real
                // hardware either; only a power cycle does, but a stuck
                // half-finished command would make the next boot unusable.
                let mut f = flash.unwrap_or_default();
                if f[0].busy() || f[1].busy() {
                    f = [Flash040::new(), Flash040::new()];
                }
                self.state.flash = Some(f);
            }
            Mapper::Expert => {
                let on_or_prg = self.state.expert_mode != 0;
                self.state.reg_enabled = on_or_prg;
                self.state.ram_writable = on_or_prg;
                self.state.ramh = on_or_prg;
            }
            _ => {}
        }
    }

    /// Moves the Expert's three-way switch (0 off, 1 prg, 2 on).
    pub fn set_expert_mode(&mut self, mode: u8) {
        if self.mapper != Mapper::Expert {
            return;
        }
        self.state.expert_mode = mode.min(2);
        match self.state.expert_mode {
            1 => {
                self.state.reg_enabled = true;
                self.state.ramh = false;
                self.state.ram_writable = true;
            }
            _ => {
                self.state.reg_enabled = false;
                self.state.ramh = false;
                self.state.ram_writable = false;
            }
        }
    }

    pub fn state(&self) -> CartState {
        let mut state = self.state.clone();
        state.flash_data = self.flash_dirty.then(|| {
            let mut v = self.roml.clone();
            v.extend_from_slice(&self.romh);
            v
        });
        state
    }

    /// Restores saved register state; the ROM stays as attached (except
    /// EasyFlash arrays that were programmed before the state was taken).
    pub fn restore_state(&mut self, mut state: CartState) {
        if let Some(data) = state.flash_data.take()
            && self.mapper == Mapper::EasyFlash
            && data.len() == 2 * FLASH_SIZE
        {
            self.roml = data[..FLASH_SIZE].to_vec();
            self.romh = data[FLASH_SIZE..].to_vec();
            self.flash_dirty = true;
        }
        self.state = state;
        if self.mapper == Mapper::EasyFlash {
            if self.state.ram.len() != 256 {
                self.state.ram.resize(256, 0xFF);
            }
            if self.state.flash.is_none() {
                self.state.flash = Some([Flash040::new(), Flash040::new()]);
            }
        }
    }

    fn easyflash_code(&self) -> u8 {
        // Jumper "off" (normal boot) rows of VICE's decode table; 0 = off,
        // 1 = 8K, 2 = 16K, 3 = Ultimax. Control bit 2 = mode, bit 1 = X
        // (EXROM), bit 0 = G (GAME).
        let c = self.state.register;
        let (mode, x, g) = ((c >> 2) & 1, (c >> 1) & 1, c & 1);
        match (mode, x, g) {
            (0, 0, _) => 3,
            (0, 1, _) => 1,
            (1, 0, 0) => 2,
            (1, 0, 1) => 3,
            (1, 1, 0) => 0,
            _ => 1,
        }
    }

    /// `(exrom_active, game_active)` for a 2-bit freezer-style mode.
    fn mode_lines(mode: u8) -> (bool, bool) {
        (mode & 2 == 0, mode & 1 != 0)
    }

    /// `(exrom_active, game_active)` as the cartridge currently drives them.
    pub fn lines(&self) -> (bool, bool) {
        match self.mapper {
            Mapper::Normal => self.header_lines,
            Mapper::Simons => (true, self.state.enabled),
            Mapper::Ocean => (true, self.roml_banks() <= 32),
            Mapper::FunPlay | Mapper::MagicDesk => (self.state.enabled, false),
            Mapper::Dinamic | Mapper::System3 => (true, false),
            Mapper::SuperGames => (self.state.enabled, self.state.enabled),
            Mapper::Zaxxon => (true, true),
            Mapper::Epyx => (self.state.enabled, false),
            Mapper::EasyFlash => match self.easyflash_code() {
                0 => (false, false),
                1 => (true, false),
                2 => (true, true),
                _ => (false, true),
            },
            Mapper::ActionReplay
            | Mapper::Kcs
            | Mapper::FinalIII
            | Mapper::RetroReplay
            | Mapper::UnifiedDiag => Self::mode_lines(self.state.mode),
            // Prg: 8K game with the RAM at ROML. On/Off leave the map alone;
            // the RAM is overlaid by `overlay_read`/`write_hook`.
            Mapper::Expert => {
                if self.state.expert_mode == 1 {
                    (true, false)
                } else {
                    (false, false)
                }
            }
        }
    }

    /// True when the VIC-II fetches through the cartridge's Ultimax view
    /// (ROMH's upper 4K appears at `$3000-$3FFF` of every VIC bank).
    pub fn vic_ultimax(&self) -> bool {
        self.lines() == (false, true) && !self.state.phi1_ram
    }

    /// The 4K the VIC-II sees at `$3000-$3FFF` in the Ultimax view.
    pub fn vic_window(&self, offset: usize) -> u8 {
        self.read_romh(0xF000 + offset as u16)
    }

    /// The bank number in the 8K-bank sense, for mappers that bank by
    /// `state.bank`.
    fn bank_index(&self) -> usize {
        match self.mapper {
            Mapper::SuperGames => (self.state.bank & 3) as usize,
            Mapper::Normal | Mapper::Simons | Mapper::Zaxxon | Mapper::Epyx => 0,
            _ => self.state.bank as usize,
        }
    }

    fn ram_offset_for_rr(&self) -> usize {
        // Retro Replay shows one of four 8K RAM banks when banking is allowed.
        if self.state.flags & 1 != 0 {
            ((self.state.bank & 3) as usize) << 13
        } else {
            0
        }
    }

    /// A CPU read from the ROML window (`$8000-$9FFF`, or `$8000` on an
    /// Ultimax cartridge). Some cartridges react to being read.
    pub fn read_roml(&mut self, addr: u16) -> u8 {
        match self.mapper {
            Mapper::Zaxxon => {
                // Reading $8xxx picks ROMH bank 0, $9xxx bank 1.
                self.state.romh_bank = if addr & 0x1000 != 0 { 1 } else { 0 };
            }
            Mapper::Epyx => self.discharge(),
            Mapper::EasyFlash => {
                let at = self.state.bank as usize * BANK + (addr & 0x1FFF) as usize;
                let mut flash = self.state.flash.take().unwrap_or_default();
                let v = flash[0].read(&self.roml, at);
                self.state.flash = Some(flash);
                return v;
            }
            _ => {}
        }
        self.peek_roml(addr)
    }

    pub fn peek_roml(&self, addr: u16) -> u8 {
        let offset = (addr & 0x1FFF) as usize;
        match self.mapper {
            Mapper::ActionReplay if self.state.export_ram => return self.state.ram[offset],
            Mapper::RetroReplay => {
                if self.state.frozen || (self.state.mode == 1 && !self.state.export_ram) {
                    return 0xFF;
                }
                if self.state.export_ram {
                    return self.state.ram[offset + (((self.state.bank & 3) as usize) << 13)];
                }
            }
            Mapper::Expert => return self.state.ram[offset],
            _ => {}
        }
        bank_slice(&self.roml, self.bank_index(), self.roml_banks())[offset]
    }

    /// A read from a ROMH window (`$A000-$BFFF` or Ultimax `$E000-$FFFF`).
    pub fn read_romh(&self, addr: u16) -> u8 {
        let offset = (addr & 0x1FFF) as usize;
        match self.mapper {
            // Ocean maps the same 8K bank into both windows.
            Mapper::Ocean => {
                bank_slice(&self.roml, self.state.bank as usize, self.roml_banks())[offset]
            }
            Mapper::SuperGames => bank_slice(
                &self.romh,
                (self.state.bank & 3) as usize,
                self.romh_banks(),
            )[offset],
            Mapper::Zaxxon => {
                bank_slice(&self.romh, self.state.romh_bank as usize, self.romh_banks())[offset]
            }
            Mapper::EasyFlash | Mapper::ActionReplay | Mapper::FinalIII => {
                bank_slice(&self.romh, self.state.bank as usize, self.romh_banks())[offset]
            }
            Mapper::RetroReplay => {
                // With RAM selected and banking not allowed, the low two bank
                // bits do not reach the ROM.
                let bank = if self.state.flags & 1 != 0 || !self.state.export_ram {
                    self.state.bank
                } else {
                    self.state.bank & !3
                };
                bank_slice(&self.romh, bank as usize, self.romh_banks())[offset]
            }
            _ => bank_slice(&self.romh, 0, self.romh_banks())[offset],
        }
    }

    /// A CPU read of the ROMH window on EasyFlash goes through the flash chip
    /// (it can be busy), unlike the plain `read_romh` used for peeks.
    pub fn cpu_read_romh(&mut self, addr: u16) -> u8 {
        if self.mapper == Mapper::EasyFlash {
            let at = self.state.bank as usize * BANK + (addr & 0x1FFF) as usize;
            let mut flash = self.state.flash.take().unwrap_or_default();
            let v = flash[1].read(&self.romh, at);
            self.state.flash = Some(flash);
            return v;
        }
        self.read_romh(addr)
    }

    /// Memory the cartridge replaces regardless of the PLA (the Expert's
    /// RAM, which sits beside the normal map). `None` = not overlaid.
    pub fn overlay_read(&self, addr: u16) -> Option<u8> {
        if self.mapper == Mapper::Expert
            && self.state.expert_mode == 2
            && self.state.ramh
            && matches!(addr, 0x8000..=0x9FFF | 0xE000..=0xFFFF)
        {
            return Some(self.state.ram[(addr & 0x1FFF) as usize]);
        }
        None
    }

    /// A CPU write into `$8000-$9FFF`, `$A000-$BFFF` or `$E000-$FFFF`.
    /// `rom_visible` says whether the PLA shows the cartridge ROM at that
    /// address; `ultimax` whether the map is Ultimax. Returns `true` when the
    /// cartridge swallowed the write (it must then not reach C64 RAM).
    pub fn write_hook(&mut self, addr: u16, value: u8, rom_visible: bool, ultimax: bool) -> bool {
        let roml = (0x8000..=0x9FFF).contains(&addr);
        match self.mapper {
            Mapper::EasyFlash if ultimax && rom_visible => {
                let mut flash = self.state.flash.take().unwrap_or_default();
                let bank = self.state.bank as usize * BANK;
                let off = (addr & 0x1FFF) as usize;
                let changed = if roml {
                    flash[0].store(&mut self.roml, bank + off, value)
                } else if addr >= 0xE000 {
                    flash[1].store(&mut self.romh, bank + off, value)
                } else {
                    false
                };
                self.state.flash = Some(flash);
                self.flash_dirty |= changed;
                roml || addr >= 0xE000
            }
            Mapper::ActionReplay if roml && rom_visible && self.state.export_ram => {
                self.state.ram[(addr & 0x1FFF) as usize] = value;
                ultimax
            }
            Mapper::RetroReplay if roml && rom_visible && self.state.export_ram && ultimax => {
                let at = (addr & 0x1FFF) as usize + (((self.state.bank & 3) as usize) << 13);
                self.state.ram[at] = value;
                true
            }
            Mapper::Expert => {
                if self.state.expert_mode == 1 && roml && rom_visible && self.state.ram_writable {
                    self.state.ram[(addr & 0x1FFF) as usize] = value;
                    return true;
                }
                if self.state.expert_mode == 2 && roml && self.state.ramh && self.state.ram_writable
                {
                    self.state.ram[(addr & 0x1FFF) as usize] = value;
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// The freeze button. Returns `true` when an NMI should be raised.
    pub fn freeze(&mut self) -> bool {
        match self.mapper {
            Mapper::ActionReplay => {
                self.state.active = true;
                self.state.mode = 3;
                self.state.bank = 0;
                self.state.export_ram = true;
                self.state.frozen = true;
                true
            }
            Mapper::Kcs => {
                self.state.mode = 3;
                self.state.frozen = true;
                true
            }
            Mapper::FinalIII => {
                self.state.reg_enabled = true;
                self.state.mode = 3;
                self.state.phi1_ram = true;
                self.state.frozen = true;
                true
            }
            Mapper::RetroReplay => {
                if self.state.flags & 2 != 0 {
                    return false; // NoFreeze
                }
                self.state.active = true;
                self.state.frozen = true;
                self.state.mode = 3;
                self.state.bank = 0;
                self.state.export_ram = false;
                true
            }
            Mapper::Expert if self.state.expert_mode == 2 => {
                self.state.reg_enabled = true;
                self.state.ram_writable = true;
                self.state.ramh = true;
                self.state.frozen = true;
                true
            }
            _ => false,
        }
    }

    /// An NMI the cartridge asked for through a register write (Final
    /// Cartridge III bit 6 low); consumed by the call.
    pub fn take_nmi(&mut self) -> bool {
        std::mem::take(&mut self.nmi)
    }

    /// True for the types that have a freeze button.
    pub fn has_freezer(&self) -> bool {
        matches!(
            self.mapper,
            Mapper::ActionReplay
                | Mapper::Kcs
                | Mapper::FinalIII
                | Mapper::RetroReplay
                | Mapper::Expert
        )
    }

    /// True once EasyFlash code programmed or erased the flash.
    pub fn is_modified(&self) -> bool {
        self.flash_dirty
    }

    /// The EasyFlash contents as a `.crt` file (banks that are entirely
    /// erased are left out, as VICE does); `None` for any other cartridge.
    pub fn to_crt(&self) -> Option<Vec<u8>> {
        if self.mapper != Mapper::EasyFlash {
            return None;
        }
        let mut chips = Vec::new();
        for bank in 0..64usize {
            for (data, load) in [(&self.roml, 0x8000u16), (&self.romh, 0xA000u16)] {
                let slice = &data[bank * BANK..(bank + 1) * BANK];
                if slice.iter().any(|&b| b != 0xFF) {
                    chips.push(c64_cartridge::Chip {
                        kind: ChipKind::Flash,
                        bank: bank as u16,
                        load_address: load,
                        data: slice.to_vec(),
                    });
                }
            }
        }
        if chips.is_empty() {
            chips.push(c64_cartridge::Chip {
                kind: ChipKind::Flash,
                bank: 0,
                load_address: 0x8000,
                data: vec![0xFF; BANK],
            });
        }
        Some(c64_cartridge::write(&Crt {
            version: 0x0100,
            hardware_type: 32,
            exrom_level: !self.header_lines.0,
            game_level: !self.header_lines.1,
            name: self.name.clone(),
            chips,
        }))
    }

    /// `$DE00-$DEFF`. `None` means nothing drives the bus (it floats).
    pub fn io1_read(&mut self, addr: u16) -> Option<u8> {
        let offset = addr & 0xFF;
        match self.mapper {
            Mapper::Simons => {
                // A read switches to 8K mode (the $A000 window goes away).
                self.state.enabled = false;
                None
            }
            Mapper::Dinamic => {
                if offset < 16 {
                    self.state.bank = offset;
                }
                None
            }
            Mapper::System3 => {
                self.state.bank = offset & 0x3F;
                None
            }
            Mapper::Epyx => {
                self.discharge();
                None
            }
            Mapper::Kcs => {
                self.state.mode = if offset & 2 != 0 { 2 } else { 0 };
                Some(self.roml[0x1E00 + offset as usize])
            }
            Mapper::FinalIII => {
                Some(self.roml[self.state.bank as usize * BANK + 0x1E00 + offset as usize])
            }
            Mapper::Expert => {
                if self.state.expert_mode == 2 && self.state.reg_enabled {
                    self.state.ramh = !self.state.ramh;
                    self.state.ram_writable = false;
                }
                None
            }
            Mapper::RetroReplay if self.state.active => match offset {
                0 | 1 => {
                    let bank = self.state.bank;
                    Some(
                        (((bank & 3) << 3) | ((bank & 4) << 5) | ((bank & 8) << 2)) as u8
                            | if self.state.flags & 1 != 0 { 2 } else { 0 }
                            | if self.state.flags & 4 != 0 { 0x40 } else { 0 },
                    )
                }
                _ => {
                    if self.state.flags & 4 != 0 && !self.state.frozen {
                        if self.state.export_ram || self.state.flags & 16 != 0 {
                            let at = 0x1E00 + offset as usize + self.ram_offset_for_rr();
                            return Some(self.state.ram[at]);
                        }
                        if self.state.mode != 3 && self.state.mode != 1 {
                            return Some(
                                self.roml
                                    [self.state.bank as usize * BANK + 0x1E00 + offset as usize],
                            );
                        }
                    }
                    None
                }
            },
            _ => None,
        }
    }

    pub fn io1_write(&mut self, addr: u16, value: u8) {
        let offset = addr & 0xFF;
        match self.mapper {
            Mapper::Simons => self.state.enabled = true,
            // The flip-flop takes data bit 0: 1 = an ordinary 8K cartridge (the standard diagnostic at `$8000`), 0 = Ultimax.
            // The board has no schematic here: it is read off the dead test's own switch routine (`LDA #$01 / STA $DE00`).
            Mapper::UnifiedDiag => self.state.mode = if value & 1 != 0 { 0 } else { 3 },
            Mapper::Ocean => {
                let mask = (self.roml_banks().next_power_of_two() - 1).min(0x3F) as u16;
                self.state.bank = value as u16 & mask;
                self.state.register = value;
            }
            Mapper::FunPlay => {
                self.state.register = value;
                self.state.bank = (((value >> 3) & 7) | ((value & 1) << 3)) as u16;
                match value & 0xC6 {
                    0x00 => self.state.enabled = true,
                    0x86 => self.state.enabled = false,
                    _ => {} // unknown register value: the lines stay as they were
                }
            }
            Mapper::MagicDesk => {
                let mask = (self.roml_banks().max(4).next_power_of_two() - 1) as u8;
                self.state.register = value & (0x80 | mask);
                self.state.bank = (value & mask) as u16;
                self.state.enabled = value & 0x80 == 0;
            }
            Mapper::System3 => self.state.bank = offset & 0x3F,
            Mapper::EasyFlash => {
                if offset & 2 == 0 {
                    self.state.bank = (value & 0x3F) as u16;
                } else {
                    self.state.register = value & 0x87;
                }
            }
            Mapper::ActionReplay if self.state.active => {
                self.state.register = value;
                self.state.export_ram = value & 0x20 != 0;
                self.state.bank = ((value >> 3) & 3) as u16;
                // Mode $22 is broken in the original Action Replay (VICE
                // treats it as 8K game with the bank).
                self.state.mode = if value & 0x23 == 0x22 { 0 } else { value & 3 };
                if value & 0x40 != 0 {
                    self.state.frozen = false;
                }
                if value & 4 != 0 {
                    self.state.active = false;
                }
            }
            Mapper::Kcs => {
                self.state.mode = if offset & 2 != 0 { 3 } else { 1 };
            }
            Mapper::Expert => {
                if self.state.expert_mode == 2 && self.state.reg_enabled {
                    self.state.ramh = !self.state.ramh;
                    self.state.ram_writable = false;
                }
            }
            Mapper::RetroReplay if self.state.active => match offset {
                0 => {
                    let mut mode = value & 3;
                    self.state.bank = (((value >> 3) & 3) | ((value >> 5) & 4)) as u16;
                    self.state.export_ram = value & 0x20 != 0;
                    if value & 0x67 == 0x22 {
                        // The Retro Replay "mode $22" quirk: RAM at $A000.
                        self.state.flags |= 16;
                        mode = 2;
                    } else {
                        self.state.flags &= !16;
                        if value & 0x40 != 0 {
                            self.state.frozen = false;
                        }
                    }
                    if self.state.frozen {
                        mode = 3;
                    }
                    self.state.mode = mode;
                    if value & 4 != 0 {
                        self.state.active = false;
                    }
                }
                1 => {
                    if self.state.flags & 8 == 0 {
                        // AllowBank, NoFreeze and the REU mapping are write-once.
                        self.state.flags &= !(1 | 2 | 4);
                        self.state.flags |= u8::from(value & 2 != 0)
                            | (u8::from(value & 4 != 0) << 1)
                            | (u8::from(value & 0x40 != 0) << 2)
                            | 8;
                    }
                    self.state.bank = (((value >> 3) & 3) | ((value >> 5) & 4)) as u16;
                }
                _ => {
                    if self.state.flags & 4 != 0
                        && !self.state.frozen
                        && (self.state.export_ram || self.state.flags & 16 != 0)
                    {
                        let at = 0x1E00 + offset as usize + self.ram_offset_for_rr();
                        self.state.ram[at] = value;
                    }
                }
            },
            _ => {}
        }
    }

    /// `$DF00-$DFFF`.
    pub fn io2_read(&mut self, addr: u16) -> Option<u8> {
        self.io2_peek(addr)
    }

    /// Same value without any side effect (no cartridge reacts to an IO2 read).
    pub fn io2_peek(&self, addr: u16) -> Option<u8> {
        let offset = (addr & 0xFF) as usize;
        match self.mapper {
            // The last 256 bytes of the ROM are always visible here.
            Mapper::Epyx => Some(self.roml[0x1F00 + offset]),
            Mapper::EasyFlash => Some(self.state.ram[offset]),
            Mapper::ActionReplay => {
                if !self.state.active {
                    return None;
                }
                if self.state.export_ram {
                    return Some(self.state.ram[0x1F00 + offset]);
                }
                Some(self.roml[self.state.bank as usize * BANK + 0x1F00 + offset])
            }
            Mapper::Kcs => {
                if offset & 0x80 != 0 {
                    // The GAME / EXROM status the freezer NMI handler reads.
                    let mode = self.state.mode;
                    Some(
                        if mode & 2 != 0 { 0x80 } else { 0 } | if mode & 1 != 0 { 0 } else { 0x40 },
                    )
                } else {
                    Some(self.state.ram[offset & 0x7F])
                }
            }
            Mapper::FinalIII => Some(self.roml[self.state.bank as usize * BANK + 0x1F00 + offset]),
            Mapper::RetroReplay => {
                if self.state.active && self.state.flags & 4 == 0 && !self.state.frozen {
                    if self.state.export_ram || self.state.flags & 16 != 0 {
                        return Some(self.state.ram[0x1F00 + offset + self.ram_offset_for_rr()]);
                    }
                    if self.state.mode != 3 && self.state.mode != 1 {
                        return Some(self.roml[self.state.bank as usize * BANK + 0x1F00 + offset]);
                    }
                }
                None
            }
            _ => None,
        }
    }

    pub fn io2_write(&mut self, addr: u16, value: u8) {
        let offset = (addr & 0xFF) as usize;
        match self.mapper {
            Mapper::SuperGames if !self.state.latched => {
                self.state.bank = (value & 3) as u16;
                self.state.enabled = (value >> 2) & 1 == 0;
                self.state.latched = (value >> 3) & 1 != 0;
                self.state.register = value;
            }
            Mapper::EasyFlash => self.state.ram[offset] = value,
            Mapper::ActionReplay if self.state.active && self.state.export_ram => {
                self.state.ram[0x1F00 + offset] = value;
            }
            Mapper::Kcs if offset & 0x80 == 0 => self.state.ram[offset & 0x7F] = value,
            Mapper::FinalIII if self.state.reg_enabled && offset == 0xFF => {
                let banks = self.roml_banks().max(1);
                self.state.reg_enabled = value & 0x80 == 0;
                self.state.mode = ((value >> 3) & 2) | (((value >> 5) & 1) ^ 1);
                self.state.bank = (value as usize & (banks - 1)) as u16;
                self.state.register = value;
                self.state.phi1_ram = false;
                self.state.frozen = false;
                if value & 0x40 == 0 {
                    self.nmi = true;
                }
            }
            Mapper::RetroReplay
                if self.state.active
                    && self.state.flags & 4 == 0
                    && !self.state.frozen
                    && (self.state.export_ram || self.state.flags & 16 != 0) =>
            {
                let at = 0x1F00 + offset + self.ram_offset_for_rr();
                self.state.ram[at] = value;
            }
            _ => {}
        }
    }

    /// Advances the Epyx capacitor and EasyFlash erase timers.
    pub fn tick(&mut self, cycles: u32) {
        match self.mapper {
            Mapper::Epyx => {
                if let Some(left) = self.state.countdown {
                    if cycles >= left {
                        self.state.countdown = None;
                        self.state.enabled = false;
                    } else {
                        self.state.countdown = Some(left - cycles);
                    }
                }
            }
            Mapper::EasyFlash => {
                if let Some(flash) = &mut self.state.flash {
                    let a = flash[0].tick(&mut self.roml, cycles);
                    let b = flash[1].tick(&mut self.romh, cycles);
                    self.flash_dirty |= a || b;
                }
            }
            _ => {}
        }
    }

    fn discharge(&mut self) {
        self.state.enabled = true;
        self.state.countdown = Some(EPYX_ROM_CYCLES);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use c64_cartridge::Chip;

    fn chip(bank: u16, load: u16, fill: u8, size: usize) -> Chip {
        Chip {
            kind: ChipKind::Rom,
            bank,
            load_address: load,
            data: vec![fill; size],
        }
    }

    fn crt(hardware: u16, exrom_level: bool, game_level: bool, chips: Vec<Chip>) -> Crt {
        Crt {
            version: 0x100,
            hardware_type: hardware,
            exrom_level,
            game_level,
            name: "TEST".to_string(),
            chips,
        }
    }

    fn a_normal_8k_cartridge_uses_the_header_lines_and_only_roml() {
        let cart =
            Cartridge::from_crt(&crt(0, false, true, vec![chip(0, 0x8000, 0x11, 0x2000)])).unwrap();
        assert_eq!(cart.lines(), (true, false));
        assert_eq!(cart.peek_roml(0x8000), 0x11);
    }

    fn a_normal_16k_chip_splits_into_roml_and_romh() {
        let mut data = vec![0x22; 0x2000];
        data.extend(vec![0x33; 0x2000]);
        let mut c = chip(0, 0x8000, 0, 0);
        c.data = data;
        let cart = Cartridge::from_crt(&crt(0, false, false, vec![c])).unwrap();
        assert_eq!(cart.lines(), (true, true));
        assert_eq!(cart.peek_roml(0x9FFF), 0x22);
        assert_eq!(cart.read_romh(0xA000), 0x33);
    }

    fn an_ultimax_cartridge_reports_exrom_inactive_game_active() {
        let cart = Cartridge::from_crt(&crt(
            0,
            true,
            false,
            vec![chip(0, 0x8000, 1, 0x2000), chip(0, 0xE000, 2, 0x2000)],
        ))
        .unwrap();
        assert_eq!(cart.lines(), (false, true));
        assert_eq!(cart.read_romh(0xFFFC), 2);
    }

    fn a_4k_chip_is_mirrored_into_its_8k_window() {
        let cart =
            Cartridge::from_crt(&crt(0, false, true, vec![chip(0, 0x8000, 7, 0x1000)])).unwrap();
        assert_eq!(cart.peek_roml(0x8FFF), 7);
        assert_eq!(cart.peek_roml(0x9FFF), 7);
    }
    /// Merged (every check kept): `a_normal_8k_cartridge_uses_the_header_lines_and_only_roml`, `a_normal_16k_chip_splits_into_roml_and_romh`, `an_ultimax_cartridge_reports_exrom_inactive_game_active`, `a_4k_chip_is_mirrored_into_its_8k_window`.
    #[test]
    fn normal_cartridge_layouts() {
        a_normal_8k_cartridge_uses_the_header_lines_and_only_roml();
        a_normal_16k_chip_splits_into_roml_and_romh();
        an_ultimax_cartridge_reports_exrom_inactive_game_active();
        a_4k_chip_is_mirrored_into_its_8k_window();
    }

    #[test]
    fn unsupported_hardware_types_say_so_and_list_what_works() {
        let err = Cartridge::from_crt(&crt(99, false, true, vec![chip(0, 0x8000, 0, 0x2000)]))
            .err()
            .unwrap();
        assert!(err.contains("type 99") && err.contains("Ocean"), "{err}");
    }

    fn ocean_selects_banks_at_de00_and_maps_one_bank_into_both_windows() {
        let chips = (0..16u16)
            .map(|b| chip(b, 0x8000, b as u8, 0x2000))
            .collect();
        let mut cart = Cartridge::from_crt(&crt(5, false, false, chips)).unwrap();
        assert_eq!(cart.lines(), (true, true), "128K Ocean is a 16K cartridge");
        assert_eq!(cart.peek_roml(0x8000), 0);
        cart.io1_write(0xDE00, 5);
        assert_eq!(cart.peek_roml(0x8000), 5);
        assert_eq!(cart.read_romh(0xA000), 5, "same bank in the ROMH window");
        cart.io1_write(0xDE00, 0x25); // masked to the 16 banks present
        assert_eq!(cart.peek_roml(0x8000), 5);
    }

    fn a_512k_ocean_is_an_8k_cartridge() {
        let chips = (0..64u16)
            .map(|b| chip(b, 0x8000, b as u8, 0x2000))
            .collect();
        let cart = Cartridge::from_crt(&crt(5, false, true, chips)).unwrap();
        assert_eq!(cart.lines(), (true, false));
    }
    /// Merged (every check kept): `ocean_selects_banks_at_de00_and_maps_one_bank_into_both_windows`, `a_512k_ocean_is_an_8k_cartridge`.
    #[test]
    fn ocean_cases() {
        ocean_selects_banks_at_de00_and_maps_one_bank_into_both_windows();
        a_512k_ocean_is_an_8k_cartridge();
    }

    #[test]
    fn magic_desk_bank_bits_and_the_disable_bit() {
        let chips = (0..8u16)
            .map(|b| chip(b, 0x8000, 0x40 + b as u8, 0x2000))
            .collect();
        let mut cart = Cartridge::from_crt(&crt(19, false, true, chips)).unwrap();
        assert_eq!(cart.lines(), (true, false));
        cart.io1_write(0xDE00, 3);
        assert_eq!(cart.peek_roml(0x8000), 0x43);
        cart.io1_write(0xDE00, 0x80 | 3);
        assert_eq!(
            cart.lines(),
            (false, false),
            "bit 7 switches the cartridge off"
        );
        cart.io1_write(0xDE00, 1);
        assert_eq!(cart.lines(), (true, false));
        assert_eq!(cart.peek_roml(0x8000), 0x41);
    }

    #[test]
    fn simons_basic_toggles_its_a000_window_with_reads_and_writes() {
        let mut cart = Cartridge::from_crt(&crt(
            4,
            false,
            false,
            vec![chip(0, 0x8000, 1, 0x2000), chip(0, 0xA000, 2, 0x2000)],
        ))
        .unwrap();
        assert_eq!(cart.lines(), (true, true));
        assert_eq!(cart.io1_read(0xDE00), None);
        assert_eq!(cart.lines(), (true, false), "a read drops to 8K mode");
        cart.io1_write(0xDE00, 0);
        assert_eq!(cart.lines(), (true, true), "a write restores 16K mode");
        assert_eq!(cart.read_romh(0xA000), 2);
    }

    #[test]
    fn fun_play_decodes_its_bank_register_and_off_value() {
        // CRT bank fields are the register encoding of the linear bank.
        let chips = (0..16u16)
            .map(|linear| {
                chip(
                    ((linear & 7) << 3) | (linear >> 3),
                    0x8000,
                    linear as u8,
                    0x2000,
                )
            })
            .collect();
        let mut cart = Cartridge::from_crt(&crt(7, false, true, chips)).unwrap();
        assert_eq!(cart.lines(), (true, false));
        assert_eq!(cart.peek_roml(0x8000), 0);
        cart.io1_write(0xDE00, 0x08); // bits 3-5 = 1
        assert_eq!(cart.peek_roml(0x8000), 1);
        cart.io1_write(0xDE00, 0x01); // bit 0 is the bank's high bit
        assert_eq!(cart.peek_roml(0x8000), 8);
        cart.io1_write(0xDE00, 0x39); // 7 | 8
        assert_eq!(cart.peek_roml(0x8000), 15);
        cart.io1_write(0xDE00, 0x86);
        assert_eq!(cart.lines(), (false, false));
        cart.io1_write(0xDE00, 0x41); // unknown pattern: unchanged
        assert_eq!(cart.lines(), (false, false));
        cart.io1_write(0xDE00, 0x00);
        assert_eq!(cart.lines(), (true, false), "register 0 turns it back on");
    }

    #[test]
    fn dinamic_banks_switch_on_reads_of_the_first_16_io1_addresses() {
        let chips = (0..16u16)
            .map(|b| chip(b, 0x8000, b as u8, 0x2000))
            .collect();
        let mut cart = Cartridge::from_crt(&crt(17, false, true, chips)).unwrap();
        cart.io1_read(0xDE07);
        assert_eq!(cart.peek_roml(0x8000), 7);
        cart.io1_read(0xDE30); // beyond the 16 bank registers: ignored
        assert_eq!(cart.peek_roml(0x8000), 7);
        cart.io1_write(0xDE02, 0); // writes do nothing
        assert_eq!(cart.peek_roml(0x8000), 7);
    }

    #[test]
    fn system3_selects_the_bank_from_the_address_on_read_and_write() {
        let chips = (0..64u16)
            .map(|b| chip(b, 0x8000, b as u8, 0x2000))
            .collect();
        let mut cart = Cartridge::from_crt(&crt(15, false, true, chips)).unwrap();
        cart.io1_write(0xDE2A, 0xFF);
        assert_eq!(cart.peek_roml(0x8000), 0x2A, "value is irrelevant");
        cart.io1_read(0xDE05);
        assert_eq!(cart.peek_roml(0x8000), 5);
    }

    #[test]
    fn super_games_banks_16k_via_df00_and_latches_until_reset() {
        let chips = (0..4u16)
            .map(|b| {
                let mut c = chip(b, 0x8000, 0, 0);
                c.data = [vec![b as u8; 0x2000], vec![0x80 + b as u8; 0x2000]].concat();
                c
            })
            .collect();
        let mut cart = Cartridge::from_crt(&crt(8, false, false, chips)).unwrap();
        assert_eq!(cart.lines(), (true, true));
        cart.io2_write(0xDF00, 2);
        assert_eq!(cart.peek_roml(0x8000), 2);
        assert_eq!(cart.read_romh(0xA000), 0x82);
        cart.io2_write(0xDF00, 0b1100 | 1); // disable + latch, bank 1
        assert_eq!(cart.lines(), (false, false));
        cart.io2_write(0xDF00, 0); // ignored: latched
        assert_eq!(cart.lines(), (false, false));
        cart.reset();
        assert_eq!(cart.lines(), (true, true));
        assert_eq!(cart.peek_roml(0x8000), 0);
    }

    #[test]
    fn zaxxon_reads_in_the_roml_window_pick_the_romh_bank() {
        let cart_chips = vec![
            chip(0, 0x8000, 0x10, 0x1000),
            chip(0, 0xA000, 0x20, 0x2000),
            chip(1, 0xA000, 0x21, 0x2000),
        ];
        let mut cart = Cartridge::from_crt(&crt(18, false, false, cart_chips)).unwrap();
        assert_eq!(cart.read_romh(0xA000), 0x20);
        assert_eq!(cart.read_roml(0x9000), 0x10, "the 4K image is mirrored");
        assert_eq!(cart.read_romh(0xA000), 0x21, "a $9xxx read selected bank 1");
        cart.read_roml(0x8000);
        assert_eq!(cart.read_romh(0xA000), 0x20);
    }

    #[test]
    fn epyx_rom_drops_out_512_cycles_after_the_last_access() {
        let mut data = vec![0u8; 0x2000];
        data[0x1F10] = 0x5A;
        let mut c = chip(0, 0x8000, 0, 0);
        c.data = data;
        let mut cart = Cartridge::from_crt(&crt(10, false, true, vec![c])).unwrap();
        assert_eq!(cart.lines(), (true, false), "visible from reset");
        cart.tick(500);
        assert_eq!(cart.lines(), (true, false));
        cart.read_roml(0x8000); // recharges the timer
        cart.tick(500);
        assert_eq!(cart.lines(), (true, false));
        cart.tick(20);
        assert_eq!(cart.lines(), (false, false), "capacitor charged: ROM off");
        // The last page of ROM stays readable at $DF00 either way.
        assert_eq!(cart.io2_read(0xDF10), Some(0x5A));
        cart.io1_read(0xDE00);
        assert_eq!(cart.lines(), (true, false), "an IO1 read brings it back");
    }

    fn easyflash() -> Cartridge {
        let mut chips = Vec::new();
        for b in 0..2u16 {
            chips.push(chip(b, 0x8000, 0x10 + b as u8, 0x2000));
            chips.push(chip(b, 0xA000, 0x20 + b as u8, 0x2000));
        }
        Cartridge::from_crt(&crt(32, true, false, chips)).unwrap()
    }

    fn easyflash_boots_in_ultimax_and_switches_modes_through_de02() {
        let mut cart = easyflash();
        assert_eq!(cart.lines(), (false, true), "reset: Ultimax");
        cart.io1_write(0xDE02, 0b0000_0010); // mode 0, X=1: 8K
        assert_eq!(cart.lines(), (true, false));
        cart.io1_write(0xDE02, 0b0000_0100); // mode 1, X=0, G=0: 16K
        assert_eq!(cart.lines(), (true, true));
        cart.io1_write(0xDE02, 0b0000_0110); // mode 1, X=1, G=0: cartridge off
        assert_eq!(cart.lines(), (false, false));
        cart.io1_write(0xDE02, 0b1000_0111); // LED bit ignored; X=1, G=1: 8K
        assert_eq!(cart.lines(), (true, false));
    }

    fn easyflash_banks_roml_and_romh_together_and_has_256_bytes_of_ram() {
        let mut cart = easyflash();
        assert_eq!(cart.peek_roml(0x8000), 0x10);
        assert_eq!(cart.read_romh(0xE000), 0x20);
        cart.io1_write(0xDE00, 1);
        assert_eq!(cart.peek_roml(0x8000), 0x11);
        assert_eq!(cart.read_romh(0xA000), 0x21);
        cart.io2_write(0xDF80, 0x99);
        assert_eq!(cart.io2_read(0xDF80), Some(0x99));
        cart.reset();
        assert_eq!(cart.io2_read(0xDF80), Some(0x99), "RAM survives a reset");
    }
    /// Merged (every check kept): `easyflash_boots_in_ultimax_and_switches_modes_through_de02`, `easyflash_banks_roml_and_romh_together_and_has_256_bytes_of_ram`.
    #[test]
    fn easyflash_registers() {
        easyflash_boots_in_ultimax_and_switches_modes_through_de02();
        easyflash_banks_roml_and_romh_together_and_has_256_bytes_of_ram();
    }

    fn unified(exrom: bool, game: bool) -> Cartridge {
        Cartridge::from_crt(&crt(
            UNIFIED_DIAG_HARDWARE,
            exrom,
            game,
            vec![chip(0, 0x8000, 0x11, BANK), chip(0, 0xE000, 0x22, BANK)],
        ))
        .unwrap()
    }

    #[test]
    fn the_unified_diagnostic_cartridge_starts_as_its_header_says_and_flips_on_an_io1_write() {
        // Header EXROM high / GAME low = the Ultimax start the board is wired for by default.
        let mut cart = unified(true, false);
        assert_eq!(cart.lines(), (false, true), "Ultimax");
        assert_eq!(cart.read_romh(0xE000), 0x22, "the dead test's ROM at $E000");
        assert!(cart.vic_ultimax());
        // A write with data bit 0 set (what the dead test's routine does) makes it an ordinary 8K cartridge.
        cart.io1_write(0xDE00, 0x01);
        assert_eq!(cart.lines(), (true, false), "8K");
        assert_eq!(
            cart.peek_roml(0x8000),
            0x11,
            "the standard diagnostic at $8000"
        );
        assert!(!cart.vic_ultimax());
        // Data bit 0 clear flips it back; any address in the I/O1 window does it.
        cart.io1_write(0xDE40, 0x00);
        assert_eq!(cart.lines(), (false, true));
        // A reset puts the jumper-chosen mode back.
        cart.io1_write(0xDE00, 0x01);
        cart.reset();
        assert_eq!(cart.lines(), (false, true));
        // The other jumper setting starts as a standard cartridge.
        assert_eq!(unified(false, true).lines(), (true, false));
    }

    #[test]
    fn the_unified_diagnostic_cartridge_needs_its_two_8k_chips_and_keeps_its_mode_in_a_state() {
        let bad = Cartridge::from_crt(&crt(
            UNIFIED_DIAG_HARDWARE,
            true,
            false,
            vec![chip(0, 0xA000, 0, BANK)],
        ));
        assert!(bad.is_err());
        let mut cart = unified(true, false);
        cart.io1_write(0xDE00, 1);
        let saved = cart.state();
        let mut other = unified(true, false);
        other.restore_state(saved);
        assert_eq!(
            other.lines(),
            (true, false),
            "the switched mode rides in the save state"
        );
    }

    #[test]
    fn state_round_trips_without_the_rom() {
        let mut cart = easyflash();
        cart.io1_write(0xDE00, 1);
        cart.io1_write(0xDE02, 0b110);
        cart.io2_write(0xDF01, 7);
        let saved = cart.state();
        let mut other = easyflash();
        other.restore_state(saved);
        assert_eq!(other.lines(), cart.lines());
        assert_eq!(other.peek_roml(0x8000), 0x11);
        assert_eq!(other.io2_read(0xDF01), Some(7));
    }

    fn banks_8k(hardware: u16, exrom: bool, game: bool, count: u16, base: u8) -> Cartridge {
        let chips = (0..count)
            .map(|b| chip(b, 0x8000, base + b as u8, 0x2000))
            .collect();
        Cartridge::from_crt(&crt(hardware, exrom, game, chips)).unwrap()
    }

    #[test]
    fn action_replay_banks_roms_exposes_ram_and_freezes_into_ultimax() {
        let mut cart = banks_8k(1, false, true, 4, 0x10);
        assert_eq!(cart.lines(), (true, false), "power on: 8K game");
        assert_eq!(cart.peek_roml(0x8000), 0x10);
        cart.io1_write(0xDE00, 0x08); // bank 1
        assert_eq!(cart.peek_roml(0x8000), 0x11);
        assert_eq!(
            cart.io2_peek(0xDF00),
            Some(0x11),
            "IO2 mirrors the ROM page"
        );
        cart.io1_write(0xDE00, 0x20 | 0x10); // bank 2, RAM on
        assert!(
            !cart.write_hook(0x8000, 0x77, true, false),
            "8K mode also writes C64 RAM"
        );
        assert_eq!(cart.peek_roml(0x8000), 0x77, "RAM replaces ROML");
        cart.io2_write(0xDF05, 0x99);
        assert_eq!(cart.io2_peek(0xDF05), Some(0x99));
        cart.io1_write(0xDE00, 0x04 | 0x02); // disable, EXROM high
        cart.io1_write(0xDE00, 0x00); // ignored: registers are off
        assert_eq!(cart.lines(), (false, false));
        assert!(cart.freeze(), "freeze raises an NMI");
        assert_eq!(cart.lines(), (false, true), "Ultimax");
        assert_eq!(cart.read_romh(0xE000), 0x10, "bank 0 supplies the vectors");
        assert!(
            cart.write_hook(0x8000, 0x55, true, true),
            "Ultimax RAM write is swallowed"
        );
        assert_eq!(cart.peek_roml(0x8000), 0x55);
    }

    #[test]
    fn kcs_power_switches_modes_with_io1_and_reports_them_in_io2() {
        let chips = vec![chip(0, 0x8000, 0x31, 0x2000), chip(0, 0xA000, 0x32, 0x2000)];
        let mut cart = Cartridge::from_crt(&crt(2, true, false, chips)).unwrap();
        assert_eq!(cart.lines(), (true, true), "starts in 16K game");
        assert_eq!(cart.io1_read(0xDE00), Some(0x31)); // addr bit1 clear -> 8K, GAME off
        assert_eq!(cart.lines(), (true, false));
        cart.io1_read(0xDE02); // bit1 set -> RAM / cartridge off
        assert_eq!(cart.lines(), (false, false));
        cart.io1_write(0xDE00, 0); // R/W low -> 16K
        assert_eq!(cart.lines(), (true, true));
        cart.io1_write(0xDE02, 0); // -> Ultimax
        assert_eq!(cart.lines(), (false, true));
        assert_eq!(
            cart.io2_peek(0xDF80),
            Some(0x80),
            "EXROM high, GAME low-active"
        );
        cart.io2_write(0xDF05, 0x42);
        cart.io2_write(0xDF85, 0x13); // the upper half is not RAM
        assert_eq!(cart.io2_peek(0xDF05), Some(0x42));
        assert_ne!(cart.io2_peek(0xDF85), Some(0x13));
        assert!(cart.freeze());
        assert_eq!(cart.lines(), (false, true));
    }

    #[test]
    fn final_cartridge_iii_register_selects_bank_and_lines_and_can_hide() {
        let chips = (0..4u16)
            .map(|b| chip(b, 0x8000, 0x40 + b as u8, 0x4000))
            .collect();
        let mut cart = Cartridge::from_crt(&crt(3, false, false, chips)).unwrap();
        assert_eq!(cart.lines(), (true, true));
        assert_eq!(cart.io1_read(0xDE00), Some(0x40));
        // bank 2, EXROM low (bit4=0), GAME high (bit5=1), NMI high (bit6=1)
        cart.io2_write(0xDFFF, 0x40 | 0x20 | 2);
        assert_eq!(cart.lines(), (true, false), "GAME inactive -> 8K game");
        assert_eq!(cart.peek_roml(0x8000), 0x42);
        assert!(!cart.take_nmi());
        // NMI bit low raises an NMI; bit 7 hides the register afterwards.
        cart.io2_write(0xDFFF, 0x80 | 0x10 | 0x20 | 3);
        assert!(cart.take_nmi());
        assert_eq!(cart.peek_roml(0x8000), 0x43);
        cart.io2_write(0xDFFF, 0x00);
        assert_eq!(cart.peek_roml(0x8000), 0x43, "register is hidden");
        assert!(cart.freeze());
        assert_eq!(cart.lines(), (false, true));
        assert!(!cart.vic_ultimax(), "the FC3 freezer's VIC still sees RAM");
        assert_eq!(cart.io2_peek(0xDF10), Some(0x43), "IO2 mirrors the bank");
    }

    #[test]
    fn retro_replay_registers_ram_and_write_once_flags() {
        let mut cart = banks_8k(36, false, true, 8, 0x50);
        assert_eq!(cart.lines(), (true, false));
        assert_eq!(cart.io1_read(0xDE00), Some(0));
        cart.io1_write(0xDE00, 0x18 | 0x20); // bank 3, RAM on, 8K
        assert_eq!(cart.peek_roml(0x8000), 0xFF, "RAM fresh from power-on");
        assert!(
            cart.write_hook(0x8000, 0x66, true, true),
            "Ultimax writes land in RAM"
        );
        assert_eq!(cart.peek_roml(0x8000), 0x66);
        assert_eq!(
            cart.read_romh(0xA000),
            0x50,
            "no banking allowed: low bank bits ignored"
        );
        cart.io1_write(0xDE01, 0x02 | 0x04); // AllowBank + NoFreeze (write once)
        cart.io1_write(0xDE01, 0x00);
        assert_eq!(
            cart.io1_read(0xDE01).unwrap() & 2,
            2,
            "second write ignored"
        );
        assert!(!cart.freeze(), "NoFreeze blocks the button");
        // io2 RAM window while REU mapping is off
        cart.io2_write(0xDF10, 0x5A);
        assert_eq!(cart.io2_peek(0xDF10), Some(0x5A));
        cart.reset();
        assert!(cart.freeze());
        assert_eq!(cart.lines(), (false, true));
        assert_eq!(cart.peek_roml(0x8000), 0xFF, "frozen: nothing at ROML");
        cart.io1_write(0xDE00, 0x40 | 0x03); // release freeze, Ultimax values
        assert!(!cart.state().frozen);
    }

    #[test]
    fn expert_prg_maps_writable_ram_at_roml_and_on_overlays_rom_windows() {
        let ram = Chip {
            kind: ChipKind::Ram,
            bank: 0,
            load_address: 0x8000,
            data: vec![0x77; 0x2000],
        };
        let mut cart = Cartridge::from_crt(&crt(62, false, true, vec![ram])).unwrap();
        assert_eq!(cart.lines(), (true, false), "PRG: 8K game");
        assert_eq!(
            cart.peek_roml(0x8123),
            0x77,
            "the CRT's RAM image is loaded"
        );
        assert!(cart.write_hook(0x8123, 0x12, true, false));
        assert_eq!(cart.peek_roml(0x8123), 0x12);
        cart.set_expert_mode(2);
        assert_eq!(cart.lines(), (false, false));
        assert_eq!(
            cart.overlay_read(0x8123),
            None,
            "ON after the switch: RAM not mapped"
        );
        cart.reset();
        assert_eq!(cart.overlay_read(0x8123), Some(0x12), "reset maps the RAM");
        assert_eq!(cart.overlay_read(0xE000), Some(cart.state().ram[0]));
        assert_eq!(cart.overlay_read(0xA000), None);
        cart.io1_write(0xDE00, 0); // toggles the RAM off
        assert_eq!(cart.overlay_read(0xE000), None);
        assert!(cart.freeze(), "the freezer maps it back and raises an NMI");
        assert!(cart.overlay_read(0xFFFA).is_some());
        cart.set_expert_mode(0);
        assert!(!cart.freeze());
    }

    fn flash_cmd(cart: &mut Cartridge, addr: u16, value: u8) {
        assert!(cart.write_hook(addr, value, true, true));
    }

    fn easyflash_programs_and_erases_through_the_ultimax_window() {
        let mut cart = easyflash();
        assert_eq!(cart.peek_roml(0x8000), 0x10);
        // Cannot program already-programmed bytes upward: erase first.
        flash_cmd(&mut cart, 0x8555, 0xAA);
        flash_cmd(&mut cart, 0x82AA, 0x55);
        flash_cmd(&mut cart, 0x8555, 0x80);
        flash_cmd(&mut cart, 0x8555, 0xAA);
        flash_cmd(&mut cart, 0x82AA, 0x55);
        flash_cmd(&mut cart, 0x8000, 0x30); // sector 0 of the low chip
        cart.tick(50);
        assert_ne!(cart.read_roml(0x8000) & 0x08, 0, "erase timer expired");
        cart.tick(1_000_000);
        assert_eq!(cart.read_roml(0x8000), 0xFF, "erased");
        assert!(cart.is_modified());
        // program 0x5A at $8010
        flash_cmd(&mut cart, 0x8555, 0xAA);
        flash_cmd(&mut cart, 0x82AA, 0x55);
        flash_cmd(&mut cart, 0x8555, 0xA0);
        flash_cmd(&mut cart, 0x8010, 0x5A);
        assert_eq!(cart.read_roml(0x8010), 0x5A);
        // the high chip is separate: untouched
        assert_eq!(cart.read_romh(0xE000), 0x20);
        // exported .crt carries the new byte and round-trips
        let bytes = cart.to_crt().unwrap();
        let parsed = c64_cartridge::parse(&bytes).unwrap();
        let low = parsed
            .chips
            .iter()
            .find(|c| c.bank == 0 && c.load_address == 0x8000)
            .expect("bank 0 low chip");
        assert_eq!(low.data[0x10], 0x5A);
        let reloaded = Cartridge::from_crt(&parsed).unwrap();
        assert_eq!(reloaded.peek_roml(0x8010), 0x5A);
        // state keeps the programmed data
        let saved = cart.state();
        let mut other = easyflash();
        other.restore_state(saved);
        assert_eq!(other.peek_roml(0x8010), 0x5A);
    }

    fn easyflash_writes_outside_ultimax_do_not_reach_the_flash() {
        let mut cart = easyflash();
        assert!(!cart.write_hook(0x8555, 0xAA, true, false));
        assert!(!cart.is_modified());
    }
    /// Merged (every check kept): `easyflash_programs_and_erases_through_the_ultimax_window`, `easyflash_writes_outside_ultimax_do_not_reach_the_flash`.
    #[test]
    fn easyflash_flash_writes() {
        easyflash_programs_and_erases_through_the_ultimax_window();
        easyflash_writes_outside_ultimax_do_not_reach_the_flash();
    }
}
