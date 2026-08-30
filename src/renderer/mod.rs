mod stats;

use self::stats::PerfStats;
use crate::terminal::{FramePatch, RgbColor, TerminalCellPatch};
use bytemuck::{Pod, Zeroable};
use glyphon::{
    Attrs, AttrsOwned, Buffer, Cache, Color as GlyphColor, ColorMode, Family, FontSystem, Metrics,
    Resolution, Shaping, Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer,
    Viewport, Weight,
};
use slint::wgpu_29::wgpu;
use std::time::Instant;

const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const FONT_FAMILY: &str = "Cascadia Mono";

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CellInstance {
    rect: [f32; 4],
    background: [f32; 4],
    foreground: [f32; 4],
    flags: [u32; 4],
}

impl Default for CellInstance {
    fn default() -> Self {
        Self::zeroed()
    }
}

struct OwnedSpan {
    text: String,
    attrs: AttrsOwned,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct CursorState {
    column: usize,
    row: usize,
    visible: bool,
    shape: i32,
    blinking: bool,
}

pub(crate) struct GpuTerminalRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    texture: wgpu::Texture,
    texture_view: wgpu::TextureView,
    width: u32,
    height: u32,
    columns: usize,
    rows: usize,
    generation: Option<u64>,
    cell_width: f32,
    cell_height: f32,
    font_size: f32,
    cell_instances: Vec<CellInstance>,
    instance_buffer: wgpu::Buffer,
    cursor_buffer: wgpu::Buffer,
    viewport_buffer: wgpu::Buffer,
    viewport_bind_group: wgpu::BindGroup,
    cell_pipeline: wgpu::RenderPipeline,
    font_system: FontSystem,
    swash_cache: SwashCache,
    glyph_viewport: Viewport,
    glyph_atlas: TextAtlas,
    glyph_renderer: TextRenderer,
    row_buffers: Vec<Buffer>,
    cursor: CursorState,
    cursor_phase: bool,
    dirty_rows: Vec<usize>,
    clear_pending: bool,
    render_pending: bool,
    stats: PerfStats,
}

