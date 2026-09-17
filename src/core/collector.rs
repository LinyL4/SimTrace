//! Polls the active game plugin and feeds telemetry into the shared buffer.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{error, info};

use crate::core::{LapStore, TelemetryBuffer};
use crate::plugins::{PluginRegistry, ProviderConfig};

/// How long to wait between reconnection attempts.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
const MAX_SAMPLES_PER_POLL: usize = 256;

/// Owns the plugin registry and the telemetry buffer.
pub struct DataCollector {
    buffer: Arc<TelemetryBuffer>,
    plugin_registry: PluginRegistry,
    /// Tracks when we last attempted a reconnect so we don't spam the game.
    last_connect_attempt: Option<Instant>,
    analysis: Arc<Mutex<LapStore>>,
}

impl DataCollector {
    pub fn new(buffer_window_secs: u64) -> Self {
        Self {
            buffer: Arc::new(TelemetryBuffer::new(Duration::from_secs(
                buffer_window_secs,
            ))),
            plugin_registry: PluginRegistry::new(),
            last_connect_attempt: None,
            analysis: Arc::new(Mutex::new(LapStore::new())),
        }
    }

    pub fn analysis(&self) -> Arc<Mutex<LapStore>> {
        self.analysis.clone()
    }

    pub fn buffer(&self) -> Arc<TelemetryBuffer> {
        self.buffer.clone()
    }

    pub fn activate_plugin(&mut self, name: &str, config: &ProviderConfig) -> anyhow::Result<()> {
        self.last_connect_attempt = None;
        self.plugin_registry.activate(name, config)?;
        info!("Activated plugin: {}", name);
        Ok(())
    }

    /// Reads telemetry from the active plugin. If the plugin is not connected,
    /// a reconnection is attempted every [`RECONNECT_INTERVAL`] automatically.
    pub fn poll(&mut self) {
        // Check connection state without holding a mutable borrow.
        let is_connected = self
            .plugin_registry
            .active_plugin()
            .is_some_and(|p| p.is_connected());

        if !is_connected {
            let should_try = self
                .last_connect_attempt
                .is_none_or(|t| t.elapsed() >= RECONNECT_INTERVAL);

            if should_try {
                self.last_connect_attempt = Some(Instant::now());
                if let Some(plugin) = self.plugin_registry.active_plugin_mut() {
                    match plugin.connect() {
                        Ok(()) => info!("Plugin '{}' connected", plugin.name()),
                        Err(e) => tracing::debug!("Reconnect attempt failed: {e}"),
                    }
                }
            }
            return;
        }

        // Connected — read telemetry.
        self.last_connect_attempt = None;
        if let Some(plugin) = self.plugin_registry.active_plugin_mut() {
            match plugin.read_telemetry_batch(MAX_SAMPLES_PER_POLL) {
                Ok(samples) => {
                    for data in samples {
                        let point = self.buffer.push(data);
                        self.analysis.lock().unwrap().push(&point);
                    }
                }
                Err(e) => error!("Error reading telemetry: {}", e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collector_starts_with_empty_buffer() {
        let collector = DataCollector::new(10);
        assert!(collector.buffer().is_empty());
    }

    #[test]
    fn test_activate_mock_plugin() {
        let mut collector = DataCollector::new(10);
        collector
            .activate_plugin("mock", &ProviderConfig::default())
            .unwrap();
    }

    #[test]
    fn test_poll_populates_buffer() {
        let mut collector = DataCollector::new(10);
        collector
            .activate_plugin("mock", &ProviderConfig::default())
            .unwrap();
        assert!(collector.buffer().is_empty());
        collector.poll();
        assert_eq!(collector.buffer().len(), 1);
    }

    #[test]
    fn analysis_ingests_without_ui_render() {
        let mut collector = DataCollector::new(10);
        collector
            .activate_plugin("mock", &ProviderConfig::default())
            .unwrap();
        for _ in 0..25 {
            collector.poll();
        }
        assert_eq!(collector.buffer().len(), 25);
        assert_eq!(collector.analysis().lock().unwrap().sample_count(), 25);
    }
}
