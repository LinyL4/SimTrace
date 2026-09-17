//! Experimental flat F1 HUD presentation.
//!
//! This renderer consumes the shared, already-windowed visualization snapshot.
//! It never reads or filters the telemetry buffer itself.

use crate::config::GraphSettings;
use crate::core::{RevLights, TelemetryPoint};
use egui::{
    Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Painter, Pos2, Rect, Shape,
    Stroke, Vec2,
};
use std::sync::Arc;

const DISPLAY_FONT_NAME: &str = "saira_semi_condensed_semibold";
const GRID_DIVISIONS: usize = 4;
const HISTORY_BANDS: usize = 8;
const RPM_SEGMENTS: usize = 15;
const PEDAL_SEGMENTS: usize = 12;
const STEERING_SEGMENTS_PER_SIDE: usize = 12;
const STEERING_ARC_DEGREES: f32 = 240.0;
const F1_SPEED_SCALE_MS: f32 = 100.0;

const TEXT_PRIMARY: Color32 = Color32::from_rgb(244, 247, 250);
const TEXT_SECONDARY: Color32 = Color32::from_rgb(166, 178, 193);
const GRID_COLOR: Color32 = Color32::from_rgb(112, 132, 153);
const BRAKE_COLOR: Color32 = Color32::from_rgb(255, 91, 96);
const THROTTLE_COLOR: Color32 = Color32::from_rgb(74, 234, 158);
const SPEED_COLOR: Color32 = Color32::from_rgb(126, 196, 225);
const STEERING_COLOR: Color32 = Color32::from_rgb(238, 245, 250);
const RPM_LOW: Color32 = Color32::from_rgb(74, 167, 235);
const RPM_HIGH: Color32 = Color32::from_rgb(255, 78, 99);
const ABS_COLOR: Color32 = Color32::from_rgb(255, 194, 55);

/// Install the single bundled display face without changing the font used by
/// Classic or by settings widgets.
pub fn install_font(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        DISPLAY_FONT_NAME.to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/SairaSemiCondensed-SemiBold.ttf"
        ))),
    );
    fonts
        .families
        .insert(display_family(), vec![DISPLAY_FONT_NAME.to_owned()]);
    ctx.set_fonts(fonts);
}

fn display_family() -> FontFamily {
    FontFamily::Name(Arc::<str>::from(DISPLAY_FONT_NAME))
}

fn display_font(size: f32) -> FontId {
    FontId::new(size, display_family())
}

#[derive(Default)]
pub struct F1OpenHudCache {
    rect: Option<Rect>,
    shapes: Vec<Shape>,
    brake_head: Option<Pos2>,
    throttle_head: Option<Pos2>,
    speed_head: Option<Pos2>,
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
    rev_lights: Option<RevLights>,
    brake_level: Option<f32>,
    throttle_level: Option<f32>,
    steering_level: Option<f32>,
    abs_active: bool,
    tc_active: bool,
}

pub struct F1OpenHud<'a> {
    points: &'a [TelemetryPoint],
    latest: Option<&'a TelemetryPoint>,
    settings: &'a GraphSettings,
    opacity: f32,
    max_steering_angle: f32,
}

