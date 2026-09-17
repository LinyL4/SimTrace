use anyhow::{bail, Context, Result};

use crate::core::{
    DriverAssistStatus, SourceMetadata, TelemetryCapabilities, TelemetryData, VehicleTelemetry,
};

pub const HEADER_LEN: usize = 29;

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
}

pub struct VehicleFields {
    pub speed_kph: u16,
    pub throttle: f32,
    pub steer: f32,
    pub brake: f32,
    pub clutch: u8,
    pub gear: i8,
    pub rpm: u16,
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
            bail!("invalid F1 assist status");
        }
        self.assists = DriverAssistStatus {
            abs_enabled: Some(abs == 1),
            traction_control_level: Some(tc),
        };
        self.assists_known = true;
        Ok(())
    }

    pub fn make_telemetry(
        &mut self,
        format: u16,
        header: Header,
        fields: VehicleFields,
    ) -> Result<Option<TelemetryData>> {
        if self
            .last_telemetry_frame
            .is_some_and(|last| header.overall_frame_identifier <= last)
        {
            return Ok(None);
        }
        if !fields.throttle.is_finite() || !fields.steer.is_finite() || !fields.brake.is_finite() {
            bail!("non-finite F1 telemetry input");
        }
        let track_position = match (self.player_lap_distance_m, self.track_length_m) {
            (Some(distance), Some(length)) if length > 0.0 => {
                Some((distance / length).rem_euclid(1.0))
            }
            _ => None,
        };
        self.last_telemetry_frame = Some(header.overall_frame_identifier);
        let discontinuity = std::mem::take(&mut self.pending_discontinuity);

        Ok(Some(TelemetryData {
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
                abs_active: false,
                tc_active: false,
                track_position: track_position.unwrap_or(0.0),
                handbrake: 0.0,
                wheel_slip: None,
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
        }))
    }
}

pub fn packet_format(bytes: &[u8]) -> Result<u16> {
    read_u16(bytes, 0).context("F1 packet missing format identifier")
}

/// Both current official specifications define this exact 29-byte header.
/// Variant decoders still supply and validate their own identity and car limit.
pub fn parse_header(
    bytes: &[u8],
    expected_format: u16,
    expected_game_year: u8,
    max_cars: usize,
) -> Result<Header> {
    if bytes.len() < HEADER_LEN {
        bail!("F1 packet shorter than header");
    }
    let format = packet_format(bytes)?;
    if format != expected_format {
        bail!("wrong F1 UDP format {format}; expected {expected_format}");
    }
    let game_year = bytes[2];
    if game_year != 0 && game_year != expected_game_year {
        bail!("wrong F1 game year {game_year}; expected {expected_game_year}");
    }
    let player_car_index = bytes[27] as usize;
    if player_car_index >= max_cars {
        bail!("invalid F1 player car index {player_car_index}");
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
        bail!("unsupported F1 packet version {version}");
    }
    if bytes.len() != expected_len {
        bail!(
            "wrong F1 packet length {}; expected {expected_len}",
            bytes.len()
        );
    }
    Ok(())
}

pub fn player_offset(index: usize, item_len: usize) -> Result<usize> {
    index
        .checked_mul(item_len)
        .and_then(|offset| HEADER_LEN.checked_add(offset))
        .context("F1 player data offset overflow")
}

pub fn read_u8(bytes: &[u8], offset: usize) -> Result<u8> {
    bytes.get(offset).copied().context("truncated F1 u8")
}

pub fn read_i8(bytes: &[u8], offset: usize) -> Result<i8> {
    Ok(read_u8(bytes, offset)? as i8)
}

pub fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .context("truncated F1 u16")?
            .try_into()?,
    ))
}

pub fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .context("truncated F1 u32")?
            .try_into()?,
    ))
}

pub fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .context("truncated F1 u64")?
            .try_into()?,
    ))
}

pub fn read_f32(bytes: &[u8], offset: usize) -> Result<f32> {
    Ok(f32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .context("truncated F1 f32")?
            .try_into()?,
    ))
}
