use std::error::Error;
use std::fmt;

use anyhow::Result;

use crate::core::{
    DriverAssistStatus, RevLights, SourceMetadata, TelemetryCapabilities, TelemetryData,
    VehicleTelemetry,
};

pub const HEADER_LEN: usize = 29;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    HeaderLength,
    HeaderField,
    PacketFormat,
    GameYear,
    PacketVersion,
    PacketLength,
    PlayerIndex,
    PayloadField,
    InvalidValue,
    Other,
}

impl RejectReason {
    pub const ALL: [Self; 10] = [
        Self::HeaderLength,
        Self::HeaderField,
        Self::PacketFormat,
        Self::GameYear,
        Self::PacketVersion,
        Self::PacketLength,
        Self::PlayerIndex,
        Self::PayloadField,
        Self::InvalidValue,
        Self::Other,
    ];
    pub const COUNT: usize = Self::ALL.len();

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HeaderLength => "header_length",
            Self::HeaderField => "header_field",
            Self::PacketFormat => "packet_format",
            Self::GameYear => "game_year",
            Self::PacketVersion => "packet_version",
            Self::PacketLength => "packet_length",
            Self::PlayerIndex => "player_index",
            Self::PayloadField => "payload_field",
            Self::InvalidValue => "invalid_value",
            Self::Other => "other",
        }
    }
}

#[derive(Debug)]
struct DecodeRejection {
    reason: RejectReason,
    detail: String,
}

impl fmt::Display for DecodeRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for DecodeRejection {}

pub fn reject(reason: RejectReason, detail: impl Into<String>) -> anyhow::Error {
    DecodeRejection {
        reason,
        detail: detail.into(),
    }
    .into()
}

pub fn reject_reason(error: &anyhow::Error) -> RejectReason {
    error
        .downcast_ref::<DecodeRejection>()
        .map(|rejection| rejection.reason)
        .unwrap_or(RejectReason::Other)
}

#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub packet_version: u8,
    pub packet_id: u8,
    pub session_uid: u64,
    pub session_time: f32,
    pub frame_identifier: u32,
    pub overall_frame_identifier: u32,
    pub player_car_index: usize,
}

#[derive(Default)]
pub struct DecoderState {
    session_uid: Option<u64>,
    pending_discontinuity: bool,
    track_length_m: Option<f32>,
    player_lap_distance_m: Option<f32>,
    assists: DriverAssistStatus,
    assists_known: bool,
    last_telemetry_frame: Option<u32>,
    /// Set once a MotionEx packet is observed in the session. Until then
    /// CarTelemetry is emitted immediately (unchanged legacy behaviour);
    /// afterwards CarTelemetry and MotionEx are paired by overall frame.
    motion_seen: bool,
    /// Most recent CarTelemetry frame waiting for its MotionEx counterpart.
    pending_car: Option<(Header, VehicleFields)>,
    /// Most recent MotionEx frame waiting for its CarTelemetry counterpart.
    pending_motion: Option<(u32, MotionFields)>,
}

#[derive(Clone, Copy)]
pub struct VehicleFields {
    pub speed_kph: u16,
    pub throttle: f32,
    pub steer: f32,
    pub brake: f32,
    pub clutch: u8,
    pub gear: i8,
    pub rpm: u16,
    pub rev_lights_percent: u8,
    pub rev_lights_bit_value: u16,
}

/// Player-only MotionEx channels used by the lock-up detector.
///
/// All wheel arrays use the EA order RL, RR, FL, FR.
#[derive(Clone, Copy, Default)]
pub struct MotionFields {
    pub wheel_speed: [f32; 4],
    pub wheel_slip_ratio: [f32; 4],
}

impl DecoderState {
    pub fn observe_session(&mut self, uid: u64) {
        if self.session_uid.is_some_and(|previous| previous != uid) {
            self.pending_discontinuity = true;
            self.track_length_m = None;
            self.player_lap_distance_m = None;
            self.assists = DriverAssistStatus::default();
            self.assists_known = false;
            self.last_telemetry_frame = None;
            self.motion_seen = false;
            self.pending_car = None;
            self.pending_motion = None;
        }
        self.session_uid = Some(uid);
    }

    pub fn set_track_length(&mut self, length: f32) {
        self.track_length_m = (length > 0.0).then_some(length);
    }

    pub fn set_lap_distance(&mut self, distance: f32) {
        self.player_lap_distance_m = distance.is_finite().then_some(distance);
    }

    pub fn set_assists(&mut self, tc: u8, abs: u8) -> Result<()> {
        if tc > 2 || abs > 1 {
            return Err(reject(
                RejectReason::InvalidValue,
                format!("invalid F1 assist status: TC={tc}, ABS={abs}"),
            ));
        }
        self.assists = DriverAssistStatus {
            abs_enabled: Some(abs == 1),
            traction_control_level: Some(tc),
        };
        self.assists_known = true;
        Ok(())
    }

