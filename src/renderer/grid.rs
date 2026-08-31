//! 终端网格的 CPU 缓存、GPU 实例缓冲和字形行缓存更新。

use super::{
    GpuTerminalRenderer,
    resources::{CellInstance, create_instance_buffer, rgba},
    text,
};
use crate::terminal::{RgbColor, TerminalCellPatch};
use glyphon::Metrics;

impl GpuTerminalRenderer {
    /// 为每个网格单元预留独立字形缓存，字符始终以列坐标为定位基准。
    pub(super) fn rebuild_text_cells(&mut self) {
        self.text_cells = std::iter::repeat_with(|| None)
            .take(self.columns.saturating_mul(self.rows))
            .collect();
    }

    /// 网格尺寸变化后重新分配所有按单元/按行索引的资源。
    pub(super) fn resize_grid(&mut self, columns: usize, rows: usize) {
        self.columns = columns;
        self.rows = rows;
        self.cell_instances = vec![CellInstance::default(); columns.saturating_mul(rows)];
        self.instance_buffer = create_instance_buffer(&self.device, columns.saturating_mul(rows));
        self.rebuild_text_cells();
        self.dirty_rows = (0..rows).collect();
        self.clear_pending = true;
        self.render_pending = true;
    }

    pub(super) fn update_row_metrics(&mut self) {
        let metrics = Metrics::new(self.font_size, self.cell_height);
        for cell in self.text_cells.iter_mut().flatten() {
            cell.update_metrics(
                &mut self.font_system,
                metrics,
                self.cell_width,
                self.cell_height,
            );
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
    }

    pub(super) fn update_viewport(&self) {
        self.queue.write_buffer(
            &self.viewport_buffer,
            0,
            bytemuck::cast_slice(&[self.width as f32, self.height as f32, 0.0, 0.0]),
        );
    }

    /// 用一行补丁更新背景色、下划线和删除线的实例数据。
    pub(super) fn update_cell_row(&mut self, row: usize, cells: &[TerminalCellPatch]) {
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

    /// 独立排版每个非空字符，使它的起点严格等于 column * cell_width。
    pub(super) fn update_text_row(&mut self, row: usize, cells: &[TerminalCellPatch]) -> usize {
        if row >= self.rows {
            return 0;
        }
        let start = row * self.columns;
        self.text_cells[start..start + self.columns].fill_with(|| None);
        let metrics = Metrics::new(self.font_size, self.cell_height);
        let mut visible_cells = 0;
        for cell in cells.iter().filter(|cell| cell.column < self.columns) {
            let buffer = text::create_cell_buffer(
                &mut self.font_system,
                metrics,
                self.cell_width,
                self.cell_height,
                cell,
            );
            visible_cells += usize::from(buffer.is_some());
            self.text_cells[start + cell.column] = buffer;
        }
        visible_cells
    }

    /// 将当前光标样式写入单实例缓冲；具体形状由着色器解释。
    pub(super) fn update_cursor_buffer(&self) {
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
