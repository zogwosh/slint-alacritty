//! Terminal shaping with an explicit UTF-8 cluster -> grid column mapping.
//! Font advances only position glyphs inside a cluster; the grid owns cluster placement.
use crate::terminal::{RgbColor, TerminalCellPatch};
use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, LayoutGlyph, Metrics, Shaping, Style, Weight, Wrap,
};
use std::{cell::RefCell, collections::HashMap};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

struct CellMetricsCache {
    font_system: FontSystem,
    values: HashMap<(String, u32), CellMetrics>,
}
thread_local! {
    static CELL_METRICS_CACHE: RefCell<CellMetricsCache> = RefCell::new(CellMetricsCache {
        font_system: super::fonts::font_system(), values: HashMap::new(),
    });
}
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
#[derive(Clone, Debug)]
pub(super) struct GridGlyph {
    pub(super) layout: LayoutGlyph,
    pub(super) start_column: usize,
    pub(super) end_column: usize,
}
/// Positioned glyphs and independent cell paint. No paragraph layout reaches the GPU.
pub(super) struct ShapedRun {
    pub(super) glyphs: Vec<GridGlyph>,
    pub(super) cells: Vec<TerminalCellPatch>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShapeStyle {
    bold: bool,
    italic: bool,
}
#[derive(Debug)]
struct TextRun {
    text: String,
    cells: Vec<TerminalCellPatch>,
    shape: ShapeStyle,
}

fn glyph_cells<'a>(
    columns: usize,
    cells: impl IntoIterator<Item = &'a TerminalCellPatch>,
) -> impl Iterator<Item = &'a TerminalCellPatch> {
    cells
        .into_iter()
        .filter(move |c| c.column < columns && !c.hidden && !c.is_blank())
}
pub(super) fn same_glyph_content<'a>(
    columns: usize,
    previous: impl IntoIterator<Item = &'a TerminalCellPatch>,
    next: impl IntoIterator<Item = &'a TerminalCellPatch>,
) -> bool {
    let key = |c: &'a TerminalCellPatch| {
        (
            c.column,
            c.width_in_columns,
            c.bold,
            c.italic,
            c.foreground,
            c.character,
            c.zerowidth.as_deref(),
        )
    };
    glyph_cells(columns, previous)
        .map(key)
        .eq(glyph_cells(columns, next).map(key))
}
fn build_text_runs(columns: usize, cells: &[TerminalCellPatch]) -> Vec<TextRun> {
    let mut runs: Vec<TextRun> = Vec::new();
    for cell in glyph_cells(columns, cells) {
        let shape = ShapeStyle {
            bold: cell.bold,
            italic: cell.italic,
        };
        let merge = runs.last().is_some_and(|r| {
            r.shape == shape
                && r.cells
                    .last()
                    .is_some_and(|c| c.column + c.width_in_columns == cell.column)
        });
        if !merge {
            runs.push(TextRun {
                text: String::new(),
                cells: Vec::new(),
                shape,
            });
        }
        let run = runs.last_mut().unwrap();
        run.text.push(cell.character);
        run.text.extend(cell.zerowidth.as_deref().unwrap_or(&[]));
        let mut cell = cell.clone();
        cell.width_in_columns = cell.width_in_columns.max(1).min(columns - cell.column);
        run.cells.push(cell);
    }
    runs
}
pub(super) fn create_row_buffers(
    fs: &mut FontSystem,
    grid: GridTextMetrics<'_>,
    columns: usize,
    cells: &[TerminalCellPatch],
) -> Vec<ShapedRun> {
    build_text_runs(columns, cells)
        .into_iter()
        .map(|run| shape_run(fs, grid, run))
        .collect()
}
fn shape_run(fs: &mut FontSystem, grid: GridTextMetrics<'_>, run: TextRun) -> ShapedRun {
    let mut buffer = Buffer::new(fs, grid.metrics);
    buffer.set_size(fs, None, Some(grid.cell_height));
    buffer.set_wrap(fs, Wrap::None);
    let attrs = Attrs::new()
        .family(Family::Name(grid.font_family))
        .weight(if run.shape.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if run.shape.italic {
            Style::Italic
        } else {
            Style::Normal
        });
    buffer.set_text(fs, &run.text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(fs, false);
    let mut glyphs: Vec<LayoutGlyph> = buffer
        .layout_runs()
        .flat_map(|r| r.glyphs.iter().cloned())
        .collect();
    // Each source cell keeps its full base+combining sequence as one mapping interval.
    let mut byte = 0;
    let mapping: Vec<_> = run
        .cells
        .iter()
        .map(|cell| {
            let start = byte;
            byte += cell.character.len_utf8()
                + cell
                    .zerowidth
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|c| c.len_utf8())
                    .sum::<usize>();
            (
                start,
                byte,
                cell.column,
                cell.column + cell.width_in_columns,
            )
        })
        .collect();
    let mut groups: Vec<(usize, usize, Vec<usize>)> = glyphs
        .iter()
        .enumerate()
        .filter_map(|(i, g)| {
            let first = mapping.partition_point(|m| m.1 <= g.start);
            let last = mapping.partition_point(|m| m.0 < g.end);
            (first < last).then(|| (mapping[first].2, mapping[last - 1].3, vec![i]))
        })
        .collect();
    groups.sort_by_key(|g| g.0);
    let mut merged: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    for (start, end, indices) in groups {
        if let Some(last) = merged.last_mut()
            && start < last.1
        {
            last.1 = last.1.max(end);
            last.2.extend(indices);
        } else {
            merged.push((start, end, indices));
        }
    }
    let mut positioned = Vec::new();
    for (start, end, mut indices) in merged {
        // Preserve shaper order within a cluster, including mark placement and fallback offsets.
        indices.sort_unstable();
        let left = indices
            .iter()
            .map(|&i| glyphs[i].x)
            .fold(f32::INFINITY, f32::min);
        let right = indices
            .iter()
            .map(|&i| glyphs[i].x + glyphs[i].w)
            .fold(f32::NEG_INFINITY, f32::max);
        let padding = (((end - start) as f32 * grid.cell_width - (right - left)) / 2.0).max(0.0);
        let shift = start as f32 * grid.cell_width + padding - left;
        for i in indices {
            glyphs[i].x += shift;
            positioned.push(GridGlyph {
                layout: glyphs[i].clone(),
                start_column: start,
                end_column: end,
            });
        }
    }
    ShapedRun {
        glyphs: positioned,
        cells: run.cells,
    }
}
/// Compose in a temporary row. Whole intersected wide cells are replaced; PTY state is untouched.
pub(super) fn compose_preedit(
    cells: &[TerminalCellPatch],
    columns: usize,
    column: usize,
    text: &str,
    foreground: RgbColor,
) -> (Vec<TerminalCellPatch>, usize) {
    let mut overlay = Vec::new();
    let mut end = column;
    for grapheme in UnicodeSegmentation::graphemes(text, true) {
        if grapheme.chars().any(char::is_control) {
            continue;
        }
        let width = UnicodeWidthStr::width(grapheme).max(1);
        if end + width > columns {
            break;
        }
        let mut chars = grapheme.chars();
        let Some(character) = chars.next() else {
            continue;
        };
        let marks: Vec<_> = chars.collect();
        overlay.push(TerminalCellPatch {
            character,
            zerowidth: (!marks.is_empty()).then(|| marks.into_boxed_slice()),
            foreground,
            background: foreground,
            column: end,
            width_in_columns: width,
            bold: false,
            italic: false,
            underline_style: 0,
            strikeout: false,
            hidden: false,
        });
        end += width;
    }
    let mut result: Vec<_> = cells
        .iter()
        .filter(|c| end == column || c.column + c.width_in_columns <= column || c.column >= end)
        .cloned()
        .collect();
    result.extend(overlay);
    result.sort_by_key(|c| c.column);
    (result, end - column)
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

/// 以物理像素字号测量主字体，得到整数单元尺寸：宽度取平均 advance，高度取
/// ascent + descent + line gap。与终端渲染器共用同一字体库，避免 UI 与渲染器各算一套。
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
            height: line_height.unwrap_or(font_size * 1.2).round().max(1.0) as u32,
        };
        cache.values.insert(key, result);
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cell(column: usize, character: char) -> TerminalCellPatch {
        TerminalCellPatch {
            column,
            character,
            zerowidth: None,
            width_in_columns: 1,
            foreground: RgbColor {
                red: 255,
                green: 255,
                blue: 255,
            },
            background: RgbColor {
                red: 0,
                green: 0,
                blue: 0,
            },
            bold: false,
            italic: false,
            hidden: false,
            underline_style: 0,
            strikeout: false,
        }
    }
    #[test]
    fn ascii_positions_do_not_accumulate_font_advance_at_any_scale() {
        let mut fs = super::super::fonts::font_system();
        for family in ["Maple Mono NF CN", "Cascadia Mono", "Consolas"] {
            for size in [13.0, 16.25, 19.5, 22.75, 26.0, 27.0] {
                let m = measure_cell(family, size);
                let grid = GridTextMetrics::new(
                    Metrics::new(size, m.height as f32),
                    m.width as f32,
                    m.height as f32,
                    family,
                );
                let cells: Vec<_> = (0..200).map(|i| cell(i, 'd')).collect();
                let runs = create_row_buffers(&mut fs, grid, 200, &cells);
                let glyphs = &runs[0].glyphs;
                assert_eq!(glyphs.len(), 200);
                for (i, g) in glyphs.iter().enumerate() {
                    assert!(
                        (g.layout.x - glyphs[0].layout.x - i as f32 * m.width as f32).abs() < 0.002,
                        "{family} {size} col={i}"
                    );
                    assert_eq!((g.start_column, g.end_column), (i, i + 1));
                }
            }
        }
    }
    #[test]
    fn unicode_and_combining_cells_share_context_and_keep_column_mapping() {
        let mut cells = vec![cell(0, 'a'), cell(1, '中'), cell(3, 'e'), cell(4, 'z')];
        cells[1].width_in_columns = 2;
        cells[2].zerowidth = Some(vec!['\u{301}'].into_boxed_slice());
        let mut fs = super::super::fonts::font_system();
        let runs = create_row_buffers(
            &mut fs,
            GridTextMetrics::new(Metrics::new(26.0, 36.0), 16.0, 36.0, "Maple Mono NF CN"),
            5,
            &cells,
        );
        assert_eq!(runs.len(), 1);
        assert!(
            runs[0]
                .glyphs
                .iter()
                .any(|g| (g.start_column, g.end_column) == (1, 3))
        );
        assert!(
            runs[0]
                .glyphs
                .iter()
                .any(|g| (g.start_column, g.end_column) == (3, 4))
        );
        assert!(runs[0].glyphs.iter().all(|g| g.end_column <= 5));
    }
    #[test]
    fn paint_does_not_split_context_but_font_style_does() {
        let mut cells = vec![cell(0, '!'), cell(1, '='), cell(2, 'x')];
        cells[1].foreground.red = 0;
        assert_eq!(build_text_runs(3, &cells).len(), 1);
        cells[2].bold = true;
        assert_eq!(build_text_runs(3, &cells).len(), 2);
    }
    #[test]
    fn background_changes_preserve_glyph_content() {
        let before = vec![cell(0, 'a')];
        let mut after = before.clone();
        after[0].background.red = 99;
        after[0].strikeout = true;
        assert!(same_glyph_content(1, &before, &after));
        after[0].character = 'b';
        assert!(!same_glyph_content(1, &before, &after));
    }
    #[test]
    fn composition_replaces_intersected_wide_cells_without_mutating_terminal() {
        let mut cells = vec![cell(0, 'a'), cell(1, '中'), cell(3, 'b'), cell(4, 'c')];
        cells[1].width_in_columns = 2;
        let original = cells.clone();
        let (composed, width) = compose_preedit(&cells, 5, 2, "XY", cells[0].foreground);
        assert_eq!(width, 2);
        assert_eq!(
            composed.iter().map(|c| c.character).collect::<String>(),
            "aXYc"
        );
        assert_eq!(cells, original);
        assert_eq!(
            compose_preedit(&cells, 5, 2, "", cells[0].foreground).0,
            original
        );
    }
    #[test]
    fn composition_never_squeezes_a_wide_grapheme_into_the_last_column() {
        let cells = vec![cell(0, 'a'), cell(1, 'b')];
        let (composed, width) = compose_preedit(&cells, 2, 1, "中", cells[0].foreground);
        assert_eq!(width, 0);
        assert_eq!(composed, cells);
    }
    #[test]
    fn composition_tracks_grapheme_widths() {
        let fg = cell(0, 'a').foreground;
        let (cells, width) = compose_preedit(&[], 10, 0, "A中e\u{301}", fg);
        assert_eq!(width, 4);
        assert_eq!(
            cells.iter().map(|c| c.column).collect::<Vec<_>>(),
            vec![0, 1, 3]
        );
    }
    #[test]
    fn ligature_clusters_keep_their_source_columns_and_paint() {
        // This font is optional on CI; exercise its real ligatures when installed.
        if !super::super::fonts::monospace_families().any(|f| f == "Maple Mono NF CN") {
            return;
        }
        let mut fs = super::super::fonts::font_system();
        let mut cells = vec![cell(0, '!'), cell(1, '='), cell(2, 'd')];
        cells[1].foreground.red = 0;
        let runs = create_row_buffers(
            &mut fs,
            GridTextMetrics::new(Metrics::new(26.0, 36.0), 16.0, 36.0, "Maple Mono NF CN"),
            3,
            &cells,
        );
        let grid = GridTextMetrics::new(Metrics::new(26.0, 36.0), 16.0, 36.0, "Maple Mono NF CN");
        let mut isolated = cells.clone();
        isolated[1].column = 2;
        isolated[2].column = 4;
        let isolated = create_row_buffers(&mut fs, grid, 5, &isolated);
        let ids = |runs: &[ShapedRun]| {
            runs.iter()
                .flat_map(|r| r.glyphs.iter().map(|g| g.layout.glyph_id))
                .collect::<Vec<_>>()
        };
        assert_ne!(
            ids(&runs),
            ids(&isolated),
            "contextual ligature substitutions must be retained"
        );
        assert!(
            runs[0]
                .glyphs
                .iter()
                .all(|g| g.start_column < g.end_column && g.end_column <= 3)
        );
        assert_ne!(runs[0].cells[0].foreground, runs[0].cells[1].foreground);
    }
}
