//! F1 Brake Limit / Lock-up detector (v0).
//!
//! Uses the F1 MotionEx per-wheel channels (wheel slip ratio + wheel speed)
//! plus vehicle speed and brake input to estimate how close the front wheels
//! are to locking under braking. It is a heuristic calibrated on captured
//! F1 25 / F1 26 Season Pack data, not a physics model.
//!
//! The detector is deterministic: all durations are advanced by an explicit
//! `dt` so unit tests can drive it without wall-clock time.
//!
//! Wheel arrays use the EA order RL, RR, FL, FR; this module consumes FL (2)
//! and FR (3).

#![allow(dead_code)]

/// Tunable parameters. Baseline values are the "F1 26 calibrated baseline v0".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrakeLimitParams {
    /// Vehicle speed at which the speed gate starts to open, km/h.
    pub speed_gate_start: f32,
    /// Vehicle speed at which the speed gate is fully open, km/h.
    pub speed_gate_full: f32,
    /// Brake input at which the brake gate starts to open.
    pub brake_gate_start: f32,
    /// Brake input at which the brake gate is fully open.
    pub brake_gate_full: f32,
    /// Slip magnitude at which approaching detection begins.
    pub slip_warn_start: f32,
    /// Slip magnitude at which the lock condition begins.
    pub slip_lock_start: f32,
    /// Slip magnitude at which slip severity reaches 1.0.
    pub slip_full_lock: f32,
    /// Wheel/speed ratio below which the wheel is considered confirmed locking.
    pub wheel_warn_ratio: f32,
    /// Wheel/speed ratio at which wheel severity reaches 1.0.
    pub wheel_lock_ratio: f32,
    /// Severity response curve exponent.
    pub severity_gamma: f32,
    /// How long the lock condition must persist before a transient lock is
    /// declared (milliseconds).
    pub lock_attack_ms: f32,
    /// How long a lock must persist before it becomes sustained (milliseconds).
    pub sustained_ms: f32,
    /// Residual glow after a lock releases (milliseconds).
    pub release_hold_ms: f32,
    /// How strongly warning colour is mixed in at full severity.
    pub warning_mix: f32,
    /// Pulse amplitude scale.
    pub pulse_strength: f32,
    /// Sustained burn amplitude scale.
    pub burn_strength: f32,
    /// Master scale for the confirmed-lock cyan amount.
    pub lock_cyan_strength: f32,
    /// Peak alpha of the history upward lock projection.
    pub lock_projection_alpha: f32,
    /// Projection mid-point alpha as a fraction of the peak (vertical falloff).
    pub lock_projection_falloff: f32,
}

impl Default for BrakeLimitParams {
    fn default() -> Self {
        Self {
            speed_gate_start: 30.0,
            speed_gate_full: 50.0,
            brake_gate_start: 0.10,
            brake_gate_full: 0.25,
            slip_warn_start: 0.15,
            slip_lock_start: 0.45,
            slip_full_lock: 0.85,
            wheel_warn_ratio: 0.80,
            wheel_lock_ratio: 0.45,
            severity_gamma: 1.6,
            lock_attack_ms: 60.0,
            sustained_ms: 280.0,
            release_hold_ms: 100.0,
            warning_mix: 0.20,
            pulse_strength: 1.0,
            burn_strength: 0.8,
            lock_cyan_strength: 1.4,
            lock_projection_alpha: 0.30,
            lock_projection_falloff: 0.35,
        }
    }
}

impl BrakeLimitParams {
    /// Slip magnitude at which approaching detection ends (hysteresis).
    pub fn slip_warn_exit(&self) -> f32 {
        self.slip_warn_start * 0.73
    }

