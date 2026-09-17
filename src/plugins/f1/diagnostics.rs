use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::Error;
use tracing::{info, warn};

use super::protocol::{self, RejectReason, HEADER_LEN};
use crate::core::TelemetryData;

const DISTINCT_HEADER_LOG_LIMIT: usize = 12;
const REJECTION_LOG_LIMIT_PER_REASON: u64 = 3;
const SUMMARY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HeaderPreview {
    pub packet_format: u16,
    pub game_year: u8,
    pub game_major_version: u8,
    pub game_minor_version: u8,
    pub packet_version: u8,
    pub packet_id: u8,
    pub player_car_index: u8,
    pub packet_len: usize,
}

impl HeaderPreview {
    fn read(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER_LEN {
            return None;
        }
        Some(Self {
            packet_format: u16::from_le_bytes([bytes[0], bytes[1]]),
            game_year: bytes[2],
            game_major_version: bytes[3],
            game_minor_version: bytes[4],
            packet_version: bytes[5],
            packet_id: bytes[6],
            player_car_index: bytes[27],
            packet_len: bytes.len(),
        })
    }
}

pub struct Diagnostics {
    datagrams_received: u64,
    headers_recognized: u64,
    format_2025_detected: u64,
    format_2026_detected: u64,
    other_formats: u64,
    packets_validated: u64,
    player_indices_validated: u64,
    telemetry_decoded: u64,
    status_decoded: u64,
    normalized_frames: u64,
    stale_telemetry_frames: u64,
    ignored_packet_ids: u64,
    packet_ids: [u64; 256],
    rejected: [u64; RejectReason::COUNT],
    distinct_headers: HashSet<HeaderPreview>,
    last_summary_at: Instant,
    last_summary_received: u64,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            datagrams_received: 0,
            headers_recognized: 0,
            format_2025_detected: 0,
            format_2026_detected: 0,
            other_formats: 0,
            packets_validated: 0,
            player_indices_validated: 0,
            telemetry_decoded: 0,
            status_decoded: 0,
            normalized_frames: 0,
            stale_telemetry_frames: 0,
            ignored_packet_ids: 0,
            packet_ids: [0; 256],
            rejected: [0; RejectReason::COUNT],
            distinct_headers: HashSet::new(),
            last_summary_at: Instant::now(),
            last_summary_received: 0,
        }
    }
}

impl Diagnostics {
    pub fn observe_datagram(&mut self, bytes: &[u8], source: SocketAddr) -> Option<HeaderPreview> {
        self.datagrams_received += 1;
        let Some(header) = HeaderPreview::read(bytes) else {
            return None;
        };
        self.headers_recognized += 1;
        self.packet_ids[header.packet_id as usize] += 1;
        match header.packet_format {
            2025 => self.format_2025_detected += 1,
            2026 => self.format_2026_detected += 1,
            _ => self.other_formats += 1,
        }

        if self.distinct_headers.len() < DISTINCT_HEADER_LOG_LIMIT
            && self.distinct_headers.insert(header)
        {
            info!(
                sender = %source,
                packet_len = header.packet_len,
                format = header.packet_format,
                game_year = header.game_year,
                game_major = header.game_major_version,
                game_minor = header.game_minor_version,
                packet_version = header.packet_version,
                packet_id = header.packet_id,
                packet_type = packet_type_name(header.packet_id),
                player_car_index = header.player_car_index,
                distinct_header = self.distinct_headers.len(),
                "F1 UDP datagram header observed"
            );
        }
        Some(header)
    }

    pub fn observe_result(
        &mut self,
        header: Option<HeaderPreview>,
        result: &anyhow::Result<Option<TelemetryData>>,
    ) {
        match result {
            Ok(sample) => {
                if let Some(header) = header {
                    match header.packet_id {
                        1 | 2 | 6 | 7 => {
                            self.packets_validated += 1;
                            self.player_indices_validated += 1;
                        }
                        _ => self.ignored_packet_ids += 1,
                    }
                    if header.packet_id == 6 {
                        self.telemetry_decoded += 1;
                        if sample.is_none() {
                            self.stale_telemetry_frames += 1;
                        }
                    } else if header.packet_id == 7 {
                        self.status_decoded += 1;
                    }
                }
                if let Some(sample) = sample {
                    self.normalized_frames += 1;
                    if self.normalized_frames <= 3 {
                        info!(
                            format = sample.source.protocol_format,
                            frame = sample.source.frame_identifier,
                            overall_frame = sample.source.overall_frame_identifier,
                            speed_mps = sample.vehicle.speed,
                            gear = sample.vehicle.gear,
                            throttle = sample.vehicle.throttle,
                            brake = sample.vehicle.brake,
                            "F1 normalized telemetry frame accepted"
                        );
                    }
                }
            }
            Err(error) => self.observe_rejection(error, header),
        }
        self.maybe_log_summary();
    }

