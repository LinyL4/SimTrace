//! Experimental flat F1 HUD presentation.
//!
//! This renderer consumes the shared, already-windowed visualization snapshot.
//! It never reads or filters the telemetry buffer itself.

use crate::config::GraphSettings;
use crate::core::{RevLights, TelemetryPoint};
use crate::renderer::f1_brake_limit::{BrakeLimitFeedback, BrakeLimitParams};
use crate::renderer::f1_glow_wgpu::{self, GlowBatch, GlowBatchBuilder};
use egui::{
    Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Painter, Pos2, Rect, Shape,
    Stroke, Vec2,
};
use std::sync::Arc;

const DISPLAY_FONT_NAME: &str = "saira_semi_condensed_semibold";
const GRID_DIVISIONS: usize = 4;
const HISTORY_BANDS: usize = 8;
const RPM_SEGMENTS: usize = 15;
const PEDAL_SEGMENTS: usize = 15;
const STEERING_SEGMENTS_PER_SIDE: usize = 18;
const STEERING_ARC_DEGREES: f32 = 150.0;
const F1_SPEED_SCALE_MS: f32 = 100.0;

const TEXT_PRIMARY: Color32 = Color32::from_rgb(244, 247, 250);
const TEXT_SECONDARY: Color32 = Color32::from_rgb(166, 178, 193);
const GRID_COLOR: Color32 = Color32::from_rgb(112, 132, 153);
const STEERING_COLOR: Color32 = Color32::from_rgb(238, 245, 250);
const ABS_COLOR: Color32 = Color32::from_rgb(255, 194, 55);
/// Warm orange used for the (subtle) approaching hint over the brake red.
const LOCK_HOT: Color32 = Color32::from_rgb(255, 138, 69);
/// High-purity cyan for confirmed wheel-lock events.
const LOCK_CYAN: Color32 = Color32::from_rgb(56, 223, 255);
/// Lock history values below this are treated as "no lock".
const LOCK_HISTORY_THRESHOLD: f32 = 0.01;
/// Halo radius shared by the history glow paths (kept identical for A/B).
const HISTORY_GLOW_RADIUS: f32 = 7.5;
/// Per-point additive halo intensity scales (match the legacy per-segment values).
const BRAKE_TRACE_INTENSITY: f32 = 0.30;
const THROTTLE_TRACE_INTENSITY: f32 = 0.30;
const LOCK_TRACE_INTENSITY: f32 = 0.45;

/// Which geometry produces the Brake/Throttle history halo. The core stroke is
/// identical in both modes; only the additive halo generation changes.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum HistoryGlowMode {
    /// Legacy per-segment additive ribbons (kept as the A/B reference).
    Legacy,
    /// Continuous polyline mesh halo (default).
    #[default]
    Continuous,
}

/// Debug-only isolation of the history trace's visual layers.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum HistoryTraceLayers {
    /// Complete history trace composition.
    #[default]
    Full,
    /// Painter body/core only; no emission, endpoint, or lock projection.
    CoreOnly,
    /// Brake/Throttle WGPU history halo only.
    HaloOnly,
    /// Complete composition without the current-point head.
    NoEndpoint,
    /// Complete composition without the Brake lock projection.
    NoProjection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HistoryTraceLayerPolicy {
    painter_core: bool,
    painter_emission: bool,
    history_halo: bool,
    endpoint: bool,
    lock_projection: bool,
}

impl HistoryTraceLayers {
    fn policy(self) -> HistoryTraceLayerPolicy {
        match self {
            Self::Full => HistoryTraceLayerPolicy {
                painter_core: true,
                painter_emission: true,
                history_halo: true,
                endpoint: true,
                lock_projection: true,
            },
            Self::CoreOnly => HistoryTraceLayerPolicy {
                painter_core: true,
                painter_emission: false,
                history_halo: false,
                endpoint: false,
                lock_projection: false,
            },
            Self::HaloOnly => HistoryTraceLayerPolicy {
                painter_core: false,
                painter_emission: false,
                history_halo: true,
                endpoint: false,
                lock_projection: false,
            },
            Self::NoEndpoint => HistoryTraceLayerPolicy {
                painter_core: true,
                painter_emission: true,
                history_halo: true,
                endpoint: false,
                lock_projection: true,
            },
            Self::NoProjection => HistoryTraceLayerPolicy {
                painter_core: true,
                painter_emission: true,
                history_halo: true,
                endpoint: true,
                lock_projection: false,
            },
        }
    }

    fn uses_legacy_halo(self, glow_mode: HistoryGlowMode) -> bool {
        self.policy().history_halo && glow_mode == HistoryGlowMode::Legacy
    }

    fn uses_continuous_halo(self, glow_mode: HistoryGlowMode) -> bool {
        self.policy().history_halo && glow_mode == HistoryGlowMode::Continuous
    }
}

#[derive(Clone, Copy)]
struct ChannelPalette {
    emission: Color32,
    body: Color32,
    hot: Color32,
    text_face: Color32,
    text_glow: Color32,
}

const BRAKE: ChannelPalette = ChannelPalette {
    emission: Color32::from_rgb(255, 45, 52),
    body: Color32::from_rgb(255, 67, 72),
    hot: Color32::from_rgb(255, 184, 170),
    text_face: Color32::from_rgb(255, 86, 89),
    text_glow: Color32::from_rgb(255, 48, 56),
};
const THROTTLE: ChannelPalette = ChannelPalette {
    emission: Color32::from_rgb(0, 255, 85),
    body: Color32::from_rgb(30, 255, 106),
    hot: Color32::from_rgb(185, 255, 211),
    text_face: Color32::from_rgb(66, 244, 130),
    text_glow: Color32::from_rgb(0, 238, 99),
};
const SPEED: ChannelPalette = ChannelPalette {
    emission: Color32::from_rgb(0, 166, 255),
    body: Color32::from_rgb(40, 190, 255),
    hot: Color32::from_rgb(194, 241, 255),
    text_face: Color32::from_rgb(95, 190, 242),
    text_glow: Color32::from_rgb(0, 166, 255),
};
const RPM_LOW: ChannelPalette = ChannelPalette {
    emission: Color32::from_rgb(43, 151, 239),
    body: Color32::from_rgb(67, 170, 241),
    hot: Color32::from_rgb(171, 221, 255),
    text_face: Color32::from_rgb(67, 170, 241),
    text_glow: Color32::from_rgb(43, 151, 239),
};
const RPM_HIGH: ChannelPalette = ChannelPalette {
    emission: Color32::from_rgb(255, 45, 70),
    body: Color32::from_rgb(255, 66, 87),
    hot: Color32::from_rgb(255, 178, 180),
    text_face: Color32::from_rgb(255, 66, 87),
    text_glow: Color32::from_rgb(255, 45, 70),
};

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
    sharp_shapes: Vec<Shape>,
    fallback_emission_shapes: Vec<Shape>,
    glow_batch: GlowBatch,
    generation: u64,
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
    brake_limit: BrakeLimitFeedback,
    brake_params: &'a BrakeLimitParams,
    /// Per-sample confirmed-lock severity aligned with `points` by index.
    lock_history: &'a [f32],
    history_glow: HistoryGlowMode,
    trace_layers: HistoryTraceLayers,
}

