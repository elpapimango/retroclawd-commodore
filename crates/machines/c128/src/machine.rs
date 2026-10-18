//! The C128 as a `machine::Machine`: an 8502 and a Z80 sharing one bus (the
//! MMU's mode register says which of them runs), the VIC-IIe, SID and two
//! CIAs ticking in 1 MHz bus cycles whichever CPU is in charge.

use machine::{Bus, Machine, RunStats};
pub use vic_ii::VicAccuracy;

use crate::bus::{C128Bus, NTSC_CLOCK_HZ, PAL_CLOCK_HZ, Roms};
use crate::keyboard::C128Key;

const PAL_FRAME_HZ: u64 = 50;
const NTSC_FRAME_HZ: u64 = 60;
/// Same 32-pixel border convention the other machines here use.
const BORDER_PX: u32 = 32;

/// A control-port joystick direction or fire button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoystickButton {
    Up,
    Down,
    Left,
    Right,
    Fire,
}

impl JoystickButton {
    fn bit(self) -> u8 {
        match self {
            Self::Up => 0x01,
            Self::Down => 0x02,
            Self::Left => 0x04,
            Self::Right => 0x08,
            Self::Fire => 0x10,
        }
    }
}

pub struct C128 {
    pub(crate) cpu: mos6502::Cpu,
    pub(crate) z80: z80::Cpu,
    pub(crate) bus: C128Bus,
    /// Half-cycles left over when the 8502 runs at 2 MHz or the Z80 at its
    /// T-state rate, so the bus-cycle count stays exact over time.
    pub(crate) clock_remainder: u32,
    pub(crate) audio_cycle_accumulator: f64,
    pub(crate) audio_buffer: Vec<f32>,
    /// Bus cycles left of holding the Commodore key down for a C64-mode
    /// boot (see `boot_into_c64_mode`).
    commodore_hold: u64,
    /// Whether the 8502 has left reset since power-on/RESET. Only the first
    /// hand-over from the Z80 starts it at a reset vector; afterwards the two
    /// processors simply resume where they stopped, which CP/M's BIOS relies
    /// on (the 8502 services a disk call and returns to the Z80's next
    /// instruction).
    pub(crate) cpu_started: bool,
}

impl C128 {
    /// A machine in its power-on state: the Z80 about to run the boot BIOS,
    /// the 8502 waiting at its reset vector.
    pub fn new(roms: Roms) -> Result<Self, String> {
        roms.check()?;
        let mut bus = C128Bus::new(roms);
        let mut cpu = mos6502::Cpu::new();
        // The 8502 is reset while the kernal ROM is visible, so its PC is
        // the kernal's reset entry; it only runs once the Z80 hands over.
        cpu.reset(&mut bus);
        Ok(Self {
            cpu,
            z80: z80::Cpu::new(),
            bus,
            clock_remainder: 0,
            audio_cycle_accumulator: 0.0,
            audio_buffer: Vec::new(),
            commodore_hold: 0,
            cpu_started: false,
        })
    }

    /// Video standard: PAL (default) or NTSC.
    pub fn set_region(&mut self, region: machine::Region) {
        let ntsc = !matches!(region, machine::Region::Pal);
        self.bus.vic.set_ntsc(ntsc);
        self.bus
            .vdc
            .set_clock(if ntsc { NTSC_CLOCK_HZ } else { PAL_CLOCK_HZ });
        self.bus.cia1.set_ntsc(ntsc);
        self.bus.cia2.set_ntsc(ntsc);
    }

    /// Boots into C64 mode, as holding the Commodore key at power-on does on
    /// the real machine: the key is held while the BIOS and kernal read it,
    /// then let go. Call before running.
    pub fn boot_into_c64_mode(&mut self) {
        self.press_key(C128Key::Commodore);
        self.commodore_hold = 1_500_000;
    }

    /// Selects which chip drives the display: 80 columns (the VDC) when the
    /// 40/80 DISPLAY key is down. The kernal reads the key at reset to decide
    /// which screen to set up, so call this before running.
    pub fn set_80_columns(&mut self, on: bool) {
        if on {
            self.press_key(C128Key::Display4080);
        } else {
            self.release_key(C128Key::Display4080);
        }
    }

