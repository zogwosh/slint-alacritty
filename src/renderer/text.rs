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
    values: HashMap<(String, u32), CellMetrics>,
}

thread_local! {
    /// 度量器在线程内复用，并缓存已测组合；FontSystem 从共享字体库克隆。
    static CELL_METRICS_CACHE: RefCell<CellMetricsCache> = RefCell::new(CellMetricsCache {
        font_system: super::fonts::font_system(),
        values: HashMap::new(),
    });
}

/// 终端单元在物理像素下的整数尺寸。整数尺寸保证列边界、行裁剪和字形子像素相位一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CellMetrics {
    pub(crate) width: u32,
    pub(crate) height: u32,
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

/// 决定字体选择与 shaping 的属性；变化时必须切断 run。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShapeStyle {
    bold: bool,
    italic: bool,
}

/// 只影响字形绘制颜色的属性；作为 rich-text span 存在于同一个 run 内，不切断连字。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PaintStyle {
    foreground: RgbColor,
}

#[derive(Debug, PartialEq, Eq)]
struct PaintSpan {
    text: String,
    paint: PaintStyle,
}

#[derive(Debug, PartialEq, Eq)]
struct TextRun {
    spans: Vec<PaintSpan>,
    shape: ShapeStyle,
    column: usize,
    columns: usize,
    mergeable_ascii: bool,
}

#[cfg(test)]
impl TextRun {
    fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
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
    // cosmic-text 判断相邻 span 能否一起 shaping 时不比较颜色，因此跨颜色边界的连字仍然成立。
    let base_attrs = attrs(grid.font_family, run.shape);
    buffer.set_rich_text(
        font_system,
        run.spans.iter().map(|span| {
            (
                span.text.as_str(),
                base_attrs.clone().color(glyph_color(span.paint.foreground)),
            )
        }),
        &base_attrs,
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
    let shape = ShapeStyle {
        bold: false,
        italic: false,
    };
    let paint = PaintStyle { foreground };
    let mut runs = Vec::new();
    let mut column = 0;
    for grapheme in UnicodeSegmentation::graphemes(text, true) {
        if column >= available_columns {
            break;
        }
        let columns = UnicodeWidthStr::width(grapheme)
            .max(1)
            .min(available_columns - column);
        let mut characters = grapheme.chars();
        let Some(character) = characters.next() else {
            continue;
        };
        let zerowidth = characters.collect::<Vec<_>>();
        push_grid_run(
            &mut runs,
            character,
            &zerowidth,
            shape,
            paint,
            column,
            columns,
        );
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

/// 以物理像素字号测量主字体，得到整数单元尺寸：宽度取平均 advance，高度取
/// ascent + descent + line gap。与终端渲染器共用同一字体库，避免 UI 与 glyphon 各算一套。
pub(crate) fn measure_cell(font_family: &str, physical_font_size: f32) -> CellMetrics {
    let font_size = physical_font_size.max(1.0);
    let key = (font_family.to_owned(), font_size.to_bits());
    CELL_METRICS_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(metrics) = cache.values.get(&key) {
            return *metrics;
        }

        let font_system = &mut cache.font_system;
        let mut buffer = Buffer::new(font_system, Metrics::new(font_size, font_size * 1.2));
        buffer.set_size(font_system, None, None);
        buffer.set_wrap(font_system, Wrap::None);
        buffer.set_text(
            font_system,
            "0000000000",
            &Attrs::new().family(Family::Name(font_family)),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(font_system, false);

        let mut advance = None;
        let mut line_height = None;
        if let Some(run) = buffer.layout_runs().next() {
            advance = Some(run.line_w / 10.0).filter(|width| width.is_finite() && *width > 0.0);
            if let Some(glyph) = run.glyphs.first()
                && let Some(font) = font_system.get_font(glyph.font_id, glyph.font_weight)
            {
                let metrics = font.as_swash().metrics(&[]).scale(font_size);
                let height = metrics.ascent + metrics.descent + metrics.leading;
                line_height = Some(height).filter(|height| height.is_finite() && *height > 0.0);
            }
        }
        let result = CellMetrics {
            width: advance.unwrap_or(font_size * 0.6).round().max(1.0) as u32,
            height: line_height
                .unwrap_or(font_size * 1.2)
                .round()
                .max(1.0) as u32,
        };
        cache.values.insert(key, result);
        result
    })
}

/// 参与 shaping 的单元：隐藏、空白和越界的单元不会产生字形。
fn glyph_cells<'a>(
    columns: usize,
    cells: impl IntoIterator<Item = &'a TerminalCellPatch>,
) -> impl Iterator<Item = &'a TerminalCellPatch> {
    cells
        .into_iter()
        .filter(move |cell| cell.column < columns && !cell.hidden && !cell.is_blank())
}

/// 判断两组单元是否会生成完全相同的字形缓冲。背景色、下划线和删除线只影响实例数据，
/// 因此不参与比较；前景色保存在 rich-text span 中，变化时必须重建缓冲。
pub(super) fn same_glyph_content<'a>(
    columns: usize,
    previous: impl IntoIterator<Item = &'a TerminalCellPatch>,
    next: impl IntoIterator<Item = &'a TerminalCellPatch>,
) -> bool {
    let key = |cell: &'a TerminalCellPatch| {
        (
            cell.column,
            cell.width_in_columns,
            cell.bold,
            cell.italic,
            cell.foreground,
            cell.character,
            cell.zerowidth.as_deref(),
        )
    };
    glyph_cells(columns, previous)
        .map(key)
        .eq(glyph_cells(columns, next).map(key))
}

