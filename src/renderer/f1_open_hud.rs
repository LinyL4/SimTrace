//! Experimental flat F1 HUD presentation.
//!
//! This renderer consumes the shared, already-windowed visualization snapshot.
//! It never reads or filters the telemetry buffer itself.

use crate::config::{GraphSettings, ParsedColors};
use crate::core::TelemetryPoint;
use egui::{Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};

const GRID_DIVISIONS: usize = 4;
const HISTORY_BANDS: usize = 4;
const RPM_SEGMENTS: usize = 18;
const F1_RPM_SCALE: f32 = 15_000.0;

const TEXT_PRIMARY: Color32 = Color32::from_rgb(242, 246, 252);
const TEXT_SECONDARY: Color32 = Color32::from_rgb(170, 181, 196);
const GRID_COLOR: Color32 = Color32::from_rgb(118, 139, 164);
const STEERING_COLOR: Color32 = Color32::from_rgb(75, 178, 255);
const RPM_LOW: Color32 = Color32::from_rgb(61, 166, 255);
const RPM_HIGH: Color32 = Color32::from_rgb(255, 61, 87);

#[derive(Default)]
pub struct F1OpenHudCache {
    rect: Option<Rect>,
    shapes: Vec<Shape>,
    brake_head: Option<Pos2>,
    throttle_head: Option<Pos2>,
    steering_head: Option<Pos2>,
    values: OpenHudValues,
}

#[derive(Default)]
struct OpenHudValues {
    brake: String,
    throttle: String,
    steering: String,
    rpm: String,
    gear: String,
    speed: String,
    speed_unit: &'static str,
    abs_active: bool,
    tc_active: bool,
}

pub struct F1OpenHud<'a> {
    points: &'a [TelemetryPoint],
    latest: Option<&'a TelemetryPoint>,
    settings: &'a GraphSettings,
    colors: &'a ParsedColors,
    opacity: f32,
    max_steering_angle: f32,
}

impl<'a> F1OpenHud<'a> {
    pub fn new(
        points: &'a [TelemetryPoint],
        latest: Option<&'a TelemetryPoint>,
        settings: &'a GraphSettings,
        colors: &'a ParsedColors,
        opacity: f32,
        max_steering_angle: f32,
    ) -> Self {
        Self {
            points,
            latest,
            settings,
            colors,
            opacity,
            max_steering_angle,
        }
    }

    pub fn show(
        &self,
        ui: &mut egui::Ui,
        size: Vec2,
        rebuild_visualization: bool,
        cache: &mut F1OpenHudCache,
    ) {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::empty());
        let painter = ui.painter().with_clip_rect(rect);
        let rect_changed = cache.rect != Some(rect);

        if rebuild_visualization || rect_changed {
            self.rebuild(rect, cache);
        }