    /// Whether the 80-column display is selected.
    pub fn is_80_columns(&self) -> bool {
        !self.bus.mmu.key_40_released
    }

    /// Pushes a joystick button; port 1 shares CIA1 Port B with the
    /// keyboard rows, port 2 is CIA1 Port A.
    pub fn press_joystick(&mut self, port: u8, button: JoystickButton) {
        if let Some(state) = self.bus.joystick.get_mut(usize::from(port.wrapping_sub(1))) {
            *state |= button.bit();
        }
    }

    pub fn release_joystick(&mut self, port: u8, button: JoystickButton) {
        if let Some(state) = self.bus.joystick.get_mut(usize::from(port.wrapping_sub(1))) {
            *state &= !button.bit();
        }
    }

    pub fn press_key(&mut self, key: C128Key) {
        self.bus.press_key(key);
    }

    pub fn release_key(&mut self, key: C128Key) {
        self.bus.release_key(key);
    }

    /// What the computer asserts onto the serial bus: `(ATN, CLOCK, DATA)`.
    pub fn iec_output(&self) -> (bool, bool, bool) {
        self.bus.iec_output()
    }

    /// The serial bus's computed `CLOCK` and `DATA` levels (true = high).
    pub fn set_iec_input(&mut self, clock: bool, data: bool) {
        self.bus.iec_clock_in = clock;
        self.bus.iec_data_in = data;
    }

    /// The 80-column chip's video RAM (for debugging and tests).
    pub fn vdc_ram(&self) -> &[u8] {
        self.bus.vdc.ram()
    }

    /// One of the VDC's registers.
    pub fn vdc_register(&self, n: usize) -> u8 {
        self.bus.vdc.reg(n)
    }

    /// Sets a register of the CPU that is running, by name.
    pub fn set_register(&mut self, name: &str, value: u64) -> Result<(), String> {
        if self.bus.mmu.cpu_is_8502() {
            self.cpu.set_register(name, value)
        } else {
            self.z80.set_register(name, value)
        }
    }

