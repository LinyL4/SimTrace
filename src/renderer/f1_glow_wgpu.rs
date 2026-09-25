//! Beta-only additive emission renderer for the F1 Open HUD.
//!
//! Two additive paths share one callback:
//! - the legacy **instance** path (per-segment ribbons, radial blobs, rounded
//!   rects) used by lock-up LEDs and emitters;
//! - a **continuous mesh** path used by the history halo, which tessellates a
//!   whole polyline once with shared cross-section vertices (miter/bevel joins)
//!   so there are no per-segment caps, inner double-energy or outer gaps.

use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use std::sync::Arc;

const INSTANCE_FLOATS: usize = 16;
const INSTANCE_SIZE: wgpu::BufferAddress =
    (INSTANCE_FLOATS * std::mem::size_of::<f32>()) as wgpu::BufferAddress;

/// Mesh vertex: position.xy, (lateral, intensity), color.rgb, viewport.xy.
const MESH_FLOATS: usize = 10;
const MESH_STRIDE: wgpu::BufferAddress =
    (MESH_FLOATS * std::mem::size_of::<f32>()) as wgpu::BufferAddress;
const MESH_VIEWPORT_OFFSET: wgpu::BufferAddress =
    (8 * std::mem::size_of::<f32>()) as wgpu::BufferAddress;

/// Lateral cross-section rows, normalised to the local half-width. Seven rows
/// reproduce the `exp(-3.5 d^2)` falloff of the legacy shader.
const MESH_LATERAL: [f32; 7] = [-1.0, -0.7, -0.35, 0.0, 0.35, 0.7, 1.0];

#[derive(Clone, Default)]
pub struct GlowBatch {
    generation: u64,
    instances: Arc<[[f32; INSTANCE_FLOATS]]>,
    mesh_vertices: Arc<[[f32; MESH_FLOATS]]>,
    mesh_indices: Arc<[u32]>,
}

impl GlowBatch {
    pub fn is_empty(&self) -> bool {
        !self.draw_plan().has_draws()
    }

    fn needs_upload(&self, uploaded_generation: Option<u64>) -> bool {
        uploaded_generation != Some(self.generation)
    }

    fn draw_plan(&self) -> GlowDrawPlan {
        GlowDrawPlan {
            instances: !self.instances.is_empty(),
            mesh: !self.mesh_vertices.is_empty() && !self.mesh_indices.is_empty(),
        }
    }

    #[cfg(test)]
    pub(crate) fn has_mesh(&self) -> bool {
        self.draw_plan().mesh
    }