fn build_text_runs(columns: usize, cells: &[TerminalCellPatch]) -> Vec<TextRun> {
    let mut runs = Vec::<TextRun>::new();
    for cell in glyph_cells(columns, cells) {
        let width = cell.width_in_columns.max(1).min(columns - cell.column);
        push_grid_run(
            &mut runs,
            cell.character,
            cell.zerowidth.as_deref().unwrap_or(&[]),
            ShapeStyle {
                bold: cell.bold,
                italic: cell.italic,
            },
            PaintStyle {
                foreground: cell.foreground,
            },
            cell.column,
            width,
        );
    }
    runs
}

#[allow(clippy::too_many_arguments)]
fn push_grid_run(
    runs: &mut Vec<TextRun>,
    character: char,
    zerowidth: &[char],
    shape: ShapeStyle,
    paint: PaintStyle,
    column: usize,
    columns: usize,
) {
    let mergeable_ascii = columns == 1 && character.is_ascii() && zerowidth.is_empty();
    let write_text = |text: &mut String| {
        text.push(character);
        text.extend(zerowidth);
    };
    if let Some(last) = runs.last_mut()
        && mergeable_ascii
        && last.mergeable_ascii
        && last.shape == shape
        && last.column + last.columns == column
    {
        match last.spans.last_mut() {
            Some(span) if span.paint == paint => write_text(&mut span.text),
            _ => {
                let mut text = String::new();
                write_text(&mut text);
                last.spans.push(PaintSpan { text, paint });
            }
        }
        last.columns += columns;
    } else {
        let mut text = String::with_capacity(4);
        write_text(&mut text);
        runs.push(TextRun {
            spans: vec![PaintSpan { text, paint }],
            shape,
            column,
            columns,
            mergeable_ascii,
        });
    }
}

fn attrs<'a>(font_family: &'a str, shape: ShapeStyle) -> Attrs<'a> {
    Attrs::new()
        .family(Family::Name(font_family))
        .weight(if shape.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if shape.italic {
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
    use super::{
        GridTextMetrics, baseline_aligned_top, build_text_runs, create_preedit_buffers,
        same_glyph_content,
    };
    use crate::terminal::{RgbColor, TerminalCellPatch};
    use glyphon::Metrics;

    fn cell(column: usize, character: char, foreground: RgbColor) -> TerminalCellPatch {
        TerminalCellPatch {
            character,
            zerowidth: None,
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
    fn foreground_color_becomes_a_paint_span_instead_of_splitting_the_run() {
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
            &[cell(0, '!', white), cell(1, '=', white), cell(2, 'x', red)],
        );

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text(), "!=x");
        assert_eq!(runs[0].column, 0);
        assert_eq!(runs[0].columns, 3);
        assert_eq!(runs[0].spans.len(), 2);
        assert_eq!(runs[0].spans[0].text, "!=");
        assert_eq!(runs[0].spans[1].text, "x");
        assert_eq!(runs[0].spans[1].paint.foreground, red);
    }

    #[test]
    fn bold_changes_split_the_shaping_run() {
        let white = RgbColor {
            red: 255,
            green: 255,
            blue: 255,
        };
        let mut bold = cell(2, 'x', white);
        bold.bold = true;
        let runs = build_text_runs(3, &[cell(0, '!', white), cell(1, '=', white), bold]);

        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text(), "!=");
        assert_eq!(runs[1].text(), "x");
    }

    #[test]
    fn background_highlight_does_not_split_a_shaping_run() {
        let white = RgbColor {
            red: 255,
            green: 255,
            blue: 255,
        };
        let mut highlighted = cell(1, '=', white);
        highlighted.background = RgbColor {
            red: 255,
            green: 204,
            blue: 0,
        };

        let runs = build_text_runs(2, &[cell(0, '!', white), highlighted]);

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text(), "!=");
        assert_eq!(runs[0].spans.len(), 1);
        assert_eq!(runs[0].columns, 2);
    }

    #[test]
    fn paint_only_changes_do_not_require_reshaping() {
        let white = RgbColor {
            red: 255,
            green: 255,
            blue: 255,
        };
        let before = [
            cell(0, 'a', white),
            cell(1, 'b', white),
            cell(2, ' ', white),
        ];
        let mut after = before.clone();
        after[1].background = RgbColor {
            red: 9,
            green: 9,
            blue: 9,
        };
        after[1].underline_style = 1;
        after[1].strikeout = true;
        after[2].hidden = true;
        assert!(same_glyph_content(3, &before, &after));

        let mut recolored = before.clone();
        recolored[0].foreground = RgbColor {
            red: 255,
            green: 0,
            blue: 0,
        };
        assert!(!same_glyph_content(3, &before, &recolored));

        let mut retyped = before.clone();
        retyped[1].character = 'c';
        assert!(!same_glyph_content(3, &before, &retyped));
    }

    #[test]
    fn wide_and_fallback_cells_keep_explicit_grid_anchors() {
        let white = RgbColor {
            red: 255,
            green: 255,
            blue: 255,
        };
        let mut cjk = cell(1, '中', white);
        cjk.width_in_columns = 2;
        let runs = build_text_runs(5, &[cell(0, 'a', white), cjk, cell(3, 'b', white)]);

        assert_eq!(runs.len(), 3);
        assert_eq!((runs[0].column, runs[0].columns), (0, 1));
        assert_eq!((runs[1].column, runs[1].columns), (1, 2));
        assert_eq!((runs[2].column, runs[2].columns), (3, 1));
    }

    #[test]
    fn cell_metrics_are_positive_integers_that_grow_with_physical_font_size() {
        let small = super::measure_cell("monospace", 12.0);
        let large = super::measure_cell("monospace", 24.0);
        assert!(small.width >= 1 && small.height >= 1);
        assert!(large.width > small.width);
        assert!(large.height > small.height);
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
        let mut font_system = super::super::fonts::font_system();
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