    pub fn log_summary(&self, event: &'static str) {
        if self.datagrams_received == 0 {
            return;
        }
        let packet_ids: Vec<(usize, u64)> = self
            .packet_ids
            .iter()
            .enumerate()
            .filter_map(|(id, count)| (*count > 0).then_some((id, *count)))
            .collect();
        let rejections: Vec<(&'static str, u64)> = RejectReason::ALL
            .iter()
            .filter_map(|reason| {
                let count = self.rejected[reason.index()];
                (count > 0).then_some((reason.as_str(), count))
            })
            .collect();
        info!(
            event,
            datagrams_received = self.datagrams_received,
            headers_recognized = self.headers_recognized,
            format_2025_detected = self.format_2025_detected,
            format_2026_detected = self.format_2026_detected,
            other_formats = self.other_formats,
            packets_validated = self.packets_validated,
            player_indices_validated = self.player_indices_validated,
            telemetry_decoded = self.telemetry_decoded,
            status_decoded = self.status_decoded,
            normalized_frames = self.normalized_frames,
            stale_telemetry_frames = self.stale_telemetry_frames,
            ignored_packet_ids = self.ignored_packet_ids,
            packet_ids = ?packet_ids,
            rejections = ?rejections,
            "F1 UDP diagnostic summary"
        );
    }

    fn observe_rejection(&mut self, error: &Error, header: Option<HeaderPreview>) {
        let reason = protocol::reject_reason(error);
        let index = reason.index();
        self.rejected[index] += 1;
        if self.rejected[index] <= REJECTION_LOG_LIMIT_PER_REASON {
            warn!(
                reason = reason.as_str(),
                occurrence = self.rejected[index],
                format = header.map(|value| value.packet_format),
                game_year = header.map(|value| value.game_year),
                packet_version = header.map(|value| value.packet_version),
                packet_id = header.map(|value| value.packet_id),
                packet_type = header.map(|value| packet_type_name(value.packet_id)),
                packet_len = header.map(|value| value.packet_len),
                player_car_index = header.map(|value| value.player_car_index),
                error = %error,
                "F1 UDP datagram rejected"
            );
        }
    }

    fn maybe_log_summary(&mut self) {
        if self.last_summary_at.elapsed() >= SUMMARY_INTERVAL
            && self.datagrams_received != self.last_summary_received
        {
            self.log_summary("periodic");
            self.last_summary_at = Instant::now();
            self.last_summary_received = self.datagrams_received;
        }
    }
}

fn packet_type_name(packet_id: u8) -> &'static str {
    match packet_id {
        0 => "Motion",
        1 => "Session",
        2 => "LapData",
        3 => "Event",
        4 => "Participants",
        5 => "CarSetups",
        6 => "CarTelemetry",
        7 => "CarStatus",
        8 => "FinalClassification",
        9 => "LobbyInfo",
        10 => "CarDamage",
        11 => "SessionHistory",
        12 => "TyreSets",
        13 => "MotionEx",
        14 => "TimeTrial",
        15 => "LapPositions",
        16 => "CarTelemetry2",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::f1::{f1_2026, ProtocolRouter};

    #[test]
    fn previews_current_header_fields_without_decoding_layout() {
        let mut bytes = vec![0_u8; HEADER_LEN];
        bytes[0..2].copy_from_slice(&2026_u16.to_le_bytes());
        bytes[2..7].copy_from_slice(&[26, 1, 3, 2, 16]);
        bytes[27] = 23;
        let preview = HeaderPreview::read(&bytes).unwrap();
        assert_eq!(preview.packet_format, 2026);
        assert_eq!(preview.game_year, 26);
        assert_eq!(preview.game_major_version, 1);
        assert_eq!(preview.game_minor_version, 3);
        assert_eq!(preview.packet_version, 2);
        assert_eq!(preview.packet_id, 16);
        assert_eq!(preview.player_car_index, 23);
    }

    #[test]
    fn counts_received_2026_telemetry_through_normalized_acceptance() {
        let source = "127.0.0.1:20777".parse().unwrap();
        let bytes = f1_2026::test_telemetry_packet(3, 7);
        let mut diagnostics = Diagnostics::default();
        let header = diagnostics.observe_datagram(&bytes, source);
        let result = ProtocolRouter::default().decode(&bytes);
        diagnostics.observe_result(header, &result);

        assert_eq!(diagnostics.datagrams_received, 1);
        assert_eq!(diagnostics.headers_recognized, 1);
        assert_eq!(diagnostics.format_2026_detected, 1);
        assert_eq!(diagnostics.packet_ids[6], 1);
        assert_eq!(diagnostics.packets_validated, 1);
        assert_eq!(diagnostics.player_indices_validated, 1);
        assert_eq!(diagnostics.telemetry_decoded, 1);
        assert_eq!(diagnostics.normalized_frames, 1);
    }

    #[test]
    fn classifies_length_rejection_without_changing_layout() {
        let source = "127.0.0.1:20777".parse().unwrap();
        let mut bytes = f1_2026::test_telemetry_packet(0, 1);
        bytes.pop();
        let mut diagnostics = Diagnostics::default();
        let header = diagnostics.observe_datagram(&bytes, source);
        let result = ProtocolRouter::default().decode(&bytes);
        diagnostics.observe_result(header, &result);

        assert!(result.is_err());
        assert_eq!(diagnostics.datagrams_received, 1);
        assert_eq!(diagnostics.format_2026_detected, 1);
        assert_eq!(diagnostics.rejected[RejectReason::PacketLength.index()], 1);
        assert_eq!(diagnostics.normalized_frames, 0);
    }
}
