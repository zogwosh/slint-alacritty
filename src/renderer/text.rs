//! 终端专用文字 shaping：ASCII 连字按连续 run 缓存，fallback/宽字符始终锚定网格列。

use crate::terminal::{RgbColor, TerminalCellPatch};
use glyphon::{
    Attrs, Buffer, Color as GlyphColor, Family, FontSystem, Metrics, Shaping, Style, Weight, Wrap,
};
use std::{cell::RefCell, collections::HashMap};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

struct CellMetricsCache {
    font_system: FontSystem,
    values: HashMap<(String, u32), (f32, f32)>,
}

thread_local! {
    /// 字体数据库扫描代价很高；设置界面的度量器在线程内复用，并缓存已测组合。
    static CELL_METRICS_CACHE: RefCell<CellMetricsCache> = RefCell::new(CellMetricsCache {
        font_system: FontSystem::new(),
        values: HashMap::new(),
    });
}

#[derive(Clone, Copy)]
pub(super) struct GridTextMetrics<'a> {
    metrics: Metrics,
    cell_width: f32,
    cell_height: f32,
    font_family: &'a str,
}

impl<'a> GridTextMetrics<'a> {
    pub(super) fn new(
        metrics: Metrics,
        cell_width: f32,
        cell_height: f32,
        font_family: &'a str,
    ) -> Self {
        Self {
            metrics,
            cell_width,
            cell_height,
            font_family,
        }
    }
}

/// 一个明确锚定到终端列的 shaping run。
pub(super) struct TextRunBuffer {
    pub(super) buffer: Buffer,
    column: usize,
    columns: usize,
    center_in_cells: bool,
    x_offset: f32,
    /// glyphon 根据本行实际 fallback 字体算出的基线；渲染时会对齐到终端固定基线。
    baseline: f32,
}