    /// Which CPU is running.
    pub fn cpu_name(&self) -> &'static str {
        if self.bus.mmu.cpu_is_8502() {
            "8502"
        } else {
            "Z80"
        }
    }

    pub fn mmu(&self) -> &crate::Mmu {
        &self.bus.mmu
    }

    /// Runs the active CPU for one instruction and returns the bus cycles
    /// (1 MHz clock) it took.
    fn run_cpu(&mut self) -> u32 {
        if self.bus.mmu.cpu_is_8502() {
            let cycles = self.cpu.step(&mut self.bus);
            // At 2 MHz two CPU cycles fit in one bus cycle.
            if self.bus.fast {
                self.clock_remainder += cycles;
                let bus_cycles = self.clock_remainder / 2;
                self.clock_remainder %= 2;
                bus_cycles
            } else {
                cycles
            }
        } else {
            // The Z80 runs at 2 MHz as well.
            let tstates = u32::from(self.z80.step(&mut self.bus));
            self.clock_remainder += tstates;
            let bus_cycles = self.clock_remainder / 2;
            self.clock_remainder %= 2;
            bus_cycles
        }
    }

    fn request_irq(&mut self) {
        if self.bus.mmu.cpu_is_8502() {
            self.cpu.request_irq();
        } else {
            self.z80.request_interrupt();
        }
    }

    fn request_nmi(&mut self) {
        if self.bus.mmu.cpu_is_8502() {
            self.cpu.request_nmi();
        } else {
            self.z80.request_nmi();
        }
    }

    /// What follows every instruction: the Z80 handing the machine to the
    /// 8502, a change of MMU mode reaching the VIC-II, and the Commodore key
    /// held for a C64-mode boot being let go.
    fn after_instruction(&mut self, was_64: bool, was_8502: bool, cycles: u32) {
        if !was_8502 && self.bus.mmu.cpu_is_8502() && !self.cpu_started {
            // The Z80 just released the machine for the first time: the 8502
            // starts from the reset vector of the map it now sees (the C64
            // kernal's when the BIOS chose C64 mode, e.g. with the Commodore
            // key held). Later switches resume it where it stopped.
            self.cpu_started = true;
            self.cpu.reset(&mut self.bus);
        }
        if self.bus.mmu.c64_mode() != was_64 {
            self.bus.sync_vic();
        }
        if self.commodore_hold > 0 {
            self.commodore_hold = self.commodore_hold.saturating_sub(u64::from(cycles));
            if self.commodore_hold == 0 {
                self.release_key(C128Key::Commodore);
            }
        }
    }

    /// Chooses line accuracy (default) or the cycle-accurate VIC-II. Cycle
    /// mode is PAL only and is refused on an NTSC machine.
    pub fn set_vic_accuracy(&mut self, accuracy: VicAccuracy) -> Result<(), String> {
        match accuracy {
            VicAccuracy::Cycle => self.bus.vic.enable_cycle_engine(),
            VicAccuracy::Line => {
                self.bus.vic.disable_cycle_engine();
                Ok(())
            }
        }
    }

    pub fn vic_accuracy(&self) -> VicAccuracy {
        if self.bus.cycle_mode() {
            VicAccuracy::Cycle
        } else {
            VicAccuracy::Line
        }
    }

    /// Clock cycles the 8502 has spent waiting for the VIC-II's BA line
    /// (cycle mode; always 0 otherwise).
    pub fn ba_stalled_cycles(&self) -> u64 {
        self.bus.stalled_cycles
    }

    /// `Machine::step` in cycle mode. An 8502 at 1 MHz runs with `in_cpu`
    /// set, so each of its bus cycles advances the machine by one clock and a
    /// read waits while BA is low. The Z80 and an 8502 in 2 MHz mode instead
    /// run the whole instruction and then tick the machine by the cycles it
    /// took: the VIC-II engine keeps its exact schedule either way, but those
    /// CPUs are not stalled by it (a 2 MHz C128 blanks the VIC-II anyway).
    fn step_cycle_mode(&mut self) -> u32 {
        let was_64 = self.bus.mmu.c64_mode();
        let was_8502 = self.bus.mmu.cpu_is_8502();
        self.bus.step_cycles = 0;
        if was_8502 && !self.bus.fast {
            self.bus.in_cpu = true;
            self.cpu.step(&mut self.bus);
            self.bus.in_cpu = false;
        } else {
            let cycles = self.run_cpu();
            for _ in 0..cycles {
                self.bus.tick_cycle();
            }
        }
        if std::mem::take(&mut self.bus.pending_irq) {
            self.request_irq();
        }
        if std::mem::take(&mut self.bus.pending_nmi) {
            self.request_nmi();
        }
        let cycles = self.bus.step_cycles;
        self.after_instruction(was_64, was_8502, cycles);
        self.accumulate_audio(cycles);
        cycles
    }

    /// Turns elapsed clock cycles into audio samples.
    fn accumulate_audio(&mut self, cycles: u32) {
        self.audio_cycle_accumulator += f64::from(cycles);
        let per_sample = self.clock_hz() as f64 / machine::AUDIO_SAMPLE_RATE as f64;
        while self.audio_cycle_accumulator >= per_sample {
            self.audio_cycle_accumulator -= per_sample;
            self.audio_buffer.push(self.bus.sid.output());
        }
    }

    fn advance_peripherals(&mut self, cycles: u32) -> u32 {
        if cycles == 0 {
            return 0;
        }
        let vic = self.bus.vic.tick(cycles);
        if vic.raster_irq {
            self.request_irq();
        }
        if let Some(line) = vic.entered_line {
            let base = self.bus.vic_bank_base();
            let color_bank = self.bus.vic_color_bank();
            let bus = &mut self.bus;
            let chargen = bus.chargen_4k().to_vec();
            if bus.vic.scan_sprite_collisions(
                line,
                &bus.ram[base..base + 0x4000],
                &chargen,
                &bus.color_ram[color_bank..color_bank + 0x400],
            ) {
                self.request_irq();
            }
        }
        let total = cycles + vic.stolen_cycles;
        if self.bus.cia1.tick(total as u8) {
            self.request_irq();
        }
        let cia2_level = self.bus.cia2.tick(total as u8);
        if self.bus.cia2_nmi_edge(cia2_level) {
            self.request_nmi();
        }
        self.bus.sid.tick(total);
        self.bus.vdc.tick(total);

        self.accumulate_audio(total);
        total
    }
}

