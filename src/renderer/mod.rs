//! GPU 终端渲染器：WGPU 绘制单元背景/装饰，项目代码负责网格排版和字形图集。

mod decoration;
mod fonts;
mod glyphs;
mod grid;
#[cfg(test)]
mod render_tests;
mod resources;
mod stats;
mod text;

pub(crate) use fonts::monospace_families;
pub(crate) use text::measure_cell;

use self::decoration::DecorationLayer;
use self::resources::{
    CellInstance, clear_color, create_cell_pipeline, create_instance_buffer, create_texture,
};
use self::stats::PerfStats;
use crate::terminal::{FramePatch, TerminalTheme};
use cosmic_text::{FontSystem, SwashCache};
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

/// IME 预编辑只保存文本和终端网格位置；字形仍走与终端正文相同的 shaping/atlas 管线。
struct ImePreedit {
    text: String,
    buffers: Vec<text::ShapedRun>,
    column: usize,
    row: usize,
    columns: usize,
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
    /// 仅由主字体确定的行内基线；fallback 字体和行内容不得修改。
    fixed_baseline: f32,
    /// 每个网格单元一个实例，主要承载背景、下划线和删除线。
    cell_instances: Vec<CellInstance>,
    instance_buffer: wgpu::Buffer,
    /// 选区与搜索高亮；独立于单元格与字形缓存，变化时只重绘相关行。
    decorations: DecorationLayer,
    cursor_buffer: wgpu::Buffer,
    ime_instance_buffer: wgpu::Buffer,
    viewport_buffer: wgpu::Buffer,
    viewport_bind_group: wgpu::BindGroup,
    cell_pipeline: wgpu::RenderPipeline,
    font_system: FontSystem,
    swash_cache: SwashCache,
    glyph_renderer: glyphs::GlyphRenderer,
    /// Shaped glyph clusters carry explicit terminal grid coordinates.
    text_rows: Vec<Vec<text::ShapedRun>>,
    /// 字体或 DPI 改变时可直接重建 shaping，无需等待 PTY 重发完整帧。
    row_cells: Vec<Vec<crate::terminal::TerminalCellPatch>>,
    ime_preedit: Option<ImePreedit>,
    cursor: CursorState,
    cursor_phase: bool,
    /// 下次渲染需要覆盖的行号，提交前会排序和去重。
    dirty_rows: Vec<usize>,
    clear_pending: bool,
    render_pending: bool,
    stats: PerfStats,
    theme: TerminalTheme,
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
        theme: TerminalTheme,
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
        let decorations = DecorationLayer::new(&device);
        let cursor_buffer = create_instance_buffer(&device, 1);
        let ime_instance_buffer = create_instance_buffer(&device, 1);

        let mut font_system = fonts::font_system();
        let fixed_baseline =
            text::measure_fixed_baseline(&mut font_system, font_family, font_size, cell_height);