impl TextRunBuffer {
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
            Some(self.columns as f32 * cell_width),
            Some(cell_height),
        );
        self.buffer
            .set_monospace_width(font_system, Some(cell_width));
        self.refresh_layout(cell_width);
    }

    pub(super) fn baseline(&self) -> f32 {
        self.baseline
    }

    pub(super) fn column(&self) -> usize {
        self.column
    }

    pub(super) fn x_offset(&self) -> f32 {
        self.x_offset
    }

    fn refresh_layout(&mut self, cell_width: f32) {
        let Some(run) = self.buffer.layout_runs().next() else {
            self.baseline = 0.0;
            self.x_offset = 0.0;
            return;
        };
        self.baseline = run.line_y;
        self.x_offset = if self.center_in_cells {
            (self.columns as f32 * cell_width - run.line_w).max(0.0) / 2.0
        } else {
            0.0
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextStyle {
    foreground: RgbColor,
    bold: bool,
    italic: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct TextRun {
    text: String,
    style: TextStyle,
    column: usize,
    columns: usize,
    mergeable_ascii: bool,
}

/// 为一行创建网格锚定的 run。连续 ASCII 可共同 shaping 连字；宽字符和 fallback
/// 字符各自以 Alacritty 给出的列位置为准，绝不使用自然 advance 推导下一列。
pub(super) fn create_row_buffers(
    font_system: &mut FontSystem,
    grid: GridTextMetrics<'_>,
    columns: usize,
    cells: &[TerminalCellPatch],
) -> Vec<TextRunBuffer> {
    build_text_runs(columns, cells)
        .into_iter()
        .map(|run| create_text_run_buffer(font_system, grid, run))
        .collect()
}

fn create_text_run_buffer(
    font_system: &mut FontSystem,
    grid: GridTextMetrics<'_>,
    run: TextRun,
) -> TextRunBuffer {
    let mut buffer = Buffer::new(font_system, grid.metrics);
    buffer.set_size(
        font_system,
        Some(run.columns as f32 * grid.cell_width),
        Some(grid.cell_height),
    );
    buffer.set_wrap(font_system, Wrap::None);
    buffer.set_monospace_width(font_system, Some(grid.cell_width));
    buffer.set_text(
        font_system,
        &run.text,
        &attrs(grid.font_family, run.style),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(font_system, false);
    let (baseline, x_offset) = buffer.layout_runs().next().map_or((0.0, 0.0), |layout| {
        let x_offset = if run.mergeable_ascii {
            0.0
        } else {
            (run.columns as f32 * grid.cell_width - layout.line_w).max(0.0) / 2.0
        };
        (layout.line_y, x_offset)
    });
    TextRunBuffer {
        buffer,
        column: run.column,
        columns: run.columns,
        center_in_cells: !run.mergeable_ascii,
        x_offset,
        baseline,
    }
}

/// IME 预编辑也按 grapheme 和 Unicode 列宽拆成相同的网格锚定 run。
pub(super) fn create_preedit_buffers(
    font_system: &mut FontSystem,
    grid: GridTextMetrics<'_>,
    available_columns: usize,
    text: &str,
    foreground: RgbColor,
) -> (Vec<TextRunBuffer>, usize) {
    if text.is_empty() || available_columns == 0 {
        return (Vec::new(), 0);
    }
    let style = TextStyle {
        foreground,
        bold: false,
        italic: false,
    };
    let mut runs = Vec::new();
    let mut column = 0;
    for grapheme in UnicodeSegmentation::graphemes(text, true) {
        if column >= available_columns {
            break;
        }
        let columns = UnicodeWidthStr::width(grapheme)
            .max(1)
            .min(available_columns - column);
        push_grid_run(&mut runs, grapheme, style, column, columns);
        column += columns;
    }
    let buffers = runs
        .into_iter()
        .map(|run| create_text_run_buffer(font_system, grid, run))
        .collect();
    (buffers, column)
}

/// 主字体在一个终端单元内的稳定基线。fallback 字体不能改变这个值。
pub(super) fn measure_fixed_baseline(
    font_system: &mut FontSystem,
    font_family: &str,
    font_size: f32,
    cell_height: f32,
) -> f32 {
    let metrics = Metrics::new(font_size.max(1.0), cell_height.max(1.0));
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(font_system, None, Some(cell_height.max(1.0)));
    buffer.set_wrap(font_system, Wrap::None);
    buffer.set_text(
        font_system,
        "Mg",
        &Attrs::new().family(Family::Name(font_family)),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(font_system, false);
    buffer
        .layout_runs()
        .next()
        .map_or(cell_height * 0.75, |run| run.line_y)
}

/// 把内容相关基线平移到终端固定基线。
pub(super) fn baseline_aligned_top(row_top: f32, fixed_baseline: f32, shaped_baseline: f32) -> f32 {
    row_top + fixed_baseline - shaped_baseline
}

/// 使用与终端渲染器相同的字体系统测量逻辑单元尺寸，避免 UI 与 glyphon 各算一套。
pub(crate) fn measure_cell(font_family: &str, font_size: f32) -> (f32, f32) {
    let font_size = font_size.max(1.0);
    let cell_height = font_size + 5.0;
    let key = (font_family.to_owned(), font_size.to_bits());
    CELL_METRICS_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(metrics) = cache.values.get(&key) {
            return *metrics;
        }

        let metrics = Metrics::new(font_size, cell_height);
        let mut buffer = Buffer::new(&mut cache.font_system, metrics);
        buffer.set_size(&mut cache.font_system, None, Some(cell_height));
        buffer.set_wrap(&mut cache.font_system, Wrap::None);
        buffer.set_text(
            &mut cache.font_system,
            "0000000000",
            &Attrs::new().family(Family::Name(font_family)),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut cache.font_system, false);
        let measured = buffer
            .layout_runs()
            .next()
            .map(|run| run.line_w / 10.0)
            .filter(|width| width.is_finite() && *width > 0.0)
            .unwrap_or(font_size * 0.6);
        let result = (measured, cell_height);
        cache.values.insert(key, result);
        result
    })
}

fn build_text_runs(columns: usize, cells: &[TerminalCellPatch]) -> Vec<TextRun> {
    let mut runs = Vec::<TextRun>::new();
    for cell in cells.iter().filter(|cell| cell.column < columns) {
        let width = cell.width_in_columns.max(1).min(columns - cell.column);
        if cell.hidden
            || cell.text.is_empty()
            || cell.text.chars().all(|character| character == ' ')
        {
            continue;
        }
        push_grid_run(&mut runs, &cell.text, cell_style(cell), cell.column, width);
    }
    runs
}

fn push_grid_run(
    runs: &mut Vec<TextRun>,
    text: &str,
    style: TextStyle,
    column: usize,
    columns: usize,
) {
    let mergeable_ascii = columns == 1 && text.is_ascii();
    if let Some(last) = runs.last_mut()
        && mergeable_ascii
        && last.mergeable_ascii
        && last.style == style
        && last.column + last.columns == column
    {
        last.text.push_str(text);
        last.columns += columns;
    } else {
        runs.push(TextRun {
            text: text.to_owned(),
            style,
            column,
            columns,
            mergeable_ascii,
        });
    }
}

fn cell_style(cell: &TerminalCellPatch) -> TextStyle {
    TextStyle {
        foreground: cell.foreground,
        bold: cell.bold,
        italic: cell.italic,
    }
}

fn attrs<'a>(font_family: &'a str, style: TextStyle) -> Attrs<'a> {
    Attrs::new()
        .family(Family::Name(font_family))
        .color(glyph_color(style.foreground))
        .weight(if style.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if style.italic {
            Style::Italic
        } else {
            Style::Normal
        })
}

fn glyph_color(color: RgbColor) -> GlyphColor {
    GlyphColor::rgb(color.red, color.green, color.blue)
}

#[cfg(test)]
mod tests {
    use super::{GridTextMetrics, baseline_aligned_top, build_text_runs, create_preedit_buffers};
    use crate::terminal::{RgbColor, TerminalCellPatch};
    use glyphon::{FontSystem, Metrics};

    fn cell(column: usize, text: &str, foreground: RgbColor) -> TerminalCellPatch {
        TerminalCellPatch {
            text: text.to_owned(),
            foreground,
            background: RgbColor {
                red: 0,
                green: 0,
                blue: 0,
            },
            column,
            width_in_columns: 1,
            bold: false,
            italic: false,
            underline_style: 0,
            strikeout: false,
            hidden: false,
        }
    }

    #[test]
    fn merges_adjacent_cells_with_the_same_text_style() {
        let white = RgbColor {
            red: 255,
            green: 255,
            blue: 255,
        };
        let red = RgbColor {
            red: 255,
            green: 0,
            blue: 0,
        };
        let runs = build_text_runs(
            3,
            &[cell(0, "!", white), cell(1, "=", white), cell(2, "x", red)],
        );

        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "!=");
        assert_eq!(runs[0].column, 0);
        assert_eq!(runs[0].columns, 2);
        assert_eq!(runs[1].text, "x");
    }

    #[test]
    fn background_highlight_does_not_split_a_shaping_run() {
        let white = RgbColor {
            red: 255,
            green: 255,
            blue: 255,
        };
        let mut highlighted = cell(1, "=", white);
        highlighted.background = RgbColor {
            red: 255,
            green: 204,
            blue: 0,
        };

        let runs = build_text_runs(2, &[cell(0, "!", white), highlighted]);

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "!=");
        assert_eq!(runs[0].columns, 2);
    }

    #[test]
    fn wide_and_fallback_cells_keep_explicit_grid_anchors() {
        let white = RgbColor {
            red: 255,
            green: 255,
            blue: 255,
        };
        let mut cjk = cell(1, "中", white);
        cjk.width_in_columns = 2;
        let runs = build_text_runs(5, &[cell(0, "a", white), cjk, cell(3, "b", white)]);

        assert_eq!(runs.len(), 3);
        assert_eq!((runs[0].column, runs[0].columns), (0, 1));
        assert_eq!((runs[1].column, runs[1].columns), (1, 2));
        assert_eq!((runs[2].column, runs[2].columns), (3, 1));
    }

    #[test]
    fn fallback_metrics_cannot_move_the_terminal_baseline() {
        let row_top = 40.0;
        let fixed_baseline = 15.0;
        let ascii_top = baseline_aligned_top(row_top, fixed_baseline, 14.0);
        let cjk_top = baseline_aligned_top(row_top, fixed_baseline, 16.5);

        assert_eq!(ascii_top + 14.0, row_top + fixed_baseline);
        assert_eq!(cjk_top + 16.5, row_top + fixed_baseline);
    }

    #[test]
    fn ime_preedit_uses_grapheme_widths_and_grid_anchors() {
        let mut font_system = FontSystem::new();
        let grid = GridTextMetrics::new(Metrics::new(15.0, 20.0), 9.0, 20.0, "monospace");
        let (runs, columns) = create_preedit_buffers(
            &mut font_system,
            grid,
            10,
            "A中e\u{301}",
            RgbColor {
                red: 255,
                green: 255,
                blue: 255,
            },
        );

        assert_eq!(columns, 4);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].column, 0);
        assert_eq!(runs[1].column, 1);
        assert_eq!(runs[2].column, 3);
    }
}
