//! GPU 终端渲染器：WGPU 绘制单元背景/装饰，glyphon 负责字形排版与缓存。

mod grid;
mod resources;
mod stats;
mod text;

use self::resources::{
    CellInstance, TEXTURE_FORMAT, clear_color, create_cell_pipeline, create_instance_buffer,
    create_texture,
};
use self::stats::PerfStats;
use crate::terminal::FramePatch;
use glyphon::{
    Cache, Color as GlyphColor, ColorMode, FontSystem, Resolution, SwashCache, TextArea, TextAtlas,
    TextBounds, TextRenderer, Viewport,
};
use slint::wgpu_29::wgpu;
use std::time::Instant;

/// 渲染器内部保存的光标快照。
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct CursorState {
    column: usize,
    row: usize,
    visible: bool,
    shape: i32,
    blinking: bool,
}

/// 将终端帧补丁合成为一张可直接交给 Slint 显示的共享纹理。
pub(crate) struct GpuTerminalRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    texture: wgpu::Texture,
    texture_view: wgpu::TextureView,
    width: u32,
    height: u32,
    columns: usize,
    rows: usize,
    /// 当前网格世代；用于拒绝没有完整基准的跨尺寸增量帧。
    generation: Option<u64>,
    cell_width: f32,
    cell_height: f32,
    font_size: f32,
    font_family: String,
    /// 每个网格单元一个实例，主要承载背景、下划线和删除线。
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
    /// 每个非空终端单元一个独立 Buffer，消除连续排版与网格坐标的累计误差。
    text_cells: Vec<Option<text::CellTextBuffer>>,
    cursor: CursorState,
    cursor_phase: bool,
    /// 下次渲染需要覆盖的行号，提交前会排序和去重。
    dirty_rows: Vec<usize>,
    clear_pending: bool,
    render_pending: bool,
    stats: PerfStats,
}

impl GpuTerminalRenderer {
    /// 使用 Slint 提供的 WGPU 设备创建共享纹理、管线、字形图集和网格缓冲区。
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
        font_family: &str,
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
            font_family: font_family.to_owned(),
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
            text_cells: Vec::new(),
            cursor: CursorState::default(),
            cursor_phase: true,
            dirty_rows: (0..rows).collect(),
            clear_pending: true,
            render_pending: true,
            stats: PerfStats::default(),
        };
        renderer.rebuild_text_cells();
        renderer.update_viewport();
        renderer
    }

    /// 将 WGPU 纹理导入为 Slint Image；两者继续共享同一底层资源。
    pub(crate) fn image(&self) -> Result<slint::Image, slint::wgpu_29::TextureImportError> {
        slint::Image::try_from(self.texture.clone())
    }

    /// 同步物理表面和字体度量；返回值表示纹理已重建，调用方需重新导入 Image。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sync_surface(
        &mut self,
        width: u32,
        height: u32,
        cell_width: f32,
        cell_height: f32,
        font_size: f32,
        font_family: &str,
    ) -> bool {
        let width = width.max(1);
        let height = height.max(1);
        let texture_changed = width != self.width || height != self.height;
        let metrics_changed = cell_width != self.cell_width
            || cell_height != self.cell_height
            || font_size != self.font_size;
        let font_changed = self.font_family != font_family;

        let previous_cell_width = self.cell_width;

        self.width = width;
        self.height = height;
        self.cell_width = cell_width;
        self.cell_height = cell_height;
        self.font_size = font_size;
        self.font_family.clear();
        self.font_family.push_str(font_family);

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
        if texture_changed || metrics_changed || font_changed {
            self.dirty_rows = (0..self.rows).collect();
            self.update_viewport();
            self.render_pending = true;
        }
        texture_changed
    }

    /// 将完整帧或增量行应用到 CPU/GPU 缓存；基准不完整时返回 false。
    pub(crate) fn apply_frame(&mut self, frame: &FramePatch) -> bool {
        let generation_changed = self.generation != Some(frame.generation);
        let grid_changed = frame.columns != self.columns || frame.rows != self.rows;
        // 新世代或新尺寸不能建立在旧纹理内容之上，必须先收到完整帧。
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
        let mut text_spans = 0;

        for row in &frame.changed_rows {
            if row.row >= self.rows {
                continue;
            }
            self.update_cell_row(row.row, &row.cells);
            text_spans += self.update_text_row(row.row, &row.cells);
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
            frame.full_redraw_reason,
            dirty_rows,
            changed_cells,
            text_spans,
            uploaded_bytes,
            started.elapsed(),
        );
        true
    }

    /// 仅在有脏行时录制并提交 GPU 命令。
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
        let full_surface = dirty_rows.len() == self.rows;
        self.glyph_viewport.update(
            &self.queue,
            Resolution {
                width: self.width,
                height: self.height,
            },
        );

        // 只提交脏行中的非空单元；每个 TextArea 的 left 来自精确网格列坐标。
        let mut text_areas = Vec::new();
        for &row in &dirty_rows {
            let start = row.saturating_mul(self.columns);
            let end = start
                .saturating_add(self.columns)
                .min(self.text_cells.len());
            for (column, cell) in self.text_cells[start..end].iter().enumerate() {
                if let Some(cell) = cell {
                    text_areas.push(TextArea {
                        buffer: &cell.buffer,
                        left: column as f32 * self.cell_width,
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
                    });
                }
            }
        }
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
        self.stats
            .record_render(full_surface, dirty_rows.len(), started.elapsed());
    }

    /// 推进闪烁相位；返回 true 表示调用方需要请求一次窗口重绘。
    pub(crate) fn tick_cursor(&mut self) -> bool {
        if !self.cursor.visible || !self.cursor.blinking {
            return false;
        }
        self.cursor_phase = !self.cursor_phase;
        self.dirty_rows.push(self.cursor.row);
        self.render_pending = true;
        true
    }
}

/// 判断完整帧是否按顺序覆盖了当前网格的每一行。
fn frame_is_complete(frame: &FramePatch) -> bool {
    frame.changed_rows.len() == frame.rows
        && frame
            .changed_rows
            .iter()
            .enumerate()
            .all(|(row, patch)| patch.row == row)
}

#[cfg(test)]
mod tests {
    use super::{frame_is_complete, resources::rgba};
    use crate::terminal::{CursorPatch, FramePatch, RgbColor, RowPatch};

    fn full_frame(rows: usize) -> FramePatch {
        FramePatch {
            generation: 1,
            columns: 80,
            rows,
            full_redraw: true,
            full_redraw_reason: Some(crate::terminal::FullRedrawReason::RendererRequest),
            changed_rows: (0..rows)
                .map(|row| RowPatch {
                    row,
                    cells: Vec::new(),
                })
                .collect(),
            cursor: CursorPatch::default(),
            scroll_offset: 0,
            scroll_history_lines: 0,
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