    fn take_motion_for(&mut self, overall_frame: u32) -> Option<MotionFields> {
        match self.pending_motion {
            Some((frame, motion)) if frame == overall_frame => {
                self.pending_motion = None;
                Some(motion)
            }
            _ => None,
        }
    }

    /// Feed one CarTelemetry sample. Returns a merged [`TelemetryData`] when a
    /// complete session/frame pair is available.
    pub fn push_car(
        &mut self,
        format: u16,
        header: Header,
        fields: VehicleFields,
    ) -> Result<Option<TelemetryData>> {
        if !fields.throttle.is_finite() || !fields.steer.is_finite() || !fields.brake.is_finite() {
            return Err(reject(
                RejectReason::InvalidValue,
                "non-finite F1 telemetry input",
            ));
        }
        if self
            .last_telemetry_frame
            .is_some_and(|last| header.overall_frame_identifier <= last)
        {
            return Ok(None);
        }

        if !self.motion_seen {
            let motion = self.take_motion_for(header.overall_frame_identifier);
            return self.finish(format, header, fields, motion).map(Some);
        }

        if let Some((pending_header, pending_fields)) = self.pending_car.take() {
            if pending_header.overall_frame_identifier == header.overall_frame_identifier {
                // Duplicate CarTelemetry for the held frame: keep the newest.
                self.pending_car = Some((header, fields));
                if let Some(motion) = self.take_motion_for(header.overall_frame_identifier) {
                    let (held_header, held_fields) = self
                        .pending_car
                        .take()
                        .expect("pending car was just set");
                    return self
                        .finish(format, held_header, held_fields, Some(motion))
                        .map(Some);
                }
                return Ok(None);
            }
            // Emit the older held frame, then hold this newer one.
            let motion = self.take_motion_for(pending_header.overall_frame_identifier);
            let emitted = self.finish(format, pending_header, pending_fields, motion)?;
            self.pending_car = Some((header, fields));
            return Ok(Some(emitted));
        }

        if let Some(motion) = self.take_motion_for(header.overall_frame_identifier) {
            return self.finish(format, header, fields, Some(motion)).map(Some);
        }
        self.pending_car = Some((header, fields));
        Ok(None)
    }

    /// Feed one player-only MotionEx sample. Returns a merged [`TelemetryData`]
    /// when it completes a held CarTelemetry frame.
    pub fn observe_motion(
        &mut self,
        format: u16,
        header: &Header,
        motion: MotionFields,
    ) -> Result<Option<TelemetryData>> {
        self.motion_seen = true;
        self.pending_motion = Some((header.overall_frame_identifier, motion));
        if let Some((car_header, car_fields)) = self.pending_car.take() {
            if car_header.overall_frame_identifier == header.overall_frame_identifier {
                let motion = self.pending_motion.take().map(|(_, motion)| motion);
                return self.finish(format, car_header, car_fields, motion).map(Some);
            }
            self.pending_car = Some((car_header, car_fields));
        }
        Ok(None)
    }

    fn finish(
        &mut self,
        format: u16,
        header: Header,
        fields: VehicleFields,
        motion: Option<MotionFields>,
    ) -> Result<TelemetryData> {
        let track_position = match (self.player_lap_distance_m, self.track_length_m) {
            (Some(distance), Some(length)) if length > 0.0 => {
                Some((distance / length).rem_euclid(1.0))
            }
            _ => None,
        };
        self.last_telemetry_frame = Some(header.overall_frame_identifier);
        let discontinuity = std::mem::take(&mut self.pending_discontinuity);
        let has_wheel = motion.is_some();

        Ok(TelemetryData {
            timestamp: (header.session_time.max(0.0) * 1000.0) as u64,
            vehicle: VehicleTelemetry {
                throttle: fields.throttle.clamp(0.0, 1.0),
                brake: fields.brake.clamp(0.0, 1.0),
                clutch: (fields.clutch as f32 / 100.0).clamp(0.0, 1.0),
                steering_angle: fields.steer.clamp(-1.0, 1.0) * 180.0,
                steering_input: fields.steer.clamp(-1.0, 1.0),
                wheel_angle_degrees: None,
                speed: fields.speed_kph as f32 / 3.6,
                gear: fields.gear as i32,
                rpm: fields.rpm as f32,
                rev_lights: Some(RevLights {
                    percent: fields.rev_lights_percent,
                    bit_value: fields.rev_lights_bit_value,
                }),
                abs_active: false,
                tc_active: false,
                track_position: track_position.unwrap_or(0.0),
                handbrake: 0.0,
                wheel_slip: motion.map(|motion| motion.wheel_slip_ratio),
                wheel_speed: motion.map(|motion| motion.wheel_speed),
                assists: self
                    .assists_known
                    .then_some(self.assists)
                    .unwrap_or_default(),
                capabilities: TelemetryCapabilities {
                    throttle: true,
                    brake: true,
                    clutch: true,
                    steering_input: true,
                    speed: true,
                    gear: true,
                    rpm: true,
                    track_position: track_position.is_some(),
                    wheel_slip: has_wheel,
                    wheel_speed: has_wheel,
                    ..Default::default()
                },
            },
            session: None,
            source: SourceMetadata {
                protocol_format: Some(format),
                session_uid: Some(header.session_uid),
                frame_identifier: Some(header.frame_identifier),
                overall_frame_identifier: Some(header.overall_frame_identifier),
                packet_time_seconds: Some(header.session_time),
                discontinuity,
            },
        })
    }
}

