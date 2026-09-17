//! Game plugin trait definition
#![allow(dead_code)]

use anyhow::Result;

use crate::core::TelemetryData;

/// Configuration for a game plugin
#[derive(Debug, Clone, Default)]
pub struct GameConfig {
    /// Maximum steering angle in degrees
    pub max_steering_angle: f32,
    /// Deadzone for pedal inputs
    pub pedal_deadzone: f32,
    /// Threshold for ABS activation detection
    pub abs_threshold: f32,
}

/// Runtime settings shared by plugins that require user configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub f1_bind_address: String,
    pub f1_udp_port: u16,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            f1_bind_address: "0.0.0.0".to_owned(),
            f1_udp_port: 20777,
        }
    }
}

/// Trait that all game plugins must implement
pub trait GamePlugin: Send + Sync {
    /// Get the plugin name
    fn name(&self) -> &str;

    /// Initialize and connect to the game
    fn connect(&mut self) -> Result<()>;

    /// Disconnect from the game
    fn disconnect(&mut self);

    /// Check if currently connected
    fn is_connected(&self) -> bool;

    /// Read telemetry data from the game
    /// Returns None if no data available yet
    fn read_telemetry(&mut self) -> Result<Option<TelemetryData>>;

    /// Drain a bounded batch. UDP providers override this so collection is not
    /// limited to one datagram per collector tick.
    fn read_telemetry_batch(&mut self, max_samples: usize) -> Result<Vec<TelemetryData>> {
        if max_samples == 0 {
            return Ok(Vec::new());
        }
        Ok(self.read_telemetry()?.into_iter().collect())
    }

    /// Get game-specific configuration
    fn get_config(&self) -> GameConfig {
        GameConfig::default()
    }

    /// Check if the game is running/available
    fn is_available(&self) -> bool {
        true
    }
}

/// Returns the static list of `(id, display_name)` pairs available on this platform.
///
/// ACC only ships on Windows; the mock entry is always present.
pub fn plugin_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("assetto_competizione", "Assetto Corsa Competizione"),
        ("ams2", "Automobilista 2"),
        ("iracing", "iRacing"),
        ("f1", "EA SPORTS F1 (UDP 2025/2026)"),
        ("mock", "Mock (Simulated Data)"),
    ]
}

/// Helper function to create a plugin by name
pub fn create_plugin(name: &str, _config: &ProviderConfig) -> Option<Box<dyn GamePlugin>> {
    match name.to_lowercase().as_str() {
        "assetto_competizione" | "assetto corsa competizione" | "acc" => {
            #[cfg(windows)]
            {
                Some(Box::new(
                    crate::plugins::assetto_competizione::AccPlugin::new(),
                ))
            }
            #[cfg(not(windows))]
            {
                None
            }
        }
        "ams2" | "automobilista 2" | "automobilista2" => {
            Some(Box::new(crate::plugins::ams2::Ams2Plugin::new()))
        }
        "iracing" | "iracing_sdk" => Some(Box::new(crate::plugins::iracing::IracingPlugin::new())),
        "f1" | "f1_25" | "f1 25" | "f1 2026" => {
            Some(Box::new(crate::plugins::f1::F1Plugin::new(_config)))
        }
        "mock" | "test" => Some(Box::new(crate::plugins::mock::MockPlugin::new())),
        _ => None,
    }
}
