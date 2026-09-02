//! 终端网格的 CPU 缓存、GPU 实例缓冲和字形行缓存更新。

use super::{
    GpuTerminalRenderer,
    resources::{CellInstance, create_instance_buffer, rgba},
    text,
};
use crate::terminal::TerminalCellPatch;
use glyphon::Metrics;

impl GpuTerminalRenderer {
    /// 每行保存少量网格锚定 run；ASCII 连字连续成组，宽字符保持独立列锚点。
    pub(super) fn rebuild_text_rows(&mut self) {
        self.text_rows = std::iter::repeat_with(Vec::new).take(self.rows).collect();
        self.row_cells = std::iter::repeat_with(Vec::new).take(self.rows).collect();
    }

    /// 网格尺寸变化后重新分配所有按单元/按行索引的资源。
    pub(super) fn resize_grid(&mut self, columns: usize, rows: usize) {
        self.columns = columns;
        self.rows = rows;
        self.cell_instances = vec![CellInstance::default(); columns.saturating_mul(rows)];
        self.instance_buffer = create_instance_buffer(&self.device, columns.saturating_mul(rows));
        self.rebuild_text_rows();
        self.relocate_ime_to_cursor();
        self.dirty_rows = (0..rows).collect();
        self.clear_pending = true;
        self.render_pending = true;
    }

    pub(super) fn update_row_metrics(&mut self) {
        let metrics = Metrics::new(self.font_size, self.cell_height);
        for run in self.text_rows.iter_mut().flatten() {
            run.update_metrics(
                &mut self.font_system,
                metrics,
                self.cell_width,
                self.cell_height,
            );
        }
        if let Some(ime) = self.ime_preedit.as_mut() {
            for run in &mut ime.buffers {
                run.update_metrics(
                    &mut self.font_system,
                    metrics,
                    self.cell_width,
                    self.cell_height,
                );
            }
        }
    }

    /// 根据新的单元尺寸重算已有实例矩形，而不改变其中的颜色和样式。
    pub(super) fn update_instance_metrics(&mut self, previous_cell_width: f32) {
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

    pub(super) fn upload_all_instances(&self) {
        self.queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&self.cell_instances),
        );
        self.update_cursor_buffer();
        self.update_ime_buffer();
    }

    pub(super) fn update_viewport(&self) {
        self.queue.write_buffer(
            &self.viewport_buffer,
            0,
            bytemuck::cast_slice(&[self.width as f32, self.height as f32, 0.0, 0.0]),
        );
    }

    /// 只更新受损列区间的背景色、下划线和删除线实例数据。
    pub(super) fn update_cell_range(
        &mut self,
        row: usize,
        start_column: usize,
        end_column: usize,
        cells: &[TerminalCellPatch],
    ) {
        let row_start = row * self.columns;
        let start_column = start_column.min(self.columns);
        let end_column = end_column.min(self.columns).max(start_column);
        self.cell_instances[row_start + start_column..row_start + end_column]
            .fill(CellInstance::default());
        for cell in cells {
            if cell.column < start_column || cell.column >= end_column {
                continue;
            }
            self.cell_instances[row_start + cell.column] = CellInstance {
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

    /// 把列补丁合入该行的完整 CPU 文本缓存，供 shaping 使用。
    pub(super) fn merge_row_cells(
        &mut self,
        row: usize,
        start_column: usize,
        end_column: usize,
        cells: &[TerminalCellPatch],
    ) {
        if row >= self.rows {
            return;
        }
        let cached = &mut self.row_cells[row];
        cached.retain(|cell| {
            let cell_end = cell.column.saturating_add(cell.width_in_columns.max(1));
            cell_end <= start_column || cell.column >= end_column
        });
        cached.extend(cells.iter().cloned());
        cached.sort_unstable_by_key(|cell| cell.column);
    }

    /// 构造网格锚定 run；返回值用于性能统计。
    pub(super) fn update_text_row(&mut self, row: usize, cells: &[TerminalCellPatch]) -> usize {
        if row >= self.rows {
            return 0;
        }
        let metrics = Metrics::new(self.font_size, self.cell_height);
        let grid = text::GridTextMetrics::new(
            metrics,
            self.cell_width,
            self.cell_height,
            &self.font_family,
        );
        let buffers = text::create_row_buffers(&mut self.font_system, grid, self.columns, cells);
        let visible_runs = buffers.len();
        self.text_rows[row] = buffers;
        self.row_cells[row] = cells.to_vec();
        visible_runs
    }

    /// 使用缓存的终端单元重建所有 shaping run，保证字体切换立即且原子地生效。
    pub(super) fn rebuild_all_text_rows(&mut self) {
        for row in 0..self.rows {
            let cells = self.row_cells[row].clone();
            self.update_text_row(row, &cells);
        }
    }

    /// 将当前光标样式写入单实例缓冲；具体形状由着色器解释。
    pub(super) fn update_cursor_buffer(&self) {
        let mut instance = CellInstance::default();
        if self.cursor.visible && self.cursor.column < self.columns && self.cursor.row < self.rows {
            let cursor_color = self.theme.foreground;
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