pub fn packet_format(bytes: &[u8]) -> Result<u16> {
    if bytes.len() < 2 {
        return Err(reject(
            RejectReason::HeaderField,
            "F1 packet missing format identifier",
        ));
    }
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Both current official specifications define this exact 29-byte header.
/// Variant decoders still supply and validate their own identity and car limit.
pub fn parse_header(
    bytes: &[u8],
    expected_format: u16,
    accepted_game_years: &[u8],
    max_cars: usize,
) -> Result<Header> {
    if bytes.len() < HEADER_LEN {
        return Err(reject(
            RejectReason::HeaderLength,
            format!(
                "F1 packet shorter than header: {}; expected at least {HEADER_LEN}",
                bytes.len()
            ),
        ));
    }
    let format = packet_format(bytes)?;
    if format != expected_format {
        return Err(reject(
            RejectReason::PacketFormat,
            format!("wrong F1 UDP format {format}; expected {expected_format}"),
        ));
    }
    let game_year = bytes[2];
    if game_year != 0 && !accepted_game_years.contains(&game_year) {
        return Err(reject(
            RejectReason::GameYear,
            format!("wrong F1 game year {game_year}; expected one of {accepted_game_years:?}"),
        ));
    }
    let player_car_index = bytes[27] as usize;
    if player_car_index >= max_cars {
        return Err(reject(
            RejectReason::PlayerIndex,
            format!("invalid F1 player car index {player_car_index}; car count is {max_cars}"),
        ));
    }
    Ok(Header {
        packet_version: bytes[5],
        packet_id: bytes[6],
        session_uid: read_u64(bytes, 7)?,
        session_time: read_f32(bytes, 15)?,
        frame_identifier: read_u32(bytes, 19)?,
        overall_frame_identifier: read_u32(bytes, 23)?,
        player_car_index,
    })
}

pub fn require_packet(bytes: &[u8], expected_len: usize, version: u8) -> Result<()> {
    if version != 1 {
        return Err(reject(
            RejectReason::PacketVersion,
            format!("unsupported F1 packet version {version}; expected 1"),
        ));
    }
    if bytes.len() != expected_len {
        return Err(reject(
            RejectReason::PacketLength,
            format!(
                "wrong F1 packet length {}; expected {expected_len}",
                bytes.len()
            ),
        ));
    }
    Ok(())
}

pub fn player_offset(index: usize, item_len: usize) -> Result<usize> {
    index
        .checked_mul(item_len)
        .and_then(|offset| HEADER_LEN.checked_add(offset))
        .ok_or_else(|| reject(RejectReason::PlayerIndex, "F1 player data offset overflow"))
}

pub fn read_u8(bytes: &[u8], offset: usize) -> Result<u8> {
    bytes
        .get(offset)
        .copied()
        .ok_or_else(|| reject(RejectReason::PayloadField, "truncated F1 u8"))
}

pub fn read_i8(bytes: &[u8], offset: usize) -> Result<i8> {
    Ok(read_u8(bytes, offset)? as i8)
}

pub fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| reject(RejectReason::PayloadField, "truncated F1 u16"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

pub fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| reject(RejectReason::PayloadField, "truncated F1 u32"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

pub fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| reject(RejectReason::PayloadField, "truncated F1 u64"))?;
    Ok(u64::from_le_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

pub fn read_f32(bytes: &[u8], offset: usize) -> Result<f32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| reject(RejectReason::PayloadField, "truncated F1 f32"))?;
    Ok(f32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

/// Read a `float[4]` from `offset`, preserving the on-wire order.
pub fn read_arr4(bytes: &[u8], offset: usize) -> Result<[f32; 4]> {
    Ok([
        read_f32(bytes, offset)?,
        read_f32(bytes, offset + 4)?,
        read_f32(bytes, offset + 8)?,
        read_f32(bytes, offset + 12)?,
    ])
}