    #[cfg(test)]
    pub(crate) fn has_instances(&self) -> bool {
        self.draw_plan().instances
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GlowDrawPlan {
    instances: bool,
    mesh: bool,
}

impl GlowDrawPlan {
    fn has_draws(self) -> bool {
        self.instances || self.mesh
    }
}

pub struct GlowBatchBuilder {
    origin: egui::Pos2,
    viewport_size: egui::Vec2,
    instances: Vec<[f32; INSTANCE_FLOATS]>,
    mesh_vertices: Vec<[f32; MESH_FLOATS]>,
    mesh_indices: Vec<u32>,
}

impl GlowBatchBuilder {
    pub fn new(viewport: egui::Rect) -> Self {
        Self {
            origin: viewport.min,
            viewport_size: viewport.size(),
            instances: Vec::new(),
            mesh_vertices: Vec::new(),
            mesh_indices: Vec::new(),
        }
    }

    pub fn ribbon_segment(
        &mut self,
        start: egui::Pos2,
        end: egui::Pos2,
        radius: f32,
        color: egui::Color32,
        intensity: f32,
    ) {
        self.push(start, end, radius, color, intensity, 0.0);
    }

    pub fn radial(
        &mut self,
        center: egui::Pos2,
        radius: f32,
        color: egui::Color32,
        intensity: f32,
    ) {
        self.push(center, center, radius, color, intensity, 2.0);
    }

    pub fn rounded_rect(
        &mut self,
        rect: egui::Rect,
        spread: f32,
        color: egui::Color32,
        intensity: f32,
    ) {
        self.push(rect.min, rect.max, spread, color, intensity, 1.0);
    }

    /// Tessellate one continuous additive halo along `points_global`.
    ///
    /// `intensities` and `colors` are per-point (already including the recency
    /// energy). Adjacent segments share cross-section vertices so there is no
    /// per-segment cap, no inner double-energy and no outer wedge gap. Every
    /// cross-section is offset by exactly `radius`, so a join never widens the
    /// nominal glow radius.
    pub fn push_polyline(
        &mut self,
        points_global: &[egui::Pos2],
        intensities: &[f32],
        colors: &[egui::Color32],
        radius: f32,
    ) {
        let count = points_global
            .len()
            .min(intensities.len())
            .min(colors.len());
        if count < 2 {
            return;
        }
        let dir = |a: egui::Pos2, b: egui::Pos2| -> egui::Vec2 {
            let delta = b - a;
            let length = delta.length();
            if length > 1e-6 {
                delta / length
            } else {
                egui::Vec2::ZERO
            }
        };
        let perp = |d: egui::Vec2| egui::vec2(-d.y, d.x);

        let start_dir = dir(points_global[0], points_global[1]);
        let mut previous = self.push_cross_section(
            points_global[0],
            perp(start_dir),
            radius,
            intensities[0],
            colors[0],
        );
        let mut previous_color = colors[0];

        for index in 1..count - 1 {
            let n0 = perp(dir(points_global[index - 1], points_global[index]));
            let n1 = perp(dir(points_global[index], points_global[index + 1]));
            let bisector = n0 + n1;
            let bisector_length = bisector.length();
            let normal = if bisector_length > 1e-4 {
                bisector / bisector_length
            } else {
                n0
            };
            if colors[index] != previous_color {
                // Hard style switch: a duplicate cross-section at the exact same
                // position lets red -> cyan switch without interpolating through
                // a pale mix, while the geometry stays connected.
                let old = self.push_cross_section(
                    points_global[index],
                    normal,
                    radius,
                    intensities[index - 1],
                    previous_color,
                );
                self.connect(previous, old);
                let new = self.push_cross_section(
                    points_global[index],
                    normal,
                    radius,
                    intensities[index],
                    colors[index],
                );
                self.connect(old, new);
                previous = new;
            } else {
                let base = self.push_cross_section(
                    points_global[index],
                    normal,
                    radius,
                    intensities[index],
                    colors[index],
                );
                self.connect(previous, base);
                previous = base;
            }
            previous_color = colors[index];
        }

        let end_dir = dir(points_global[count - 2], points_global[count - 1]);
        let last = self.push_cross_section(
            points_global[count - 1],
            perp(end_dir),
            radius,
            intensities[count - 1],
            colors[count - 1],
        );
        self.connect(previous, last);
    }

    fn push_cross_section(
        &mut self,
        center_global: egui::Pos2,
        normal: egui::Vec2,
        radius: f32,
        intensity: f32,
        color: egui::Color32,
    ) -> u32 {
        let center = center_global - self.origin.to_vec2();
        let [r, g, b, _] = color.to_array();
        let base = self.mesh_vertices.len() as u32;
        for lateral in MESH_LATERAL {
            let point = center + normal * (lateral * radius);
            self.mesh_vertices.push([
                point.x,
                point.y,
                lateral,
                intensity.clamp(0.0, 1.0),
                r as f32 / 255.0,
                g as f32 / 255.0,
                b as f32 / 255.0,
                0.0,
                self.viewport_size.x.max(1.0),
                self.viewport_size.y.max(1.0),
            ]);
        }
        base
    }

    fn connect(&mut self, a: u32, b: u32) {
        let rows = MESH_LATERAL.len() as u32;
        for row in 0..rows - 1 {
            let a0 = a + row;
            let a1 = a + row + 1;
            let b0 = b + row;
            let b1 = b + row + 1;
            self.mesh_indices
                .extend_from_slice(&[a0, b0, b1, a0, b1, a1]);
        }
    }

    fn push(
        &mut self,
        a: egui::Pos2,
        b: egui::Pos2,
        radius: f32,
        color: egui::Color32,
        intensity: f32,
        kind: f32,
    ) {
        let a = a - self.origin.to_vec2();
        let b = b - self.origin.to_vec2();
        let [r, g, b_color, _] = color.to_array();
        self.instances.push([
            a.x,
            a.y,
            b.x,
            b.y,
            radius.max(0.5),
            intensity.clamp(0.0, 1.0),
            kind,
            0.0,
            r as f32 / 255.0,
            g as f32 / 255.0,
            b_color as f32 / 255.0,
            0.0,
            self.viewport_size.x.max(1.0),
            self.viewport_size.y.max(1.0),
            0.0,
            0.0,
        ]);
    }

    pub fn finish(self, generation: u64) -> GlowBatch {
        GlowBatch {
            generation,
            instances: self.instances.into(),
            mesh_vertices: self.mesh_vertices.into(),
            mesh_indices: self.mesh_indices.into(),
        }
    }
}

pub fn install(render_state: Option<&egui_wgpu::RenderState>) -> bool {
    let Some(render_state) = render_state else {
        tracing::warn!("F1 additive glow unavailable: no wgpu render state");
        return false;
    };

    let shader = render_state
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("f1_open_hud_glow_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("f1_glow_wgpu.wgsl").into()),
        });
    let pipeline_layout =
        render_state
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("f1_open_hud_glow_pipeline_layout"),
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });
    // Bounded additive (screen) RGB keeps premultiplied RGB <= alpha. Alpha uses
    // coverage union, matching premultiplied composition.
    let blend = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrc,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
    };
    let make_pipeline = |label: &str,
                         vs: &str,
                         fs: &str,
                         buffers: &[wgpu::VertexBufferLayout]| {
        render_state
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    buffers,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: render_state.target_format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
    };
    let instance_pipeline = make_pipeline(
        "f1_open_hud_glow_pipeline",
        "vs_main",
        "fs_main",
        &[wgpu::VertexBufferLayout {
            array_stride: INSTANCE_SIZE,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 16,
                    shader_location: 1,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 32,
                    shader_location: 2,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 48,
                    shader_location: 3,
                },
            ],
        }],
    );
    let mesh_pipeline = make_pipeline(
        "f1_open_hud_glow_mesh_pipeline",
        "vs_mesh",
        "fs_mesh",
        &[wgpu::VertexBufferLayout {
            array_stride: MESH_STRIDE,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 8,
                    shader_location: 1,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 16,
                    shader_location: 2,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: MESH_VIEWPORT_OFFSET,
                    shader_location: 3,
                },
            ],
        }],
    );
    let instance_buffer = render_state.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("f1_open_hud_glow_instances"),
        size: INSTANCE_SIZE,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mesh_vertex_buffer = render_state.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("f1_open_hud_glow_mesh_vertices"),
        size: MESH_STRIDE,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mesh_index_buffer = render_state.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("f1_open_hud_glow_mesh_indices"),
        size: 4,
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    render_state
        .renderer
        .write()
        .callback_resources
        .insert(GlowRenderResources {
            instance_pipeline,
            mesh_pipeline,
            instance_buffer,
            mesh_vertex_buffer,
            mesh_index_buffer,
            instance_capacity: 1,
            mesh_vertex_capacity: 1,
            mesh_index_capacity: 1,
            uploaded_generation: None,
        });
    tracing::info!("F1 Open HUD additive glow renderer installed");
    true
}