    /// Slip magnitude at which the lock condition ends (hysteresis).
    pub fn slip_lock_exit(&self) -> f32 {
        self.slip_lock_start * 0.67
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LockState {
    #[default]
    Normal,
    Approaching,
    TransientLock,
    SustainedLock,
    Recovery,
}

impl LockState {
    pub fn label(self) -> &'static str {
        match self {
            LockState::Normal => "Normal",
            LockState::Approaching => "Approaching",
            LockState::TransientLock => "Transient Lock",
            LockState::SustainedLock => "Sustained Lock",
            LockState::Recovery => "Recovery",
        }
    }
}

/// Current detector output consumed by the HUD and the debug panel.
#[derive(Clone, Copy, Debug, Default)]
pub struct BrakeLimitFeedback {
    pub severity_fl: f32,
    pub severity_fr: f32,
    pub global_severity: f32,
    pub symmetry: f32,
    pub balance: f32,
    pub state: LockState,
    pub lock_duration_ms: f32,
    /// Confirmed lock (transient or sustained), used for history recording.
    pub lock_confirmed: bool,
    /// One-shot pulse envelope (already scaled by pulse strength).
    pub pulse: f32,
    /// Sustained burn level (already scaled by burn strength).
    pub burn: f32,
    /// Subtle warm-orange amount for the approaching phase.
    pub approach_mix: f32,
    /// Cyan lock amount for the left half of the brake bar.
    pub lock_fl: f32,
    /// Cyan lock amount for the right half of the brake bar.
    pub lock_fr: f32,
    /// Final colour mix amount in `0..=1` (kept for debug).
    pub color_mix: f32,
}

/// Per-frame detector input. `None` wheel channels (no MotionEx yet, or an
/// unpaired frame) are treated as "no lock evidence".
#[derive(Clone, Copy, Debug, Default)]
pub struct WheelSample {
    pub speed_ms: f32,
    pub brake: f32,
    /// FL / FR slip ratio.
    pub slip_fl: Option<f32>,
    pub slip_fr: Option<f32>,
    /// FL / FR wheel speed (raw EA units).
    pub wheel_speed_fl: Option<f32>,
    pub wheel_speed_fr: Option<f32>,
}

impl WheelSample {
    /// Extract the detector inputs from a normalized telemetry vehicle. Wheel
    /// arrays are the EA order RL, RR, FL, FR, so FL = index 2 and FR = 3.
    pub fn from_vehicle(vehicle: &crate::core::VehicleTelemetry) -> Self {
        let slip = vehicle.wheel_slip;
        let wheel_speed = vehicle.wheel_speed;
        Self {
            speed_ms: vehicle.speed,
            brake: vehicle.brake,
            slip_fl: slip.map(|values| values[2]),
            slip_fr: slip.map(|values| values[3]),
            wheel_speed_fl: wheel_speed.map(|values| values[2]),
            wheel_speed_fr: wheel_speed.map(|values| values[3]),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct WheelEval {
    severity: f32,
    slip_mag: f32,
    wheel_ratio: f32,
}

pub struct BrakeLimitDetector {
    params: BrakeLimitParams,
    time: f32,
    approaching: bool,
    locked_fl: bool,
    locked_fr: bool,
    lock_condition_since: Option<f32>,
    lock_active: bool,
    lock_started_at: Option<f32>,
    release_at: Option<f32>,
    pulse_at: Option<f32>,
    episode_peak: f32,
    state: LockState,
    output: BrakeLimitFeedback,
}

impl Default for BrakeLimitDetector {
    fn default() -> Self {
        Self::new(BrakeLimitParams::default())
    }
}

impl BrakeLimitDetector {
    pub fn new(params: BrakeLimitParams) -> Self {
        Self {
            params,
            time: 0.0,
            approaching: false,
            locked_fl: false,
            locked_fr: false,
            lock_condition_since: None,
            lock_active: false,
            lock_started_at: None,
            release_at: None,
            pulse_at: None,
            episode_peak: 0.0,
            state: LockState::Normal,
            output: BrakeLimitFeedback::default(),
        }
    }

    pub fn params(&self) -> BrakeLimitParams {
        self.params
    }

    pub fn set_params(&mut self, params: BrakeLimitParams) {
        self.params = params;
    }

    pub fn feedback(&self) -> BrakeLimitFeedback {
        self.output
    }

    /// True while a confirmed lock (transient or sustained) is active.
    pub fn lock_confirmed(&self) -> bool {
        self.lock_active
    }

    pub fn state(&self) -> LockState {
        self.state
    }

    /// Advance the detector by `dt` seconds using one telemetry sample.
    pub fn update(&mut self, dt: f32, sample: &WheelSample) {
        let dt = dt.clamp(0.0, 0.25);
        self.time += dt;
        let p = self.params;

        let speed_kmh = sample.speed_ms.max(0.0) * 3.6;
        let speed_gate = smoothstep(p.speed_gate_start, p.speed_gate_full, speed_kmh);
        let brake_gate = smoothstep(p.brake_gate_start, p.brake_gate_full, sample.brake);
        let gate = speed_gate * brake_gate;

        let fl = self.eval_wheel(sample.slip_fl, sample.wheel_speed_fl, sample.speed_ms, gate);
        let fr = self.eval_wheel(sample.slip_fr, sample.wheel_speed_fr, sample.speed_ms, gate);

        // Per-side lock hysteresis.
        self.locked_fl = side_lock(self.locked_fl, fl, &p);
        self.locked_fr = side_lock(self.locked_fr, fr, &p);
        let raw_lock = self.locked_fl || self.locked_fr;

        // Approaching hysteresis (global).
        let warn_enter = fl.slip_mag >= p.slip_warn_start || fr.slip_mag >= p.slip_warn_start;
        let warn_hold = fl.slip_mag >= p.slip_warn_exit() || fr.slip_mag >= p.slip_warn_exit();
        if warn_enter {
            self.approaching = true;
        } else if !warn_hold {
            self.approaching = false;
        }

        let global = fl.severity.max(fr.severity);
        if self.lock_active {
            self.episode_peak = self.episode_peak.max(global);
        }

        // Debounce the lock condition by lock_attack_ms.
        let attack_s = p.lock_attack_ms / 1000.0;
        if raw_lock {
            let since = *self.lock_condition_since.get_or_insert(self.time);
            if !self.lock_active && self.time - since >= attack_s {
                self.lock_active = true;
                self.lock_started_at = Some(self.time);
                self.pulse_at = Some(self.time);
                self.episode_peak = global;
            }
        } else {
            self.lock_condition_since = None;
            if self.lock_active {
                self.lock_active = false;
                self.release_at = Some(self.time);
            }
        }

        let sustained_s = p.sustained_ms / 1000.0;
        let release_s = p.release_hold_ms / 1000.0;
        self.state = if self.lock_active {
            let duration = self.lock_started_at.map_or(0.0, |start| self.time - start);
            if duration >= sustained_s {
                LockState::SustainedLock
            } else {
                LockState::TransientLock
            }
        } else if let Some(release) = self.release_at {
            if self.time - release < release_s {
                LockState::Recovery
            } else {
                self.release_at = None;
                if self.approaching {
                    LockState::Approaching
                } else {
                    LockState::Normal
                }
            }
        } else if self.approaching {
            LockState::Approaching
        } else {
            LockState::Normal
        };

        let lock_duration_ms = if self.lock_active {
            self.lock_started_at.map_or(0.0, |start| self.time - start) * 1000.0
        } else {
            0.0
        };

        // One-shot cyan pulse, amplitude follows the worst severity of the
        // episode. It fires once per lock episode and decays before sustained.
        let pulse_env = self.pulse_at.map_or(0.0, |start| pulse_envelope(self.time - start));
        let pulse = if self.state == LockState::TransientLock {
            (pulse_env * self.episode_peak * p.pulse_strength).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // Sustained burn is a stable highlight (no per-frame flicker).
        let burn = if self.state == LockState::SustainedLock {
            (self.episode_peak * p.burn_strength).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let recovery = if self.state == LockState::Recovery {
            1.0 - (self.time - self.release_at.unwrap_or(self.time)) / release_s
        } else {
            0.0
        }
        .clamp(0.0, 1.0);

        // Confirmed-lock cyan amount and its per-side split. The per-side
        // fraction uses each wheel's share of the global severity so an FL-only
        // lock only lights the left half.
        let lock_amount = pulse
            .max(burn)
            .max(recovery * self.episode_peak * 0.7)
            * p.lock_cyan_strength;
        let lock_amount = lock_amount.clamp(0.0, 1.0);
        let side_fraction = |side: f32| {
            if global > f32::EPSILON {
                (side / global).clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let lock_fl = lock_amount * side_fraction(fl.severity);
        let lock_fr = lock_amount * side_fraction(fr.severity);
        let approach_mix = (p.warning_mix * global).clamp(0.0, 1.0);
        // Approaching must not look like a lock: cap the orange amount below
        // the point where it reads as an alarm.
        let approach_mix = approach_mix.min(0.35);

        self.output = BrakeLimitFeedback {
            severity_fl: fl.severity,
            severity_fr: fr.severity,
            global_severity: global,
            symmetry: fl.severity.min(fr.severity),
            balance: fl.severity - fr.severity,
            state: self.state,
            lock_duration_ms,
            lock_confirmed: self.lock_active,
            pulse,
            burn,
            approach_mix,
            lock_fl,
            lock_fr,
            color_mix: lock_amount.max(approach_mix),
        };
    }

    fn eval_wheel(
        &self,
        slip: Option<f32>,
        wheel_speed: Option<f32>,
        speed_ms: f32,
        gate: f32,
    ) -> WheelEval {
        let p = self.params;
        let slip = slip.unwrap_or(0.0);
        let slip_mag = (-slip).max(0.0);
        let ratio = match wheel_speed {
            Some(wheel) => wheel / speed_ms.max(1.0),
            None => 1.0,
        };
        let slip_norm = smoothstep(p.slip_warn_start, p.slip_full_lock, slip_mag);
        // wheel_norm rises as the wheel slows relative to the car.
        let wheel_norm = 1.0 - smoothstep(p.wheel_lock_ratio, p.wheel_warn_ratio, ratio);
        let raw = 0.65 * slip_norm + 0.35 * wheel_norm;
        let severity = raw.clamp(0.0, 1.0).powf(p.severity_gamma) * gate;
        WheelEval {
            severity: severity.clamp(0.0, 1.0),
            // The state machine sees slip scaled by the speed/brake gate so it
            // cannot fire when those gates are closed.
            slip_mag: slip_mag * gate,
            wheel_ratio: ratio,
        }
    }
}

/// Per-sample confirmed-lock history aligned with `points` by index.
///
/// Runs a fresh detector pass over the window; a sample contributes 0.0 unless
/// a confirmed lock was active, in which case it stores the global severity
/// (floored so even a bare confirmed lock is visible). Time width is the true
/// sample spacing; nothing is widened.
pub fn lock_severity_history(
    params: BrakeLimitParams,
    points: &[crate::core::TelemetryPoint],
) -> Vec<f32> {
    let mut detector = BrakeLimitDetector::new(params);
    let mut history = Vec::with_capacity(points.len());
    let mut previous: Option<std::time::Instant> = None;
    for point in points {
        let dt = previous
            .map(|previous| {
                point
                    .captured_at
                    .duration_since(previous)
                    .as_secs_f32()
            })
            .unwrap_or(1.0 / 60.0);
        detector.update(dt, &WheelSample::from_vehicle(&point.telemetry));
        let value = if detector.lock_confirmed() {
            detector.feedback().global_severity.max(0.5)
        } else {
            0.0
        };
        history.push(value);
        previous = Some(point.captured_at);
    }
    history
}

fn side_lock(current: bool, eval: WheelEval, params: &BrakeLimitParams) -> bool {
    if current {
        eval.slip_mag >= params.slip_lock_exit()
    } else {
        eval.slip_mag >= params.slip_lock_start && eval.wheel_ratio <= params.wheel_warn_ratio
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if (edge1 - edge0).abs() < f32::EPSILON {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn pulse_envelope(elapsed: f32) -> f32 {
    const ATTACK: f32 = 0.06;
    const HOLD: f32 = 0.04;
    const DECAY: f32 = 0.16;
    if elapsed < 0.0 {
        0.0
    } else if elapsed < ATTACK {
        elapsed / ATTACK
    } else if elapsed < ATTACK + HOLD {
        1.0
    } else if elapsed < ATTACK + HOLD + DECAY {
        1.0 - (elapsed - ATTACK - HOLD) / DECAY
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn healthy() -> WheelSample {
        WheelSample {
            speed_ms: 200.0 / 3.6,
            brake: 0.8,
            slip_fl: Some(-0.10),
            slip_fr: Some(-0.10),
            wheel_speed_fl: Some((200.0 / 3.6) * 0.98),
            wheel_speed_fr: Some((200.0 / 3.6) * 0.98),
        }
    }

    fn run(detector: &mut BrakeLimitDetector, sample: &WheelSample, seconds: f32) {
        let steps = (seconds / DT).ceil() as usize;
        for _ in 0..steps {
            detector.update(DT, sample);
        }
    }

    fn fl_lock(slip_fr: f32) -> WheelSample {
        WheelSample {
            speed_ms: 200.0 / 3.6,
            brake: 0.9,
            slip_fl: Some(-1.0),
            slip_fr: Some(slip_fr),
            wheel_speed_fl: Some((200.0 / 3.6) * 0.15),
            wheel_speed_fr: Some((200.0 / 3.6) * 0.95),
        }
    }

    fn fr_lock(slip_fl: f32) -> WheelSample {
        WheelSample {
            speed_ms: 200.0 / 3.6,
            brake: 0.9,
            slip_fl: Some(slip_fl),
            slip_fr: Some(-1.0),
            wheel_speed_fl: Some((200.0 / 3.6) * 0.95),
            wheel_speed_fr: Some((200.0 / 3.6) * 0.15),
        }
    }

    fn dual_lock() -> WheelSample {
        WheelSample {
            speed_ms: 200.0 / 3.6,
            brake: 1.0,
            slip_fl: Some(-1.0),
            slip_fr: Some(-1.0),
            wheel_speed_fl: Some((200.0 / 3.6) * 0.05),
            wheel_speed_fr: Some((200.0 / 3.6) * 0.05),
        }
    }

    #[test]
    fn healthy_braking_stays_normal() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 1.0);
        let out = detector.feedback();
        assert_eq!(out.state, LockState::Normal);
        assert!(out.global_severity < 0.05, "sev {}", out.global_severity);
        assert_eq!(out.pulse, 0.0);
        assert_eq!(out.burn, 0.0);
    }

    #[test]
    fn low_speed_deep_slip_is_speed_gated() {
        let mut detector = BrakeLimitDetector::default();
        let sample = WheelSample {
            speed_ms: 10.0 / 3.6, // below speed_gate_start
            brake: 1.0,
            slip_fl: Some(-1.0),
            slip_fr: Some(-1.0),
            wheel_speed_fl: Some(0.0),
            wheel_speed_fr: Some(0.0),
        };
        run(&mut detector, &sample, 0.5);
        let out = detector.feedback();
        assert_eq!(out.state, LockState::Normal);
        assert!(out.global_severity < 0.01);
    }

    #[test]
    fn low_brake_slip_is_brake_gated() {
        let mut detector = BrakeLimitDetector::default();
        let sample = WheelSample {
            brake: 0.05, // below brake_gate_start
            ..dual_lock()
        };
        run(&mut detector, &sample, 0.5);
        let out = detector.feedback();
        assert_eq!(out.state, LockState::Normal);
        assert!(out.global_severity < 0.01);
    }

    #[test]
    fn fl_only_transient_lock() {
        let mut detector = BrakeLimitDetector::default();
        // Baseline healthy frames engage motion tracking, then FL locks.
        run(&mut detector, &healthy(), 0.2);
        run(&mut detector, &fl_lock(-0.10), 0.15);
        let out = detector.feedback();
        assert_eq!(out.state, LockState::TransientLock);
        assert!(out.severity_fl > 0.5, "fl {}", out.severity_fl);
        assert!(out.severity_fr < 0.2, "fr {}", out.severity_fr);
        assert!(out.pulse > 0.0);
    }

    #[test]
    fn fr_only_transient_lock() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        run(&mut detector, &fr_lock(-0.10), 0.15);
        let out = detector.feedback();
        assert_eq!(out.state, LockState::TransientLock);
        assert!(out.severity_fr > 0.5);
        assert!(out.severity_fl < 0.2);
        assert!(out.balance < 0.0);
    }

    #[test]
    fn dual_front_sustained_lock() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        run(&mut detector, &dual_lock(), 0.6);
        let out = detector.feedback();
        assert_eq!(out.state, LockState::SustainedLock);
        assert!(out.burn > 0.0);
        assert!(out.global_severity > 0.8);
    }

    #[test]
    fn transient_does_not_jump_straight_to_sustained() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        run(&mut detector, &dual_lock(), 0.15); // > attack, < sustained
        assert_eq!(detector.state(), LockState::TransientLock);
    }

    #[test]
    fn sustained_release_enters_recovery_then_normal() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        run(&mut detector, &dual_lock(), 0.6);
        assert_eq!(detector.state(), LockState::SustainedLock);
        // Release and observe the short recovery tail.
        detector.update(DT, &healthy());
        assert_eq!(detector.state(), LockState::Recovery);
        run(&mut detector, &healthy(), 0.3);
        assert!(matches!(
            detector.state(),
            LockState::Normal | LockState::Approaching
        ));
    }

    #[test]
    fn approaching_hysteresis_avoids_flicker() {
        let mut detector = BrakeLimitDetector::default();
        let mut sample = healthy();
        sample.slip_fl = Some(-0.16); // just over enter
        sample.slip_fr = Some(-0.16);
        run(&mut detector, &sample, 0.05);
        assert_eq!(detector.state(), LockState::Approaching);

        // Dip below enter but above exit: must stay approaching.
        sample.slip_fl = Some(-0.12);
        sample.slip_fr = Some(-0.12);
        run(&mut detector, &sample, 0.05);
        assert_eq!(detector.state(), LockState::Approaching);

        // Drop below exit: returns to normal.
        sample.slip_fl = Some(-0.08);
        sample.slip_fr = Some(-0.08);
        run(&mut detector, &sample, 0.05);
        assert_eq!(detector.state(), LockState::Normal);
    }

    #[test]
    fn missing_wheel_data_does_not_trigger_lock() {
        let mut detector = BrakeLimitDetector::default();
        let sample = WheelSample {
            speed_ms: 200.0 / 3.6,
            brake: 1.0,
            slip_fl: None,
            slip_fr: None,
            wheel_speed_fl: None,
            wheel_speed_fr: None,
        };
        run(&mut detector, &sample, 0.5);
        assert_eq!(detector.state(), LockState::Normal);
        assert_eq!(detector.feedback().global_severity, 0.0);
    }

    #[test]
    fn approaching_does_not_emit_cyan_lock() {
        let mut detector = BrakeLimitDetector::default();
        let mut sample = healthy();
        sample.slip_fl = Some(-0.17);
        sample.slip_fr = Some(-0.17);
        run(&mut detector, &sample, 0.15);
        let out = detector.feedback();
        assert_eq!(out.state, LockState::Approaching);
        assert!(!out.lock_confirmed);
        assert_eq!(out.lock_fl, 0.0);
        assert_eq!(out.lock_fr, 0.0);
        assert!(out.approach_mix > 0.0 && out.approach_mix <= 0.35);
    }

    #[test]
    fn fl_only_lock_emphasises_left_only() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        run(&mut detector, &fl_lock(-0.10), 0.15);
        let out = detector.feedback();
        assert!(out.lock_confirmed);
        assert!(out.lock_fl > 0.2, "lock_fl {}", out.lock_fl);
        assert!(out.lock_fr < 0.05, "lock_fr {}", out.lock_fr);
        assert!(out.lock_fl > out.lock_fr);
    }

    #[test]
    fn fr_only_lock_emphasises_right_only() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        run(&mut detector, &fr_lock(-0.10), 0.15);
        let out = detector.feedback();
        assert!(out.lock_fr > 0.2);
        assert!(out.lock_fl < 0.05);
        assert!(out.lock_fr > out.lock_fl);
    }

    #[test]
    fn dual_lock_lights_both_sides() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        run(&mut detector, &dual_lock(), 0.6);
        let out = detector.feedback();
        assert!(out.lock_fl > 0.2);
        assert!(out.lock_fr > 0.2);
    }

    #[test]
    fn transient_pulses_once_then_decays() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        let sample = fl_lock(-0.10);
        let mut peak = 0.0_f32;
        let steps = (0.25 / DT).ceil() as usize;
        for _ in 0..steps {
            detector.update(DT, &sample);
            peak = peak.max(detector.feedback().pulse);
        }
        assert!(peak > 0.3, "pulse peak {peak}");
        // Later in the same transient episode the pulse is decaying rather than
        // re-firing.
        let tail = detector.feedback().pulse;
        assert!(tail < peak, "pulse tail {tail} peak {peak}");
        assert_eq!(detector.state(), LockState::TransientLock);
    }

    #[test]
    fn sustained_does_not_repeat_pulse() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        run(&mut detector, &dual_lock(), 0.6);
        let out = detector.feedback();
        assert_eq!(out.state, LockState::SustainedLock);
        assert_eq!(out.pulse, 0.0);
        assert!(out.burn > 0.0);
    }

