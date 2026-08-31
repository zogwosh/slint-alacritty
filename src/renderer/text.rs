//! 将终端字符独立排版到固定网格单元，避免连续排版产生累计水平漂移。

use crate::terminal::{RgbColor, TerminalCellPatch};
use glyphon::{
    Attrs, Buffer, Color as GlyphColor, Family, FontSystem, Metrics, Shaping, Style, Weight,
};

/// 单个终端字符格的 glyphon 缓冲区；宽字符可以覆盖多个网格列。
pub(super) struct CellTextBuffer {
    pub(super) buffer: Buffer,
    width_in_columns: usize,
}

impl CellTextBuffer {
    pub(super) fn update_metrics(
        &mut self,
        font_system: &mut FontSystem,
        metrics: Metrics,
        cell_width: f32,
        cell_height: f32,
    ) {
        self.buffer.set_metrics_and_size(
            font_system,
            metrics,
            Some(self.width_in_columns as f32 * cell_width),
            Some(cell_height),
        );
        self.buffer
            .set_monospace_width(font_system, Some(cell_width));
    }
}

/// 为一个有可见字形的终端单元创建独立布局缓冲区。
pub(super) fn create_cell_buffer(
    font_system: &mut FontSystem,
    metrics: Metrics,
    cell_width: f32,
    cell_height: f32,
    font_family: &str,
    cell: &TerminalCellPatch,
) -> Option<CellTextBuffer> {
    if cell.hidden || cell.text.is_empty() || cell.text.chars().all(|character| character == ' ') {
        return None;
    }

    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(
        font_system,
        Some(cell.width_in_columns as f32 * cell_width),
        Some(cell_height),
    );
    buffer.set_monospace_width(font_system, Some(cell_width));

    let attrs = Attrs::new()
        .family(Family::Name(font_family))
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
    buffer.set_text(font_system, &cell.text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(font_system, false);
    Some(CellTextBuffer {
        buffer,
        width_in_columns: cell.width_in_columns,
    })
}

fn glyph_color(color: RgbColor) -> GlyphColor {
    GlyphColor::rgb(color.red, color.green, color.blue)
}
