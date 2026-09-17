//! Beta-only additive emission renderer for the F1 Open HUD.

use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use std::sync::Arc;

const INSTANCE_FLOATS: usize = 16;
const INSTANCE_SIZE: wgpu::BufferAddress =
    (INSTANCE_FLOATS * std::mem::size_of::<f32>()) as wgpu::BufferAddress;

#[derive(Clone, Default)]
pub struct GlowBatch {
    generation: u64,
    instances: Arc<[[f32; INSTANCE_FLOATS]]>,
}

impl GlowBatch {
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }
}

pub struct GlowBatchBuilder {
    origin: egui::Pos2,
    viewport_size: egui::Vec2,
    instances: Vec<[f32; INSTANCE_FLOATS]>,
}

impl GlowBatchBuilder {
    pub fn new(viewport: egui::Rect) -> Self {
        Self {
            origin: viewport.min,
            viewport_size: viewport.size(),
            instances: Vec::new(),
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
    let pipeline = render_state
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("f1_open_hud_glow_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
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
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: render_state.target_format,
                    // Bounded additive (screen) RGB keeps premultiplied RGB <= alpha.
                    // Alpha uses coverage union, matching premultiplied composition.
                    blend: Some(wgpu::BlendState {
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
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
    let instance_buffer = render_state.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("f1_open_hud_glow_instances"),
        size: INSTANCE_SIZE,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    render_state
        .renderer
        .write()
        .callback_resources
        .insert(GlowRenderResources {
            pipeline,
            instance_buffer,
            capacity: 1,
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
        if resources.uploaded_generation == Some(self.batch.generation) {
            return Vec::new();
        }

        let instance_count = self.batch.instances.len().max(1);
        if instance_count > resources.capacity {
            resources.capacity = instance_count.next_power_of_two();
            resources.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("f1_open_hud_glow_instances"),
                size: INSTANCE_SIZE * resources.capacity as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        if !self.batch.instances.is_empty() {
            let mut bytes = Vec::with_capacity(self.batch.instances.len() * INSTANCE_SIZE as usize);
            for instance in self.batch.instances.iter() {
                for value in instance {
                    bytes.extend_from_slice(&value.to_ne_bytes());
                }
            }
            queue.write_buffer(&resources.instance_buffer, 0, &bytes);
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
        if self.batch.instances.is_empty() {
            return;
        }
        let Some(resources) = resources.get::<GlowRenderResources>() else {
            return;
        };
        render_pass.set_pipeline(&resources.pipeline);
        render_pass.set_vertex_buffer(0, resources.instance_buffer.slice(..));
        render_pass.draw(0..6, 0..self.batch.instances.len() as u32);
    }
}

struct GlowRenderResources {
    pipeline: wgpu::RenderPipeline,
    instance_buffer: wgpu::Buffer,
    capacity: usize,
    uploaded_generation: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