    #[test]
    fn recovery_then_relock_pulses_again() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        run(&mut detector, &fl_lock(-0.10), 0.15);
        assert!(detector.feedback().pulse > 0.0);
        // Release into recovery / normal.
        run(&mut detector, &healthy(), 0.4);
        // Re-lock: a new episode must pulse again.
        run(&mut detector, &fl_lock(-0.10), 0.1);
        assert!(detector.feedback().pulse > 0.0);
        assert!(detector.feedback().lock_confirmed);
    }

    fn history_point(
        at: std::time::Instant,
        brake: f32,
        slip_fl: f32,
        slip_fr: f32,
        wheel_ratio: f32,
    ) -> crate::core::TelemetryPoint {
        let speed = 200.0 / 3.6;
        let mut vehicle = crate::core::VehicleTelemetry {
            speed,
            brake,
            wheel_slip: Some([0.0, 0.0, slip_fl, slip_fr]),
            wheel_speed: Some([speed, speed, speed * wheel_ratio, speed * wheel_ratio]),
            ..Default::default()
        };
        vehicle.capabilities.wheel_slip = true;
        vehicle.capabilities.wheel_speed = true;
        crate::core::TelemetryPoint {
            captured_at: at,
            telemetry: vehicle,
            abs_active: false,
            source: Default::default(),
        }
    }

