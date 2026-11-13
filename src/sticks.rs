//! Stick model and the timed stick sequences (arm, disarm, calibration sweep).
//!
//! Pure logic with no I/O, so it can be unit-tested with synthetic time.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

/// How long the arm/disarm stick position is held.
///
/// The TX15 needs 4 s; the extra half second absorbs timing jitter between
/// this program and the simulator so the arm is not missed by a few ms.
pub const ARM_HOLD: Duration = Duration::from_millis(4500);

/// Length of a calibration sweep (three full cycles of each swept axis).
pub const CALIBRATION_SWEEP: Duration = Duration::from_secs(6);
const CALIBRATION_PERIOD_S: f32 = 2.0;

/// Logical range of every joystick axis.
pub const AXIS_MAX: i32 = 32767;
pub const AXIS_MIN: i32 = -AXIS_MAX;

/// Normalised stick positions, as they leave the virtual joystick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sticks {
    /// Roll (aileron): -1 = full left, +1 = full right.
    pub roll: f32,
    /// Pitch (elevator): -1 = stick pulled back, +1 = stick pushed forward.
    pub pitch: f32,
    /// Throttle: 0 = idle (stick fully down), 1 = full.
    pub throttle: f32,
    /// Yaw (rudder): -1 = full left, +1 = full right.
    pub yaw: f32,
}

impl Sticks {
    pub const NEUTRAL: Sticks = Sticks { roll: 0.0, pitch: 0.0, throttle: 0.0, yaw: 0.0 };

    /// Raw axis values in device order: roll, pitch, throttle, yaw.
    ///
    /// Same direction convention as an EdgeTX radio in USB joystick mode:
    /// stick right / forward / throttle up = axis maximum.
    pub fn to_axes(self) -> [i32; 4] {
        [
            bipolar(self.roll),
            bipolar(self.pitch),
            bipolar(self.throttle * 2.0 - 1.0),
            bipolar(self.yaw),
        ]
    }
}

fn bipolar(v: f32) -> i32 {
    (v.clamp(-1.0, 1.0) * AXIS_MAX as f32).round() as i32
}

/// One of the four sticks the joystick sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Roll,
    Pitch,
    Throttle,
    Yaw,
}

impl Axis {
    /// In device order (see [`Sticks::to_axes`]).
    pub const ALL: [Axis; 4] = [Axis::Roll, Axis::Pitch, Axis::Throttle, Axis::Yaw];
}

/// Which axes a calibration sweep moves.
///
/// Sweeping a single axis makes it easy to find in the simulator's channel
/// mapping and to calibrate it on its own: only that input moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sweep {
    #[default]
    All,
    Only(Axis),
}

impl Sweep {
    /// Every choice, in the order a picker lists them.
    pub const CHOICES: [Sweep; 5] = [
        Sweep::All,
        Sweep::Only(Axis::Roll),
        Sweep::Only(Axis::Pitch),
        Sweep::Only(Axis::Throttle),
        Sweep::Only(Axis::Yaw),
    ];

