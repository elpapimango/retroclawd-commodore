//! The real IEC (Inter-connect Electronics Commodore) serial bus: 3 open-
//! collector lines (`ATN`/`CLOCK`/`DATA`), idle-high via real pull-up
//! resistors, each pulled low ("asserted") whenever *any* device wired to
//! it drives it low -- a genuine wired-AND bus, not a point-to-point
//! signal. Lives in this crate (not `c64`, not `machine`) because a disk
//! drive is the thing that gives this bus its name and its reason to
//! exist -- `c64::C64` depends on this crate purely to reach this one
//! shared type, same "the format crate that most naturally owns a shared
//! shape hands it out, the other side just consumes it" reasoning
//! `z80-snapshot` already uses for handing back a real `z80::Cpu`.
//!
//! Modeled here with exactly the 2 real participants this project
//! currently wires to one bus (`c64::C64` and this crate's own `C1541`).
//! Real IEC hardware supports up to 5 simultaneous devices (a computer
//! plus up to 4 drives/printers) -- generalizing `IecBus` to more
//! participants before a second real device exists to prove that shape
//! against would be guessing, same "don't invent a shared shape from one
//! data point" stance this project already takes elsewhere (see
//! `zx-tape`'s own doc comment on why no shared `Loader` trait exists
//! yet). Only `ATN` is a real, hard, computer-only line on stock IEC
//! hardware -- no peripheral drive ever drives it -- so `IecBus` doesn't
//! even offer a `set_drive_atn`-shaped method, not merely leave one
//! unused.
//!
//! This struct has no knowledge of `C64`/`C1541` at all -- propagating
//! each side's own real output into it, and its computed line levels back
//! into each side's own input state, is the caller's job (this crate's own
//! `tests/` haven't been written for this pass, but `c64`'s own
//! `tests/iec_1541.rs` integration test drives exactly this loop by hand;
//! a real `retroclawd` dual-machine stepping loop, once `.d64` support
//! exists, will need to do the same thing continuously -- see `TODO.md`).

/// One open-collector bus line's current state: who (of the 2 real
/// participants this crate models) is actively pulling it low right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct IecLine {
    computer_asserts: bool,
    drive_asserts: bool,
}

impl IecLine {
    /// The real, physical wired-AND level: high (`true`, released) only if
    /// *neither* participant is pulling it low.
    fn level(&self) -> bool {
        !(self.computer_asserts || self.drive_asserts)
    }
}

/// The 3-line IEC serial bus connecting a Commodore computer to a disk
/// drive -- see this module's own doc comment for the full citation and
/// scope.
#[derive(Debug, Clone, Copy, Default)]
pub struct IecBus {
    atn: IecLine,
    clock: IecLine,
    data: IecLine,
}

impl IecBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets what the computer side (`c64::C64::iec_output`) is currently
    /// asserting on all 3 lines.
    pub fn set_computer_outputs(&mut self, atn: bool, clock: bool, data: bool) {
        self.atn.computer_asserts = atn;
        self.clock.computer_asserts = clock;
        self.data.computer_asserts = data;
    }

    /// Sets what the drive side (`C1541::iec_output`) is currently
    /// asserting -- no `atn` parameter at all, since a stock IEC drive
    /// never drives that line on real hardware.
    pub fn set_drive_outputs(&mut self, clock: bool, data: bool) {
        self.clock.drive_asserts = clock;
        self.data.drive_asserts = data;
    }

    pub fn atn(&self) -> bool {
        self.atn.level()
    }
    pub fn clock(&self) -> bool {
        self.clock.level()
    }
    pub fn data(&self) -> bool {
        self.data.level()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_bus_reads_high_on_every_line() {
        let bus = IecBus::new();
        assert!(bus.atn());
        assert!(bus.clock());
        assert!(bus.data());
    }

    #[test]
    fn either_participant_alone_pulls_a_line_low() {
        let mut bus = IecBus::new();
        bus.set_computer_outputs(true, false, false);
        assert!(!bus.atn());
        assert!(bus.clock() && bus.data());

        let mut bus2 = IecBus::new();
        bus2.set_drive_outputs(true, false);
        assert!(!bus2.clock());
        assert!(bus2.data());
    }

    #[test]
    fn a_line_only_releases_once_every_participant_releases_it() {
        let mut bus = IecBus::new();
        bus.set_computer_outputs(false, true, false);
        bus.set_drive_outputs(true, false);
        assert!(!bus.clock(), "both asserting -- still low");

        bus.set_computer_outputs(false, false, false);
        assert!(!bus.clock(), "drive still asserting -- still low");

        bus.set_drive_outputs(false, false);
        assert!(bus.clock(), "both released -- back to idle high");
    }
}