        painter.extend(cache.shapes.iter().cloned());
        self.draw_labels(&painter, rect, cache);
    }

    fn rebuild(&self, rect: Rect, cache: &mut F1OpenHudCache) {
        cache.rect = Some(rect);
        cache.shapes.clear();
        cache.brake_head = None;
        cache.throttle_head = None;
        cache.steering_head = None;
        cache.values = self.formatted_values();

        let layout = OpenHudLayout::new(rect);
        self.build_grid(layout.trace_rect, &mut cache.shapes);

        let capabilities = self.latest.map(|p| p.telemetry.capabilities);
        if capabilities.is_some_and(|c| c.brake) {
            cache.brake_head = self.build_channel(
                layout.trace_rect,
                layout.lane_y(0),
                |point| point.telemetry.brake,
                ChannelScale::Pedal,
                self.colors.brake,
                &mut cache.shapes,
            );
        }
        if capabilities.is_some_and(|c| c.throttle) {
            cache.throttle_head = self.build_channel(
                layout.trace_rect,
                layout.lane_y(1),
                |point| point.telemetry.throttle,
                ChannelScale::Pedal,
                self.colors.throttle,
                &mut cache.shapes,
            );
        }
        if capabilities.is_some_and(|c| c.steering_input) {
            cache.steering_head = self.build_channel(
                layout.trace_rect,
                layout.lane_y(2),
                |point| point.telemetry.steering_input,
                ChannelScale::Steering,
                STEERING_COLOR,
                &mut cache.shapes,
            );
        }

        self.build_rpm_segments(layout.center_rect, &mut cache.shapes);
    }

    fn build_grid(&self, rect: Rect, shapes: &mut Vec<Shape>) {
        let grid = with_opacity(GRID_COLOR, self.opacity * 0.11);
        for division in 0..=GRID_DIVISIONS {
            let x = egui::lerp(rect.x_range(), division as f32 / GRID_DIVISIONS as f32);
            shapes.push(Shape::line_segment(
                [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                Stroke::new(0.75_f32, grid),
            ));
        }
        for lane in 0..3 {
            let y = OpenHudLayout::lane_y_in(rect, lane);
            shapes.push(Shape::line_segment(
                [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
                Stroke::new(0.5_f32, grid),
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_channel(
        &self,
        rect: Rect,
        lane_y: f32,
        value: impl Fn(&TelemetryPoint) -> f32,
        scale: ChannelScale,
        color: Color32,
        shapes: &mut Vec<Shape>,
    ) -> Option<Pos2> {
        let now = std::time::Instant::now();
        let window_seconds = self.settings.window_seconds.max(0.001) as f32;
        let lane_height = rect.height() / 3.0;
        let mut bands: [Vec<Pos2>; HISTORY_BANDS] = std::array::from_fn(|_| Vec::new());
        let mut previous: Option<(usize, Pos2)> = None;

        for point in self.points {
            let age = now.duration_since(point.captured_at).as_secs_f32();
            let recency = (1.0 - age / window_seconds).clamp(0.0, 1.0);
            let x = egui::lerp(rect.x_range(), recency);
            let y = scale.y(lane_y, lane_height, value(point));
            let pos = Pos2::new(x, y);
            let band = ((recency * HISTORY_BANDS as f32) as usize).min(HISTORY_BANDS - 1);

            if let Some((previous_band, previous_pos)) = previous {
                if previous_band != band && bands[band].is_empty() {
                    bands[band].push(previous_pos);
                }
            }
            bands[band].push(pos);
            previous = Some((band, pos));
        }

        let current = self
            .latest
            .map(|point| Pos2::new(rect.max.x, scale.y(lane_y, lane_height, value(point))));
        if let Some(head) = current {
            if let Some((band, last)) = previous {
                if band != HISTORY_BANDS - 1 {
                    bands[HISTORY_BANDS - 1].push(last);
                }
                bands[HISTORY_BANDS - 1].push(head);
            } else {
                bands[HISTORY_BANDS - 1].push(head);
            }
        }

        let alpha = [0.20_f32, 0.34, 0.54, 0.82];
        for (band, points) in bands.into_iter().enumerate() {
            if points.len() < 2 {
                continue;
            }
            let energy = alpha[band] * self.opacity;
            shapes.push(Shape::line(
                points.clone(),
                Stroke::new(6.0_f32, with_opacity(color, energy * 0.16)),
            ));
            shapes.push(Shape::line(
                points,
                Stroke::new(1.65_f32, with_opacity(color, energy)),
            ));
        }
        current
    }

    fn build_rpm_segments(&self, rect: Rect, shapes: &mut Vec<Shape>) {
        let Some(latest) = self.latest else {
            return;
        };
        if !latest.telemetry.capabilities.rpm {
            return;
        }
        let ratio = (latest.telemetry.rpm / F1_RPM_SCALE).clamp(0.0, 1.0);
        let active = (ratio * RPM_SEGMENTS as f32).ceil() as usize;
        let gap = 3.0;
        let width = (rect.width() - gap * (RPM_SEGMENTS - 1) as f32) / RPM_SEGMENTS as f32;
        let y = rect.min.y + rect.height() * 0.18;
        for index in 0..RPM_SEGMENTS {
            let x = rect.min.x + index as f32 * (width + gap);
            let segment = Rect::from_min_size(Pos2::new(x, y), Vec2::new(width.max(1.0), 5.0));
            let high = index >= RPM_SEGMENTS * 3 / 4;
            let base = if high { RPM_HIGH } else { RPM_LOW };
            let alpha = if index < active { 0.9 } else { 0.12 };
            if index < active {
                shapes.push(Shape::rect_filled(
                    segment.expand(2.0),
                    1.0,
                    with_opacity(base, self.opacity * 0.10),
                ));
            }
            shapes.push(Shape::rect_filled(
                segment,
                0.75,
                with_opacity(base, self.opacity * alpha),
            ));
        }
    }

    fn formatted_values(&self) -> OpenHudValues {
        let Some(point) = self.latest else {
            return OpenHudValues {
                brake: "—".to_owned(),
                throttle: "—".to_owned(),
                steering: "—".to_owned(),
                rpm: "—".to_owned(),
                gear: "—".to_owned(),
                speed: "—".to_owned(),
                speed_unit: if self.settings.speed_mph {
                    "MPH"
                } else {
                    "KM/H"
                },
                ..Default::default()
            };
        };
        let telemetry = &point.telemetry;
        let capabilities = telemetry.capabilities;
        let gear = match telemetry.gear {
            -1 => "R".to_owned(),
            0 => "N".to_owned(),
            value => value.to_string(),
        };
        let speed = if self.settings.speed_mph {
            telemetry.speed * 2.237
        } else {
            telemetry.speed * 3.6
        };
        OpenHudValues {
            brake: supported(capabilities.brake, || {
                format!("{:.0}%", telemetry.brake * 100.0)
            }),
            throttle: supported(capabilities.throttle, || {
                format!("{:.0}%", telemetry.throttle * 100.0)
            }),
            steering: supported(capabilities.steering_input, || {
                format!(
                    "{:.0}°",
                    telemetry.effective_steering_degrees(self.max_steering_angle)
                )
            }),
            rpm: supported(capabilities.rpm, || format!("{:.0}", telemetry.rpm)),
            gear: supported(capabilities.gear, || gear),
            speed: supported(capabilities.speed, || format!("{speed:.0}")),
            speed_unit: if self.settings.speed_mph {
                "MPH"
            } else {
                "KM/H"
            },
            abs_active: capabilities.abs_activity && telemetry.abs_active,
            tc_active: capabilities.tc_activity && telemetry.tc_active,
        }
    }

    fn draw_labels(&self, painter: &Painter, rect: Rect, cache: &F1OpenHudCache) {
        let layout = OpenHudLayout::new(rect);
        let alpha = self.opacity;
        let label_font = FontId::new(12.0, FontFamily::Monospace);
        let value_font = FontId::new(13.0, FontFamily::Monospace);

        shadowed_text(
            painter,
            Pos2::new(layout.left_rect.min.x, layout.left_rect.min.y + 4.0),
            Align2::LEFT_TOP,
            "DRIVER INPUT /  F1 OPEN HUD  ·  BETA",
            FontId::new(10.0, FontFamily::Monospace),
            with_opacity(TEXT_SECONDARY, alpha * 0.72),
        );

        let channels = [
            (
                "BRAKE",
                &cache.values.brake,
                self.colors.brake,
                cache.brake_head,
            ),
            (
                "THROTTLE",
                &cache.values.throttle,
                self.colors.throttle,
                cache.throttle_head,
            ),
            (
                "STEERING",
                &cache.values.steering,
                STEERING_COLOR,
                cache.steering_head,
            ),
        ];
        for (lane, (label, value, color, head)) in channels.into_iter().enumerate() {
            let y = layout.lane_y(lane);
            shadowed_text(
                painter,
                Pos2::new(layout.left_rect.min.x, y),
                Align2::LEFT_CENTER,
                label,
                label_font.clone(),
                with_opacity(color, alpha),
            );
            shadowed_text(
                painter,
                Pos2::new(layout.left_rect.max.x, y),
                Align2::RIGHT_CENTER,
                value,
                value_font.clone(),
                with_opacity(color, alpha),
            );
            if let Some(head) = head {
                draw_head(painter, head, color, alpha);
            }
        }

        for division in 0..=GRID_DIVISIONS {
            let seconds = -self.settings.window_seconds
                + self.settings.window_seconds * division as f64 / GRID_DIVISIONS as f64;
            let x = egui::lerp(
                layout.trace_rect.x_range(),
                division as f32 / GRID_DIVISIONS as f32,
            );
            shadowed_text(
                painter,
                Pos2::new(x, layout.trace_rect.max.y + 10.0),
                Align2::CENTER_TOP,
                if division == GRID_DIVISIONS {
                    "0".to_owned()
                } else {
                    format!("{seconds:.0}s")
                },
                FontId::new(8.0, FontFamily::Monospace),
                with_opacity(TEXT_SECONDARY, alpha * 0.55),
            );
        }

        let center = layout.center_rect.center();
        shadowed_text(
            painter,
            Pos2::new(
                center.x,
                layout.center_rect.min.y + layout.center_rect.height() * 0.29,
            ),
            Align2::CENTER_CENTER,
            format!("{} RPM", cache.values.rpm),
            FontId::new(
                (layout.center_rect.width() * 0.07).clamp(12.0, 18.0),
                FontFamily::Monospace,
            ),
            with_opacity(TEXT_PRIMARY, alpha),
        );
        shadowed_text(
            painter,
            Pos2::new(
                center.x,
                layout.center_rect.min.y + layout.center_rect.height() * 0.50,
            ),
            Align2::CENTER_CENTER,
            &cache.values.gear,
            FontId::new(
                (layout.center_rect.height() * 0.28).clamp(48.0, 92.0),
                FontFamily::Proportional,
            ),
            with_opacity(TEXT_PRIMARY, alpha),
        );
        shadowed_text(
            painter,
            Pos2::new(
                center.x,
                layout.center_rect.min.y + layout.center_rect.height() * 0.72,
            ),
            Align2::CENTER_CENTER,
            &cache.values.speed,
            FontId::new(
                (layout.center_rect.height() * 0.14).clamp(27.0, 48.0),
                FontFamily::Monospace,
            ),
            with_opacity(TEXT_PRIMARY, alpha),
        );
        shadowed_text(
            painter,
            Pos2::new(
                center.x,
                layout.center_rect.min.y + layout.center_rect.height() * 0.82,
            ),
            Align2::CENTER_CENTER,
            cache.values.speed_unit,
            FontId::new(11.0, FontFamily::Monospace),
            with_opacity(TEXT_SECONDARY, alpha),
        );

        let state_y = layout.center_rect.max.y - 20.0;
        if cache.values.abs_active {
            shadowed_text(
                painter,
                Pos2::new(center.x - 32.0, state_y),
                Align2::CENTER_CENTER,
                "ABS",
                FontId::new(11.0, FontFamily::Monospace),
                with_opacity(Color32::from_rgb(255, 190, 35), alpha),
            );
        }
        if cache.values.tc_active {
            shadowed_text(
                painter,
                Pos2::new(center.x + 32.0, state_y),
                Align2::CENTER_CENTER,
                "TC",
                FontId::new(11.0, FontFamily::Monospace),
                with_opacity(self.colors.tc_active, alpha),
            );
        }
    }
}

struct OpenHudLayout {
    left_rect: Rect,
    trace_rect: Rect,
    center_rect: Rect,
}

impl OpenHudLayout {
    fn new(rect: Rect) -> Self {
        let outer = rect.shrink2(Vec2::new(14.0, 10.0));
        let left_width = outer.width() * 0.66;
        let gap = 22.0_f32.min(outer.width() * 0.04);
        let left_rect = Rect::from_min_size(outer.min, Vec2::new(left_width, outer.height()));
        let label_width = (left_width * 0.20).clamp(76.0, 112.0);
        let value_width = (left_width * 0.13).clamp(46.0, 72.0);
        let trace_rect = Rect::from_min_max(
            Pos2::new(
                left_rect.min.x + label_width,
                left_rect.min.y + outer.height() * 0.16,
            ),
            Pos2::new(
                left_rect.max.x - value_width,
                left_rect.max.y - outer.height() * 0.12,
            ),
        );
        let center_rect =
            Rect::from_min_max(Pos2::new(left_rect.max.x + gap, outer.min.y), outer.max);
        Self {
            left_rect,
            trace_rect,
            center_rect,
        }
    }

    fn lane_y(&self, lane: usize) -> f32 {
        Self::lane_y_in(self.trace_rect, lane)
    }

    fn lane_y_in(rect: Rect, lane: usize) -> f32 {
        rect.min.y + rect.height() * (lane as f32 + 0.5) / 3.0
    }
}

#[derive(Clone, Copy)]
enum ChannelScale {
    Pedal,
    Steering,
}

impl ChannelScale {
    fn y(self, center: f32, lane_height: f32, value: f32) -> f32 {
        match self {
            Self::Pedal => center + (0.5 - value.clamp(0.0, 1.0)) * lane_height * 0.64,
            Self::Steering => center - value.clamp(-1.0, 1.0) * lane_height * 0.32,
        }
    }
}

fn supported(available: bool, value: impl FnOnce() -> String) -> String {
    if available {
        value()
    } else {
        "—".to_owned()
    }
}

fn with_opacity(color: Color32, opacity: f32) -> Color32 {
    let [r, g, b, a] = color.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, (a as f32 * opacity.clamp(0.0, 1.0)) as u8)
}

fn draw_head(painter: &Painter, center: Pos2, color: Color32, opacity: f32) {
    painter.circle_filled(center, 9.0, with_opacity(color, opacity * 0.08));
    painter.circle_filled(center, 5.5, with_opacity(color, opacity * 0.20));
    painter.circle_filled(center, 2.8, with_opacity(color, opacity));
    painter.circle_stroke(
        center,
        3.8,
        Stroke::new(0.8_f32, with_opacity(Color32::WHITE, opacity * 0.8)),
    );
}

fn shadowed_text(
    painter: &Painter,
    position: Pos2,
    anchor: Align2,
    text: impl ToString,
    font: FontId,
    color: Color32,
) {
    let text = text.to_string();
    painter.text(
        position + Vec2::new(1.25, 1.5),
        anchor,
        &text,
        font.clone(),
        Color32::from_black_alpha(165),
    );
    painter.text(position, anchor, text, font, color);
}
