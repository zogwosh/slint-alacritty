//! 将 Alacritty 的终端网格提取为渲染器可消费的增量帧。

use super::{
    palette::{RgbColor, TerminalTheme, resolve_color},
    search::{SearchHighlight, SearchSnapshot},
};
use alacritty_terminal::{
    event::EventListener,
    grid::Dimensions,
    index::{Column, Line, Point},
    selection::SelectionRange,
    term::{Term, TermDamage, cell::Flags},
    vte::ansi::CursorShape,
};

/// 一个可见字符单元的完整绘制信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalCellPatch {
    pub(crate) text: String,
    pub(crate) foreground: RgbColor,
    pub(crate) background: RgbColor,
    pub(crate) column: usize,
    /// 普通字符为 1，宽字符为 2；占位单元不会单独生成补丁。
    pub(crate) width_in_columns: usize,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline_style: i32,
    pub(crate) strikeout: bool,
    pub(crate) hidden: bool,
}

/// 某一行发生变化的列区间；end_column 为开区间。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowPatch {
    pub(crate) row: usize,
    pub(crate) start_column: usize,
    pub(crate) end_column: usize,
    pub(crate) cells: Vec<TerminalCellPatch>,
}

#[derive(Clone, Copy)]
struct DamagedSpan {
    row: usize,
    start_column: usize,
    end_column: usize,
}

/// 光标位置与样式的轻量快照。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CursorPatch {
    pub(crate) column: usize,
    pub(crate) row: usize,
    pub(crate) visible: bool,
    pub(crate) shape: i32,
    pub(crate) blinking: bool,
}

/// 后台提交给渲染器的一帧增量数据。
#[derive(Debug)]
pub(crate) struct FramePatch {
    /// 尺寸变化时递增，用于阻止不同网格世代的补丁相互合并。
    pub(crate) generation: u64,
    pub(crate) columns: usize,
    pub(crate) rows: usize,
    pub(crate) full_redraw: bool,
    pub(crate) full_redraw_reason: Option<FullRedrawReason>,
    /// 仅包含受损行；完整重绘时应覆盖所有行。
    pub(crate) changed_rows: Vec<RowPatch>,
    pub(crate) cursor: CursorPatch,
    pub(crate) scroll_offset: usize,
    pub(crate) scroll_history_lines: usize,
    pub(crate) search: SearchSnapshot,
    pub(crate) title: Option<String>,
    pub(crate) exit_message: Option<String>,
}

/// 触发完整重绘的来源，主要用于正确性判断与性能统计。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FullRedrawReason {
    TerminalDamage,
    RendererRequest,
    Resize,
}