    pub fn moves(self, axis: Axis) -> bool {
        self == Sweep::All || self == Sweep::Only(axis)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceKind {
    /// Throttle down + yaw right, held for [`ARM_HOLD`].
    Arm,
    /// Throttle down + yaw left, held for [`ARM_HOLD`].
    Disarm,
    /// The chosen axes swept through their full range, for mapping and
    /// calibrating them in the sim. Axes not swept stay at neutral
    /// (sticks centred, throttle idle).
    Calibrate(Sweep),
}

impl SequenceKind {
    pub fn duration(self) -> Duration {
        match self {
            SequenceKind::Arm | SequenceKind::Disarm => ARM_HOLD,
            SequenceKind::Calibrate(_) => CALIBRATION_SWEEP,
        }
    }

    /// Stick positions `elapsed` into the sequence.
    fn sticks_at(self, elapsed: Duration) -> Sticks {
        match self {
            SequenceKind::Arm => Sticks { yaw: 1.0, ..Sticks::NEUTRAL },
            SequenceKind::Disarm => Sticks { yaw: -1.0, ..Sticks::NEUTRAL },
            SequenceKind::Calibrate(sweep) => {
                let phase = TAU * elapsed.as_secs_f32() / CALIBRATION_PERIOD_S;
                // Over-drive and clip the sine so each end stop is held for a
                // moment instead of just touched at one sample.
                let stick = (1.2 * phase.sin()).clamp(-1.0, 1.0);
                let throttle = (0.6 * (1.0 - phase.cos())).clamp(0.0, 1.0);
                let n = Sticks::NEUTRAL;
                let pick = |axis, swept, neutral| if sweep.moves(axis) { swept } else { neutral };
                Sticks {
                    roll: pick(Axis::Roll, stick, n.roll),
                    pitch: pick(Axis::Pitch, stick, n.pitch),
                    throttle: pick(Axis::Throttle, throttle, n.throttle),
                    yaw: pick(Axis::Yaw, stick, n.yaw),
                }
            }
        }
    }
}

/// A sequence that is currently running.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Active {
    pub kind: SequenceKind,
    pub elapsed: Duration,
}

impl Active {
    /// 0..=1 completion fraction.
    pub fn progress(&self) -> f32 {
        (self.elapsed.as_secs_f32() / self.kind.duration().as_secs_f32()).min(1.0)
    }
}

/// The sliders the user moves plus the sequence state machine.
///
/// Shared between the GUI thread (writes sliders, starts sequences) and the
/// output thread (calls [`Controller::output`] at a fixed rate).
#[derive(Debug, Default)]
pub struct Controller {
    /// Throttle slider, 0..=1.
    pub throttle: f32,
    /// Pitch slider, -1..=1.
    pub pitch: f32,
    running: Option<(SequenceKind, Instant)>,
    last_completed: Option<SequenceKind>,
}

impl Controller {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a sequence, replacing any that is running.
    ///
    /// Sliders are reset to throttle idle / pitch centre, which is where the
    /// sticks are when the sequence hands control back.
    pub fn start(&mut self, kind: SequenceKind, now: Instant) {
        self.throttle = 0.0;
        self.pitch = 0.0;
        self.running = Some((kind, now));
        self.last_completed = None;
    }

    pub fn cancel(&mut self) {
        self.running = None;
    }

    /// The running sequence, if it has not finished by `now`.
    pub fn active(&self, now: Instant) -> Option<Active> {
        let (kind, start) = self.running?;
        let elapsed = now.saturating_duration_since(start);
        (elapsed < kind.duration()).then_some(Active { kind, elapsed })
    }

    /// The last sequence that ran to completion (cleared when a new one starts).
    pub fn last_completed(&self) -> Option<SequenceKind> {
        self.last_completed
    }

