//! Decoder for the official F1 25: 2026 Season Pack UDP layout.

use anyhow::Result;

use crate::core::TelemetryData;

use super::protocol::{self, DecoderState, VehicleFields, HEADER_LEN};

pub const PACKET_FORMAT: u16 = 2026;
const GAME_YEAR: u8 = 26;
const MAX_CARS: usize = 24;
const SESSION_PACKET_LEN: usize = 926;
const LAP_PACKET_LEN: usize = 1399;
const CAR_TELEMETRY_PACKET_LEN: usize = 1448;
const CAR_STATUS_PACKET_LEN: usize = 1445;
const LAP_DATA_LEN: usize = 57;
const CAR_TELEMETRY_DATA_LEN: usize = 59;
const CAR_STATUS_DATA_LEN: usize = 59;
const PACKET_SESSION: u8 = 1;
const PACKET_LAP_DATA: u8 = 2;
const PACKET_CAR_TELEMETRY: u8 = 6;
const PACKET_CAR_STATUS: u8 = 7;

#[derive(Default)]
pub struct Decoder {
    state: DecoderState,
}

impl Decoder {
    pub fn decode(&mut self, bytes: &[u8]) -> Result<Option<TelemetryData>> {
        let header = protocol::parse_header(bytes, PACKET_FORMAT, GAME_YEAR, MAX_CARS)?;
        self.state.observe_session(header.session_uid);
        match header.packet_id {
            PACKET_SESSION => {
                protocol::require_packet(bytes, SESSION_PACKET_LEN, header.packet_version)?;
                self.state
                    .set_track_length(protocol::read_u16(bytes, HEADER_LEN + 4)? as f32);
                Ok(None)
            }
            PACKET_LAP_DATA => {
                protocol::require_packet(bytes, LAP_PACKET_LEN, header.packet_version)?;
                let base = protocol::player_offset(header.player_car_index, LAP_DATA_LEN)?;
                self.state
                    .set_lap_distance(protocol::read_f32(bytes, base + 20)?);
                Ok(None)
            }
            PACKET_CAR_STATUS => {
                protocol::require_packet(bytes, CAR_STATUS_PACKET_LEN, header.packet_version)?;
                let base = protocol::player_offset(header.player_car_index, CAR_STATUS_DATA_LEN)?;
                self.state.set_assists(
                    protocol::read_u8(bytes, base)?,
                    protocol::read_u8(bytes, base + 1)?,
                )?;
                Ok(None)
            }
            PACKET_CAR_TELEMETRY => {
                protocol::require_packet(bytes, CAR_TELEMETRY_PACKET_LEN, header.packet_version)?;
                let base =
                    protocol::player_offset(header.player_car_index, CAR_TELEMETRY_DATA_LEN)?;
                self.state.make_telemetry(
                    PACKET_FORMAT,
                    header,
                    VehicleFields {
                        speed_kph: protocol::read_u16(bytes, base)?,
                        throttle: protocol::read_f32(bytes, base + 2)?,
                        steer: protocol::read_f32(bytes, base + 6)?,
                        brake: protocol::read_f32(bytes, base + 10)?,
                        clutch: protocol::read_u8(bytes, base + 14)?,
                        gear: protocol::read_i8(bytes, base + 15)?,
                        rpm: protocol::read_u16(bytes, base + 16)?,
                    },
                )
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
pub(super) fn test_telemetry_packet(player: usize, frame: u32) -> Vec<u8> {
    let mut bytes = test_packet(
        PACKET_CAR_TELEMETRY,
        CAR_TELEMETRY_PACKET_LEN,
        player,
        frame,
    );
    let base = HEADER_LEN + player * CAR_TELEMETRY_DATA_LEN;
    bytes[base..base + 2].copy_from_slice(&288_u16.to_le_bytes());
    bytes[base + 2..base + 6].copy_from_slice(&0.75_f32.to_le_bytes());
    bytes[base + 6..base + 10].copy_from_slice(&(-0.5_f32).to_le_bytes());
    bytes[base + 10..base + 14].copy_from_slice(&0.25_f32.to_le_bytes());
    bytes[base + 14] = 40;
    bytes[base + 15] = 7;
    bytes[base + 16..base + 18].copy_from_slice(&12_345_u16.to_le_bytes());
    bytes
}

#[cfg(test)]
fn test_packet(id: u8, len: usize, player: usize, frame: u32) -> Vec<u8> {
    let mut bytes = vec![0_u8; len];
    bytes[0..2].copy_from_slice(&PACKET_FORMAT.to_le_bytes());
    bytes[2] = GAME_YEAR;
    bytes[5] = 1;
    bytes[6] = id;
    bytes[7..15].copy_from_slice(&5678_u64.to_le_bytes());
    bytes[15..19].copy_from_slice(&12.5_f32.to_le_bytes());
    bytes[19..23].copy_from_slice(&frame.to_le_bytes());
    bytes[23..27].copy_from_slice(&frame.to_le_bytes());
    bytes[27] = player as u8;
    bytes[28] = 255;
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_native_2026_player_values_and_identity() {
        let data = Decoder::default()
            .decode(&test_telemetry_packet(23, 7))
            .unwrap()
            .unwrap();
        assert_eq!(data.vehicle.speed, 80.0);
        assert_eq!(data.vehicle.throttle, 0.75);
        assert_eq!(data.vehicle.brake, 0.25);
        assert_eq!(data.vehicle.clutch, 0.4);
        assert_eq!(data.vehicle.steering_input, -0.5);
        assert_eq!(data.vehicle.gear, 7);
        assert_eq!(data.vehicle.rpm, 12_345.0);
        assert_eq!(data.source.protocol_format, Some(2026));
    }

    #[test]
    fn validates_2026_version_length_year_and_car_limit() {
        let mut wrong_version = test_telemetry_packet(0, 1);
        wrong_version[5] = 2;
        assert!(Decoder::default().decode(&wrong_version).is_err());
        let mut wrong_year = test_telemetry_packet(0, 1);
        wrong_year[2] = 25;
        assert!(Decoder::default().decode(&wrong_year).is_err());
        assert!(Decoder::default()
            .decode(&test_telemetry_packet(0, 1)[..100])
            .is_err());
        let mut wrong_car = test_telemetry_packet(0, 1);
        wrong_car[27] = 24;
        assert!(Decoder::default().decode(&wrong_car).is_err());
    }

    #[test]
    fn uses_2026_specific_item_sizes_for_player_selection() {
        let mut bytes = test_telemetry_packet(5, 8);
        bytes[HEADER_LEN + 2..HEADER_LEN + 6].copy_from_slice(&0.1_f32.to_le_bytes());
        let data = Decoder::default().decode(&bytes).unwrap().unwrap();
        assert_eq!(data.vehicle.throttle, 0.75);
    }

    #[test]
    fn ignores_stale_telemetry_frames() {
        let mut decoder = Decoder::default();
        assert!(decoder
            .decode(&test_telemetry_packet(0, 10))
            .unwrap()
            .is_some());
        assert!(decoder
            .decode(&test_telemetry_packet(0, 10))
            .unwrap()
            .is_none());
        assert!(decoder
            .decode(&test_telemetry_packet(0, 9))
            .unwrap()
            .is_none());
    }

    #[test]
    fn preserves_2026_assists_and_track_position() {
        let mut decoder = Decoder::default();
        let mut session = test_packet(PACKET_SESSION, SESSION_PACKET_LEN, 2, 1);
        session[HEADER_LEN + 4..HEADER_LEN + 6].copy_from_slice(&5000_u16.to_le_bytes());
        decoder.decode(&session).unwrap();
        let mut lap = test_packet(PACKET_LAP_DATA, LAP_PACKET_LEN, 2, 2);
        let lap_base = HEADER_LEN + 2 * LAP_DATA_LEN;
        lap[lap_base + 20..lap_base + 24].copy_from_slice(&1250.0_f32.to_le_bytes());
        decoder.decode(&lap).unwrap();
        let mut status = test_packet(PACKET_CAR_STATUS, CAR_STATUS_PACKET_LEN, 2, 3);
        let status_base = HEADER_LEN + 2 * CAR_STATUS_DATA_LEN;
        status[status_base] = 2;
        status[status_base + 1] = 1;
        decoder.decode(&status).unwrap();
        let data = decoder
            .decode(&test_telemetry_packet(2, 4))
            .unwrap()
            .unwrap();
        assert_eq!(data.vehicle.track_position, 0.25);
        assert_eq!(data.vehicle.assists.abs_enabled, Some(true));
        assert_eq!(data.vehicle.assists.traction_control_level, Some(2));
    }
}
