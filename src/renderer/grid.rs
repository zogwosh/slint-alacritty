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
        self.decorations.clear();
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
    pub(super) fn update_instance_metrics(&mut self) {
        for (index, instance) in self.cell_instances.iter_mut().enumerate() {
            let columns = instance.flags[3];
            if columns == 0 {
                continue;
            }
            instance.rect = [
                (index % self.columns) as f32 * self.cell_width,
                (index / self.columns) as f32 * self.cell_height,
                columns as f32 * self.cell_width,
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
                // flags.w 保存实例占据的列数，尺寸变化时据此重算矩形。
                flags: [
                    cell.underline_style.max(0) as u32,
                    u32::from(cell.strikeout),
                    0,
                    cell.width_in_columns.max(1) as u32,
                ],
            };
        }
    }

    /// 把列补丁原地合入该行的完整 CPU 文本缓存（缓存按列有序）；
    /// 返回 true 表示影响字形的属性发生了变化，Err 表示缓存已不可信、需要完整帧重建。
    pub(super) fn merge_row_cells(
        &mut self,
        row: usize,
        start_column: usize,
        end_column: usize,
        cells: &[TerminalCellPatch],
    ) -> Result<bool, RowCacheCorrupted> {
        if row >= self.rows {
            return Ok(false);
        }
        merge_row_patch(
            self.columns,
            &mut self.row_cells[row],
            start_column,
            end_column,
            cells,
        )
    }

    /// 光标所在单元占据的列数；落在宽字符上时光标要覆盖两格。
    fn cursor_width_in_columns(&self) -> usize {
        self.row_cells
            .get(self.cursor.row)
            .and_then(|cells| {
                let index = cells.partition_point(|cell| cell.column < self.cursor.column);
                cells
                    .get(index)
                    .filter(|cell| cell.column == self.cursor.column)
            })
            .map_or(1, |cell| cell.width_in_columns.max(1))
            .min(self.columns.saturating_sub(self.cursor.column).max(1))
    }

    /// 用该行缓存的单元重新构造网格锚定 run；返回 run 数量用于性能统计。
    pub(super) fn update_text_row(&mut self, row: usize) -> usize {
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
        let cells = std::mem::take(&mut self.row_cells[row]);
        let buffers = text::create_row_buffers(&mut self.font_system, grid, self.columns, &cells);
        self.row_cells[row] = cells;
        let visible_runs = buffers.len();
        self.text_rows[row] = buffers;
        visible_runs
    }

    /// 使用缓存的终端单元重建所有 shaping run，保证字体切换立即且原子地生效。
    pub(super) fn rebuild_all_text_rows(&mut self) {
        for row in 0..self.rows {
            self.update_text_row(row);
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
                    self.cursor_width_in_columns() as f32 * self.cell_width,
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

/// 合并后的行缓存违反了“按列有序、互不重叠、不越界”的不变量。
/// 出现这种情况说明补丁与缓存的假设不一致，唯一安全的做法是丢弃缓存并请求完整帧。
#[derive(Debug, PartialEq, Eq)]
pub(super) struct RowCacheCorrupted;

/// 把 `[start_column, end_column)` 的列补丁原地合入按列有序的行缓存；
/// 返回 true 表示该区间内影响字形的内容发生了变化。
/// 调用方必须保证区间不会从宽字符占位格开始、也不会在宽字符首格结束；
/// 合并后会校验这一点，违反时返回错误而不是留下会画错的缓存。
pub(super) fn merge_row_patch(
    columns: usize,
    cached: &mut Vec<TerminalCellPatch>,
    start_column: usize,
    end_column: usize,
    cells: &[TerminalCellPatch],
) -> Result<bool, RowCacheCorrupted> {
    let overlaps = |cell: &TerminalCellPatch| {
        let cell_end = cell.column.saturating_add(cell.width_in_columns.max(1));
        cell_end > start_column && cell.column < end_column
    };
    // 只有被替换区间内的字形内容才可能变化，比较前无需构造整行副本。
    let glyphs_changed = !text::same_glyph_content(
        columns,
        cached.iter().filter(|cell| overlaps(cell)),
        cells,
    );
    cached.retain(|cell| !overlaps(cell));
    let insert_at = cached.partition_point(|cell| cell.column < start_column);
    cached.splice(insert_at..insert_at, cells.iter().cloned());

    // 只有插入窗口及其两侧邻居可能破坏不变量，校验代价与补丁大小成正比。
    let check_start = insert_at.saturating_sub(1);
    let check_end = (insert_at + cells.len() + 1).min(cached.len());
    let window = &cached[check_start..check_end];
    let in_bounds = window
        .iter()
        .all(|cell| cell.column < columns && cell.width_in_columns >= 1);
    let ordered = window
        .windows(2)
        .all(|pair| pair[0].column + pair[0].width_in_columns <= pair[1].column);
    if in_bounds && ordered {
        Ok(glyphs_changed)
    } else {
        cached.clear();
        Err(RowCacheCorrupted)
    }
}

#[cfg(test)]
mod tests {
    use super::merge_row_patch;
    use crate::terminal::{RgbColor, TerminalCellPatch};

    fn cell(column: usize, character: char, width: usize) -> TerminalCellPatch {
        let color = RgbColor {
            red: 255,
            green: 255,
            blue: 255,
        };
        TerminalCellPatch {
            character,
            zerowidth: None,
            foreground: color,
            background: color,
            column,
            width_in_columns: width,
            bold: false,
            italic: false,
            underline_style: 0,
            strikeout: false,
            hidden: false,
        }
    }

    #[test]
    fn partial_patch_keeps_cells_outside_its_range_and_stays_sorted() {
        let mut cached = vec![cell(0, 'a', 1), cell(1, 'b', 1), cell(2, 'c', 1)];
        let changed = merge_row_patch(3, &mut cached, 1, 2, &[cell(1, 'x', 1)]);
        assert_eq!(changed, Ok(true));
        let text = cached.iter().map(|cell| cell.character).collect::<String>();
        assert_eq!(text, "axc");
        assert!(cached.windows(2).all(|pair| pair[0].column < pair[1].column));
    }

    #[test]
    fn patch_adjacent_to_a_wide_character_leaves_the_wide_character_intact() {
        let mut cached = vec![cell(4, '中', 2), cell(6, 'a', 1)];
        // 区间 [6, 7) 已由 capture_frame 保证不会切进 4..6 的宽字符。
        let changed = merge_row_patch(8, &mut cached, 6, 7, &[cell(6, 'b', 1)]);
        assert_eq!(changed, Ok(true));
        assert_eq!(cached.len(), 2);
        assert_eq!((cached[0].column, cached[0].character), (4, '中'));
        assert_eq!((cached[1].column, cached[1].character), (6, 'b'));
    }

    #[test]
    fn background_only_changes_are_not_reported_as_glyph_changes() {
        let mut cached = vec![cell(0, 'a', 1)];
        let mut recolored = cell(0, 'a', 1);
        recolored.background = RgbColor {
            red: 1,
            green: 2,
            blue: 3,
        };
        assert_eq!(merge_row_patch(1, &mut cached, 0, 1, &[recolored]), Ok(false));
        assert_eq!(cached[0].background.red, 1);
    }

    #[test]
    fn a_patch_that_would_corrupt_the_row_cache_is_rejected_and_the_cache_dropped() {
        // 补丁声称只覆盖 [5, 6)，却带着一个 2 列宽的单元，与缓存中第 6 列的单元重叠。
        let mut cached = vec![cell(6, 'a', 1)];
        let result = merge_row_patch(8, &mut cached, 5, 6, &[cell(5, '中', 2)]);
        assert_eq!(result, Err(super::RowCacheCorrupted));
        assert!(cached.is_empty());

        // 越界列同样不能进入缓存。
        let mut cached = Vec::new();
        assert!(merge_row_patch(4, &mut cached, 0, 4, &[cell(4, 'x', 1)]).is_err());
        assert!(cached.is_empty());
    }
}