        let glyph_renderer = glyphs::GlyphRenderer::new(&device);

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
            fixed_baseline,
            cell_instances: vec![CellInstance::default(); columns.saturating_mul(rows)],
            instance_buffer,
            decorations,
            cursor_buffer,
            ime_instance_buffer,
            viewport_buffer,
            viewport_bind_group,
            cell_pipeline,
            font_system,
            swash_cache: SwashCache::new(),
            glyph_renderer,
            text_rows: Vec::new(),
            row_cells: Vec::new(),
            ime_preedit: None,
            cursor: CursorState::default(),
            cursor_phase: true,
            dirty_rows: (0..rows).collect(),
            clear_pending: true,
            render_pending: true,
            stats: PerfStats::from_environment(),
            theme,
        };
        renderer.rebuild_text_rows();
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
        if metrics_changed || font_changed {
            self.fixed_baseline = text::measure_fixed_baseline(
                &mut self.font_system,
                &self.font_family,
                self.font_size,
                self.cell_height,
            );
        }
        if metrics_changed {
            self.update_instance_metrics();
            self.upload_all_instances();
            self.decorations.update_metrics(
                &self.device,
                &self.queue,
                self.cell_width,
                self.cell_height,
            );
            self.clear_pending = true;
        }
        if texture_changed || metrics_changed {
            self.update_row_metrics();
        }
        if font_changed {
            self.rebuild_all_text_rows();
            if let Some(value) = self.ime_preedit.as_ref().map(|ime| ime.text.clone()) {
                self.set_ime_preedit(&value);
            }
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
        let mut reshaped_rows = 0;
        let mut text_dirty_rows = vec![false; self.rows];
        // 任何一行的缓存被判定不可信，本帧都视为失败，由调用方请求完整帧重建全部状态。
        let mut cache_corrupted = false;

        for row in &frame.changed_rows {
            let start_column = row.start_column.min(self.columns);
            let end_column = row.end_column.min(self.columns);
            if row.row >= self.rows || start_column >= end_column {
                continue;
            }
            self.update_cell_range(row.row, start_column, end_column, &row.cells);
            // 只有文字、字体属性或前景色变化才需要重新 shaping；背景/下划线只更新实例。
            match self.merge_row_cells(row.row, start_column, end_column, &row.cells) {
                Ok(true) => text_dirty_rows[row.row] = true,
                Ok(false) => {}
                Err(_) => {
                    eprintln!(
                        "terminal row cache rejected patch for row {} ({}..{}); requesting a full frame",
                        row.row, start_column, end_column
                    );
                    cache_corrupted = true;
                    text_dirty_rows[row.row] = true;
                }
            }
            // 完整帧覆盖全部实例，随后一次性上传，避免逐行提交几十次小写入。
            if !frame.full_redraw {
                let start = row.row * self.columns + start_column;
                let end = row.row * self.columns + end_column;
                let bytes = bytemuck::cast_slice(&self.cell_instances[start..end]);
                self.queue.write_buffer(
                    &self.instance_buffer,
                    (start * std::mem::size_of::<CellInstance>()) as u64,
                    bytes,
                );
                uploaded_bytes += bytes.len();
            }
            self.dirty_rows.push(row.row);
        }
        if frame.full_redraw {
            let bytes = bytemuck::cast_slice(&self.cell_instances);
            self.queue.write_buffer(&self.instance_buffer, 0, bytes);
            uploaded_bytes += bytes.len();
        }
        for (row, dirty) in text_dirty_rows.into_iter().enumerate() {
            if dirty {
                text_spans += self.update_text_row(row);
                reshaped_rows += 1;
            }
        }

        let decoration_rows = self.decorations.replace(
            &self.device,
            &self.queue,
            &frame.decorations,
            self.cell_width,
            self.cell_height,
            self.theme,
        );
        self.dirty_rows.extend(decoration_rows);

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
        self.relocate_ime_to_cursor();
        self.render_pending = true;
        self.stats.record_apply(
            frame.full_redraw_reason,
            dirty_rows,
            changed_cells,
            reshaped_rows,
            text_spans,
            uploaded_bytes,
            started.elapsed(),
        );
        if cache_corrupted {
            // 被清空的行已按空行重新 shaping 并标脏，画面不会残留错误字形；
            // 返回 false 让调用方立刻索取完整帧。
            self.generation = None;
            return false;
        }
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
        self.glyph_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.swash_cache,
            dirty_rows
                .iter()
                .filter(|&&row| row < self.rows)
                .map(|&row| {
                    let runs = self
                        .ime_preedit
                        .as_ref()
                        .filter(|ime| ime.row == row)
                        .map_or(self.text_rows[row].as_slice(), |ime| ime.buffers.as_slice());
                    (row, runs)
                }),
            (self.width, self.height),
            (self.cell_width, self.cell_height),
            self.fixed_baseline,
        );
        // 光标只能随其所在行一起重绘；用 Load 叠加半透明光标会逐帧累积 alpha。
        let cursor_row_dirty = dirty_rows.binary_search(&self.cursor.row).is_ok();

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
                            wgpu::LoadOp::Clear(clear_color(self.theme.background))
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
            // 装饰在单元背景之上、字形之下按 alpha 混合，因此不会改变字形颜色或排版。
            if !self.decorations.is_empty() {
                pass.set_vertex_buffer(0, self.decorations.buffer().slice(..));
                for &row in &dirty_rows {
                    let instances = self.decorations.instance_range(row);
                    if !instances.is_empty() {
                        pass.draw(0..6, instances);
                    }
                }
            }
            if let Some(ime) = &self.ime_preedit
                && dirty_rows.binary_search(&ime.row).is_ok()
            {
                pass.set_vertex_buffer(0, self.ime_instance_buffer.slice(..));
                pass.draw(0..6, 0..1);
            }
            self.glyph_renderer.render(&mut pass);
            if cursor_row_dirty
                && self.cursor.visible
                && (!self.cursor.blinking || self.cursor_phase)
            {
                pass.set_pipeline(&self.cell_pipeline);
                pass.set_bind_group(0, &self.viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, self.cursor_buffer.slice(..));
                pass.draw(0..6, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
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

    /// 更新输入法预编辑文本；空字符串会恢复该行的正常终端内容。
    pub(crate) fn set_ime_preedit(&mut self, value: &str) -> bool {
        let previous_row = self.ime_preedit.as_ref().map(|ime| ime.row);
        if value.is_empty() || !self.cursor.visible || self.cursor.row >= self.rows {
            self.ime_preedit = None;
            self.update_cursor_buffer();
            self.queue.write_buffer(
                &self.ime_instance_buffer,
                0,
                bytemuck::bytes_of(&CellInstance::default()),
            );
            if let Some(row) = previous_row {
                self.dirty_rows.push(row);
                self.render_pending = true;
            }
            return previous_row.is_some();
        }

        let column = self.cursor.column.min(self.columns.saturating_sub(1));
        let row = self.cursor.row;
        let metrics = cosmic_text::Metrics::new(self.font_size, self.cell_height);
        let grid = text::GridTextMetrics::new(
            metrics,
            self.cell_width,
            self.cell_height,
            &self.font_family,
        );
        let (cells, columns) = text::compose_preedit(
            &self.row_cells[row],
            self.columns,
            column,
            value,
            self.theme.foreground,
        );
        let buffers = text::create_row_buffers(&mut self.font_system, grid, self.columns, &cells);
        self.ime_preedit = Some(ImePreedit {
            text: value.to_owned(),
            buffers,
            column,
            row,
            columns,
        });
        if let Some(previous_row) = previous_row {
            self.dirty_rows.push(previous_row);
        }
        self.dirty_rows.push(row);
        self.update_ime_buffer();
        self.update_cursor_buffer();
        self.render_pending = true;
        true
    }

    fn relocate_ime_to_cursor(&mut self) {
        if let Some(text) = self.ime_preedit.as_ref().map(|ime| ime.text.clone()) {
            self.set_ime_preedit(&text);
        }
    }

    fn update_ime_buffer(&self) {
        let instance = self
            .ime_preedit
            .as_ref()
            .map_or_else(CellInstance::default, |ime| CellInstance {
                rect: [
                    ime.column as f32 * self.cell_width,
                    ime.row as f32 * self.cell_height,
                    ime.columns as f32 * self.cell_width,
                    self.cell_height,
                ],
                background: resources::rgba(self.theme.background, 1.0),
                foreground: resources::rgba(self.theme.foreground, 1.0),
                flags: [1, 0, 0, 0],
            });
        self.queue
            .write_buffer(&self.ime_instance_buffer, 0, bytemuck::bytes_of(&instance));
    }
}

/// 判断完整帧是否按顺序覆盖了当前网格的每一行。
fn frame_is_complete(frame: &FramePatch) -> bool {
    frame.changed_rows.len() >= frame.rows
        && frame
            .changed_rows
            .iter()
            .take(frame.rows)
            .enumerate()
            .all(|(row, patch)| {
                patch.row == row && patch.start_column == 0 && patch.end_column == frame.columns
            })
}

#[cfg(test)]
mod tests {
    use super::{
        frame_is_complete,
        resources::{TEXTURE_FORMAT, rgba},
    };
    use crate::terminal::{CursorPatch, FramePatch, RgbColor, RowPatch};
    use slint::wgpu_29::wgpu;

    fn full_frame(rows: usize) -> FramePatch {
        FramePatch {
            generation: 1,
            columns: 80,
            rows,
            full_redraw: true,
            full_redraw_reason: Some(crate::terminal::FullRedrawReason::RendererRequest),
            metadata_only: false,
            changed_rows: (0..rows)
                .map(|row| RowPatch {
                    row,
                    start_column: 0,
                    end_column: 80,
                    cells: Vec::new(),
                })
                .collect(),
            cursor: CursorPatch::default(),
            scroll_offset: 0,
            scroll_history_lines: 0,
            search: crate::terminal::SearchSnapshot::default(),
            decorations: Vec::new(),
            title: None,
            exit_message: None,
        }
    }

    #[test]
    fn srgb_bytes_pass_through_unchanged_into_a_non_srgb_texture() {
        // 颜色不做伽马变换，目标格式也不能再编码一次，否则 Slint 采样后会失真。
        assert_eq!(TEXTURE_FORMAT, wgpu::TextureFormat::Rgba8Unorm);
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
