//! Telemetry buffer - stores telemetry points with a sliding time window
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::RwLock;
use std::time::Duration;

use crate::core::{TelemetryData, TelemetryPoint};

const EXPECTED_MAX_SAMPLE_RATE_HZ: usize = 240;
const MIN_POINTS: usize = 10;

/// Buffer storing telemetry points with a configurable time window
pub struct TelemetryBuffer {
    /// Maximum time window to keep
    window_duration: Duration,
    /// Stored telemetry points
    data: RwLock<VecDeque<TelemetryPoint>>,
    /// Minimum points to keep (prevents empty buffer)
    min_points: usize,
    max_points: usize,
}

impl TelemetryBuffer {
    /// Create a new buffer with the specified time window
    pub fn new(window_duration: Duration) -> Self {
        let max_points = ((window_duration.as_secs_f64() * EXPECTED_MAX_SAMPLE_RATE_HZ as f64)
            .ceil() as usize)
            .max(MIN_POINTS);
        Self::with_capacity(window_duration, max_points)
    }

    fn with_capacity(window_duration: Duration, max_points: usize) -> Self {
        Self {
            window_duration,
            data: RwLock::new(VecDeque::with_capacity(max_points)),
            min_points: MIN_POINTS.min(max_points),
            max_points,
        }
    }

    /// Push a new telemetry point
    pub fn push(&self, telemetry: TelemetryData) -> TelemetryPoint {
        let point = TelemetryPoint::new(telemetry);
        let mut data = self.data.write().unwrap();
        data.push_back(point.clone());
        self.prune_old_points(&mut data);
        while data.len() > self.max_points {
            data.pop_front();
        }
        point
    }

    /// Get all points within the current time window
    pub fn get_points(&self) -> Vec<TelemetryPoint> {
        let data = self.data.read().unwrap();
        data.iter().cloned().collect()
    }

    /// Get points for a specific time range
    pub fn get_points_in_range(&self, start: Duration, end: Duration) -> Vec<TelemetryPoint> {
        let data = self.data.read().unwrap();
        data.iter()
            .filter(|p| {
                let point_duration = p.captured_at.elapsed();
                point_duration <= end && point_duration >= start
            })
            .cloned()
            .collect()
    }

    /// Get the latest point
    pub fn latest(&self) -> Option<TelemetryPoint> {
        let data = self.data.read().unwrap();
        data.back().cloned()
    }

    /// Clear all data
    pub fn clear(&self) {
        self.data.write().unwrap().clear();
    }

    /// Returns the configured time window.
    pub fn window_duration(&self) -> Duration {
        self.window_duration
    }

    /// Get the number of points in the buffer
    pub fn len(&self) -> usize {
        self.data.read().unwrap().len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.data.read().unwrap().is_empty()
    }

    /// Remove old points outside the time window
    fn prune_old_points(&self, data: &mut VecDeque<TelemetryPoint>) {
        let now = std::time::Instant::now();
        let cutoff = now - self.window_duration;

        while data.len() > self.min_points
            && data.front().is_some_and(|point| point.captured_at < cutoff)
        {
            data.pop_front();
        }
    }
}

impl Default for TelemetryBuffer {
    fn default() -> Self {
        Self::new(Duration::from_secs(10))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn data(throttle: f32) -> TelemetryData {
        TelemetryData {
            timestamp: 0,
            vehicle: crate::core::VehicleTelemetry {
                throttle,
                ..Default::default()
            },
            session: None,
            source: Default::default(),
        }
    }

    #[test]
    fn test_push_and_get() {
        let buffer = TelemetryBuffer::new(Duration::from_secs(10));
        buffer.push(data(0.5));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.get_points()[0].telemetry.throttle, 0.5);
    }

    #[test]
    fn test_latest() {
        let buffer = TelemetryBuffer::new(Duration::from_secs(10));
        assert!(buffer.latest().is_none());
        buffer.push(data(0.0));
        assert!(buffer.latest().is_some());
    }

    #[test]
    fn test_clear_empties_buffer() {
        let buffer = TelemetryBuffer::new(Duration::from_secs(10));
        for _ in 0..5 {
            buffer.push(data(0.0));
        }
        assert_eq!(buffer.len(), 5);
        buffer.clear();
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert!(buffer.latest().is_none());
    }

    #[test]
    fn test_is_empty_on_new_buffer() {
        let buffer = TelemetryBuffer::new(Duration::from_secs(10));
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn test_min_points_preserved_after_window_expires() {
        // Use a very short window so points expire quickly.
        let buffer = TelemetryBuffer::new(Duration::from_millis(50));
        let min_points = 10;

        for _ in 0..(min_points + 5) {
            buffer.push(data(0.0));
        }
        assert!((min_points..=min_points + 5).contains(&buffer.len()));

        // Wait for all existing points to fall outside the window.
        thread::sleep(Duration::from_millis(100));

        // A new push triggers pruning; the min_points floor should keep old entries.
        buffer.push(data(0.0));
        assert!(
            buffer.len() >= min_points,
            "expected at least {} points, got {}",
            min_points,
            buffer.len()
        );
    }

    #[test]
    fn test_window_pruning_removes_old_points() {
        let buffer = TelemetryBuffer::new(Duration::from_millis(50));
        let min_points = 10;

        // Overfill the buffer, then wait for points to age out.
        for _ in 0..(min_points * 3) {
            buffer.push(data(0.0));
        }
        thread::sleep(Duration::from_millis(100));

        // After a push, expired entries should be pruned down to min_points.
        buffer.push(data(0.0));
        assert!(buffer.len() <= min_points + 1); // +1 for the point just pushed
    }

    #[test]
    fn capacity_is_strictly_bounded() {
        let buffer = TelemetryBuffer::with_capacity(Duration::from_secs(60), 16);
        for i in 0..100 {
            buffer.push(data(i as f32));
        }
        assert_eq!(buffer.len(), 16);
        assert_eq!(buffer.get_points()[0].telemetry.throttle, 84.0);
    }
}
