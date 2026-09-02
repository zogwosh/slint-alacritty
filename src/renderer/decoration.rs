//! 装饰层：选区与搜索高亮只是叠加在单元背景之上的矩形，不参与文字 shaping。

use super::resources::{CellInstance, create_instance_buffer, rgba_from_theme};
use crate::terminal::{DecorationKind, DecorationRange, RgbaColor, TerminalTheme};
use slint::wgpu_29::wgpu;

/// 装饰层 GPU 实例缓存；实例按行排序，渲染时可为每个脏行取出连续区间。
pub(super) struct DecorationLayer {
    ranges: Vec<DecorationRange>,
    instances: Vec<CellInstance>,
    buffer: wgpu::Buffer,
    capacity: usize,
}

impl DecorationLayer {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let capacity = 64;
        Self {
            ranges: Vec::new(),
            instances: Vec::new(),
            buffer: create_instance_buffer(device, capacity),
            capacity,
        }
    }

    pub(super) fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    pub(super) fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// 某一行的装饰实例在缓冲区中的下标区间；实例与 ranges 一一对应。
    pub(super) fn instance_range(&self, row: usize) -> std::ops::Range<u32> {
        let start = self.ranges.partition_point(|range| range.row < row);
        let end = self.ranges.partition_point(|range| range.row <= row);
        start as u32..end as u32
    }

    /// 用新的视口装饰（已按行排序且裁剪到网格内）替换旧值，返回需要重绘的行；
    /// 未变化时不上传任何数据。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn replace(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ranges: &[DecorationRange],
        cell_width: f32,
        cell_height: f32,
        theme: TerminalTheme,
    ) -> Vec<usize> {
        let dirty_rows = changed_rows(&self.ranges, ranges);
        if dirty_rows.is_empty() {
            return dirty_rows;
        }
        self.ranges.clear();
        self.ranges.extend_from_slice(ranges);
        self.instances = self
            .ranges
            .iter()
            .map(|range| CellInstance {
                rect: [
                    range.start_column as f32 * cell_width,
                    range.row as f32 * cell_height,
                    (range.end_column - range.start_column) as f32 * cell_width,
                    cell_height,
                ],
                background: rgba_from_theme(decoration_color(range.kind, theme)),
                foreground: [0.0; 4],
                flags: [0; 4],
            })
            .collect();
        self.upload(device, queue);
        dirty_rows
    }

    /// 单元尺寸变化后重算矩形；调用方负责标记全部行为脏。
    pub(super) fn update_metrics(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cell_width: f32,
        cell_height: f32,
    ) {
        for (instance, range) in self.instances.iter_mut().zip(&self.ranges) {
            instance.rect = [
                range.start_column as f32 * cell_width,
                range.row as f32 * cell_height,
                (range.end_column - range.start_column) as f32 * cell_width,
                cell_height,
            ];
        }
        self.upload(device, queue);
    }

    pub(super) fn clear(&mut self) {
        self.ranges.clear();
        self.instances.clear();
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.instances.is_empty() {
            return;
        }
        if self.instances.len() > self.capacity {
            self.capacity = self.instances.len().next_power_of_two();
            self.buffer = create_instance_buffer(device, self.capacity);
        }
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&self.instances));
    }
}

fn decoration_color(kind: DecorationKind, theme: TerminalTheme) -> RgbaColor {
    match kind {
        DecorationKind::Selection => theme.selection_background,
        DecorationKind::SearchMatch => theme.search_match_background,
        DecorationKind::SearchCurrent => theme.search_current_background,
    }
}

/// 比较两组按行排序的装饰，返回内容有差异的行号（升序）。
fn changed_rows(previous: &[DecorationRange], next: &[DecorationRange]) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut previous_groups = previous.chunk_by(|a, b| a.row == b.row).peekable();
    let mut next_groups = next.chunk_by(|a, b| a.row == b.row).peekable();
    loop {
        match (previous_groups.peek(), next_groups.peek()) {
            (None, None) => break,
            (Some(old), None) => {
                rows.push(old[0].row);
                previous_groups.next();
            }
            (None, Some(new)) => {
                rows.push(new[0].row);
                next_groups.next();
            }
            (Some(old), Some(new)) => {
                if old[0].row < new[0].row {
                    rows.push(old[0].row);
                    previous_groups.next();
                } else if new[0].row < old[0].row {
                    rows.push(new[0].row);
                    next_groups.next();
                } else {
                    if old != new {
                        rows.push(old[0].row);
                    }
                    previous_groups.next();
                    next_groups.next();
                }
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::changed_rows;
    use crate::terminal::{DecorationKind, DecorationRange};

    fn range(row: usize, start: usize, end: usize, kind: DecorationKind) -> DecorationRange {
        DecorationRange {
            row,
            start_column: start,
            end_column: end,
            kind,
        }
    }

    #[test]
    fn only_rows_whose_decorations_changed_are_reported() {
        let previous = vec![
            range(1, 0, 4, DecorationKind::SearchMatch),
            range(3, 2, 5, DecorationKind::SearchCurrent),
            range(7, 0, 9, DecorationKind::Selection),
        ];
        let next = vec![
            range(1, 0, 4, DecorationKind::SearchMatch),
            range(3, 2, 5, DecorationKind::SearchMatch),
            range(5, 2, 5, DecorationKind::SearchCurrent),
        ];

        assert_eq!(changed_rows(&previous, &next), vec![3, 5, 7]);
        assert!(changed_rows(&next, &next).is_empty());
    }
}