    #[test]
    fn lock_history_records_confirmed_lock_only() {
        let start = std::time::Instant::now();
        let step = std::time::Duration::from_millis(16);
        let mut points = Vec::new();
        for index in 0..30 {
            points.push(history_point(start + step * index, 0.8, -0.10, -0.10, 0.98));
        }
        for index in 30..50 {
            points.push(history_point(start + step * index, 1.0, -1.0, -1.0, 0.05));
        }
        for index in 50..70 {
            points.push(history_point(start + step * index, 0.8, -0.10, -0.10, 0.98));
        }
        let history = lock_severity_history(BrakeLimitParams::default(), &points);
        assert_eq!(history.len(), points.len());
        assert!(history[..30].iter().all(|value| *value == 0.0));
        assert!(history[35..50].iter().any(|value| *value > 0.0));
        // Regain grip: the tail must return to non-lock.
        assert!(history[65..].iter().all(|value| *value == 0.0));
    }

    #[test]
    fn lock_grip_relock_produces_two_separate_regions() {
        let start = std::time::Instant::now();
        let step = std::time::Duration::from_millis(16);
        let mut points = Vec::new();
        for index in 0..20 {
            points.push(history_point(start + step * index, 0.8, -0.10, -0.10, 0.98));
        }
        for index in 20..32 {
            points.push(history_point(start + step * index, 1.0, -1.0, -1.0, 0.05));
        }
        for index in 32..60 {
            points.push(history_point(start + step * index, 0.8, -0.10, -0.10, 0.98));
        }
        for index in 60..72 {
            points.push(history_point(start + step * index, 1.0, -1.0, -1.0, 0.05));
        }
        for index in 72..90 {
            points.push(history_point(start + step * index, 0.8, -0.10, -0.10, 0.98));
        }
        let history = lock_severity_history(BrakeLimitParams::default(), &points);
        let mut regions = 0;
        let mut in_lock = false;
        for value in &history {
            let locked = *value > 0.0;
            if locked && !in_lock {
                regions += 1;
            }
            in_lock = locked;
        }
        assert_eq!(regions, 2, "expected two lock regions");
    }