impl GpuTerminalRenderer {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        columns: usize,
        rows: usize,
        cell_width: f32,
        cell_height: f32,
        font_size: f32,
    ) -> Self {
        let device = device.clone();
        let queue = queue.clone();
        let (texture, texture_view) = create_texture(&device, width, height);

        let viewport_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terminal viewport"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let viewport_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terminal viewport layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let viewport_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terminal viewport bind group"),
            layout: &viewport_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buffer.as_entire_binding(),
            }],
        });
        let cell_pipeline = create_cell_pipeline(&device, &viewport_layout);
        let instance_buffer = create_instance_buffer(&device, columns.saturating_mul(rows));
        let cursor_buffer = create_instance_buffer(&device, 1);

        let cache = Cache::new(&device);
        let glyph_viewport = Viewport::new(&device, &cache);
        let mut glyph_atlas =
            TextAtlas::with_color_mode(&device, &queue, &cache, TEXTURE_FORMAT, ColorMode::Web);
        let glyph_renderer = TextRenderer::new(
            &mut glyph_atlas,
            &device,
            wgpu::MultisampleState::default(),
            None,
        );

        let mut renderer = Self {
            device,
            queue,
            texture,
            texture_view,
            width,
            height,
            columns,
            rows,
            generation: None,
            cell_width,
            cell_height,
            font_size,
            cell_instances: vec![CellInstance::default(); columns.saturating_mul(rows)],
            instance_buffer,
            cursor_buffer,
            viewport_buffer,
            viewport_bind_group,
            cell_pipeline,
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            glyph_viewport,
            glyph_atlas,
            glyph_renderer,
            row_buffers: Vec::new(),
            cursor: CursorState::default(),
            cursor_phase: true,
            dirty_rows: (0..rows).collect(),
            clear_pending: true,
            render_pending: true,
            stats: PerfStats::default(),
        };
        renderer.rebuild_rows();
        renderer.update_viewport();
        renderer
    }

    pub(crate) fn image(&self) -> Result<slint::Image, slint::wgpu_29::TextureImportError> {
        slint::Image::try_from(self.texture.clone())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sync_surface(
        &mut self,
        width: u32,
        height: u32,
        cell_width: f32,
        cell_height: f32,
        font_size: f32,
    ) -> bool {
        let width = width.max(1);
        let height = height.max(1);
        let texture_changed = width != self.width || height != self.height;
        let metrics_changed = cell_width != self.cell_width
            || cell_height != self.cell_height
            || font_size != self.font_size;

        let previous_cell_width = self.cell_width;

        self.width = width;
        self.height = height;
        self.cell_width = cell_width;
        self.cell_height = cell_height;
        self.font_size = font_size;

        if texture_changed {
            (self.texture, self.texture_view) = create_texture(&self.device, width, height);
            self.clear_pending = true;
        }
        if metrics_changed {
            self.update_instance_metrics(previous_cell_width);
            self.upload_all_instances();
            self.clear_pending = true;
        }
        if texture_changed || metrics_changed {
            self.update_row_metrics();
        }
        if texture_changed || metrics_changed {
            self.dirty_rows = (0..self.rows).collect();
            self.update_viewport();
            self.render_pending = true;
        }
        texture_changed
    }

    pub(crate) fn apply_frame(&mut self, frame: &FramePatch) -> bool {
        let generation_changed = self.generation != Some(frame.generation);
        let grid_changed = frame.columns != self.columns || frame.rows != self.rows;
        if (generation_changed || grid_changed) && !frame.full_redraw {
            return false;
        }
        if frame.full_redraw && !frame_is_complete(frame) {
            return false;
        }
        if grid_changed {
            self.resize_grid(frame.columns, frame.rows);
        }
        self.generation = Some(frame.generation);

        let started = Instant::now();
        let dirty_rows = frame.changed_rows.len();
        let changed_cells = frame
            .changed_rows
            .iter()
            .map(|row| row.cells.len())
            .sum::<usize>();
        let mut uploaded_bytes = 0;

        for row in &frame.changed_rows {
            if row.row >= self.rows {
                continue;
            }
            self.update_cell_row(row.row, &row.cells);
            self.update_text_row(row.row, &row.cells);
            let start = row.row * self.columns;
            let end = start + self.columns;
            let bytes = bytemuck::cast_slice(&self.cell_instances[start..end]);
            self.queue.write_buffer(
                &self.instance_buffer,
                (start * std::mem::size_of::<CellInstance>()) as u64,
                bytes,
            );
            uploaded_bytes += bytes.len();
            self.dirty_rows.push(row.row);
        }

        let previous_cursor = self.cursor;
        let next_cursor = CursorState {
            column: frame.cursor.column,
            row: frame.cursor.row,
            visible: frame.cursor.visible,
            shape: frame.cursor.shape,
            blinking: frame.cursor.blinking,
        };
        if next_cursor != self.cursor {
            self.cursor_phase = true;
            if previous_cursor.row < self.rows {
                self.dirty_rows.push(previous_cursor.row);
            }
            if next_cursor.row < self.rows {
                self.dirty_rows.push(next_cursor.row);
            }
        }
        self.cursor = next_cursor;
        self.update_cursor_buffer();
        self.render_pending = true;
        self.stats.record_apply(
            frame.full_redraw,
            dirty_rows,
            changed_cells,
            uploaded_bytes,
            started.elapsed(),
        );
        true
    }

    pub(crate) fn render_if_needed(&mut self) {
        if !self.render_pending {
            return;
        }
        let started = Instant::now();
        self.dirty_rows.sort_unstable();
        self.dirty_rows.dedup();
        if self.clear_pending {
            self.dirty_rows.clear();
            self.dirty_rows.extend(0..self.rows);
        }
        let dirty_rows = std::mem::take(&mut self.dirty_rows);
        if dirty_rows.is_empty() {
            self.render_pending = false;
            return;
        }
        self.glyph_viewport.update(
            &self.queue,
            Resolution {
                width: self.width,
                height: self.height,
            },
        );

        let text_areas = dirty_rows.iter().filter_map(|&row| {
            self.row_buffers.get(row).map(|buffer| TextArea {
                buffer,
                left: 0.0,
                top: row as f32 * self.cell_height,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: self.width as i32,
                    bottom: self.height as i32,
                },
                default_color: GlyphColor::rgb(230, 237, 243),
                custom_glyphs: &[],
            })
        });
        if let Err(error) = self.glyph_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.glyph_atlas,
            &self.glyph_viewport,
            text_areas,
            &mut self.swash_cache,
        ) {
            eprintln!("terminal glyph preparation failed: {error}");
            self.dirty_rows = dirty_rows;
            return;
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("terminal frame encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminal frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.texture_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if self.clear_pending {
                            wgpu::LoadOp::Clear(clear_color())
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.cell_pipeline);
            pass.set_bind_group(0, &self.viewport_bind_group, &[]);
            pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
            for &row in &dirty_rows {
                let first = row.saturating_mul(self.columns) as u32;
                let end = first.saturating_add(self.columns as u32);
                pass.draw(0..6, first..end);
            }
            if let Err(error) =
                self.glyph_renderer
                    .render(&self.glyph_atlas, &self.glyph_viewport, &mut pass)
            {
                eprintln!("terminal glyph rendering failed: {error}");
            }
            if self.cursor.visible && (!self.cursor.blinking || self.cursor_phase) {
                pass.set_pipeline(&self.cell_pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.cursor_buffer.slice(..));
                pass.draw(0..6, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        self.glyph_atlas.trim();
        self.clear_pending = false;
        self.render_pending = false;
        self.stats.record_render(started.elapsed());
    }

    pub(crate) fn tick_cursor(&mut self) -> bool {
        if !self.cursor.visible || !self.cursor.blinking {
            return false;
        }
        self.cursor_phase = !self.cursor_phase;
        self.dirty_rows.push(self.cursor.row);
        self.render_pending = true;
        true
    }

    fn rebuild_rows(&mut self) {
        let metrics = Metrics::new(self.font_size, self.cell_height);
        self.row_buffers = (0..self.rows)
            .map(|_| {
                let mut buffer = Buffer::new(&mut self.font_system, metrics);
                buffer.set_size(
                    &mut self.font_system,
                    Some(self.width as f32),
                    Some(self.cell_height),
                );
                buffer.set_monospace_width(&mut self.font_system, Some(self.cell_width));
                buffer
            })
            .collect();
    }

    fn resize_grid(&mut self, columns: usize, rows: usize) {
        self.columns = columns;
        self.rows = rows;
        self.cell_instances = vec![CellInstance::default(); columns.saturating_mul(rows)];
        self.instance_buffer = create_instance_buffer(&self.device, columns.saturating_mul(rows));
        self.rebuild_rows();
        self.dirty_rows = (0..rows).collect();
        self.clear_pending = true;
        self.render_pending = true;
    }

    fn update_row_metrics(&mut self) {
        let metrics = Metrics::new(self.font_size, self.cell_height);
        for buffer in &mut self.row_buffers {
            buffer.set_metrics_and_size(
                &mut self.font_system,
                metrics,
                Some(self.width as f32),
                Some(self.cell_height),
            );
            buffer.set_monospace_width(&mut self.font_system, Some(self.cell_width));
        }
    }

    fn update_instance_metrics(&mut self, previous_cell_width: f32) {
        if previous_cell_width <= 0.0 {
            return;
        }
        for (index, instance) in self.cell_instances.iter_mut().enumerate() {
            if instance.rect[2] <= 0.0 {
                continue;
            }
            let columns = instance.rect[2] / previous_cell_width;
            instance.rect = [
                (index % self.columns) as f32 * self.cell_width,
                (index / self.columns) as f32 * self.cell_height,
                columns * self.cell_width,
                self.cell_height,
            ];
        }
    }

    fn upload_all_instances(&self) {
        self.queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&self.cell_instances),
        );
        self.update_cursor_buffer();
    }

    fn update_viewport(&self) {
        self.queue.write_buffer(
            &self.viewport_buffer,
            0,
            bytemuck::cast_slice(&[self.width as f32, self.height as f32, 0.0, 0.0]),
        );
    }

    fn update_cell_row(&mut self, row: usize, cells: &[TerminalCellPatch]) {
        let start = row * self.columns;
        self.cell_instances[start..start + self.columns].fill(CellInstance::default());
        for cell in cells {
            if cell.column >= self.columns {
                continue;
            }
            self.cell_instances[start + cell.column] = CellInstance {
                rect: [
                    cell.column as f32 * self.cell_width,
                    row as f32 * self.cell_height,
                    cell.width_in_columns as f32 * self.cell_width,
                    self.cell_height,
                ],
                background: rgba(cell.background, 1.0),
                foreground: rgba(cell.foreground, 1.0),
                flags: [
                    cell.underline_style.max(0) as u32,
                    u32::from(cell.strikeout),
                    0,
                    0,
                ],
            };
        }
    }

    fn update_text_row(&mut self, row: usize, cells: &[TerminalCellPatch]) {
        let Some(buffer) = self.row_buffers.get_mut(row) else {
            return;
        };
        let mut spans = Vec::<OwnedSpan>::with_capacity(cells.len() + 2);
        let mut next_column = 0;
        for cell in cells {
            if cell.column > next_column {
                push_span(
                    &mut spans,
                    " ".repeat(cell.column - next_column),
                    default_attrs(RgbColor {
                        red: 0xe6,
                        green: 0xed,
                        blue: 0xf3,
                    }),
                );
            }
            let text = if cell.hidden || cell.text.is_empty() {
                " ".repeat(cell.width_in_columns)
            } else {
                cell.text.clone()
            };
            let mut attrs = Attrs::new()
                .family(Family::Name(FONT_FAMILY))
                .color(glyph_color(cell.foreground))
                .weight(if cell.bold {
                    Weight::BOLD
                } else {
                    Weight::NORMAL
                })
                .style(if cell.italic {
                    Style::Italic
                } else {
                    Style::Normal
                });
            if cell.hidden {
                attrs = attrs.color(GlyphColor::rgba(0, 0, 0, 0));
            }
            push_span(&mut spans, text, AttrsOwned::new(&attrs));
            next_column = cell.column + cell.width_in_columns;
        }
        if next_column < self.columns {
            push_span(
                &mut spans,
                " ".repeat(self.columns - next_column),
                default_attrs(RgbColor {
                    red: 0xe6,
                    green: 0xed,
                    blue: 0xf3,
                }),
            );
        }

        let default = Attrs::new().family(Family::Name(FONT_FAMILY));
        buffer.set_rich_text(
            &mut self.font_system,
            spans
                .iter()
                .map(|span| (span.text.as_str(), span.attrs.as_attrs())),
            &default,
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);
    }

    fn update_cursor_buffer(&self) {
        let mut instance = CellInstance::default();
        if self.cursor.visible && self.cursor.column < self.columns && self.cursor.row < self.rows {
            let cursor_color = RgbColor {
                red: 0xe6,
                green: 0xed,
                blue: 0xf3,
            };
            instance = CellInstance {
                rect: [
                    self.cursor.column as f32 * self.cell_width,
                    self.cursor.row as f32 * self.cell_height,
                    self.cell_width,
                    self.cell_height,
                ],
                background: rgba(cursor_color, 0.60),
                foreground: rgba(cursor_color, 1.0),
                flags: [0, 0, (self.cursor.shape.max(0) + 1) as u32, 0],
            };
        }
        self.queue
            .write_buffer(&self.cursor_buffer, 0, bytemuck::bytes_of(&instance));
    }
}