/// 读取 Alacritty 的 damage 信息并捕获一帧；函数末尾会清除已消费的 damage。
#[allow(clippy::too_many_arguments)]
pub(super) fn capture_frame<T: EventListener>(
    terminal: &mut Term<T>,
    columns: usize,
    rows: usize,
    generation: u64,
    forced_full_redraw: Option<FullRedrawReason>,
    extra_dirty_rows: &[usize],
    theme: TerminalTheme,
    search: SearchSnapshot,
) -> FramePatch {
    // 光标位置与当前输入行在视觉上是一个不可拆分的状态。部分 shell 在启动提示符
    // 输出期间只留下最终光标 damage；若只复制该列，光标会出现而提示符要等选择后才显示。
    let cursor_point = terminal.grid().cursor.point;
    let cursor_damage_row = (cursor_point.line.0 >= 0)
        .then_some(cursor_point.line.0 as usize)
        .filter(|row| *row < rows);

    // 外部强制重绘优先于 Alacritty 自己报告的局部 damage。
    let full_spans = || {
        (0..rows)
            .map(|row| DamagedSpan {
                row,
                start_column: 0,
                end_column: columns,
            })
            .collect::<Vec<_>>()
    };
    let (full_redraw_reason, mut damaged_spans) = if let Some(reason) = forced_full_redraw {
        (Some(reason), full_spans())
    } else {
        match terminal.damage() {
            TermDamage::Full => (Some(FullRedrawReason::TerminalDamage), full_spans()),
            TermDamage::Partial(lines) => {
                let spans = lines
                    .filter_map(|damage| {
                        (damage.line < rows && damage.left < columns).then_some(DamagedSpan {
                            row: damage.line,
                            // 扩一列以覆盖宽字符从/向 damage 边界跨越的情况。
                            start_column: damage.left.saturating_sub(1),
                            end_column: damage.right.saturating_add(2).min(columns),
                        })
                    })
                    .collect::<Vec<_>>();
                (None, spans)
            }
        }
    };
    if full_redraw_reason.is_none()
        && let Some(row) = cursor_damage_row
    {
        if let Some(span) = damaged_spans.iter_mut().find(|span| span.row == row) {
            span.start_column = 0;
            span.end_column = columns;
        } else {
            damaged_spans.push(DamagedSpan {
                row,
                start_column: 0,
                end_column: columns,
            });
        }
    }
    for row in extra_dirty_rows.iter().copied().filter(|row| *row < rows) {
        if let Some(span) = damaged_spans.iter_mut().find(|span| span.row == row) {
            span.start_column = 0;
            span.end_column = columns;
        } else {
            damaged_spans.push(DamagedSpan {
                row,
                start_column: 0,
                end_column: columns,
            });
        }
    }
    damaged_spans.sort_unstable_by_key(|span| span.row);
    let full_redraw = full_redraw_reason.is_some();

    // renderable_content 会把滚动偏移、光标和选择区整理为当前视口快照。
    let cursor_blinking = terminal.cursor_style().blinking;
    let content = terminal.renderable_content();
    let display_offset = content.display_offset as i32;
    let scroll_offset = content.display_offset;
    let scroll_history_lines = terminal
        .total_lines()
        .saturating_sub(terminal.screen_lines());
    let cursor_row = content.cursor.point.line.0 + display_offset;
    let cursor = CursorPatch {
        column: content.cursor.point.column.0,
        row: cursor_row.max(0) as usize,
        visible: content.cursor.shape != CursorShape::Hidden
            && cursor_row >= 0
            && cursor_row < rows as i32,
        shape: cursor_shape(content.cursor.shape),
        blinking: cursor_blinking,
    };
    let colors = content.colors;
    let selection = content.selection;
    let grid = terminal.grid();

    let changed_rows = damaged_spans
        .into_iter()
        .map(|span| {
            let row = span.row;
            let grid_line = Line(row as i32 - display_offset);
            let mut cells = Vec::with_capacity(span.end_column - span.start_column);

            for column in span.start_column..span.end_column {
                let cell = &grid[grid_line][Column(column)];
                // 宽字符的第二格只是占位符，由前一格的 width_in_columns 覆盖。
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }

                let mut foreground = resolve_color(cell.fg, colors, true, theme);
                let mut background = resolve_color(cell.bg, colors, false, theme);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut foreground, &mut background);
                }
                if cell.flags.contains(Flags::DIM) {
                    foreground = dim_color(foreground);
                }
                if selection_contains_cell(selection, grid_line, column, cell.flags) {
                    foreground = theme.selection_foreground;
                    background = theme.selection_background;
                }
                let point = Point::new(grid_line, Column(column));
                let search_highlight = search.highlight_at(point).or_else(|| {
                    cell.flags
                        .contains(Flags::WIDE_CHAR)
                        .then(|| search.highlight_at(Point::new(grid_line, Column(column + 1))))
                        .flatten()
                });
                match search_highlight {
                    Some(SearchHighlight::Match) => {
                        background = theme.search_match_background;
                    }
                    Some(SearchHighlight::Current) => {
                        background = theme.search_current_background;
                    }
                    None => {}
                }

                let hidden = cell.flags.contains(Flags::HIDDEN);
                let width_in_columns = cell_width_in_columns(cell.flags);
                let underline_style = underline_style(cell.flags);
                let mut text = String::new();
                if !hidden && !cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                    text.push(cell.c);
                    if let Some(zerowidth) = cell.zerowidth() {
                        text.extend(zerowidth);
                    }
                }

                cells.push(TerminalCellPatch {
                    text,
                    foreground,
                    background,
                    column,
                    width_in_columns,
                    bold: cell.flags.contains(Flags::BOLD),
                    italic: cell.flags.contains(Flags::ITALIC),
                    underline_style,
                    strikeout: cell.flags.contains(Flags::STRIKEOUT),
                    hidden,
                });
            }

            RowPatch {
                row,
                start_column: span.start_column,
                end_column: span.end_column,
                cells,
            }
        })
        .collect();

    terminal.reset_damage();

    FramePatch {
        generation,
        columns,
        rows,
        full_redraw,
        full_redraw_reason,
        changed_rows,
        cursor,
        scroll_offset,
        scroll_history_lines,
        search,
        title: None,
        exit_message: None,
    }
}