impl<'a> F1OpenHud<'a> {
    pub fn new(
        points: &'a [TelemetryPoint],
        latest: Option<&'a TelemetryPoint>,
        settings: &'a GraphSettings,
        opacity: f32,
        max_steering_angle: f32,
    ) -> Self {
        Self {
            points,
            latest,
            settings,
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
        cache.speed_head = None;
        cache.values = self.formatted_values();

        let layout = OpenHudLayout::new(rect);
        self.build_grid(layout.trace_rect, &mut cache.shapes);

        let capabilities = self.latest.map(|point| point.telemetry.capabilities);
        let mut brake_bands: [Vec<Pos2>; HISTORY_BANDS] = std::array::from_fn(|_| Vec::new());
        let mut throttle_bands: [Vec<Pos2>; HISTORY_BANDS] = std::array::from_fn(|_| Vec::new());
        let mut speed_bands: [Vec<Pos2>; HISTORY_BANDS] = std::array::from_fn(|_| Vec::new());
        let mut previous: Option<(usize, [Pos2; 3])> = None;
        let now = std::time::Instant::now();
        let window_seconds = self.settings.window_seconds.max(0.001) as f32;

        // One shared history pass prepares all three channels in the same plot.
        for point in self.points {
            let age = now.duration_since(point.captured_at).as_secs_f32();
            let recency = (1.0 - age / window_seconds).clamp(0.0, 1.0);
            let x = egui::lerp(layout.trace_rect.x_range(), recency);
            let positions = [
                Pos2::new(x, pedal_y(layout.trace_rect, point.telemetry.brake)),
                Pos2::new(x, pedal_y(layout.trace_rect, point.telemetry.throttle)),
                Pos2::new(x, speed_y(layout.trace_rect, point.telemetry.speed)),
            ];
            let band = ((recency * HISTORY_BANDS as f32) as usize).min(HISTORY_BANDS - 1);

            if let Some((previous_band, previous_positions)) = previous {
                if previous_band != band && brake_bands[band].is_empty() {
                    brake_bands[band].push(previous_positions[0]);
                    throttle_bands[band].push(previous_positions[1]);
                    speed_bands[band].push(previous_positions[2]);
                }
            }
            brake_bands[band].push(positions[0]);
            throttle_bands[band].push(positions[1]);
            speed_bands[band].push(positions[2]);
            previous = Some((band, positions));
        }

        if let Some(latest) = self.latest {
            let x = layout.trace_rect.max.x;
            let heads = [
                Pos2::new(x, pedal_y(layout.trace_rect, latest.telemetry.brake)),
                Pos2::new(x, pedal_y(layout.trace_rect, latest.telemetry.throttle)),
                Pos2::new(x, speed_y(layout.trace_rect, latest.telemetry.speed)),
            ];
            if let Some((band, last)) = previous {
                if band != HISTORY_BANDS - 1 {
                    brake_bands[HISTORY_BANDS - 1].push(last[0]);
                    throttle_bands[HISTORY_BANDS - 1].push(last[1]);
                    speed_bands[HISTORY_BANDS - 1].push(last[2]);
                }
            }
            brake_bands[HISTORY_BANDS - 1].push(heads[0]);
            throttle_bands[HISTORY_BANDS - 1].push(heads[1]);
            speed_bands[HISTORY_BANDS - 1].push(heads[2]);

            if capabilities.is_some_and(|value| value.brake) {
                cache.brake_head = Some(heads[0]);
            }
            if capabilities.is_some_and(|value| value.throttle) {
                cache.throttle_head = Some(heads[1]);
            }
            if capabilities.is_some_and(|value| value.speed) {
                cache.speed_head = Some(heads[2]);
            }
        }

        if capabilities.is_some_and(|value| value.brake) {
            self.build_trace_layers(brake_bands, BRAKE_COLOR, 1.0, &mut cache.shapes);
        }
        if capabilities.is_some_and(|value| value.throttle) {
            self.build_trace_layers(throttle_bands, THROTTLE_COLOR, 1.0, &mut cache.shapes);
        }
        if capabilities.is_some_and(|value| value.speed) {
            self.build_trace_layers(speed_bands, SPEED_COLOR, 0.68, &mut cache.shapes);
        }

        self.build_pedal_meter(
            layout.brake_meter_rect,
            cache.values.brake_level,
            BRAKE_COLOR,
            &mut cache.shapes,
        );
        self.build_pedal_meter(
            layout.throttle_meter_rect,
            cache.values.throttle_level,
            THROTTLE_COLOR,
            &mut cache.shapes,
        );
        self.build_steering_arc(
            layout.steering_rect,
            cache.values.steering_level,
            &mut cache.shapes,
        );

        self.build_rev_lights(
            layout.center_rect,
            cache.values.rev_lights,
            &mut cache.shapes,
        );
    }

    fn build_trace_layers(
        &self,
        bands: [Vec<Pos2>; HISTORY_BANDS],
        color: Color32,
        strength: f32,
        shapes: &mut Vec<Shape>,
    ) {
        for (band, points) in bands.into_iter().enumerate() {
            if points.len() < 2 {
                continue;
            }
            let recency = (band + 1) as f32 / HISTORY_BANDS as f32;
            let energy = (0.14 + 0.86 * recency.powf(1.35)) * self.opacity * strength;
            shapes.push(Shape::line(
                points.clone(),
                Stroke::new(6.0_f32, with_opacity(color, energy * 0.10)),
            ));
            shapes.push(Shape::line(
                points.clone(),
                Stroke::new(3.2_f32, with_opacity(color, energy * 0.13)),
            ));
            shapes.push(Shape::line(
                points,
                Stroke::new(1.55_f32, with_opacity(color, energy * 0.94)),
            ));
        }
    }

    fn build_grid(&self, rect: Rect, shapes: &mut Vec<Shape>) {
        for division in 0..=GRID_DIVISIONS {
            let recency = division as f32 / GRID_DIVISIONS as f32;
            let x = egui::lerp(rect.x_range(), recency);
            let strength = if division == GRID_DIVISIONS {
                0.14
            } else {
                0.035 + recency * 0.035
            };
            shapes.push(Shape::line_segment(
                [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                Stroke::new(0.65_f32, with_opacity(GRID_COLOR, self.opacity * strength)),
            ));
        }
        for division in 1..4 {
            let y = egui::lerp(rect.y_range(), division as f32 / 4.0);
            shapes.push(Shape::line_segment(
                [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
                Stroke::new(0.5_f32, with_opacity(GRID_COLOR, self.opacity * 0.025)),
            ));
        }
    }

    fn build_pedal_meter(
        &self,
        rect: Rect,
        level: Option<f32>,
        color: Color32,
        shapes: &mut Vec<Shape>,
    ) {
        let Some(level) = level else {
            return;
        };
        let gap = (rect.height() * 0.018).clamp(1.5, 3.0);
        let segment_height =
            ((rect.height() - gap * (PEDAL_SEGMENTS - 1) as f32) / PEDAL_SEGMENTS as f32).max(2.0);
        let active_count = (level.clamp(0.0, 1.0) * PEDAL_SEGMENTS as f32).ceil() as usize;

        for index in 0..PEDAL_SEGMENTS {
            let y = rect.max.y - (index + 1) as f32 * segment_height - index as f32 * gap;
            let segment = Rect::from_min_size(
                Pos2::new(rect.min.x, y),
                Vec2::new(rect.width(), segment_height),
            );
            let state = if index + 1 == active_count && active_count > 0 {
                SegmentState::Edge
            } else if index < active_count {
                SegmentState::Active
            } else {
                SegmentState::Inactive
            };
            push_luminous_segment(
                shapes,
                SegmentGeometry::Rect(segment, 1.2),
                color,
                state,
                self.opacity,
            );
        }
    }

    fn build_steering_arc(&self, rect: Rect, steering: Option<f32>, shapes: &mut Vec<Shape>) {
        let Some(steering) = steering else {
            return;
        };
        let side = STEERING_SEGMENTS_PER_SIDE;
        let segment_count = side * 2 + 1;
        let center_index = side;
        let center = Pos2::new(rect.center().x, rect.center().y + rect.height() * 0.12);
        let outer_radius = (rect.width().min(rect.height()) * 0.48).max(8.0);
        let inner_radius = (outer_radius - (outer_radius * 0.13).clamp(3.5, 6.0)).max(2.0);
        let total_arc = STEERING_ARC_DEGREES.to_radians();
        let center_angle = -std::f32::consts::FRAC_PI_2;
        let start = center_angle - total_arc * 0.5;
        let step = total_arc / segment_count as f32;
        let gap = step * 0.28;
        let active_count = (steering.abs().clamp(0.0, 1.0) * side as f32).ceil() as usize;
        let edge_index = if steering < 0.0 && active_count > 0 {
            Some(center_index - active_count)
        } else if steering > 0.0 && active_count > 0 {
            Some(center_index + active_count)
        } else {
            None
        };

        for index in 0..segment_count {
            let state = if index == center_index {
                SegmentState::Center
            } else {
                let active = if steering < 0.0 {
                    index < center_index && center_index - index <= active_count
                } else {
                    index > center_index && index - center_index <= active_count
                };
                if Some(index) == edge_index {
                    SegmentState::Edge
                } else if active {
                    SegmentState::Active
                } else {
                    SegmentState::Inactive
                }
            };
            let angle_min = start + index as f32 * step + gap * 0.5;
            let angle_max = start + (index + 1) as f32 * step - gap * 0.5;
            push_luminous_segment(
                shapes,
                SegmentGeometry::Polygon(arc_segment(
                    center,
                    inner_radius,
                    outer_radius,
                    angle_min,
                    angle_max,
                )),
                STEERING_COLOR,
                state,
                self.opacity,
            );
        }
    }

    fn build_rev_lights(&self, rect: Rect, rev_lights: Option<RevLights>, shapes: &mut Vec<Shape>) {
        let Some(rev_lights) = rev_lights else {
            return;
        };
        let gap = 3.0_f32;
        let width = (rect.width() - gap * (RPM_SEGMENTS - 1) as f32) / RPM_SEGMENTS as f32;
        let y = rect.min.y + rect.height() * 0.16;

        let active_edge = (0..RPM_SEGMENTS)
            .rev()
            .find(|index| rev_lights.bit_value & (1_u16 << index) != 0);
        for index in 0..RPM_SEGMENTS {
            let x = rect.min.x + index as f32 * (width + gap);
            let segment = Rect::from_min_size(Pos2::new(x, y), Vec2::new(width.max(1.0), 4.5));
            let active = rev_lights.bit_value & (1_u16 << index) != 0;
            let base = if index < 10 { RPM_LOW } else { RPM_HIGH };
            let state = if active_edge == Some(index) {
                SegmentState::Edge
            } else if active {
                SegmentState::Active
            } else {
                SegmentState::Inactive
            };
            push_luminous_segment(
                shapes,
                SegmentGeometry::Rect(segment, 0.9),
                base,
                state,
                self.opacity,
            );
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
        let steering_degrees = telemetry.effective_steering_degrees(self.max_steering_angle);
        OpenHudValues {
            brake: supported(capabilities.brake, || {
                format!("{:.0}%", telemetry.brake * 100.0)
            }),
            throttle: supported(capabilities.throttle, || {
                format!("{:.0}%", telemetry.throttle * 100.0)
            }),
            steering: supported(capabilities.steering_input, || {
                if steering_degrees.abs() < 0.5 {
                    "0°".to_owned()
                } else {
                    format!("{steering_degrees:+.0}°")
                }
            }),
            rpm: supported(capabilities.rpm, || format!("{:.0}", telemetry.rpm)),
            gear: supported(capabilities.gear, || gear),
            speed: supported(capabilities.speed, || format!("{speed:.0}")),
            speed_unit: if self.settings.speed_mph {
                "MPH"
            } else {
                "KM/H"
            },
            rev_lights: telemetry.rev_lights,
            brake_level: capabilities.brake.then_some(telemetry.brake),
            throttle_level: capabilities.throttle.then_some(telemetry.throttle),
            steering_level: capabilities
                .steering_input
                .then_some(telemetry.steering_input.clamp(-1.0, 1.0)),
            abs_active: capabilities.abs_activity && telemetry.abs_active,
            tc_active: capabilities.tc_activity && telemetry.tc_active,
        }
    }

    fn draw_labels(&self, painter: &Painter, rect: Rect, cache: &F1OpenHudCache) {
        let layout = OpenHudLayout::new(rect);
        let alpha = self.opacity;
        let label_font = display_font(12.5);
        let value_font = display_font(14.0);
        let label_top = layout.trace_rect.min.y + 13.0;
        let label_step = 24.0;
        let channels = [
            ("BRAKE", &cache.values.brake, BRAKE_COLOR),
            ("THROTTLE", &cache.values.throttle, THROTTLE_COLOR),
            ("SPEED", &cache.values.speed, SPEED_COLOR),
        ];

        for (index, (label, value, color)) in channels.into_iter().enumerate() {
            let y = label_top + index as f32 * label_step;
            luminous_text(
                painter,
                Pos2::new(layout.left_rect.min.x, y),
                Align2::LEFT_CENTER,
                label,
                label_font.clone(),
                color,
                color,
                alpha * if index == 2 { 0.76 } else { 0.94 },
                TextClass::Channel,
            );
            luminous_text(
                painter,
                Pos2::new(layout.left_rect.max.x, y),
                Align2::RIGHT_CENTER,
                value,
                value_font.clone(),
                color,
                color,
                alpha * if index == 2 { 0.78 } else { 1.0 },
                TextClass::Channel,
            );
        }

        for (head, color, strength) in [
            (cache.brake_head, BRAKE_COLOR, 1.0),
            (cache.throttle_head, THROTTLE_COLOR, 1.0),
            (cache.speed_head, SPEED_COLOR, 0.68),
        ] {
            if let Some(head) = head {
                draw_head(painter, head, color, alpha * strength);
            }
        }

        for division in 0..=GRID_DIVISIONS {
            let seconds = -self.settings.window_seconds
                + self.settings.window_seconds * division as f64 / GRID_DIVISIONS as f64;
            let x = egui::lerp(
                layout.trace_rect.x_range(),
                division as f32 / GRID_DIVISIONS as f32,
            );
            luminous_text(
                painter,
                Pos2::new(x, layout.trace_rect.max.y + 9.0),
                Align2::CENTER_TOP,
                if division == GRID_DIVISIONS {
                    "0".to_owned()
                } else {
                    format!("{seconds:.0}s")
                },
                FontId::new(8.0, FontFamily::Proportional),
                TEXT_SECONDARY,
                TEXT_SECONDARY,
                alpha * 0.46,
                TextClass::Secondary,
            );
        }

        for (meter, value, label, color) in [
            (
                layout.brake_meter_rect,
                &cache.values.brake,
                "BRAKE",
                BRAKE_COLOR,
            ),
            (
                layout.throttle_meter_rect,
                &cache.values.throttle,
                "THROTTLE",
                THROTTLE_COLOR,
            ),
        ] {
            luminous_text(
                painter,
                Pos2::new(meter.center().x, meter.min.y - 9.0),
                Align2::CENTER_BOTTOM,
                value,
                display_font(13.0),
                color,
                color,
                alpha,
                TextClass::Channel,
            );
            luminous_text(
                painter,
                Pos2::new(meter.center().x, meter.max.y + 8.0),
                Align2::CENTER_TOP,
                label,
                display_font(8.5),
                color,
                color,
                alpha * 0.78,
                TextClass::Channel,
            );
        }

        luminous_text(
            painter,
            Pos2::new(
                layout.steering_rect.center().x,
                layout.steering_rect.center().y + layout.steering_rect.height() * 0.14,
            ),
            Align2::CENTER_CENTER,
            &cache.values.steering,
            display_font(15.0),
            TEXT_PRIMARY,
            STEERING_COLOR,
            alpha,
            TextClass::Channel,
        );
        luminous_text(
            painter,
            Pos2::new(
                layout.steering_rect.center().x,
                layout.steering_rect.max.y - 2.0,
            ),
            Align2::CENTER_BOTTOM,
            "STEERING",
            display_font(8.0),
            TEXT_SECONDARY,
            STEERING_COLOR,
            alpha * 0.58,
            TextClass::Secondary,
        );

        let center = layout.center_rect.center();
        luminous_text(
            painter,
            Pos2::new(
                center.x,
                layout.center_rect.min.y + layout.center_rect.height() * 0.27,
            ),
            Align2::CENTER_CENTER,
            format!("{}  RPM", cache.values.rpm),
            display_font((layout.center_rect.width() * 0.075).clamp(12.0, 18.0)),
            TEXT_SECONDARY,
            TEXT_PRIMARY,
            alpha * 0.88,
            TextClass::Secondary,
        );
        luminous_text(
            painter,
            Pos2::new(
                center.x,
                layout.center_rect.min.y + layout.center_rect.height() * 0.49,
            ),
            Align2::CENTER_CENTER,
            &cache.values.gear,
            display_font((layout.center_rect.height() * 0.29).clamp(50.0, 94.0)),
            TEXT_PRIMARY,
            STEERING_COLOR,
            alpha,
            TextClass::Major,
        );
        luminous_text(
            painter,
            Pos2::new(
                center.x,
                layout.center_rect.min.y + layout.center_rect.height() * 0.72,
            ),
            Align2::CENTER_CENTER,
            &cache.values.speed,
            display_font((layout.center_rect.height() * 0.14).clamp(27.0, 48.0)),
            TEXT_PRIMARY,
            STEERING_COLOR,
            alpha * 0.94,
            TextClass::Major,
        );
        luminous_text(
            painter,
            Pos2::new(
                center.x,
                layout.center_rect.min.y + layout.center_rect.height() * 0.82,
            ),
            Align2::CENTER_CENTER,
            cache.values.speed_unit,
            display_font(10.0),
            TEXT_SECONDARY,
            TEXT_SECONDARY,
            alpha * 0.78,
            TextClass::Secondary,
        );

        let state_y = layout.center_rect.max.y - 19.0;
        if cache.values.abs_active {
            luminous_text(
                painter,
                Pos2::new(center.x - 28.0, state_y),
                Align2::CENTER_CENTER,
                "ABS",
                display_font(10.5),
                ABS_COLOR,
                ABS_COLOR,
                alpha,
                TextClass::Channel,
            );
        }
        if cache.values.tc_active {
            luminous_text(
                painter,
                Pos2::new(center.x + 28.0, state_y),
                Align2::CENTER_CENTER,
                "TC",
                display_font(10.5),
                THROTTLE_COLOR,
                THROTTLE_COLOR,
                alpha,
                TextClass::Channel,
            );
        }
    }
}

struct OpenHudLayout {
    left_rect: Rect,
    trace_rect: Rect,
    brake_meter_rect: Rect,
    throttle_meter_rect: Rect,
    steering_rect: Rect,
    center_rect: Rect,
}

impl OpenHudLayout {
    fn new(rect: Rect) -> Self {
        let outer = rect.shrink2(Vec2::new(14.0, 10.0));
        let left_width = outer.width() * 0.55;
        let instrument_width = (outer.width() * 0.15).clamp(96.0, 160.0);
        let gap = 14.0_f32.min(outer.width() * 0.025);
        let left_rect = Rect::from_min_size(outer.min, Vec2::new(left_width, outer.height()));
        let label_width = (left_width * 0.20).clamp(78.0, 116.0);
        let value_width = (left_width * 0.13).clamp(48.0, 74.0);
        let trace_rect = Rect::from_min_max(
            Pos2::new(
                left_rect.min.x + label_width,
                left_rect.min.y + outer.height() * 0.12,
            ),
            Pos2::new(
                left_rect.max.x - value_width,
                left_rect.max.y - outer.height() * 0.12,
            ),
        );
        let instrument_rect = Rect::from_min_size(
            Pos2::new(left_rect.max.x + gap, outer.min.y),
            Vec2::new(instrument_width, outer.height()),
        );
        let meter_top = instrument_rect.min.y + outer.height() * 0.15;
        let meter_bottom = instrument_rect.min.y + outer.height() * 0.53;
        let meter_width = (instrument_rect.width() * 0.12).clamp(8.0, 14.0);
        let brake_x = instrument_rect.min.x + instrument_rect.width() * 0.28;
        let throttle_x = instrument_rect.min.x + instrument_rect.width() * 0.72;
        let brake_meter_rect = Rect::from_min_max(
            Pos2::new(brake_x - meter_width * 0.5, meter_top),
            Pos2::new(brake_x + meter_width * 0.5, meter_bottom),
        );
        let throttle_meter_rect = Rect::from_min_max(
            Pos2::new(throttle_x - meter_width * 0.5, meter_top),
            Pos2::new(throttle_x + meter_width * 0.5, meter_bottom),
        );
        let steering_rect = Rect::from_min_max(
            Pos2::new(
                instrument_rect.min.x,
                instrument_rect.min.y + outer.height() * 0.57,
            ),
            instrument_rect.max,
        );
        let center_rect = Rect::from_min_max(
            Pos2::new(instrument_rect.max.x + gap, outer.min.y),
            outer.max,
        );
        Self {
            left_rect,
            trace_rect,
            brake_meter_rect,
            throttle_meter_rect,
            steering_rect,
            center_rect,
        }
    }
}

fn pedal_y(rect: Rect, value: f32) -> f32 {
    let pad = rect.height() * 0.06;
    rect.max.y - pad - value.clamp(0.0, 1.0) * (rect.height() - 2.0 * pad)
}

fn speed_y(rect: Rect, speed_ms: f32) -> f32 {
    pedal_y(rect, speed_ms / F1_SPEED_SCALE_MS)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SegmentState {
    Inactive,
    Center,
    Active,
    Edge,
}

enum SegmentGeometry {
    Rect(Rect, f32),
    Polygon(Vec<Pos2>),
}

fn push_luminous_segment(
    shapes: &mut Vec<Shape>,
    geometry: SegmentGeometry,
    color: Color32,
    state: SegmentState,
    opacity: f32,
) {
    let (body_alpha, glow_alpha, glow_width) = match state {
        SegmentState::Inactive => (0.045, 0.0, 0.0),
        SegmentState::Center => (0.30, 0.035, 2.0),
        SegmentState::Active => (0.82, 0.10, 3.0),
        SegmentState::Edge => (1.0, 0.15, 4.0),
    };
    let active = matches!(state, SegmentState::Active | SegmentState::Edge);

    match geometry {
        SegmentGeometry::Rect(rect, rounding) => {
            if glow_alpha > 0.0 {
                shapes.push(Shape::rect_filled(
                    rect.expand(glow_width * 0.65),
                    rounding + 1.0,
                    with_opacity(color, opacity * glow_alpha),
                ));
            }
            shapes.push(Shape::rect_filled(
                rect,
                rounding,
                with_opacity(color, opacity * body_alpha),
            ));
            if active {
                let highlight_y = rect.min.y + (rect.height() * 0.25).max(0.7);
                shapes.push(Shape::line_segment(
                    [
                        Pos2::new(rect.min.x + 1.0, highlight_y),
                        Pos2::new(rect.max.x - 1.0, highlight_y),
                    ],
                    Stroke::new(
                        0.65_f32,
                        with_opacity(
                            Color32::WHITE,
                            opacity
                                * if state == SegmentState::Edge {
                                    0.58
                                } else {
                                    0.36
                                },
                        ),
                    ),
                ));
            }
        }
        SegmentGeometry::Polygon(points) => {
            if glow_alpha > 0.0 {
                shapes.push(Shape::convex_polygon(
                    points.clone(),
                    with_opacity(color, opacity * glow_alpha * 0.55),
                    Stroke::new(glow_width, with_opacity(color, opacity * glow_alpha)),
                ));
            }
            shapes.push(Shape::convex_polygon(
                points,
                with_opacity(color, opacity * body_alpha),
                Stroke::new(
                    if active { 0.65_f32 } else { 0.0_f32 },
                    with_opacity(Color32::WHITE, opacity * if active { 0.30 } else { 0.0 }),
                ),
            ));
        }
    }
}

fn arc_segment(
    center: Pos2,
    inner_radius: f32,
    outer_radius: f32,
    angle_min: f32,
    angle_max: f32,
) -> Vec<Pos2> {
    let polar = |radius: f32, angle: f32| {
        Pos2::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        )
    };
    vec![
        polar(inner_radius, angle_min),
        polar(outer_radius, angle_min),
        polar(outer_radius, angle_max),
        polar(inner_radius, angle_max),
    ]
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
    painter.circle_filled(center, 5.5, with_opacity(color, opacity * 0.09));
    painter.circle_filled(center, 3.2, with_opacity(color, opacity * 0.26));
    painter.circle_filled(center, 1.75, with_opacity(Color32::WHITE, opacity));
}

#[derive(Clone, Copy)]
enum TextClass {
    Channel,
    Major,
    Secondary,
}

#[allow(clippy::too_many_arguments)]
fn luminous_text(
    painter: &Painter,
    position: Pos2,
    anchor: Align2,
    text: impl ToString,
    font: FontId,
    core_color: Color32,
    glow_color: Color32,
    opacity: f32,
    class: TextClass,
) {
    let text = text.to_string();
    painter.text(
        position + Vec2::new(0.9, 1.15),
        anchor,
        &text,
        font.clone(),
        with_opacity(Color32::from_black_alpha(150), opacity * 0.62),
    );

    let (spread, glow_alpha) = match class {
        TextClass::Channel => ((font.size * 0.11).clamp(1.25, 2.2), 0.16),
        TextClass::Major => ((font.size * 0.04).clamp(1.8, 3.2), 0.075),
        TextClass::Secondary => (0.0, 0.0),
    };
    if glow_alpha > 0.0 {
        for offset in [
            Vec2::new(-spread, 0.0),
            Vec2::new(spread, 0.0),
            Vec2::new(0.0, -spread),
            Vec2::new(0.0, spread),
        ] {
            painter.text(
                position + offset,
                anchor,
                &text,
                font.clone(),
                with_opacity(glow_color, opacity * glow_alpha),
            );
        }
    }
    painter.text(
        position,
        anchor,
        text,
        font,
        with_opacity(core_color, opacity),
    );
}
