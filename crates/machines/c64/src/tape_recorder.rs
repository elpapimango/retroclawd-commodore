//! Records what the KERNAL (or a turbo saver) writes to the Datasette: the
//! cassette WRITE line is bit 3 of the 6510's on-chip port, and the motor
//! (bit 5, active low) gates the recording exactly like it gates playback.
//!
//! The KERNAL writes every pulse as a *full wave*: two toggles of the line
//! half a pulse apart (measured on the real ROM: short ~362, medium ~510,
//! long ~680 cycles, each half exactly half). Recorded edges are therefore
//! paired up into one `.tap` pulse each -- the time from the first toggle of
//! a wave to the first toggle of the next. (Measuring between falling edges
//! instead mixes the halves of neighbouring waves and gives intermediate
//! values such as short+medium averages that no tape tool produces.) A
//! motor stop ends the current run of edges.

/// Pulse widths the KERNAL writes (short, medium, long), in cycles.
const KERNAL_PULSES: [u32; 3] = [362, 510, 680];

#[derive(Debug, Clone, Default)]
pub struct TapeRecorder {
    level: bool,
    edges: Vec<u64>,
    pulses: Vec<u32>,
}

impl TapeRecorder {
    pub fn new() -> Self {
        Self {
            level: true,
            ..Self::default()
        }
    }

    /// Called whenever the port changes: `motor_on` is the Datasette motor
    /// state, `level` the WRITE line, `stamp` the CPU cycle count.
    pub fn note(&mut self, stamp: u64, motor_on: bool, level: bool) {
        if !motor_on {
            self.finish_run();
            self.level = level;
            return;
        }
        if level != self.level {
            self.edges.push(stamp);
        }
        self.level = level;
    }

    /// Turns the edges of the run in progress into pulses (two toggles per
    /// wave). Which toggle starts a wave is not visible in the edges, and a
    /// run can begin on either (the line's resting level decides), so both
    /// pairings are tried and the one whose lengths sit closest to the three
    /// KERNAL pulse widths wins -- the wrong one yields mixed short+medium
    /// style lengths that match none of them.
    fn finish_run(&mut self) {
        let pairing = |skip: usize| -> Vec<u32> {
            self.edges
                .get(skip..)
                .unwrap_or(&[])
                .windows(3)
                .step_by(2)
                .map(|w| w[2].saturating_sub(w[0]).min(u64::from(u32::MAX)) as u32)
                .collect()
        };
        let cost = |pulses: &[u32]| -> u64 {
            pulses
                .iter()
                .map(|&p| {
                    KERNAL_PULSES
                        .iter()
                        .map(|&c| u64::from(p.abs_diff(c)))
                        .min()
                        .unwrap_or(0)
                })
                .sum()
        };
        let (a, b) = (pairing(0), pairing(1));
        // Compare average error so a run one pulse shorter is not favoured.
        let better_b = !b.is_empty()
            && (a.is_empty() || cost(&b) * a.len() as u64 <= cost(&a) * b.len() as u64);
        self.pulses.extend(if better_b { b } else { a });
        self.edges.clear();
    }

    /// All pulses recorded so far, including the run still in progress.
    pub fn pulses(&self) -> Vec<u32> {
        let mut copy = self.clone();
        copy.finish_run();
        copy.pulses
    }

    /// Adds finished pulses (the fast SAVE trap's output) after what the
    /// recorder has so far.
    pub fn append_pulses(&mut self, pulses: &[u32]) {
        self.finish_run();
        self.pulses.extend_from_slice(pulses);
    }

    pub fn is_empty(&self) -> bool {
        self.pulses.is_empty() && self.edges.len() < 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_toggles_make_one_pulse_the_time_to_the_next_waves_first_toggle() {
        let mut r = TapeRecorder::new();
        r.note(100, true, false); // wave 1 starts
        r.note(280, true, true);
        r.note(460, true, false); // wave 2 starts: wave 1 was 360 long
        r.note(710, true, true);
        r.note(960, true, false); // wave 3 starts: wave 2 was 500 long
        assert_eq!(r.pulses(), vec![360, 500]);
    }

    fn the_motor_stopping_ends_the_run_and_the_gap_is_not_a_pulse() {
        let mut r = TapeRecorder::new();
        r.note(0, true, false);
        r.note(180, true, true);
        r.note(360, true, false);
        r.note(400, false, false); // motor off: the run ends
        r.note(9000, true, true);
        r.note(9180, true, false);
        r.note(9360, true, true);
        assert_eq!(r.pulses(), vec![360, 360]);
    }
    /// Merged (every check kept): `two_toggles_make_one_pulse_the_time_to_the_next_waves_first_toggle`, `the_motor_stopping_ends_the_run_and_the_gap_is_not_a_pulse`.
    #[test]
    fn recorder_cases() {
        two_toggles_make_one_pulse_the_time_to_the_next_waves_first_toggle();
        the_motor_stopping_ends_the_run_and_the_gap_is_not_a_pulse();
    }
}
