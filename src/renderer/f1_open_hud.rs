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

const TEXT_PRIMARY: Color32 = Color32::from_rgb(244, 247, 250);
const TEXT_SECONDARY: Color32 = Color32::from_rgb(166, 178, 193);
const GRID_COLOR: Color32 = Color32::from_rgb(112, 132, 153);
const BRAKE_COLOR: Color32 = Color32::from_rgb(255, 91, 96);
const THROTTLE_COLOR: Color32 = Color32::from_rgb(74, 234, 158);
const STEERING_COLOR: Color32 = Color32::from_rgb(89, 184, 255);
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
    rev_lights: Option<RevLights>,
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
        cache.steering_head = None;
        cache.values = self.formatted_values();

        let layout = OpenHudLayout::new(rect);
        self.build_grid(layout.trace_rect, &mut cache.shapes);

        let capabilities = self.latest.map(|point| point.telemetry.capabilities);
        let mut brake_bands: [Vec<Pos2>; HISTORY_BANDS] = std::array::from_fn(|_| Vec::new());
        let mut throttle_bands: [Vec<Pos2>; HISTORY_BANDS] = std::array::from_fn(|_| Vec::new());
        let mut steering_bands: [Vec<Pos2>; HISTORY_BANDS] = std::array::from_fn(|_| Vec::new());
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
                Pos2::new(
                    x,
                    steering_y(layout.trace_rect, point.telemetry.steering_input),
                ),
            ];
            let band = ((recency * HISTORY_BANDS as f32) as usize).min(HISTORY_BANDS - 1);

            if let Some((previous_band, previous_positions)) = previous {
                if previous_band != band && brake_bands[band].is_empty() {
                    brake_bands[band].push(previous_positions[0]);
                    throttle_bands[band].push(previous_positions[1]);
                    steering_bands[band].push(previous_positions[2]);
                }
            }
            brake_bands[band].push(positions[0]);
            throttle_bands[band].push(positions[1]);
            steering_bands[band].push(positions[2]);
            previous = Some((band, positions));
        }

        if let Some(latest) = self.latest {
            let x = layout.trace_rect.max.x;
            let heads = [
                Pos2::new(x, pedal_y(layout.trace_rect, latest.telemetry.brake)),
                Pos2::new(x, pedal_y(layout.trace_rect, latest.telemetry.throttle)),
                Pos2::new(
                    x,
                    steering_y(layout.trace_rect, latest.telemetry.steering_input),
                ),
            ];
            if let Some((band, last)) = previous {
                if band != HISTORY_BANDS - 1 {
                    brake_bands[HISTORY_BANDS - 1].push(last[0]);
                    throttle_bands[HISTORY_BANDS - 1].push(last[1]);
                    steering_bands[HISTORY_BANDS - 1].push(last[2]);
                }
            }
            brake_bands[HISTORY_BANDS - 1].push(heads[0]);
            throttle_bands[HISTORY_BANDS - 1].push(heads[1]);
            steering_bands[HISTORY_BANDS - 1].push(heads[2]);

            if capabilities.is_some_and(|value| value.brake) {
                cache.brake_head = Some(heads[0]);
            }
            if capabilities.is_some_and(|value| value.throttle) {
                cache.throttle_head = Some(heads[1]);
            }
            if capabilities.is_some_and(|value| value.steering_input) {
                cache.steering_head = Some(heads[2]);
            }
        }

        if capabilities.is_some_and(|value| value.brake) {
            self.build_trace_layers(brake_bands, BRAKE_COLOR, &mut cache.shapes);
        }
        if capabilities.is_some_and(|value| value.throttle) {
            self.build_trace_layers(throttle_bands, THROTTLE_COLOR, &mut cache.shapes);
        }
        if capabilities.is_some_and(|value| value.steering_input) {
            self.build_trace_layers(steering_bands, STEERING_COLOR, &mut cache.shapes);
        }

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
        shapes: &mut Vec<Shape>,
    ) {
        for (band, points) in bands.into_iter().enumerate() {
            if points.len() < 2 {
                continue;
            }
            let recency = (band + 1) as f32 / HISTORY_BANDS as f32;
            let energy = (0.14 + 0.86 * recency.powf(1.35)) * self.opacity;
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

    fn build_rev_lights(&self, rect: Rect, rev_lights: Option<RevLights>, shapes: &mut Vec<Shape>) {
        let Some(rev_lights) = rev_lights else {
            return;
        };
        let gap = 3.0_f32;
        let width = (rect.width() - gap * (RPM_SEGMENTS - 1) as f32) / RPM_SEGMENTS as f32;
        let y = rect.min.y + rect.height() * 0.16;

        for index in 0..RPM_SEGMENTS {
            let x = rect.min.x + index as f32 * (width + gap);
            let segment = Rect::from_min_size(Pos2::new(x, y), Vec2::new(width.max(1.0), 4.5));
            let active = rev_lights.bit_value & (1_u16 << index) != 0;
            let base = if index < 10 { RPM_LOW } else { RPM_HIGH };
            if active {
                shapes.push(Shape::rect_filled(
                    segment.expand(2.0),
                    1.0,
                    with_opacity(base, self.opacity * 0.10),
                ));
            }
            shapes.push(Shape::rect_filled(
                segment,
                0.75,
                with_opacity(base, self.opacity * if active { 0.92 } else { 0.055 }),
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
            rev_lights: telemetry.rev_lights,
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
            ("STEERING", &cache.values.steering, STEERING_COLOR),
        ];

        for (index, (label, value, color)) in channels.into_iter().enumerate() {
            let y = label_top + index as f32 * label_step;
            shadowed_text(
                painter,
                Pos2::new(layout.left_rect.min.x, y),
                Align2::LEFT_CENTER,
                label,
                label_font.clone(),
                with_opacity(color, alpha * 0.92),
            );
            shadowed_text(
                painter,
                Pos2::new(layout.left_rect.max.x, y),
                Align2::RIGHT_CENTER,
                value,
                value_font.clone(),
                with_opacity(color, alpha),
            );
        }

        for (head, color) in [
            (cache.brake_head, BRAKE_COLOR),
            (cache.throttle_head, THROTTLE_COLOR),
            (cache.steering_head, STEERING_COLOR),
        ] {
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
                Pos2::new(x, layout.trace_rect.max.y + 9.0),
                Align2::CENTER_TOP,
                if division == GRID_DIVISIONS {
                    "0".to_owned()
                } else {
                    format!("{seconds:.0}s")
                },
                FontId::new(8.0, FontFamily::Proportional),
                with_opacity(TEXT_SECONDARY, alpha * 0.46),
            );
        }

        let center = layout.center_rect.center();
        shadowed_text(
            painter,
            Pos2::new(
                center.x,
                layout.center_rect.min.y + layout.center_rect.height() * 0.27,
            ),
            Align2::CENTER_CENTER,
            format!("{}  RPM", cache.values.rpm),
            display_font((layout.center_rect.width() * 0.075).clamp(12.0, 18.0)),
            with_opacity(TEXT_SECONDARY, alpha * 0.94),
        );
        shadowed_text(
            painter,
            Pos2::new(
                center.x,
                layout.center_rect.min.y + layout.center_rect.height() * 0.49,
            ),
            Align2::CENTER_CENTER,
            &cache.values.gear,
            display_font((layout.center_rect.height() * 0.29).clamp(50.0, 94.0)),
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
            display_font((layout.center_rect.height() * 0.14).clamp(27.0, 48.0)),
            with_opacity(TEXT_PRIMARY, alpha * 0.96),
        );
        shadowed_text(
            painter,
            Pos2::new(
                center.x,
                layout.center_rect.min.y + layout.center_rect.height() * 0.82,
            ),
            Align2::CENTER_CENTER,
            cache.values.speed_unit,
            display_font(10.0),
            with_opacity(TEXT_SECONDARY, alpha * 0.82),
        );

        let state_y = layout.center_rect.max.y - 19.0;
        if cache.values.abs_active {
            shadowed_text(
                painter,
                Pos2::new(center.x - 28.0, state_y),
                Align2::CENTER_CENTER,
                "ABS",
                display_font(10.5),
                with_opacity(ABS_COLOR, alpha),
            );
        }
        if cache.values.tc_active {
            shadowed_text(
                painter,
                Pos2::new(center.x + 28.0, state_y),
                Align2::CENTER_CENTER,
                "TC",
                display_font(10.5),
                with_opacity(THROTTLE_COLOR, alpha),
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
        let left_width = outer.width() * 0.67;
        let gap = 22.0_f32.min(outer.width() * 0.04);
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
        let center_rect =
            Rect::from_min_max(Pos2::new(left_rect.max.x + gap, outer.min.y), outer.max);
        Self {
            left_rect,
            trace_rect,
            center_rect,
        }
    }
}

fn pedal_y(rect: Rect, value: f32) -> f32 {
    let pad = rect.height() * 0.06;
    rect.max.y - pad - value.clamp(0.0, 1.0) * (rect.height() - 2.0 * pad)
}

fn steering_y(rect: Rect, value: f32) -> f32 {
    rect.center().y - value.clamp(-1.0, 1.0) * rect.height() * 0.44
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
        position + Vec2::new(1.0, 1.25),
        anchor,
        &text,
        font.clone(),
        Color32::from_black_alpha(145),
    );
    painter.text(position, anchor, text, font, color);
}