    /// Stick positions to send at `now`. Retires a finished sequence.
    pub fn output(&mut self, now: Instant) -> Sticks {
        if let Some((kind, _)) = self.running {
            match self.active(now) {
                Some(active) => return kind.sticks_at(active.elapsed),
                None => {
                    self.running = None;
                    self.last_completed = Some(kind);
                }
            }
        }
        Sticks {
            pitch: self.pitch.clamp(-1.0, 1.0),
            throttle: self.throttle.clamp(0.0, 1.0),
            ..Sticks::NEUTRAL
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn axis_mapping_matches_radio_convention() {
        assert_eq!(Sticks::NEUTRAL.to_axes(), [0, 0, AXIS_MIN, 0]);
        let full = Sticks { roll: 1.0, pitch: -1.0, throttle: 1.0, yaw: -1.0 };
        assert_eq!(full.to_axes(), [AXIS_MAX, AXIS_MIN, AXIS_MAX, AXIS_MIN]);
        let half_throttle = Sticks { throttle: 0.5, ..Sticks::NEUTRAL };
        assert_eq!(half_throttle.to_axes()[2], 0);
        let over = Sticks { roll: 3.0, pitch: -3.0, throttle: 2.0, yaw: 0.0 };
        assert_eq!(over.to_axes(), [AXIS_MAX, AXIS_MIN, AXIS_MAX, 0]);
    }

    #[test]
    fn sliders_pass_through_when_idle() {
        let mut c = Controller::new();
        c.throttle = 0.4;
        c.pitch = -0.25;
        let out = c.output(Instant::now());
        assert_eq!(out, Sticks { roll: 0.0, pitch: -0.25, throttle: 0.4, yaw: 0.0 });
    }

    #[test]
    fn arm_holds_throttle_down_yaw_right_then_releases() {
        let t0 = Instant::now();
        let mut c = Controller::new();
        c.throttle = 0.7;
        c.pitch = 0.5;
        c.start(SequenceKind::Arm, t0);

        // Sliders are reset so control resumes from a safe position.
        assert_eq!((c.throttle, c.pitch), (0.0, 0.0));

        for t in [0, 1000, 4000, 4499] {
            let out = c.output(t0 + ms(t));
            assert_eq!(out, Sticks { yaw: 1.0, ..Sticks::NEUTRAL }, "at {t} ms");
            assert!(c.active(t0 + ms(t)).is_some());
        }
        assert!(ARM_HOLD >= Duration::from_secs(4), "must cover the TX15's 4 s hold");

        let out = c.output(t0 + ARM_HOLD);
        assert_eq!(out, Sticks::NEUTRAL);
        assert!(c.active(t0 + ARM_HOLD).is_none());
        assert_eq!(c.last_completed(), Some(SequenceKind::Arm));
    }

    #[test]
    fn disarm_uses_yaw_left() {
        let t0 = Instant::now();
        let mut c = Controller::new();
        c.start(SequenceKind::Disarm, t0);
        assert_eq!(c.output(t0 + ms(2000)), Sticks { yaw: -1.0, ..Sticks::NEUTRAL });
    }

    #[test]
    fn cancel_returns_to_sliders_without_completing() {
        let t0 = Instant::now();
        let mut c = Controller::new();
        c.start(SequenceKind::Arm, t0);
        c.output(t0 + ms(1000));
        c.cancel();
        c.throttle = 0.3;
        assert_eq!(c.output(t0 + ms(1100)), Sticks { throttle: 0.3, ..Sticks::NEUTRAL });
        assert_eq!(c.last_completed(), None);
    }

    /// Runs a sweep to the end; returns the lowest and highest raw value seen
    /// on each axis, and checks the sticks are neutral afterwards.
    fn run_sweep(sweep: Sweep) -> ([i32; 4], [i32; 4]) {
        let t0 = Instant::now();
        let mut c = Controller::new();
        c.start(SequenceKind::Calibrate(sweep), t0);
        let mut seen_min = [i32::MAX; 4];
        let mut seen_max = [i32::MIN; 4];
        let mut t = 0;
        while c.active(t0 + ms(t)).is_some() {
            let axes = c.output(t0 + ms(t)).to_axes();
            for i in 0..4 {
                seen_min[i] = seen_min[i].min(axes[i]);
                seen_max[i] = seen_max[i].max(axes[i]);
            }
            t += 10;
        }
        assert_eq!(c.output(t0 + CALIBRATION_SWEEP), Sticks::NEUTRAL);
        assert_eq!(c.last_completed(), Some(SequenceKind::Calibrate(sweep)));
        (seen_min, seen_max)
    }

    #[test]
    fn calibration_reaches_every_end_stop_and_ends_neutral() {
        let (seen_min, seen_max) = run_sweep(Sweep::All);
        assert_eq!(seen_min, [AXIS_MIN; 4]);
        assert_eq!(seen_max, [AXIS_MAX; 4]);
    }

    #[test]
    fn single_axis_sweep_moves_only_that_axis() {
        let neutral = Sticks::NEUTRAL.to_axes();
        for (i, axis) in Axis::ALL.into_iter().enumerate() {
            let (seen_min, seen_max) = run_sweep(Sweep::Only(axis));
            for j in 0..4 {
                if j == i {
                    assert_eq!((seen_min[j], seen_max[j]), (AXIS_MIN, AXIS_MAX), "{axis:?}");
                } else {
                    // Throttle stays at idle, the other sticks centred.
                    assert_eq!((seen_min[j], seen_max[j]), (neutral[j], neutral[j]), "{axis:?}");
                }
            }
        }
    }

    #[test]
    fn sweep_choices_list_all_then_each_axis_once() {
        assert_eq!(Sweep::CHOICES[0], Sweep::All);
        let singles: Vec<Sweep> = Axis::ALL.into_iter().map(Sweep::Only).collect();
        assert_eq!(Sweep::CHOICES[1..], singles[..]);
        assert!(Axis::ALL.into_iter().all(|a| Sweep::All.moves(a)));
    }
}
