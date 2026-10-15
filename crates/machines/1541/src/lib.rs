mod bus;
mod disk;
mod drive1581;
mod floppy81;
mod gcr;
mod iec;
mod save_state;

pub use bus::{C1541, DriveModel, ROM_SIZE, ROM_SIZE_1571};
pub use disk::Medium;
pub use drive1581::{C1581, ROM_SIZE_1581};
pub use iec::IecBus;

/// What the serial-bus wiring needs from a drive of any model.
pub trait IecDrive: machine::Machine {
    /// What the drive asserts onto `(CLOCK, DATA)`.
    fn iec_output(&self) -> (bool, bool);
    /// The bus's computed line levels `(ATN, CLOCK, DATA)` as the drive sees them.
    fn set_iec_input(&mut self, atn: bool, clock: bool, data: bool);
}

impl IecDrive for C1541 {
    fn iec_output(&self) -> (bool, bool) {
        C1541::iec_output(self)
    }

    fn set_iec_input(&mut self, atn: bool, clock: bool, data: bool) {
        C1541::set_iec_input(self, atn, clock, data);
    }
}

impl IecDrive for C1581 {
    fn iec_output(&self) -> (bool, bool) {
        C1581::iec_output(self)
    }

    fn set_iec_input(&mut self, atn: bool, clock: bool, data: bool) {
        C1581::set_iec_input(self, atn, clock, data);
    }
}