impl Machine for C128 {
    fn clock_hz(&self) -> u64 {
        if self.bus.vic.is_ntsc() {
            NTSC_CLOCK_HZ
        } else {
            PAL_CLOCK_HZ
        }
    }

    fn frame_hz(&self) -> u64 {
        if self.bus.vic.is_ntsc() {
            NTSC_FRAME_HZ
        } else {
            PAL_FRAME_HZ
        }
    }

    fn run(&mut self, running: &mut dyn FnMut(&dyn Machine) -> bool) -> RunStats {
        machine::Clock::new(self.frame_hz()).run(self, running)
    }

    fn step(&mut self) -> u32 {
        if self.bus.cycle_mode() {
            return self.step_cycle_mode();
        }
        let was_64 = self.bus.mmu.c64_mode();
        let was_8502 = self.bus.mmu.cpu_is_8502();
        let cycles = self.run_cpu();
        self.after_instruction(was_64, was_8502, cycles);
        self.advance_peripherals(cycles)
    }

    fn halted(&self) -> bool {
        if self.bus.mmu.cpu_is_8502() {
            self.cpu.halted
        } else {
            false
        }
    }

    fn pc(&self) -> machine::Addr {
        if self.bus.mmu.cpu_is_8502() {
            u64::from(self.cpu.pc)
        } else {
            u64::from(self.z80.pc)
        }
    }

    fn registers(&self) -> Vec<(&'static str, u64)> {
        if self.bus.mmu.cpu_is_8502() {
            self.cpu.registers()
        } else {
            self.z80.registers()
        }
    }

    fn render_frame(&self) -> Option<(u32, u32, Vec<u8>)> {
        if self.is_80_columns() {
            // The 640x200 picture is shown line-doubled (an RGBI monitor
            // draws it at 4:3), so the window keeps a sensible shape.
            let (w, h, rgb) = self.bus.vdc.render();
            let row = w as usize * 3;
            let mut doubled = Vec::with_capacity(rgb.len() * 2);
            for line in rgb.chunks_exact(row) {
                doubled.extend_from_slice(line);
                doubled.extend_from_slice(line);
            }
            return Some(machine::with_border(
                w,
                h * 2,
                &doubled,
                BORDER_PX,
                BORDER_PX * 2,
                self.bus.vdc.border_color(),
            ));
        }
        // Cycle mode draws the picture as the cycles run (border unit,
        // sprites and all), border included; until the first frame is done
        // fall back to decoding the registers like line mode.
        if self.bus.cycle_mode()
            && let Some(frame) = self.bus.vic.cycle_frame()
        {
            return Some(frame);
        }
        let base = self.bus.vic_bank_base();
        let color_bank = self.bus.vic_color_bank();
        let (w, h, rgb) = self.bus.vic.render(
            &self.bus.ram[base..base + 0x4000],
            self.bus.chargen_4k(),
            &self.bus.color_ram[color_bank..color_bank + 0x400],
        );
        let (r, g, b) = self.bus.vic.border_color();
        Some(machine::with_border(
            w,
            h,
            &rgb,
            BORDER_PX,
            BORDER_PX,
            [r, g, b],
        ))
    }

    fn read_memory(&self, addr: machine::Addr) -> u8 {
        let addr = addr as u16;
        self.bus.peek(addr)
    }

    fn load(&mut self, addr: machine::Addr, bytes: &[u8]) -> usize {
        let addr = addr as u16;
        let mut a = addr;
        for &b in bytes {
            self.bus.write(a, b);
            a = a.wrapping_add(1);
        }
        bytes.len()
    }

    fn set_pc(&mut self, addr: machine::Addr) {
        let addr = addr as u16;
        if self.bus.mmu.cpu_is_8502() {
            self.cpu.pc = addr;
        } else {
            self.z80.pc = addr;
        }
    }

    fn reset(&mut self) {
        self.bus.mmu.reset();
        self.cpu.reset(&mut self.bus);
        self.z80 = z80::Cpu::new();
        self.cpu_started = false;
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.audio_buffer)
    }

    fn save_state(&self) -> Vec<u8> {
        crate::save_state::encode(self)
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        crate::save_state::decode(self, bytes)
    }
}