    #[test]
    fn visual_baselines_match_art_pass() {
        let params = BrakeLimitParams::default();
        assert!((params.warning_mix - 0.20).abs() < 1e-6);
        assert!((params.lock_projection_alpha - 0.30).abs() < 1e-6);
        assert!((params.lock_cyan_strength - 1.4).abs() < 1e-6);
    }

    #[test]
    fn confirmed_lock_cyan_dominates_normal_red() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        // Healthy braking produces no cyan at all.
        assert_eq!(detector.feedback().lock_fl, 0.0);
        assert_eq!(detector.feedback().lock_fr, 0.0);
        run(&mut detector, &dual_lock(), 0.6);
        let out = detector.feedback();
        assert!(
            out.lock_fl > 0.7 && out.lock_fr > 0.7,
            "cyan dominance too low: L {} R {}",
            out.lock_fl,
            out.lock_fr
        );
    }

    #[test]
    fn recovery_returns_full_red_without_cyan() {
        let mut detector = BrakeLimitDetector::default();
        run(&mut detector, &healthy(), 0.2);
        run(&mut detector, &dual_lock(), 0.6);
        run(&mut detector, &healthy(), 0.4);
        let out = detector.feedback();
        assert_eq!(out.lock_fl, 0.0);
        assert_eq!(out.lock_fr, 0.0);
    }
}
