//! Common telemetry model - normalized data structure for all games
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Declares which normalized channels contain real provider data.
///
/// Numeric fields remain available for existing renderers, while this mask makes
/// an unsupported channel distinct from a legitimate zero/false value.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TelemetryCapabilities {
    pub throttle: bool,
    pub brake: bool,
    pub clutch: bool,
    pub steering_input: bool,
    pub wheel_angle_degrees: bool,
    pub speed: bool,
    pub gear: bool,
    pub rpm: bool,
    pub abs_activity: bool,
    pub tc_activity: bool,
    pub handbrake: bool,
    pub track_position: bool,
    pub wheel_slip: bool,
    /// Per-wheel rotational speed is available (used by F1 MotionEx).
    pub wheel_speed: bool,
}

/// Status reported by the game, distinct from momentary ABS/TC intervention.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DriverAssistStatus {
    pub abs_enabled: Option<bool>,
    /// F1 convention: 0 = off, 1 = medium, 2 = full.
    pub traction_control_level: Option<u8>,
}

/// Shift-light state supplied by a telemetry provider.
///
/// F1 exposes both the percentage and the exact 15-light bitfield. Keeping the
/// pair optional distinguishes unsupported providers from a legitimate all-off
/// state without adding a capability bit for one provider-specific detail.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RevLights {
    pub percent: u8,
    pub bit_value: u16,
}

/// Optional packet-origin metadata. `captured_at` on [`TelemetryPoint`] remains
/// the authoritative local monotonic receipt time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
pub struct SourceMetadata {
    #[serde(default)]
    pub protocol_format: Option<u16>,
    pub session_uid: Option<u64>,
    pub frame_identifier: Option<u32>,
    pub overall_frame_identifier: Option<u32>,
    pub packet_time_seconds: Option<f32>,
    pub discontinuity: bool,
}

/// Main telemetry data structure returned by game plugins
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryData {
    /// Timestamp from the game (if available)
    pub timestamp: u64,
    /// Vehicle telemetry data
    pub vehicle: VehicleTelemetry,
    /// Session information (optional, for future features)
    pub session: Option<SessionInfo>,
    #[serde(default)]
    pub source: SourceMetadata,
}

/// Normalized vehicle telemetry
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VehicleTelemetry {
    /// Throttle position: 0.0 (no throttle) to 1.0 (full throttle)
    pub throttle: f32,
    /// Brake position: 0.0 (no brake) to 1.0 (full brake)
    pub brake: f32,
    /// Clutch position: 0.0 (no clutch) to 1.0 (full clutch)
    pub clutch: f32,
    /// Legacy renderer steering angle. Check `wheel_angle_degrees` and its
    /// capability before treating this as a physical wheel angle.
    pub steering_angle: f32,
    /// Normalized steering input: -1.0 (left) to 1.0 (right).
    pub steering_input: f32,
    /// Physical wheel angle when a provider exposes it directly.
    pub wheel_angle_degrees: Option<f32>,
    /// Vehicle speed in m/s
    pub speed: f32,
    /// Current gear (1-7, 0 = neutral, -1 = reverse)
    pub gear: i32,
    /// Engine RPM
    pub rpm: f32,
    /// Provider-supplied shift-light state, when available.
    pub rev_lights: Option<RevLights>,
    /// ABS is currently active
    pub abs_active: bool,
    /// Traction control is currently active
    pub tc_active: bool,
    /// Track position: 0.0 to 1.0 along the track
    pub track_position: f32,
    /// Handbrake input: 0.0 to 1.0.
    pub handbrake: f32,
    /// Provider-defined normalized wheel slip, RL/RR/FL/FR when available.
    pub wheel_slip: Option<[f32; 4]>,
    /// Provider-supplied per-wheel speed, RL/RR/FL/FR when available.
    ///
    /// Added for the F1 MotionEx lock-up detector, which needs wheel speed to
    /// confirm a real lock against wheel-slip ratio. Providers without this
    /// channel leave it `None`. Units are provider-defined (raw F1 MotionEx
    /// wheel speed; unit is not documented by EA).
    pub wheel_speed: Option<[f32; 4]>,
    pub assists: DriverAssistStatus,
    pub capabilities: TelemetryCapabilities,
}

/// Session information (optional)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionInfo {
    /// Session number
    pub session_number: u32,
    /// Session type (practice, qualifying, race, etc.)
    pub session_type: String,
    /// Total session time in seconds
    pub session_time: f32,
    /// Track length in meters
    pub track_length: f32,
    /// Track name
    pub track_name: String,
    /// Car name
    pub car_name: String,
}

/// A telemetry point stored in the buffer
#[derive(Debug, Clone)]
pub struct TelemetryPoint {
    /// When this point was captured
    pub captured_at: Instant,
    /// Telemetry data
    pub telemetry: VehicleTelemetry,
    /// ABS state at capture time (persisted for coloring)
    pub abs_active: bool,
    pub source: SourceMetadata,
}

impl TelemetryPoint {
    pub fn new(data: TelemetryData) -> Self {
        let abs_active = data.vehicle.abs_active;
        Self {
            captured_at: Instant::now(),
            telemetry: data.vehicle,
            abs_active,
            source: data.source,
        }
    }
}

impl VehicleTelemetry {
    /// Create a new vehicle telemetry with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if any pedal is being pressed
    pub fn has_pedal_input(&self) -> bool {
        self.throttle > 0.01 || self.brake > 0.01 || self.clutch > 0.01
    }

    /// Get the maximum pedal input (throttle or brake)
    pub fn max_pedal(&self) -> f32 {
        self.throttle.max(self.brake)
    }

    /// Steering angle for existing visualizations. Providers with a physical
    /// angle use it; normalized-only providers use the configured half-lock.
    pub fn effective_steering_degrees(&self, half_lock_degrees: f32) -> f32 {
        self.wheel_angle_degrees
            .unwrap_or(self.steering_input.clamp(-1.0, 1.0) * half_lock_degrees)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pedal_input_detection() {
        let telemetry = VehicleTelemetry {
            throttle: 0.5,
            brake: 0.0,
            ..Default::default()
        };
        assert!(telemetry.has_pedal_input());

        let telemetry = VehicleTelemetry::default();
        assert!(!telemetry.has_pedal_input());
    }

    #[test]
    fn test_max_pedal() {
        let telemetry = VehicleTelemetry {
            throttle: 0.3,
            brake: 0.7,
            ..Default::default()
        };
        assert_eq!(telemetry.max_pedal(), 0.7);
    }

    #[test]
    fn unsupported_is_distinct_from_zero() {
        let unsupported = VehicleTelemetry::default();
        let supported_zero = VehicleTelemetry {
            capabilities: TelemetryCapabilities {
                throttle: true,
                abs_activity: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(unsupported.throttle, supported_zero.throttle);
        assert!(!unsupported.capabilities.throttle);
        assert!(supported_zero.capabilities.throttle);
        assert!(!unsupported.capabilities.abs_activity);
        assert!(supported_zero.capabilities.abs_activity);
    }

    #[test]
    fn unsupported_rev_lights_are_distinct_from_all_off() {
        let unsupported = VehicleTelemetry::default();
        let all_off = VehicleTelemetry {
            rev_lights: Some(RevLights {
                percent: 0,
                bit_value: 0,
            }),
            ..Default::default()
        };

        assert_eq!(unsupported.rev_lights, None);
        assert_eq!(all_off.rev_lights, Some(RevLights::default()));
    }
}