pub fn callback(rect: egui::Rect, batch: GlowBatch) -> egui::PaintCallback {
    egui_wgpu::Callback::new_paint_callback(rect, GlowCallback { batch })
}

struct GlowCallback {
    batch: GlowBatch,
}

impl egui_wgpu::CallbackTrait for GlowCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(resources) = resources.get_mut::<GlowRenderResources>() else {
            return Vec::new();
        };
        if !self.batch.needs_upload(resources.uploaded_generation) {
            return Vec::new();
        }
        let draw_plan = self.batch.draw_plan();

        let instance_count = self.batch.instances.len().max(1);
        if instance_count > resources.instance_capacity {
            resources.instance_capacity = instance_count.next_power_of_two();
            resources.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("f1_open_hud_glow_instances"),
                size: INSTANCE_SIZE * resources.instance_capacity as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let vertex_count = self.batch.mesh_vertices.len().max(1);
        if vertex_count > resources.mesh_vertex_capacity {
            resources.mesh_vertex_capacity = vertex_count.next_power_of_two();
            resources.mesh_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("f1_open_hud_glow_mesh_vertices"),
                size: MESH_STRIDE * resources.mesh_vertex_capacity as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let index_count = self.batch.mesh_indices.len().max(1);
        if index_count > resources.mesh_index_capacity {
            resources.mesh_index_capacity = index_count.next_power_of_two();
            resources.mesh_index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("f1_open_hud_glow_mesh_indices"),
                size: 4 * resources.mesh_index_capacity as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        if draw_plan.instances {
            let mut bytes = Vec::with_capacity(self.batch.instances.len() * INSTANCE_SIZE as usize);
            for instance in self.batch.instances.iter() {
                for value in instance {
                    bytes.extend_from_slice(&value.to_ne_bytes());
                }
            }
            queue.write_buffer(&resources.instance_buffer, 0, &bytes);
        }
        if draw_plan.mesh {
            let mut bytes =
                Vec::with_capacity(self.batch.mesh_vertices.len() * MESH_STRIDE as usize);
            for vertex in self.batch.mesh_vertices.iter() {
                for value in vertex {
                    bytes.extend_from_slice(&value.to_ne_bytes());
                }
            }
            queue.write_buffer(&resources.mesh_vertex_buffer, 0, &bytes);
        }
        if draw_plan.mesh {
            let mut bytes = Vec::with_capacity(self.batch.mesh_indices.len() * 4);
            for index in self.batch.mesh_indices.iter() {
                bytes.extend_from_slice(&index.to_ne_bytes());
            }
            queue.write_buffer(&resources.mesh_index_buffer, 0, &bytes);
        }
        resources.uploaded_generation = Some(self.batch.generation);
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let draw_plan = self.batch.draw_plan();
        if !draw_plan.has_draws() {
            return;
        }
        let Some(resources) = resources.get::<GlowRenderResources>() else {
            return;
        };
        if draw_plan.instances {
            render_pass.set_pipeline(&resources.instance_pipeline);
            render_pass.set_vertex_buffer(0, resources.instance_buffer.slice(..));
            render_pass.draw(0..6, 0..self.batch.instances.len() as u32);
        }
        if draw_plan.mesh {
            render_pass.set_pipeline(&resources.mesh_pipeline);
            render_pass.set_vertex_buffer(0, resources.mesh_vertex_buffer.slice(..));
            render_pass.set_index_buffer(
                resources.mesh_index_buffer.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            render_pass.draw_indexed(0..self.batch.mesh_indices.len() as u32, 0, 0..1);
        }
    }
}

struct GlowRenderResources {
    instance_pipeline: wgpu::RenderPipeline,
    mesh_pipeline: wgpu::RenderPipeline,
    instance_buffer: wgpu::Buffer,
    mesh_vertex_buffer: wgpu::Buffer,
    mesh_index_buffer: wgpu::Buffer,
    instance_capacity: usize,
    mesh_vertex_capacity: usize,
    mesh_index_capacity: usize,
    uploaded_generation: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn straight(count: usize) -> Vec<egui::Pos2> {
        (0..count)
            .map(|i| egui::pos2(i as f32 * 4.0, 10.0))
            .collect()
    }

    #[test]
    fn builder_converts_to_viewport_local_coordinates() {
        let viewport = egui::Rect::from_min_size(egui::pos2(50.0, 20.0), egui::vec2(300.0, 100.0));
        let mut builder = GlowBatchBuilder::new(viewport);
        builder.ribbon_segment(
            egui::pos2(55.0, 24.0),
            egui::pos2(65.0, 28.0),
            7.0,
            egui::Color32::RED,
            0.25,
        );
        let batch = builder.finish(9);

        assert_eq!(batch.generation, 9);
        assert_eq!(batch.instances.len(), 1);
        assert_eq!(&batch.instances[0][0..4], &[5.0, 4.0, 15.0, 8.0]);
        assert_eq!(&batch.instances[0][12..14], &[300.0, 100.0]);
    }

    #[test]
    fn straight_polyline_has_no_joint_duplicates() {
        let viewport = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0));
        let points = straight(10);
        let intensities = vec![0.3_f32; points.len()];
        let colors = vec![egui::Color32::RED; points.len()];
        let mut builder = GlowBatchBuilder::new(viewport);
        builder.push_polyline(&points, &intensities, &colors, 7.5);
        let batch = builder.finish(1);

        let rows = MESH_LATERAL.len();
        // One cross-section per point, no duplicated caps.
        assert_eq!(batch.mesh_vertices.len(), points.len() * rows);
        // (sections - 1) * (rows - 1) quads * 6 indices.
        assert_eq!(batch.mesh_indices.len(), (points.len() - 1) * (rows - 1) * 6);
    }

    #[test]
    fn collinear_join_uses_single_miter_cross_section() {
        let viewport = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0));
        let points = straight(5);
        let intensities = vec![0.3_f32; points.len()];
        let colors = vec![egui::Color32::RED; points.len()];
        let mut builder = GlowBatchBuilder::new(viewport);
        builder.push_polyline(&points, &intensities, &colors, 7.5);
        let batch = builder.finish(1);
        assert_eq!(batch.mesh_vertices.len(), points.len() * MESH_LATERAL.len());
    }

    #[test]
    fn sharp_turn_stays_connected_without_radius_growth() {
        let viewport = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0));
        let points = vec![
            egui::pos2(0.0, 10.0),
            egui::pos2(50.0, 10.0),
            egui::pos2(0.0, 10.5),
            egui::pos2(50.0, 11.0),
        ];
        let intensities = vec![0.3_f32; points.len()];
        let colors = vec![egui::Color32::RED; points.len()];
        let mut builder = GlowBatchBuilder::new(viewport);
        builder.push_polyline(&points, &intensities, &colors, 7.5);
        let batch = builder.finish(1);
        // One shared cross-section per sample (constant radius, no miter spike).
        assert_eq!(batch.mesh_vertices.len(), points.len() * MESH_LATERAL.len());
        assert!(batch
            .mesh_indices
            .iter()
            .all(|i| (*i as usize) < batch.mesh_vertices.len()));
        // Nominal radius is never exceeded: the outermost rows sit exactly at
        // +/-radius from the centreline sample.
        for vertex in batch.mesh_vertices.iter() {
            if (vertex[2].abs() - 1.0).abs() < 1e-4 {
                // row at +/-1; distance from its centre is radius by construction.
                assert!(vertex[2].abs() <= 1.0 + 1e-4);
            }
        }
    }

    #[test]
    fn colored_polyline_hard_switches_and_shares_boundary_vertex() {
        let viewport = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0));
        let points = straight(6);
        let intensities = vec![0.3_f32; points.len()];
        let mut colors = vec![egui::Color32::RED; points.len()];
        for color in colors.iter_mut().skip(3) {
            *color = egui::Color32::from_rgb(56, 223, 255);
        }
        let mut builder = GlowBatchBuilder::new(viewport);
        builder.push_polyline(&points, &intensities, &colors, 7.5);
        let batch = builder.finish(1);
        // One extra cross-section at the red->cyan boundary (duplicated position,
        // different colour) so the strip stays connected and the colour hard
        // switches instead of interpolating through a pale mix.
        assert_eq!(batch.mesh_vertices.len(), 7 * MESH_LATERAL.len());
    }

    #[test]
    fn instances_and_mesh_coexist() {
        let viewport = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0));
        let mut builder = GlowBatchBuilder::new(viewport);
        builder.radial(egui::pos2(5.0, 5.0), 15.0, egui::Color32::RED, 0.3);
        builder.push_polyline(
            &straight(4),
            &vec![0.2; 4],
            &vec![egui::Color32::GREEN; 4],
            7.5,
        );
        let batch = builder.finish(2);
        assert_eq!(batch.instances.len(), 1);
        assert!(!batch.mesh_indices.is_empty());
        assert!(!batch.is_empty());
    }

    #[test]
    fn polyline_does_not_add_instances_and_ribbon_does_not_add_mesh() {
        let viewport = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0));
        // Continuous halo path is pure mesh (core stroke stays on the painter).
        let mut mesh_only = GlowBatchBuilder::new(viewport);
        mesh_only.push_polyline(&straight(4), &vec![0.2; 4], &vec![egui::Color32::RED; 4], 7.5);
        let batch = mesh_only.finish(1);
        assert!(batch.instances.is_empty());
        assert!(!batch.mesh_indices.is_empty());

        // Legacy path is pure instances (no mesh), so radial emitters keep working.
        let mut legacy_only = GlowBatchBuilder::new(viewport);
        legacy_only.ribbon_segment(
            egui::pos2(1.0, 1.0),
            egui::pos2(9.0, 2.0),
            7.5,
            egui::Color32::RED,
            0.3,
        );
        let batch = legacy_only.finish(1);
        assert_eq!(batch.instances.len(), 1);
        assert!(batch.mesh_indices.is_empty());
    }

    #[test]
    fn mesh_only_batch_is_non_empty_and_plans_indexed_draw() {
        let viewport =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0));
        let mut builder = GlowBatchBuilder::new(viewport);
        builder.push_polyline(
            &straight(4),
            &vec![0.2; 4],
            &vec![egui::Color32::RED; 4],
            7.5,
        );
        let batch = builder.finish(41);

        assert!(!batch.is_empty());
        assert_eq!(
            batch.draw_plan(),
            GlowDrawPlan {
                instances: false,
                mesh: true,
            }
        );
    }

    #[test]
    fn mesh_only_generation_controls_upload() {
        let viewport =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0));
        let mut builder = GlowBatchBuilder::new(viewport);
        builder.push_polyline(
            &straight(4),
            &vec![0.2; 4],
            &vec![egui::Color32::RED; 4],
            7.5,
        );
        let batch = builder.finish(41);

        assert!(batch.needs_upload(None));
        assert!(batch.needs_upload(Some(40)));
        assert!(!batch.needs_upload(Some(41)));
    }

    #[test]
    fn mesh_viewport_attribute_reads_the_two_viewport_floats() {
        let viewport =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0));
        let mut builder = GlowBatchBuilder::new(viewport);
        builder.push_polyline(
            &straight(2),
            &vec![0.2; 2],
            &vec![egui::Color32::RED; 2],
            7.5,
        );
        let batch = builder.finish(1);

        assert_eq!(MESH_VIEWPORT_OFFSET, 32);
        assert_eq!(&batch.mesh_vertices[0][8..10], &[400.0, 100.0]);
    }

    #[test]
    fn lock_grip_relock_polyline_stays_continuous() {
        let viewport = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0));
        let points = straight(10);
        let intensities = vec![0.3_f32; points.len()];
        // red, red, cyan, cyan, red, red, cyan, cyan, red, red
        let colors: Vec<egui::Color32> = (0..10)
            .map(|i| {
                if (2..4).contains(&i) || (6..8).contains(&i) {
                    egui::Color32::from_rgb(56, 223, 255)
                } else {
                    egui::Color32::RED
                }
            })
            .collect();
        let mut builder = GlowBatchBuilder::new(viewport);
        builder.push_polyline(&points, &intensities, &colors, 7.5);
        let batch = builder.finish(1);
        // 10 samples plus 4 hard-switch duplicates at the red<->cyan boundaries:
        // every sample is still one connected strip with no gap.
        let cross_sections = 10 + 4;
        assert_eq!(batch.mesh_vertices.len(), cross_sections * MESH_LATERAL.len());
        assert_eq!(
            batch.mesh_indices.len(),
            (cross_sections - 1) * (MESH_LATERAL.len() - 1) * 6
        );
    }
}