fn dim_color(color: RgbColor) -> RgbColor {
    RgbColor {
        red: (u16::from(color.red) * 2 / 3) as u8,
        green: (u16::from(color.green) * 2 / 3) as u8,
        blue: (u16::from(color.blue) * 2 / 3) as u8,
    }
}

fn cell_width_in_columns(flags: Flags) -> usize {
    if flags.contains(Flags::WIDE_CHAR) {
        2
    } else {
        1
    }
}

/// 判断选择区是否覆盖该单元；宽字符任一半被选中都应高亮整个字形。
fn selection_contains_cell(
    selection: Option<SelectionRange>,
    line: Line,
    column: usize,
    flags: Flags,
) -> bool {
    selection.is_some_and(|range| {
        range.contains(Point::new(line, Column(column)))
            || (flags.contains(Flags::WIDE_CHAR)
                && range.contains(Point::new(line, Column(column + 1))))
    })
}

/// 把 Alacritty 光标枚举转换为 GPU 着色器约定的整数编码。
fn cursor_shape(shape: CursorShape) -> i32 {
    match shape {
        CursorShape::Block => 0,
        CursorShape::Underline => 1,
        CursorShape::Beam => 2,
        CursorShape::HollowBlock => 3,
        CursorShape::Hidden => 0,
    }
}

/// 把互斥的下划线标志转换为 GPU 着色器使用的样式编号。
fn underline_style(flags: Flags) -> i32 {
    if flags.contains(Flags::DOUBLE_UNDERLINE) {
        2
    } else if flags.contains(Flags::UNDERCURL) {
        3
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        4
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        5
    } else if flags.contains(Flags::UNDERLINE) {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RgbColor, cell_width_in_columns, cursor_shape, dim_color, selection_contains_cell,
        underline_style,
    };
    use alacritty_terminal::{
        index::{Column, Line, Point},
        selection::SelectionRange,
        term::cell::Flags,
        vte::ansi::CursorShape,
    };

    #[test]
    fn dim_reduces_each_color_channel() {
        assert_eq!(
            dim_color(RgbColor {
                red: 150,
                green: 90,
                blue: 30,
            }),
            RgbColor {
                red: 100,
                green: 60,
                blue: 20,
            }
        );
    }

    #[test]
    fn preserves_cursor_shapes() {
        assert_eq!(cursor_shape(CursorShape::Block), 0);
        assert_eq!(cursor_shape(CursorShape::Underline), 1);
        assert_eq!(cursor_shape(CursorShape::Beam), 2);
        assert_eq!(cursor_shape(CursorShape::HollowBlock), 3);
    }

    #[test]
    fn preserves_extended_underline_styles() {
        assert_eq!(underline_style(Flags::UNDERLINE), 1);
        assert_eq!(underline_style(Flags::DOUBLE_UNDERLINE), 2);
        assert_eq!(underline_style(Flags::UNDERCURL), 3);
        assert_eq!(underline_style(Flags::DOTTED_UNDERLINE), 4);
        assert_eq!(underline_style(Flags::DASHED_UNDERLINE), 5);
    }

    #[test]
    fn wide_cells_occupy_two_columns() {
        assert_eq!(cell_width_in_columns(Flags::empty()), 1);
        assert_eq!(cell_width_in_columns(Flags::WIDE_CHAR), 2);
    }

    #[test]
    fn selecting_a_wide_character_spacer_selects_its_glyph_cell() {
        let selection = SelectionRange::new(
            Point::new(Line(0), Column(4)),
            Point::new(Line(0), Column(4)),
            false,
        );

        assert!(selection_contains_cell(
            Some(selection),
            Line(0),
            3,
            Flags::WIDE_CHAR,
        ));
    }
}