fn push_span(spans: &mut Vec<OwnedSpan>, text: String, attrs: AttrsOwned) {
    if !text.is_empty() {
        spans.push(OwnedSpan { text, attrs });
    }
}

fn default_attrs(color: RgbColor) -> AttrsOwned {
    AttrsOwned::new(
        &Attrs::new()
            .family(Family::Name(FONT_FAMILY))
            .color(glyph_color(color)),
    )
}

fn glyph_color(color: RgbColor) -> GlyphColor {
    GlyphColor::rgb(color.red, color.green, color.blue)
}

fn rgba(color: RgbColor, alpha: f32) -> [f32; 4] {
    [
        f32::from(color.red) / 255.0,
        f32::from(color.green) / 255.0,
        f32::from(color.blue) / 255.0,
        alpha,
    ]
}

fn clear_color() -> wgpu::Color {
    let color = RgbColor {
        red: 0x0d,
        green: 0x11,
        blue: 0x17,
    };
    wgpu::Color {
        r: f64::from(color.red) / 255.0,
        g: f64::from(color.green) / 255.0,
        b: f64::from(color.blue) / 255.0,
        a: 1.0,
    }
}

fn frame_is_complete(frame: &FramePatch) -> bool {
    frame.changed_rows.len() == frame.rows
        && frame
            .changed_rows
            .iter()
            .enumerate()
            .all(|(row, patch)| patch.row == row)
}