impl<'a> F1OpenHud<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        points: &'a [TelemetryPoint],
        latest: Option<&'a TelemetryPoint>,
        settings: &'a GraphSettings,
        opacity: f32,
        max_steering_angle: f32,
        brake_limit: BrakeLimitFeedback,
        brake_params: &'a BrakeLimitParams,
        lock_history: &'a [f32],
        history_glow: HistoryGlowMode,
        trace_layers: HistoryTraceLayers,
    ) -> Self {
        Self {
            points,
            latest,
            settings,
            opacity,
            max_steering_angle,
            brake_limit,
            brake_params,
            lock_history,
            history_glow,
            trace_layers,
        }
    }

    pub fn show(
        &self,
        ui: &mut egui::Ui,
        size: Vec2,
        rebuild_visualization: bool,
        additive_glow_available: bool,
        cache: &mut F1OpenHudCache,
    ) {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::empty());
        let painter = ui.painter().with_clip_rect(rect);
        let rect_changed = cache.rect != Some(rect);

        if rebuild_visualization || rect_changed {
            self.rebuild(rect, cache);
        }

        let use_additive = additive_glow_available && !cache.glow_batch.is_empty();
        if use_additive {
            painter.add(f1_glow_wgpu::callback(rect, cache.glow_batch.clone()));
        } else {
            painter.extend(cache.fallback_emission_shapes.iter().cloned());
        }
        painter.extend(cache.sharp_shapes.iter().cloned());
        self.draw_labels(&painter, rect, cache, use_additive);
    }

    fn rebuild(&self, rect: Rect, cache: &mut F1OpenHudCache) {
        cache.rect = Some(rect);
        cache.sharp_shapes.clear();
        cache.fallback_emission_shapes.clear();
        cache.brake_head = None;
        cache.throttle_head = None;
        cache.speed_head = None;
        cache.values = self.formatted_values();
        cache.generation = cache.generation.wrapping_add(1);
        let mut glow = GlowBatchBuilder::new(rect);

        let layout = OpenHudLayout::new(rect);
        self.build_grid(layout.trace_rect, &mut cache.sharp_shapes);

        let capabilities = self.latest.map(|point| point.telemetry.capabilities);
        let mut brake_bands: [Vec<Pos2>; HISTORY_BANDS] = std::array::from_fn(|_| Vec::new());
        let mut throttle_bands: [Vec<Pos2>; HISTORY_BANDS] = std::array::from_fn(|_| Vec::new());
        let mut speed_bands: [Vec<Pos2>; HISTORY_BANDS] = std::array::from_fn(|_| Vec::new());
        // Parallel lock history for the brake bands, kept index-aligned with the
        // brake points so lock segments share the exact same time axis.
        let mut brake_lock_bands: [Vec<f32>; HISTORY_BANDS] = std::array::from_fn(|_| Vec::new());
        // Ordered full-trace polylines for the continuous halo (per-point energy,
        // no band steps, shared vertices at every sample including lock edges).
        let mut brake_line: Vec<Pos2> = Vec::new();
        let mut brake_colors: Vec<Color32> = Vec::new();
        let mut brake_intensity: Vec<f32> = Vec::new();
        let mut brake_primary_energy: Vec<f32> = Vec::new();
        let mut brake_primary_locked: Vec<bool> = Vec::new();
        let mut throttle_line: Vec<Pos2> = Vec::new();
        let mut throttle_colors: Vec<Color32> = Vec::new();
        let mut throttle_intensity: Vec<f32> = Vec::new();
        let mut throttle_primary_energy: Vec<f32> = Vec::new();
        let mut speed_line: Vec<Pos2> = Vec::new();
        let mut speed_primary_energy: Vec<f32> = Vec::new();
        let mut previous: Option<(usize, [Pos2; 3], f32)> = None;
        let now = std::time::Instant::now();
        let window_seconds = self.settings.window_seconds.max(0.001) as f32;

        // One shared history pass prepares all three channels in the same plot.
        for (index, point) in self.points.iter().enumerate() {
            let lock = self.lock_history.get(index).copied().unwrap_or(0.0);
            let age = now.duration_since(point.captured_at).as_secs_f32();
            let recency = (1.0 - age / window_seconds).clamp(0.0, 1.0);
            let x = egui::lerp(layout.trace_rect.x_range(), recency);
            let positions = [
                Pos2::new(x, pedal_y(layout.trace_rect, point.telemetry.brake)),
                Pos2::new(x, pedal_y(layout.trace_rect, point.telemetry.throttle)),
                Pos2::new(x, speed_y(layout.trace_rect, point.telemetry.speed)),
            ];
            let band = ((recency * HISTORY_BANDS as f32) as usize).min(HISTORY_BANDS - 1);

            let energy = history_energy(recency, self.opacity);
            let locked = lock > LOCK_HISTORY_THRESHOLD;
            brake_line.push(positions[0]);
            brake_colors.push(if locked { LOCK_CYAN } else { BRAKE.emission });
            brake_intensity.push(
                energy
                    * if locked {
                        LOCK_TRACE_INTENSITY
                    } else {
                        BRAKE_TRACE_INTENSITY
                    },
            );
            brake_primary_energy.push(energy);
            brake_primary_locked.push(locked);
            throttle_line.push(positions[1]);
            throttle_colors.push(THROTTLE.emission);
            throttle_intensity.push(energy * THROTTLE_TRACE_INTENSITY);
            throttle_primary_energy.push(energy);
            speed_line.push(positions[2]);
            speed_primary_energy.push(energy);

            if let Some((previous_band, previous_positions, previous_lock)) = previous {
                if previous_band != band {
                    // Structural continuity: never let a band boundary cut a
                    // segment. The previous band is extended with this sample
                    // (forward bridge) and the new band starts at the previous
                    // sample (backward bridge), so the two stroked polylines
                    // overlap by one full segment and there is no butt-cap gap.
                    brake_bands[previous_band].push(positions[0]);
                    throttle_bands[previous_band].push(positions[1]);
                    speed_bands[previous_band].push(positions[2]);
                    brake_lock_bands[previous_band].push(lock);
                    if brake_bands[band].is_empty() {
                        brake_bands[band].push(previous_positions[0]);
                        throttle_bands[band].push(previous_positions[1]);
                        speed_bands[band].push(previous_positions[2]);
                        brake_lock_bands[band].push(previous_lock);
                    }
                }
            }
            brake_bands[band].push(positions[0]);
            throttle_bands[band].push(positions[1]);
            speed_bands[band].push(positions[2]);
            brake_lock_bands[band].push(lock);
            previous = Some((band, positions, lock));
        }

        if let Some(latest) = self.latest {
            let x = layout.trace_rect.max.x;
            let heads = [
                Pos2::new(x, pedal_y(layout.trace_rect, latest.telemetry.brake)),
                Pos2::new(x, pedal_y(layout.trace_rect, latest.telemetry.throttle)),
                Pos2::new(x, speed_y(layout.trace_rect, latest.telemetry.speed)),
            ];
            let head_energy = history_energy(1.0, self.opacity);
            let head_locked =
                self.lock_history.last().copied().unwrap_or(0.0) > LOCK_HISTORY_THRESHOLD;
            brake_line.push(heads[0]);
            brake_colors.push(if head_locked {
                LOCK_CYAN
            } else {
                BRAKE.emission
            });
            brake_intensity.push(
                head_energy
                    * if head_locked {
                        LOCK_TRACE_INTENSITY
                    } else {
                        BRAKE_TRACE_INTENSITY
                    },
            );
            brake_primary_energy.push(head_energy);
            brake_primary_locked.push(head_locked);
            throttle_line.push(heads[1]);
            throttle_colors.push(THROTTLE.emission);
            throttle_intensity.push(head_energy * THROTTLE_TRACE_INTENSITY);
            throttle_primary_energy.push(head_energy);
            speed_line.push(heads[2]);
            speed_primary_energy.push(head_energy);
            let head_lock = self.lock_history.last().copied().unwrap_or(0.0);
            for (band, (positions, lock)) in
                head_bridge_samples(previous, (heads, head_lock), HISTORY_BANDS)
            {
                brake_bands[band].push(positions[0]);
                throttle_bands[band].push(positions[1]);
                speed_bands[band].push(positions[2]);
                brake_lock_bands[band].push(lock);
            }

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

        let painter_core = self.trace_layers.policy().painter_core;
        if capabilities.is_some_and(|value| value.brake) {
            if painter_core {
                push_continuous_primary_stroke(
                    &brake_line,
                    &brake_primary_energy,
                    BRAKE,
                    true,
                    Some(&brake_primary_locked),
                    &mut cache.sharp_shapes,
                );
            }
            self.build_brake_trace(
                &brake_bands,
                &brake_lock_bands,
                layout.trace_rect,
                &mut cache.sharp_shapes,
                &mut cache.fallback_emission_shapes,
                &mut glow,
            );
        }
        if capabilities.is_some_and(|value| value.throttle) {
            if painter_core {
                push_continuous_primary_stroke(
                    &throttle_line,
                    &throttle_primary_energy,
                    THROTTLE,
                    true,
                    None,
                    &mut cache.sharp_shapes,
                );
            }
            self.build_trace_layers(
                &throttle_bands,
                THROTTLE,
                true,
                true,
                &mut cache.sharp_shapes,
                &mut cache.fallback_emission_shapes,
                &mut glow,
            );
        }
        if capabilities.is_some_and(|value| value.speed) {
            if painter_core {
                push_continuous_primary_stroke(
                    &speed_line,
                    &speed_primary_energy,
                    SPEED,
                    false,
                    None,
                    &mut cache.sharp_shapes,
                );
            }
            self.build_trace_layers(
                &speed_bands,
                SPEED,
                false,
                false,
                &mut cache.sharp_shapes,
                &mut cache.fallback_emission_shapes,
                &mut glow,
            );
        }

        // Continuous halo: one tessellated polyline per trace, with a shared
        // vertex at every sample (including red/cyan lock edges).
        if self.trace_layers.uses_continuous_halo(self.history_glow) {
            if capabilities.is_some_and(|value| value.throttle) {
                glow.push_polyline(
                    &throttle_line,
                    &throttle_intensity,
                    &throttle_colors,
                    HISTORY_GLOW_RADIUS,
                );
            }
            if capabilities.is_some_and(|value| value.brake) {
                glow.push_polyline(
                    &brake_line,
                    &brake_intensity,
                    &brake_colors,
                    HISTORY_GLOW_RADIUS,
                );
            }
        }

        if self.trace_layers.policy().endpoint {
            for (head, palette) in [(cache.brake_head, BRAKE), (cache.throttle_head, THROTTLE)] {
                if let Some(head) = head {
                    glow.radial(head, 15.0, palette.emission, self.opacity * 0.34);
                }
            }
        }

        self.build_pedal_meter(
            layout.brake_meter_rect,
            cache.values.brake_level,
            BRAKE,
            Some(&self.brake_limit),
            &mut cache.sharp_shapes,
            &mut cache.fallback_emission_shapes,
            &mut glow,
        );
        self.build_pedal_meter(
            layout.throttle_meter_rect,
            cache.values.throttle_level,
            THROTTLE,
            None,
            &mut cache.sharp_shapes,
            &mut cache.fallback_emission_shapes,
            &mut glow,
        );
        self.build_steering_arc(
            layout.steering_rect,
            cache.values.steering_level,
            &mut cache.sharp_shapes,
        );

        self.build_rev_lights(
            layout.center_rect,
            cache.values.rev_lights,
            &mut cache.sharp_shapes,
            &mut cache.fallback_emission_shapes,
            &mut glow,
        );
        // A low-energy radial emitter gives the speed readout a continuous halo
        // without stacking offset glyph copies around its edges.
        glow.radial(
            gear_value_position(&layout),
            44.0,
            STEERING_COLOR,
            self.opacity * 0.11,
        );
        glow.radial(
            speed_value_position(&layout),
            34.0,
            STEERING_COLOR,
            self.opacity * 0.10,
        );
        cache.glow_batch = glow.finish(cache.generation);
    }

    fn band_energy(&self, band: usize) -> f32 {
        let recency = (band + 1) as f32 / HISTORY_BANDS as f32;
        (0.08 + 0.92 * recency.powf(1.6)) * self.opacity
    }

    /// Draw one contiguous run of the multi-layer history trace.
    #[allow(clippy::too_many_arguments)]
    fn draw_trace_run(
        &self,
        points: &[Pos2],
        palette: ChannelPalette,
        energy: f32,
        primary: bool,
        additive_emission: bool,
        sharp_shapes: &mut Vec<Shape>,
        fallback_emission_shapes: &mut Vec<Shape>,
        glow: &mut GlowBatchBuilder,
    ) {
        if points.len() < 2 {
            return;
        }
        let (outer_width, outer_alpha, inner_width, inner_alpha): (f32, f32, f32, f32) = if primary
        {
            (10.5, 0.055, 5.5, 0.17)
        } else {
            (6.0, 0.035, 3.2, 0.09)
        };
        let emission_shapes = if additive_emission {
            &mut *fallback_emission_shapes
        } else {
            &mut *sharp_shapes
        };
        let layer_policy = self.trace_layers.policy();
        if layer_policy.painter_emission {
            emission_shapes.push(Shape::line(
                points.to_vec(),
                Stroke::new(
                    outer_width,
                    with_opacity(palette.emission, energy * outer_alpha),
                ),
            ));
            emission_shapes.push(Shape::line(
                points.to_vec(),
                Stroke::new(
                    inner_width,
                    with_opacity(palette.emission, energy * inner_alpha),
                ),
            ));
        }
        if additive_emission && self.trace_layers.uses_legacy_halo(self.history_glow) {
            for segment in points.windows(2) {
                glow.ribbon_segment(segment[0], segment[1], 7.5, palette.emission, energy * 0.30);
            }
        }
        // The bright body/core is submitted once for the complete ordered trace
        // by `push_continuous_primary_stroke`. Band runs remain secondary layers
        // only; submitting their sharp strokes here would reintroduce butt-cap
        // seams on steep segments and at recency boundaries.
    }

    fn build_trace_layers(
        &self,
        bands: &[Vec<Pos2>; HISTORY_BANDS],
        palette: ChannelPalette,
        primary: bool,
        additive_emission: bool,
        sharp_shapes: &mut Vec<Shape>,
        fallback_emission_shapes: &mut Vec<Shape>,
        glow: &mut GlowBatchBuilder,
    ) {
        for (band, points) in bands.iter().enumerate() {
            self.draw_trace_run(
                points,
                palette,
                self.band_energy(band),
                primary,
                additive_emission,
                sharp_shapes,
                fallback_emission_shapes,
                glow,
            );
        }
    }

    /// Brake history drawn as a true state switch: non-lock runs keep the red
    /// trace (including its emission/glow), while confirmed-lock runs are drawn
    /// with a dominant cyan trace plus an upward projection. Lock runs draw no
    /// red at all, so the transition is RED -> CYAN -> RED rather than a
    /// red/cyan colour overlay.
    #[allow(clippy::too_many_arguments)]
    fn build_brake_trace(
        &self,
        bands: &[Vec<Pos2>; HISTORY_BANDS],
        lock_bands: &[Vec<f32>; HISTORY_BANDS],
        trace_rect: Rect,
        sharp_shapes: &mut Vec<Shape>,
        fallback_emission_shapes: &mut Vec<Shape>,
        glow: &mut GlowBatchBuilder,
    ) {
        for (band, points) in bands.iter().enumerate() {
            let energy = self.band_energy(band);
            let locks = &lock_bands[band];
            for run in collect_runs(points, locks, LOCK_HISTORY_THRESHOLD, false) {
                self.draw_trace_run(
                    &run,
                    BRAKE,
                    energy,
                    true,
                    true,
                    sharp_shapes,
                    fallback_emission_shapes,
                    glow,
                );
            }
            for run in collect_runs(points, locks, LOCK_HISTORY_THRESHOLD, true) {
                self.draw_lock_run(&run, trace_rect, sharp_shapes, glow);
            }
        }
    }

    /// Confirmed-lock history run: dominant cyan trace, stronger glow, and the
    /// upward projection. A single-sample run keeps its true width via a hot
    /// point plus a narrow projection.
    fn draw_lock_run(
        &self,
        run: &[Pos2],
        trace_rect: Rect,
        sharp_shapes: &mut Vec<Shape>,
        glow: &mut GlowBatchBuilder,
    ) {
        let alpha = self.opacity;
        let layer_policy = self.trace_layers.policy();
        if run.len() >= 2 && self.trace_layers.uses_legacy_halo(self.history_glow) {
            for segment in run.windows(2) {
                glow.ribbon_segment(segment[0], segment[1], 7.0, LOCK_CYAN, alpha * 0.45);
            }
        }
        if layer_policy.lock_projection {
            if let Some(mesh) = lock_projection_mesh(
                run,
                trace_rect,
                self.brake_params.lock_projection_alpha,
                self.brake_params.lock_projection_falloff,
                LOCK_CYAN,
                alpha,
            ) {
                sharp_shapes.push(Shape::mesh(mesh));
            }
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
        palette: ChannelPalette,
        feedback: Option<&BrakeLimitFeedback>,
        sharp_shapes: &mut Vec<Shape>,
        fallback_emission_shapes: &mut Vec<Shape>,
        glow: &mut GlowBatchBuilder,
    ) {
        let Some(level) = level else {
            return;
        };
        let (gap, segment_height) = pedal_segment_geometry(rect.height());
        let active_count = (level.clamp(0.0, 1.0) * PEDAL_SEGMENTS as f32).ceil() as usize;
        // Approaching is deliberately subtle: a small warm-orange mix only.
        let approach_mix = feedback.map_or(0.0, |value| value.approach_mix.clamp(0.0, 1.0));
        let lock_fl = feedback.map_or(0.0, |value| value.lock_fl.clamp(0.0, 1.0));
        let lock_fr = feedback.map_or(0.0, |value| value.lock_fr.clamp(0.0, 1.0));
        let pulse = feedback.map_or(0.0, |value| value.pulse.clamp(0.0, 1.0));
        let burn = feedback.map_or(0.0, |value| value.burn.clamp(0.0, 1.0));
        let base = mix_color(palette.body, LOCK_HOT, approach_mix);
        // Any confirmed lock drives the half to near-pure cyan so it reads as an
        // emergency state change rather than a light overlay on red.
        let lock_cover = |value: f32| {
            let t = ((value - 0.05) / 0.45).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let fl_cover = lock_cover(lock_fl);
        let fr_cover = lock_cover(lock_fr);
        let white = (burn * 0.30 + pulse * 0.18).clamp(0.0, 0.45);
        let left_color = mix_color(mix_color(base, LOCK_CYAN, fl_cover), Color32::WHITE, white);
        let right_color = mix_color(mix_color(base, LOCK_CYAN, fr_cover), Color32::WHITE, white);
        let glow_amount = fl_cover.max(fr_cover);

        for index in 0..PEDAL_SEGMENTS {
            let y = rect.max.y - (index + 1) as f32 * segment_height - index as f32 * gap;
            let segment = Rect::from_min_size(
                Pos2::new(rect.min.x, y),
                Vec2::new(rect.width(), segment_height),
            );
            if index < active_count {
                // Base segment (approaching tint), then per-side cyan emphasis so
                // FL-only lights the left half, FR-only the right half, and a
                // dual lock lights the whole bar.
                sharp_shapes.push(Shape::rect_filled(
                    segment,
                    1.0,
                    with_opacity(base, self.opacity * 0.92),
                ));
                let mid = segment.center().x;
                if fl_cover > 0.02 {
                    let half = Rect::from_min_max(segment.min, Pos2::new(mid, segment.max.y));
                    sharp_shapes.push(Shape::rect_filled(
                        half,
                        0.0,
                        with_opacity(left_color, self.opacity),
                    ));
                }
                if fr_cover > 0.02 {
                    let half = Rect::from_min_max(Pos2::new(mid, segment.min.y), segment.max);
                    sharp_shapes.push(Shape::rect_filled(
                        half,
                        0.0,
                        with_opacity(right_color, self.opacity),
                    ));
                }
                if glow_amount > 0.02 {
                    glow.rounded_rect(segment, 2.0, LOCK_CYAN, self.opacity * glow_amount * 0.7);
                    fallback_emission_shapes.push(Shape::rect_filled(
                        segment.expand(1.2),
                        2.0,
                        with_opacity(LOCK_CYAN, self.opacity * glow_amount * 0.30),
                    ));
                } else if approach_mix > 0.02 {
                    glow.rounded_rect(segment, 2.0, LOCK_HOT, self.opacity * approach_mix * 0.4);
                }
            } else {
                sharp_shapes.push(Shape::rect_filled(
                    segment,
                    1.0,
                    with_opacity(Color32::from_rgb(128, 132, 148), self.opacity * 0.28),
                ));
            }
        }
    }

    fn build_steering_arc(&self, rect: Rect, steering: Option<f32>, shapes: &mut Vec<Shape>) {
        let Some(steering) = steering else {
            return;
        };
        let side = STEERING_SEGMENTS_PER_SIDE;
        let segment_count = side * 2 + 1;
        let center_index = side;
        let center = Pos2::new(rect.center().x, rect.min.y + rect.height() * 0.70);
        let outer_radius = (rect.width() * 0.49).max(8.0);
        let inner_radius = (outer_radius - (outer_radius * 0.16).clamp(7.0, 12.0)).max(2.0);
        let total_arc = STEERING_ARC_DEGREES.to_radians();
        let center_angle = -std::f32::consts::FRAC_PI_2;
        let start = center_angle - total_arc * 0.5;
        let step = total_arc / segment_count as f32;
        let gap = step * 0.60;
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

    fn build_rev_lights(
        &self,
        rect: Rect,
        rev_lights: Option<RevLights>,
        sharp_shapes: &mut Vec<Shape>,
        fallback_emission_shapes: &mut Vec<Shape>,
        glow: &mut GlowBatchBuilder,
    ) {
        let Some(rev_lights) = rev_lights else {
            return;
        };
        let gap = 4.0_f32;
        let width = (rect.width() - gap * (RPM_SEGMENTS - 1) as f32) / RPM_SEGMENTS as f32;
        let y = rect.min.y + rect.height() * 0.16;

        let active_edge = (0..RPM_SEGMENTS)
            .rev()
            .find(|index| rev_lights.bit_value & (1_u16 << index) != 0);
        for index in 0..RPM_SEGMENTS {
            let x = rect.min.x + index as f32 * (width + gap);
            let segment = Rect::from_min_size(Pos2::new(x, y), Vec2::new(width.max(1.0), 5.5));
            let active = rev_lights.bit_value & (1_u16 << index) != 0;
            let palette = if index < 10 { RPM_LOW } else { RPM_HIGH };
            let state = if active_edge == Some(index) {
                SegmentState::Edge
            } else if active {
                SegmentState::Active
            } else {
                SegmentState::Inactive
            };
            push_additive_segment(
                sharp_shapes,
                fallback_emission_shapes,
                glow,
                SegmentGeometry::Rect(segment, 0.9),
                palette.emission,
                palette.body,
                palette.hot,
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

    fn draw_labels(
        &self,
        painter: &Painter,
        rect: Rect,
        cache: &F1OpenHudCache,
        additive_glow_active: bool,
    ) {
        let layout = OpenHudLayout::new(rect);
        let alpha = self.opacity;
        let label_font = display_font(12.5);
        let value_font = display_font(14.0);
        let label_top = layout.trace_rect.min.y + 13.0;
        let label_step = 24.0;

        if self.trace_layers.policy().endpoint {
            for (head, palette, strength, migrated) in [
                (cache.brake_head, BRAKE, 1.0, true),
                (cache.throttle_head, THROTTLE, 1.0, true),
                (cache.speed_head, SPEED, 0.68, false),
            ] {
                if let Some(head) = head {
                    if additive_glow_active && migrated {
                        draw_head_core(painter, head, palette, alpha * strength);
                    } else {
                        draw_head(painter, head, palette, alpha * strength);
                    }
                }
            }
        }

        let channels = [
            ("BRAKE", &cache.values.brake, BRAKE),
            ("THROTTLE", &cache.values.throttle, THROTTLE),
            ("SPEED", &cache.values.speed, SPEED),
        ];

        for (index, (label, value, palette)) in channels.into_iter().enumerate() {
            let y = label_top + index as f32 * label_step;
            luminous_text(
                painter,
                Pos2::new(layout.left_rect.min.x, y),
                Align2::LEFT_CENTER,
                label,
                label_font.clone(),
                palette.text_face,
                palette.text_glow,
                alpha * 0.98,
                TextClass::Channel,
            );
            luminous_text(
                painter,
                Pos2::new(layout.left_rect.max.x, y),
                Align2::RIGHT_CENTER,
                value,
                value_font.clone(),
                palette.text_face,
                palette.text_glow,
                alpha,
                TextClass::Channel,
            );
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

        for (meter, value, label, palette) in [
            (layout.brake_meter_rect, &cache.values.brake, "BRAKE", BRAKE),
            (
                layout.throttle_meter_rect,
                &cache.values.throttle,
                "THROTTLE",
                THROTTLE,
            ),
        ] {
            luminous_text(
                painter,
                Pos2::new(meter.center().x, meter.min.y - 9.0),
                Align2::CENTER_BOTTOM,
                value,
                display_font(13.0),
                palette.text_face,
                palette.text_glow,
                alpha,
                TextClass::Channel,
            );
            luminous_text(
                painter,
                Pos2::new(meter.center().x, meter.max.y + 8.0),
                Align2::CENTER_TOP,
                label,
                display_font(8.5),
                palette.text_face,
                palette.text_glow,
                alpha * 0.78,
                TextClass::Channel,
            );
        }

        luminous_text(
            painter,
            steering_value_position(&layout),
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
            steering_label_position(&layout),
            Align2::CENTER_CENTER,
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
            gear_value_position(&layout),
            Align2::CENTER_CENTER,
            &cache.values.gear,
            display_font((layout.center_rect.height() * 0.29).clamp(50.0, 94.0)),
            TEXT_PRIMARY,
            STEERING_COLOR,
            alpha,
            TextClass::Secondary,
        );
        luminous_text(
            painter,
            speed_value_position(&layout),
            Align2::CENTER_CENTER,
            &cache.values.speed,
            display_font((layout.center_rect.height() * 0.14).clamp(27.0, 48.0)),
            TEXT_PRIMARY,
            STEERING_COLOR,
            alpha * 0.94,
            TextClass::Secondary,
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
                THROTTLE.text_face,
                THROTTLE.text_glow,
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
        let meter_width = (instrument_rect.width() * 0.23).clamp(18.0, 28.0);
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

fn speed_value_position(layout: &OpenHudLayout) -> Pos2 {
    Pos2::new(
        layout.center_rect.center().x,
        layout.center_rect.min.y + layout.center_rect.height() * 0.72,
    )
}

fn gear_value_position(layout: &OpenHudLayout) -> Pos2 {
    Pos2::new(
        layout.center_rect.center().x,
        layout.center_rect.min.y + layout.center_rect.height() * 0.49,
    )
}

fn steering_value_position(layout: &OpenHudLayout) -> Pos2 {
    Pos2::new(
        layout.steering_rect.center().x,
        layout.steering_rect.min.y + layout.steering_rect.height() * 0.70,
    )
}

fn steering_label_position(layout: &OpenHudLayout) -> Pos2 {
    let value = steering_value_position(layout);
    Pos2::new(
        value.x,
        (value.y + 23.0).min(layout.steering_rect.max.y - 4.0),
    )
}

fn pedal_y(rect: Rect, value: f32) -> f32 {
    let pad = rect.height() * 0.06;
    rect.max.y - pad - value.clamp(0.0, 1.0) * (rect.height() - 2.0 * pad)
}

fn pedal_segment_geometry(total_height: f32) -> (f32, f32) {
    let gap = (total_height * 0.030).clamp(2.0, 4.0);
    let segment_height =
        ((total_height - gap * (PEDAL_SEGMENTS - 1) as f32) / PEDAL_SEGMENTS as f32).max(1.0);
    (gap, segment_height)
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

fn push_additive_segment(
    sharp_shapes: &mut Vec<Shape>,
    fallback_emission_shapes: &mut Vec<Shape>,
    glow: &mut GlowBatchBuilder,
    geometry: SegmentGeometry,
    emission_color: Color32,
    body_color: Color32,
    _hot_color: Color32,
    state: SegmentState,
    opacity: f32,
) {
    let SegmentGeometry::Rect(rect, rounding) = geometry else {
        unreachable!("the additive proof-of-concept only migrates rectangular segments");
    };
    let (body_alpha, inner_alpha, outer_alpha, spread) = match state {
        SegmentState::Inactive => (0.30, 0.0, 0.0, 0.0),
        SegmentState::Center => (0.95, 0.10, 0.035, 2.0),
        SegmentState::Active => (0.88, 0.15, 0.045, 4.0),
        SegmentState::Edge => (1.0, 0.22, 0.075, 5.0),
    };
    let active = matches!(state, SegmentState::Active | SegmentState::Edge);
    if outer_alpha > 0.0 {
        fallback_emission_shapes.push(Shape::rect_filled(
            rect.expand(spread),
            rounding + 2.0,
            with_opacity(emission_color, opacity * outer_alpha),
        ));
        fallback_emission_shapes.push(Shape::rect_filled(
            rect.expand(spread * 0.42),
            rounding + 1.0,
            with_opacity(emission_color, opacity * inner_alpha),
        ));
    }
    sharp_shapes.push(Shape::rect_filled(
        rect,
        rounding,
        with_opacity(
            if active {
                body_color
            } else {
                Color32::from_rgb(128, 132, 148)
            },
            opacity * body_alpha,
        ),
    ));
    if active {
        let intensity = if state == SegmentState::Edge {
            0.38
        } else {
            0.26
        };
        glow.rounded_rect(rect, 3.5, emission_color, opacity * intensity * 1.5);

        // Uniform LED face without a directional bevel highlight.
        sharp_shapes.push(Shape::rect_filled(
            rect.shrink(0.15),
            rounding,
            with_opacity(hot_face(body_color, 0.46), opacity * 0.92),
        ));
    }
}

fn push_luminous_segment(
    shapes: &mut Vec<Shape>,
    geometry: SegmentGeometry,
    color: Color32,
    state: SegmentState,
    opacity: f32,
) {
    let (body_alpha, inner_alpha, outer_alpha, spread) = match state {
        SegmentState::Inactive => (0.10, 0.0, 0.0, 0.0),
        SegmentState::Center => (0.95, 0.10, 0.035, 2.0),
        SegmentState::Active => (0.88, 0.15, 0.045, 4.0),
        SegmentState::Edge => (1.0, 0.22, 0.075, 5.0),
    };
    let active = matches!(state, SegmentState::Active | SegmentState::Edge);
    let hot = hot_face(color, 0.76);

    match geometry {
        SegmentGeometry::Rect(rect, rounding) => {
            if outer_alpha > 0.0 {
                shapes.push(Shape::rect_filled(
                    rect.expand(spread),
                    rounding + 2.0,
                    with_opacity(color, opacity * outer_alpha),
                ));
                shapes.push(Shape::rect_filled(
                    rect.expand(spread * 0.42),
                    rounding + 1.0,
                    with_opacity(color, opacity * inner_alpha),
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
                        0.8_f32,
                        with_opacity(
                            hot,
                            opacity
                                * if state == SegmentState::Edge {
                                    0.92
                                } else {
                                    0.68
                                },
                        ),
                    ),
                ));
            }
        }
        SegmentGeometry::Polygon(points) => {
            if outer_alpha > 0.0 {
                shapes.push(Shape::convex_polygon(
                    points.clone(),
                    with_opacity(color, opacity * outer_alpha * 0.45),
                    Stroke::new(spread, with_opacity(color, opacity * outer_alpha)),
                ));
                shapes.push(Shape::convex_polygon(
                    points.clone(),
                    with_opacity(color, opacity * inner_alpha * 0.55),
                    Stroke::new(spread * 0.42, with_opacity(color, opacity * inner_alpha)),
                ));
            }
            shapes.push(Shape::convex_polygon(
                points,
                with_opacity(color, opacity * body_alpha),
                Stroke::new(
                    if active { 0.75_f32 } else { 0.0_f32 },
                    with_opacity(hot, opacity * if active { 0.72 } else { 0.0 }),
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

/// Linear blend of two colours, `t = 0` returns `a`, `t = 1` returns `b`.
fn mix_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let [ar, ag, ab, aa] = a.to_array();
    let [br, bg, bb, _] = b.to_array();
    let lerp = |from: u8, to: u8| (from as f32 + (to as f32 - from as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(lerp(ar, br), lerp(ag, bg), lerp(ab, bb), aa)
}

/// Build the minimal head bridge without adding the head twice to the newest band.
fn head_bridge_samples(
    previous: Option<(usize, [Pos2; 3], f32)>,
    head: ([Pos2; 3], f32),
    band_count: usize,
) -> Vec<(usize, ([Pos2; 3], f32))> {
    let newest_band = band_count.saturating_sub(1);
    let mut samples = Vec::with_capacity(3);
    if let Some((previous_band, previous_positions, previous_lock)) = previous {
        samples.push((previous_band.min(newest_band), head));
        if previous_band != newest_band {
            samples.push((newest_band, (previous_positions, previous_lock)));
            samples.push((newest_band, head));
        }
    } else {
        samples.push((newest_band, head));
    }
    samples
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimaryStrokeState {
    Normal,
    BrakeLock,
}

#[derive(Clone, Copy, Debug)]
struct PrimaryStrokeSample {
    position: Pos2,
    energy: f32,
    state: PrimaryStrokeState,
}

#[derive(Clone, Copy)]
enum PrimaryStrokeLayer {
    Body,
    Core,
}

#[derive(Debug)]
struct PrimaryStrokeMeshes {
    body: egui::Mesh,
    core: egui::Mesh,
}

/// Submit the sharp history trace as two ordered meshes: one body and one core.
/// Recency and Brake lock state are vertex attributes, so neither one can split
/// the geometry into separate Painter paths.
fn push_continuous_primary_stroke(
    points: &[Pos2],
    energies: &[f32],
    palette: ChannelPalette,
    primary: bool,
    brake_locked: Option<&[bool]>,
    shapes: &mut Vec<Shape>,
) {
    if let Some(meshes) =
        build_continuous_primary_stroke(points, energies, palette, primary, brake_locked)
    {
        shapes.push(Shape::mesh(meshes.body));
        shapes.push(Shape::mesh(meshes.core));
    }
}

fn build_continuous_primary_stroke(
    points: &[Pos2],
    energies: &[f32],
    palette: ChannelPalette,
    primary: bool,
    brake_locked: Option<&[bool]>,
) -> Option<PrimaryStrokeMeshes> {
    let count = points.len().min(energies.len());
    if count < 2 {
        return None;
    }

    let state_at = |index: usize| {
        if brake_locked
            .and_then(|states| states.get(index))
            .copied()
            .unwrap_or(false)
        {
            PrimaryStrokeState::BrakeLock
        } else {
            PrimaryStrokeState::Normal
        }
    };
    let mut samples = Vec::with_capacity(count * 2);
    samples.push(PrimaryStrokeSample {
        position: points[0],
        energy: energies[0],
        state: state_at(0),
    });
    for index in 1..count {
        let previous_state = state_at(index - 1);
        let state = state_at(index);
        if state != previous_state {
            let boundary_position = points[index - 1].lerp(points[index], 0.5);
            let boundary_energy = (energies[index - 1] + energies[index]) * 0.5;
            // Coincident cross-sections make the palette switch instantaneous:
            // the preceding triangles end in the old colour and the following
            // triangles begin in the new one, while the centreline never gaps.
            samples.push(PrimaryStrokeSample {
                position: boundary_position,
                energy: boundary_energy,
                state: previous_state,
            });
            samples.push(PrimaryStrokeSample {
                position: boundary_position,
                energy: boundary_energy,
                state,
            });
        }
        samples.push(PrimaryStrokeSample {
            position: points[index],
            energy: energies[index],
            state,
        });
    }

    Some(PrimaryStrokeMeshes {
        body: primary_stroke_layer_mesh(&samples, palette, primary, PrimaryStrokeLayer::Body),
        core: primary_stroke_layer_mesh(&samples, palette, primary, PrimaryStrokeLayer::Core),
    })
}

fn primary_stroke_layer_mesh(
    samples: &[PrimaryStrokeSample],
    palette: ChannelPalette,
    primary: bool,
    layer: PrimaryStrokeLayer,
) -> egui::Mesh {
    let mut mesh = egui::Mesh::default();
    let positions: Vec<Pos2> = samples.iter().map(|sample| sample.position).collect();
    let normals: Vec<Vec2> = (0..samples.len())
        .map(|index| strip_normal_at(&positions, index))
        .collect();

    for (index, sample) in samples.iter().enumerate() {
        let (width, color, alpha) = primary_stroke_visual(palette, primary, sample.state, layer);
        let color = with_opacity(color, sample.energy * alpha);
        let offset = normals[index] * (width * 0.5);
        mesh.colored_vertex(sample.position + offset, color);
        mesh.colored_vertex(sample.position - offset, color);
        if index > 0 {
            let previous = (index as u32 - 1) * 2;
            let current = index as u32 * 2;
            mesh.add_triangle(previous, current, current + 1);
            mesh.add_triangle(previous, current + 1, previous + 1);
        }
    }
    mesh
}

fn primary_stroke_visual(
    palette: ChannelPalette,
    primary: bool,
    state: PrimaryStrokeState,
    layer: PrimaryStrokeLayer,
) -> (f32, Color32, f32) {
    if state == PrimaryStrokeState::BrakeLock {
        return match layer {
            PrimaryStrokeLayer::Body => (3.4, LOCK_CYAN, 0.98),
            PrimaryStrokeLayer::Core => (1.5, Color32::from_rgb(220, 248, 255), 1.0),
        };
    }
    match (primary, layer) {
        (true, PrimaryStrokeLayer::Body) => (2.45, palette.body, 0.88),
        (true, PrimaryStrokeLayer::Core) => (0.9, palette.hot, 0.96),
        (false, PrimaryStrokeLayer::Body) => (1.55, palette.body, 0.62),
        (false, PrimaryStrokeLayer::Core) => (0.6, palette.hot, 0.58),
    }
}

/// Average the incoming and outgoing directions for a stable bevel-like join.
/// Coincident hard-switch vertices are skipped when finding neighbours, so both
/// copies receive the same cross-section and meet without a wedge or a gap.
fn strip_normal_at(points: &[Pos2], index: usize) -> Vec2 {
    const EPSILON_SQUARED: f32 = 1.0e-8;
    let position = points[index];
    let previous = (0..index)
        .rev()
        .map(|candidate| position - points[candidate])
        .find(|delta| delta.length_sq() > EPSILON_SQUARED)
        .map(normalized_vec2);
    let next = ((index + 1)..points.len())
        .map(|candidate| points[candidate] - position)
        .find(|delta| delta.length_sq() > EPSILON_SQUARED)
        .map(normalized_vec2);
    let direction = match (previous, next) {
        (Some(incoming), Some(outgoing)) => {
            let average = incoming + outgoing;
            if average.length_sq() > EPSILON_SQUARED {
                normalized_vec2(average)
            } else {
                outgoing
            }
        }
        (Some(incoming), None) => incoming,
        (None, Some(outgoing)) => outgoing,
        (None, None) => Vec2::X,
    };
    Vec2::new(-direction.y, direction.x)
}

fn normalized_vec2(vector: Vec2) -> Vec2 {
    vector / vector.length().max(1.0e-4)
}

/// Continuous per-point halo energy.
///
/// The legacy path evaluates one energy per band at the band's *upper* recency
/// (`(band + 1) / HISTORY_BANDS`), which biases it brighter than a raw per-point
/// `recency^1.6`. To keep the Continuous halo at the same energy level while
/// removing the 8 hard steps, interpolate linearly between the legacy per-band
/// values across each band: it matches the legacy value exactly at every band
/// boundary and never jumps.
fn history_energy(recency: f32, opacity: f32) -> f32 {
    let band_value = |band: usize| {
        let band = band.min(HISTORY_BANDS - 1);
        let upper = ((band + 1) as f32 / HISTORY_BANDS as f32).min(1.0);
        (0.08 + 0.92 * upper.powf(1.6)) * opacity
    };
    let scaled = (recency.clamp(0.0, 1.0) * HISTORY_BANDS as f32).min(HISTORY_BANDS as f32);
    let band = scaled.floor() as usize;
    let t = (scaled - band as f32).clamp(0.0, 1.0);
    let current = band_value(band);
    let next = band_value(band + 1);
    current + (next - current) * t
}

/// Group index-aligned lock values into contiguous runs of chart points.
///
/// `want_locked = true` returns confirmed-lock runs (cyan), `false` returns the
/// non-lock runs (red). Runs keep the exact sample spacing; a single-sample lock
/// yields a one-point run (its true 1-2 px width) and is never widened.
fn collect_runs(
    points: &[Pos2],
    locks: &[f32],
    threshold: f32,
    want_locked: bool,
) -> Vec<Vec<Pos2>> {
    let mut runs: Vec<Vec<Pos2>> = Vec::new();
    let mut current: Vec<Pos2> = Vec::new();
    for (index, point) in points.iter().enumerate() {
        let locked = locks.get(index).copied().unwrap_or(0.0) > threshold;
        if locked == want_locked {
            current.push(*point);
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

/// Build a vertical cyan gradient strip from a lock run up to the chart top.
///
/// Geometry is clamped to `rect` so the projection never leaves the brake
/// chart. Alpha fades from `alpha_near` at the trace to zero at the top, with a
/// mid-point controlled by `falloff`.
fn lock_projection_mesh(
    run: &[Pos2],
    rect: Rect,
    alpha_near: f32,
    falloff: f32,
    color: Color32,
    opacity: f32,
) -> Option<egui::Mesh> {
    if run.is_empty() || rect.height() <= 0.0 {
        return None;
    }
    let near = (alpha_near * opacity).clamp(0.0, 1.0);
    if near <= 0.0001 {
        return None;
    }
    let mid_alpha = (near * falloff.clamp(0.0, 1.0)).clamp(0.0, 1.0);
    let bottom_color = with_opacity(color, near);
    let mid_color = with_opacity(color, mid_alpha);
    let top_color = with_opacity(color, 0.0);
    let top_y = rect.min.y;
    let clamp_x = |x: f32| x.clamp(rect.min.x, rect.max.x);
    let clamp_y = |y: f32| y.clamp(rect.min.y, rect.max.y);

    let mut columns: Vec<(f32, f32)> = run
        .iter()
        .map(|point| (clamp_x(point.x), clamp_y(point.y)))
        .collect();
    if columns.len() == 1 {
        let (x, y) = columns[0];
        columns = vec![(clamp_x(x - 0.6), y), (clamp_x(x + 0.6), y)];
    }

    const ROWS: u32 = 3;
    let mut mesh = egui::Mesh::default();
    for (x, y) in &columns {
        let mid_y = (y + top_y) * 0.5;
        mesh.colored_vertex(Pos2::new(*x, *y), bottom_color);
        mesh.colored_vertex(Pos2::new(*x, mid_y), mid_color);
        mesh.colored_vertex(Pos2::new(*x, top_y), top_color);
    }
    for column in 0..columns.len().saturating_sub(1) as u32 {
        let base = column * ROWS;
        let next = (column + 1) * ROWS;
        // Lower band (trace -> mid).
        mesh.add_triangle(base, base + 1, next + 1);
        mesh.add_triangle(base, next + 1, next);
        // Upper band (mid -> top).
        mesh.add_triangle(base + 1, base + 2, next + 2);
        mesh.add_triangle(base + 1, next + 2, next + 1);
    }
    Some(mesh)
}

fn hot_face(color: Color32, white_mix: f32) -> Color32 {
    let [r, g, b, a] = color.to_array();
    let mix = white_mix.clamp(0.0, 1.0);
    let heat = |channel: u8| channel as f32 + (255.0 - channel as f32) * mix;
    Color32::from_rgba_unmultiplied(heat(r) as u8, heat(g) as u8, heat(b) as u8, a)
}

fn draw_head(painter: &Painter, center: Pos2, palette: ChannelPalette, opacity: f32) {
    painter.circle_filled(
        center,
        15.0,
        with_opacity(palette.emission, opacity * 0.045),
    );
    painter.circle_filled(center, 8.0, with_opacity(palette.emission, opacity * 0.15));
    draw_head_core(painter, center, palette, opacity);
}

fn draw_head_core(painter: &Painter, center: Pos2, palette: ChannelPalette, opacity: f32) {
    painter.circle_filled(center, 4.2, with_opacity(palette.body, opacity * 0.88));
    painter.circle_filled(center, 1.8, with_opacity(palette.hot, opacity));
}

#[derive(Clone, Copy)]
enum TextClass {
    Channel,
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
        with_opacity(Color32::from_black_alpha(145), opacity * 0.30),
    );

    let (outer_spread, outer_alpha, inner_spread, inner_alpha, face_mix) = match class {
        TextClass::Channel => (
            (font.size * 0.20).clamp(2.4, 4.0),
            0.05,
            (font.size * 0.09).clamp(1.0, 2.0),
            0.14,
            0.16,
        ),
        TextClass::Secondary => (0.0, 0.0, 0.0, 0.0, 0.0),
    };
    if outer_alpha > 0.0 {
        for offset in [
            Vec2::new(-outer_spread, 0.0),
            Vec2::new(outer_spread, 0.0),
            Vec2::new(0.0, -outer_spread),
            Vec2::new(0.0, outer_spread),
        ] {
            painter.text(
                position + offset,
                anchor,
                &text,
                font.clone(),
                with_opacity(glow_color, opacity * outer_alpha),
            );
        }
        for offset in [
            Vec2::new(-inner_spread, 0.0),
            Vec2::new(inner_spread, 0.0),
            Vec2::new(0.0, -inner_spread),
            Vec2::new(0.0, inner_spread),
        ] {
            painter.text(
                position + offset,
                anchor,
                &text,
                font.clone(),
                with_opacity(glow_color, opacity * inner_alpha),
            );
        }
    }
    painter.text(
        position,
        anchor,
        text,
        font,
        with_opacity(hot_face(core_color, face_mix), opacity),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_trace_layers_enable_complete_composition() {
        assert_eq!(
            HistoryTraceLayers::Full.policy(),
            HistoryTraceLayerPolicy {
                painter_core: true,
                painter_emission: true,
                history_halo: true,
                endpoint: true,
                lock_projection: true,
            }
        );
    }

    #[test]
    fn core_only_submits_only_history_painter_core() {
        assert_eq!(
            HistoryTraceLayers::CoreOnly.policy(),
            HistoryTraceLayerPolicy {
                painter_core: true,
                painter_emission: false,
                history_halo: false,
                endpoint: false,
                lock_projection: false,
            }
        );
        assert!(!HistoryTraceLayers::CoreOnly.uses_legacy_halo(HistoryGlowMode::Legacy));
        assert!(!HistoryTraceLayers::CoreOnly.uses_continuous_halo(HistoryGlowMode::Continuous));
    }

    #[test]
    fn halo_only_keeps_both_glow_ab_routes_without_painter_layers() {
        let policy = HistoryTraceLayers::HaloOnly.policy();
        assert!(!policy.painter_core);
        assert!(!policy.painter_emission);
        assert!(!policy.endpoint);
        assert!(!policy.lock_projection);
        assert!(HistoryTraceLayers::HaloOnly.uses_legacy_halo(HistoryGlowMode::Legacy));
        assert!(HistoryTraceLayers::HaloOnly.uses_continuous_halo(HistoryGlowMode::Continuous));
    }

    fn rebuild_halo_mode(glow_mode: HistoryGlowMode, layers: HistoryTraceLayers) -> F1OpenHudCache {
        let capabilities = crate::core::TelemetryCapabilities {
            brake: true,
            throttle: true,
            speed: true,
            ..Default::default()
        };
        let now = std::time::Instant::now();
        let points = vec![
            TelemetryPoint {
                captured_at: now - std::time::Duration::from_millis(20),
                telemetry: crate::core::VehicleTelemetry {
                    brake: 0.2,
                    throttle: 0.8,
                    speed: 40.0,
                    capabilities,
                    ..Default::default()
                },
                abs_active: false,
                source: Default::default(),
            },
            TelemetryPoint {
                captured_at: now,
                telemetry: crate::core::VehicleTelemetry {
                    brake: 0.8,
                    throttle: 0.2,
                    speed: 60.0,
                    capabilities,
                    ..Default::default()
                },
                abs_active: false,
                source: Default::default(),
            },
        ];
        let settings = crate::config::AppSettings::default().graph;
        let brake_params = BrakeLimitParams::default();
        let hud = F1OpenHud::new(
            &points,
            points.last(),
            &settings,
            1.0,
            450.0,
            BrakeLimitFeedback::default(),
            &brake_params,
            &[0.0, 0.0],
            glow_mode,
            layers,
        );
        let mut cache = F1OpenHudCache::default();
        hud.rebuild(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 420.0)),
            &mut cache,
        );
        cache
    }

    #[test]
    fn halo_only_continuous_submits_non_empty_mesh_batch() {
        let cache = rebuild_halo_mode(HistoryGlowMode::Continuous, HistoryTraceLayers::HaloOnly);
        assert!(cache.glow_batch.has_mesh());
        assert!(!cache.glow_batch.is_empty());
    }

    #[test]
    fn full_continuous_still_submits_original_continuous_halo() {
        let cache = rebuild_halo_mode(HistoryGlowMode::Continuous, HistoryTraceLayers::Full);
        assert!(cache.glow_batch.has_mesh());
        assert!(!cache.glow_batch.is_empty());
    }

    #[test]
    fn core_only_uses_six_primary_meshes_without_segmented_paths() {
        let cache = rebuild_halo_mode(HistoryGlowMode::Continuous, HistoryTraceLayers::CoreOnly);
        let meshes = cache
            .sharp_shapes
            .iter()
            .filter(|shape| matches!(shape, Shape::Mesh(_)))
            .count();
        assert_eq!(meshes, 6, "body/core mesh for each of three traces");
        assert!(!cache.glow_batch.has_mesh());
    }

    #[test]
    fn halo_only_does_not_submit_painter_primary_meshes() {
        let cache = rebuild_halo_mode(HistoryGlowMode::Continuous, HistoryTraceLayers::HaloOnly);
        assert!(cache
            .sharp_shapes
            .iter()
            .all(|shape| !matches!(shape, Shape::Mesh(_))));
    }

    #[test]
    fn halo_only_legacy_still_submits_instance_batch() {
        let cache = rebuild_halo_mode(HistoryGlowMode::Legacy, HistoryTraceLayers::HaloOnly);
        assert!(cache.glow_batch.has_instances());
        assert!(!cache.glow_batch.is_empty());
    }

    #[test]
    fn no_endpoint_only_removes_history_heads() {
        let policy = HistoryTraceLayers::NoEndpoint.policy();
        assert!(policy.painter_core);
        assert!(policy.painter_emission);
        assert!(policy.history_halo);
        assert!(!policy.endpoint);
        assert!(policy.lock_projection);
    }

    #[test]
    fn no_projection_only_removes_lock_projection() {
        let policy = HistoryTraceLayers::NoProjection.policy();
        assert!(policy.painter_core);
        assert!(policy.painter_emission);
        assert!(policy.history_halo);
        assert!(policy.endpoint);
        assert!(!policy.lock_projection);
    }

    fn continuous_primary(
        points: &[Pos2],
        energies: &[f32],
        palette: ChannelPalette,
        primary: bool,
        locks: Option<&[bool]>,
    ) -> PrimaryStrokeMeshes {
        build_continuous_primary_stroke(points, energies, palette, primary, locks)
            .expect("continuous primary mesh")
    }

    fn section_center(mesh: &egui::Mesh, section: usize) -> Pos2 {
        let left = mesh.vertices[section * 2].pos;
        let right = mesh.vertices[section * 2 + 1].pos;
        left.lerp(right, 0.5)
    }

    fn assert_one_connected_strip(mesh: &egui::Mesh, sections: usize) {
        assert_eq!(mesh.vertices.len(), sections * 2);
        assert_eq!(mesh.indices.len(), (sections - 1) * 6);
        assert!(mesh.vertices.iter().all(|vertex| vertex.pos.is_finite()));
        assert!(mesh
            .indices
            .iter()
            .all(|index| (*index as usize) < mesh.vertices.len()));
    }

    #[test]
    fn throttle_primary_crosses_recency_boundary_without_split() {
        let boundary = 4.0 / HISTORY_BANDS as f32;
        let points = [point(0.0, 10.0), point(2.0, 20.0), point(4.0, 15.0)];
        let energies = [
            history_energy(boundary - 0.001, 1.0),
            history_energy(boundary, 1.0),
            history_energy(boundary + 0.001, 1.0),
        ];
        let meshes = continuous_primary(&points, &energies, THROTTLE, true, None);
        assert_one_connected_strip(&meshes.body, points.len());
        assert_one_connected_strip(&meshes.core, points.len());
    }

    #[test]
    fn speed_primary_crosses_recency_boundary_without_wgpu_or_split() {
        let points = [point(0.0, 12.0), point(2.0, 18.0), point(4.0, 14.0)];
        let energies = [0.2, 0.21, 0.22];
        let meshes = continuous_primary(&points, &energies, SPEED, false, None);
        assert_one_connected_strip(&meshes.body, points.len());
        assert_one_connected_strip(&meshes.core, points.len());
    }

    #[test]
    fn brake_primary_crosses_recency_boundary_without_split() {
        let points = [point(0.0, 14.0), point(2.0, 20.0), point(4.0, 16.0)];
        let energies = [0.2, 0.21, 0.22];
        let meshes = continuous_primary(&points, &energies, BRAKE, true, Some(&[false; 3]));
        assert_one_connected_strip(&meshes.body, points.len());
        assert_one_connected_strip(&meshes.core, points.len());
    }

    #[test]
    fn steep_vertical_primary_segments_remain_finite_and_connected() {
        let points = [point(0.0, 10.0), point(0.05, 90.0), point(0.1, 12.0)];
        let meshes = continuous_primary(&points, &[0.4, 0.5, 0.6], BRAKE, true, None);
        assert_one_connected_strip(&meshes.body, points.len());
        assert_one_connected_strip(&meshes.core, points.len());
    }

    fn assert_hard_switch(
        locks: &[bool],
        old_state: PrimaryStrokeState,
        new_state: PrimaryStrokeState,
    ) {
        let points = [point(0.0, 10.0), point(2.0, 90.0)];
        let meshes = continuous_primary(&points, &[1.0, 1.0], BRAKE, true, Some(locks));
        assert_one_connected_strip(&meshes.body, 4);
        assert_eq!(
            section_center(&meshes.body, 1),
            section_center(&meshes.body, 2)
        );
        let (_, old_color, old_alpha) =
            primary_stroke_visual(BRAKE, true, old_state, PrimaryStrokeLayer::Body);
        let (_, new_color, new_alpha) =
            primary_stroke_visual(BRAKE, true, new_state, PrimaryStrokeLayer::Body);
        assert_eq!(
            meshes.body.vertices[2].color,
            with_opacity(old_color, old_alpha)
        );
        assert_eq!(
            meshes.body.vertices[4].color,
            with_opacity(new_color, new_alpha)
        );
    }

    #[test]
    fn brake_red_to_cyan_is_continuous_and_hard_switched() {
        assert_hard_switch(
            &[false, true],
            PrimaryStrokeState::Normal,
            PrimaryStrokeState::BrakeLock,
        );
    }

    #[test]
    fn brake_cyan_to_red_is_continuous_and_hard_switched() {
        assert_hard_switch(
            &[true, false],
            PrimaryStrokeState::BrakeLock,
            PrimaryStrokeState::Normal,
        );
    }

    #[test]
    fn lock_grip_relock_primary_is_one_continuous_mesh() {
        let points: Vec<Pos2> = (0..5).map(|x| point(x as f32, (x * 10) as f32)).collect();
        let locks = [true, true, false, false, true];
        let meshes = continuous_primary(&points, &[0.4; 5], BRAKE, true, Some(&locks));
        // Five samples plus two coincident sections for each of two switches.
        assert_one_connected_strip(&meshes.body, 9);
    }

    #[test]
    fn single_sample_lock_preserves_core_before_and_after() {
        let points = [point(0.0, 10.0), point(2.0, 90.0), point(4.0, 10.0)];
        let locks = [false, true, false];
        let meshes = continuous_primary(&points, &[0.4, 0.5, 0.6], BRAKE, true, Some(&locks));
        // Both edges of the one-sample lock have coincident hard-switch sections.
        assert_one_connected_strip(&meshes.core, 7);
        assert_eq!(
            section_center(&meshes.core, 1),
            section_center(&meshes.core, 2)
        );
        assert_eq!(
            section_center(&meshes.core, 4),
            section_center(&meshes.core, 5)
        );
    }

    #[test]
    fn head_bridge_does_not_repeat_head_inside_newest_band() {
        let last = ([point(8.0, 1.0), point(8.0, 2.0), point(8.0, 3.0)], 0.0);
        let head = ([point(10.0, 1.0), point(10.0, 2.0), point(10.0, 3.0)], 0.0);
        let samples = head_bridge_samples(
            Some((HISTORY_BANDS - 1, last.0, last.1)),
            head,
            HISTORY_BANDS,
        );
        assert_eq!(samples, vec![(HISTORY_BANDS - 1, head)]);

        let bridged = head_bridge_samples(
            Some((HISTORY_BANDS - 2, last.0, last.1)),
            head,
            HISTORY_BANDS,
        );
        assert_eq!(bridged.len(), 3);
        assert_eq!(bridged[1], (HISTORY_BANDS - 1, last));
        assert_eq!(bridged[2], (HISTORY_BANDS - 1, head));
    }

    #[test]
    fn fifteen_pedal_segments_preserve_meter_height() {
        let total_height = 180.0;
        let (gap, segment_height) = pedal_segment_geometry(total_height);
        let reconstructed =
            segment_height * PEDAL_SEGMENTS as f32 + gap * (PEDAL_SEGMENTS - 1) as f32;

        assert_eq!(PEDAL_SEGMENTS, 15);
        assert!((reconstructed - total_height).abs() < 0.001);
        assert!((2.0..=4.0).contains(&gap));
    }

    fn point(x: f32, y: f32) -> Pos2 {
        Pos2::new(x, y)
    }

    #[test]
    fn lock_runs_split_lock_grip_relock() {
        let points = [
            point(0.0, 5.0),
            point(1.0, 5.0),
            point(2.0, 5.0),
            point(3.0, 5.0),
            point(4.0, 5.0),
            point(5.0, 5.0),
            point(6.0, 5.0),
        ];
        let locks = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0];
        let runs = collect_runs(&points, &locks, LOCK_HISTORY_THRESHOLD, true);
        assert_eq!(runs.len(), 2, "lock -> grip -> relock must be two runs");
        assert_eq!(runs[0].len(), 1);
        assert_eq!(runs[1].len(), 2);
    }

    #[test]
    fn extremely_short_lock_keeps_true_width() {
        let points = [point(0.0, 5.0), point(1.0, 5.0), point(2.0, 5.0)];
        let locks = [0.0, 1.0, 0.0];
        let runs = collect_runs(&points, &locks, LOCK_HISTORY_THRESHOLD, true);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len(), 1, "single-sample lock must not be widened");
        assert_eq!(runs[0][0].x, 1.0);
    }

    #[test]
    fn approaching_does_not_create_lock_runs() {
        let points = [point(0.0, 5.0), point(1.0, 5.0)];
        let locks = [0.0, 0.0];
        let runs = collect_runs(&points, &locks, LOCK_HISTORY_THRESHOLD, true);
        assert!(runs.is_empty());
    }

    #[test]
    fn projection_mesh_is_clipped_to_chart_bounds() {
        let rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(10.0, 20.0));
        // Includes an out-of-bounds point to exercise clamping.
        let run = [point(5.0, 15.0), point(7.0, 10.0), point(-4.0, 40.0)];
        let mesh = lock_projection_mesh(&run, rect, 0.3, 0.4, Color32::WHITE, 1.0)
            .expect("projection mesh");
        assert!(!mesh.vertices.is_empty());
        for vertex in &mesh.vertices {
            assert!(
                vertex.pos.x >= rect.min.x - 1e-4
                    && vertex.pos.x <= rect.max.x + 1e-4
                    && vertex.pos.y >= rect.min.y - 1e-4
                    && vertex.pos.y <= rect.max.y + 1e-4,
                "vertex {:?} escaped chart bounds",
                vertex.pos
            );
        }
        // The top row must sit exactly on the chart top.
        assert!(mesh
            .vertices
            .iter()
            .any(|vertex| (vertex.pos.y - rect.min.y).abs() < 1e-4));
    }

    #[test]
    fn projection_mesh_supports_single_sample_lock() {
        let rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(10.0, 20.0));
        let mesh = lock_projection_mesh(&[point(4.0, 12.0)], rect, 0.30, 0.35, LOCK_CYAN, 1.0);
        assert!(mesh.is_some());
    }

    #[test]
    fn non_lock_runs_are_drawn_red_lock_runs_are_not() {
        let points = [
            point(0.0, 5.0),
            point(1.0, 5.0),
            point(2.0, 5.0),
            point(3.0, 5.0),
        ];
        // Indices 1..2 are locked.
        let locks = [0.0, 1.0, 1.0, 0.0];
        let red = collect_runs(&points, &locks, LOCK_HISTORY_THRESHOLD, false);
        let cyan = collect_runs(&points, &locks, LOCK_HISTORY_THRESHOLD, true);
        assert_eq!(red.len(), 2);
        assert_eq!(cyan.len(), 1);
        // The red runs must not contain any locked index.
        assert!(red
            .iter()
            .flatten()
            .all(|point| point.x == 0.0 || point.x == 3.0));
        assert!(cyan[0].iter().all(|point| point.x == 1.0 || point.x == 2.0));
    }

    #[test]
    fn lock_grip_relock_switches_red_and_cyan_runs() {
        let points: Vec<Pos2> = (0..10).map(|i| point(i as f32, 5.0)).collect();
        let locks = [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0];
        let red = collect_runs(&points, &locks, LOCK_HISTORY_THRESHOLD, false);
        let cyan = collect_runs(&points, &locks, LOCK_HISTORY_THRESHOLD, true);
        assert_eq!(cyan.len(), 2, "two separate lock events");
        assert_eq!(red.len(), 3, "red before, between and after the locks");
        assert!(red
            .iter()
            .flatten()
            .all(|p| p.x != 2.0 && p.x != 3.0 && p.x != 6.0 && p.x != 7.0));
        assert!(cyan
            .iter()
            .flatten()
            .all(|p| p.x == 2.0 || p.x == 3.0 || p.x == 6.0 || p.x == 7.0));
    }

    #[test]
    fn history_energy_is_continuous_across_band_boundaries() {
        let opacity = 1.0;
        let band_boundary = 4.0 / 8.0;
        let before = history_energy(band_boundary - 1e-3, opacity);
        let after = history_energy(band_boundary + 1e-3, opacity);
        assert!(
            (after - before).abs() < 0.01,
            "energy must not step at band boundaries: {before} -> {after}"
        );
        assert!(history_energy(0.9, opacity) > history_energy(0.2, opacity));
    }

    #[test]
    fn lock_color_switch_matches_lock_run_start() {
        let points: Vec<Pos2> = (0..10).map(|i| point(i as f32, 5.0)).collect();
        let locks = [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0];
        let runs = collect_runs(&points, &locks, LOCK_HISTORY_THRESHOLD, true);
        let starts: Vec<f32> = runs
            .iter()
            .filter_map(|run| run.first().map(|p| p.x))
            .collect();
        // The continuous polyline switches colour exactly at these samples, so
        // the red->cyan (and cyan->red) boundaries share a vertex with no gap.
        assert_eq!(starts, vec![2.0, 6.0]);
    }
}
