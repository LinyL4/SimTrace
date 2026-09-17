//! EA SPORTS F1 UDP provider.
//!
//! The socket transport is shared. Packet identity selects a protocol-specific
//! decoder before any packet layout is interpreted.

mod f1_2025;
mod f1_2026;
mod protocol;

use std::io::ErrorKind;
use std::net::UdpSocket;

use anyhow::{bail, Context, Result};
use tracing::{debug, info};

use crate::core::TelemetryData;
use crate::plugins::{GameConfig, GamePlugin, ProviderConfig};

const MAX_DATAGRAMS_PER_DRAIN: usize = 1024;

#[derive(Default)]
struct ProtocolRouter {
    f1_2025: f1_2025::Decoder,
    f1_2026: f1_2026::Decoder,
}

impl ProtocolRouter {
    fn decode(&mut self, bytes: &[u8]) -> Result<Option<TelemetryData>> {
        match protocol::packet_format(bytes)? {
            f1_2025::PACKET_FORMAT => self.f1_2025.decode(bytes),
            f1_2026::PACKET_FORMAT => self.f1_2026.decode(bytes),
            format => bail!("unsupported F1 UDP format {format}"),
        }
    }
}

pub struct F1Plugin {
    bind_address: String,
    port: u16,
    socket: Option<UdpSocket>,
    router: ProtocolRouter,
}

impl F1Plugin {
    pub fn new(config: &ProviderConfig) -> Self {
        Self {
            bind_address: config.f1_bind_address.clone(),
            port: config.f1_udp_port,
            socket: None,
            router: ProtocolRouter::default(),
        }
    }
}

impl GamePlugin for F1Plugin {
    fn name(&self) -> &str {
        "EA SPORTS F1 (UDP 2025/2026)"
    }

    fn connect(&mut self) -> Result<()> {
        let socket =
            UdpSocket::bind((self.bind_address.as_str(), self.port)).with_context(|| {
                format!(
                    "cannot bind F1 UDP listener to {}:{}",
                    self.bind_address, self.port
                )
            })?;
        socket.set_nonblocking(true)?;
        info!(address = %self.bind_address, port = self.port, "F1 UDP listener ready");
        self.socket = Some(socket);
        Ok(())
    }

    fn disconnect(&mut self) {
        self.socket = None;
        self.router = ProtocolRouter::default();
    }

    fn is_connected(&self) -> bool {
        self.socket.is_some()
    }

    fn read_telemetry(&mut self) -> Result<Option<TelemetryData>> {
        Ok(self.read_telemetry_batch(1)?.into_iter().next())
    }

    fn read_telemetry_batch(&mut self, max_samples: usize) -> Result<Vec<TelemetryData>> {
        let Some(socket) = self.socket.as_ref() else {
            return Ok(Vec::new());
        };
        let mut datagram = [0_u8; 2048];
        let mut samples = Vec::new();
        for _ in 0..MAX_DATAGRAMS_PER_DRAIN {
            if samples.len() >= max_samples {
                break;
            }
            match socket.recv_from(&mut datagram) {
                Ok((len, _source)) => match self.router.decode(&datagram[..len]) {
                    Ok(Some(sample)) => samples.push(sample),
                    Ok(None) => {}
                    Err(error) => {
                        debug!(%error, packet_len = len, "discarded invalid F1 UDP packet")
                    }
                },
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(samples)
    }

    fn get_config(&self) -> GameConfig {
        GameConfig {
            max_steering_angle: 180.0,
            pedal_deadzone: 0.0,
            abs_threshold: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_selects_each_protocol_by_packet_format() {
        let mut router = ProtocolRouter::default();
        let p25 = f1_2025::test_telemetry_packet(2, 10);
        let p26 = f1_2026::test_telemetry_packet(3, 10);
        let d25 = router.decode(&p25).unwrap().unwrap();
        let d26 = router.decode(&p26).unwrap().unwrap();
        assert_eq!(d25.source.protocol_format, Some(2025));
        assert_eq!(d26.source.protocol_format, Some(2026));
        assert_eq!(d25.vehicle.throttle, d26.vehicle.throttle);
    }

    #[test]
    fn router_rejects_unknown_protocol_before_layout_decode() {
        let mut bytes = vec![0_u8; 29];
        bytes[0..2].copy_from_slice(&2024_u16.to_le_bytes());
        assert!(ProtocolRouter::default().decode(&bytes).is_err());
    }
}