fn create_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terminal surface"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TEXTURE_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn create_instance_buffer(device: &wgpu::Device, count: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("terminal cell instances"),
        size: (count.max(1) * std::mem::size_of::<CellInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_cell_pipeline(
    device: &wgpu::Device,
    viewport_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("terminal cell shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("cell.wgsl").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("terminal cell pipeline layout"),
        bind_group_layouts: &[Some(viewport_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("terminal cell pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<CellInstance>() as u64,
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
                        format: wgpu::VertexFormat::Uint32x4,
                        offset: 48,
                        shader_location: 3,
                    },
                ],
            }],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: TEXTURE_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{frame_is_complete, rgba};
    use crate::terminal::{CursorPatch, FramePatch, RgbColor, RowPatch};

    fn full_frame(rows: usize) -> FramePatch {
        FramePatch {
            generation: 1,
            columns: 80,
            rows,
            full_redraw: true,
            changed_rows: (0..rows)
                .map(|row| RowPatch {
                    row,
                    cells: Vec::new(),
                })
                .collect(),
            cursor: CursorPatch::default(),
            title: None,
            exit_message: None,
        }
    }

    #[test]
    fn preserves_srgb_values_for_slint_texture_composition() {
        let color = rgba(
            RgbColor {
                red: 0x0d,
                green: 0x11,
                blue: 0x17,
            },
            0.5,
        );
        assert_eq!(color[3], 0.5);
        assert!((color[0] - 13.0 / 255.0).abs() < f32::EPSILON);
        assert!((color[1] - 17.0 / 255.0).abs() < f32::EPSILON);
        assert!((color[2] - 23.0 / 255.0).abs() < f32::EPSILON);
    }

    #[test]
    fn accepts_only_complete_ordered_full_frames_for_reconstruction() {
        let mut frame = full_frame(3);
        assert!(frame_is_complete(&frame));

        frame.changed_rows.pop();
        assert!(!frame_is_complete(&frame));

        let mut frame = full_frame(3);
        frame.changed_rows.swap(1, 2);
        assert!(!frame_is_complete(&frame));
    }
}
