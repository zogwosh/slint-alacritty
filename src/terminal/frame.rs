//! 将 Alacritty 的终端网格提取为渲染器可消费的增量帧。

use super::{
    palette::{RgbColor, TerminalTheme, resolve_color},
    search::{SearchHighlight, SearchSnapshot},
};
use alacritty_terminal::{
    event::EventListener,
    grid::Dimensions,
    index::{Column, Line, Point},
    term::{
        Term, TermDamage,
        cell::{Cell, Flags},
        search::Match,
    },
    vte::ansi::CursorShape,
};

/// 一个可见字符单元的完整绘制信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalCellPatch {
    /// 单元的基字符；空白单元为 ' '。绝大多数单元只有这一个字符，避免逐格堆分配。
    pub(crate) character: char,
    /// 附着在基字符上的零宽字符（组合标记等），极少出现时才分配。
    pub(crate) zerowidth: Option<Box<[char]>>,
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

impl TerminalCellPatch {
    /// 不会产生任何字形的单元。
    pub(crate) fn is_blank(&self) -> bool {
        self.character == ' ' && self.zerowidth.is_none()
    }
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

/// 装饰层的类型；只决定绘制颜色，不影响单元格文字与字体属性。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecorationKind {
    Selection,
    SearchMatch,
    SearchCurrent,
}

/// 视口内某一行上的一段装饰背景；end_column 为开区间。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DecorationRange {
    pub(crate) row: usize,
    pub(crate) start_column: usize,
    pub(crate) end_column: usize,
    pub(crate) kind: DecorationKind,
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
    /// 后台会话产出的轻量帧：只有 title/exit_message 有效，视口相关字段不得写入 UI。
    pub(crate) metadata_only: bool,
    /// 仅包含受损行；完整重绘时应覆盖所有行。
    pub(crate) changed_rows: Vec<RowPatch>,
    pub(crate) cursor: CursorPatch,
    pub(crate) scroll_offset: usize,
    pub(crate) scroll_history_lines: usize,
    pub(crate) search: SearchSnapshot,
    /// 当前视口内全部选区与搜索高亮；每帧完整给出，按行排序。
    pub(crate) decorations: Vec<DecorationRange>,
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
///
/// 选区与搜索高亮不会写进单元格，而是转换为独立的装饰区间，
/// 因此它们的变化不会触发任何行的重新 shaping。
#[allow(clippy::too_many_arguments)]
pub(super) fn capture_frame<T: EventListener>(
    terminal: &mut Term<T>,
    columns: usize,
    rows: usize,
    generation: u64,
    forced_full_redraw: Option<FullRedrawReason>,
    theme: TerminalTheme,
    search: SearchSnapshot,
    search_matches: &[(Match, SearchHighlight)],
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
                        // right 为闭区间；边界落在宽字符上的情况随后按 grid 标志扩展。
                        (damage.line < rows && damage.left < columns).then_some(DamagedSpan {
                            row: damage.line,
                            start_column: damage.left,
                            end_column: damage.right.saturating_add(1).min(columns),
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
        .filter(|span| span.start_column < span.end_column)
        .map(|span| {
            let row = span.row;
            let grid_line = Line(row as i32 - display_offset);
            // 补丁区间绝不能从占位格开始或在宽字符首格结束，否则合并缓存时会丢失该字形。
            let (start_column, end_column) = expand_to_wide_cells(
                |column| &grid[grid_line][Column(column)],
                span.start_column,
                span.end_column - 1,
                columns,
            );
            let mut cells = Vec::with_capacity(end_column - start_column);

            for column in start_column..end_column {
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

                let hidden = cell.flags.contains(Flags::HIDDEN);
                let width_in_columns = cell_width_in_columns(cell.flags);
                let underline_style = underline_style(cell.flags);
                let (character, zerowidth) =
                    if hidden || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                        (' ', None)
                    } else {
                        (
                            cell.c,
                            cell.zerowidth()
                                .filter(|zerowidth| !zerowidth.is_empty())
                                .map(Box::from),
                        )
                    };

                cells.push(TerminalCellPatch {
                    character,
                    zerowidth,
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
                start_column,
                end_column,
                cells,
            }
        })
        .collect();

    // 视口在网格坐标中的行范围；装饰只需要覆盖这些行。
    let visible_lines = (
        Line(-display_offset),
        Line(rows as i32 - 1 - display_offset),
    );
    let mut decorations = Vec::new();
    let mut push_range = |line: Line, start: usize, end_inclusive: usize, kind| {
        let row = line.0 + display_offset;
        if row < 0 || row >= rows as i32 {
            return;
        }
        let (start_column, end_column) = expand_to_wide_cells(
            |column| &grid[line][Column(column)],
            start,
            end_inclusive,
            columns,
        );
        if start_column < end_column {
            decorations.push(DecorationRange {
                row: row as usize,
                start_column,
                end_column,
                kind,
            });
        }
    };
    if let Some(range) = selection {
        for_each_row_span(
            range.start,
            range.end,
            range.is_block,
            columns,
            visible_lines,
            |line, start, end| push_range(line, start, end, DecorationKind::Selection),
        );
    }
    for (range, highlight) in search_matches {
        let kind = match highlight {
            SearchHighlight::Match => DecorationKind::SearchMatch,
            SearchHighlight::Current => DecorationKind::SearchCurrent,
        };
        for_each_row_span(
            *range.start(),
            *range.end(),
            false,
            columns,
            visible_lines,
            |line, start, end| push_range(line, start, end, kind),
        );
    }
    decorations.sort_by_key(|decoration| decoration.row);

    terminal.reset_damage();

    FramePatch {
        generation,
        columns,
        rows,
        full_redraw,
        full_redraw_reason,
        metadata_only: false,
        changed_rows,
        cursor,
        scroll_offset,
        scroll_history_lines,
        search,
        decorations,
        title: None,
        exit_message: None,
    }
}

/// 把跨行区间拆成逐行的闭区间列，只访问 visible 范围内的行。块选区每行都使用相同的列范围。
fn for_each_row_span(
    start: Point,
    end: Point,
    block: bool,
    columns: usize,
    visible: (Line, Line),
    mut visit: impl FnMut(Line, usize, usize),
) {
    if columns == 0 || end.line < start.line {
        return;
    }
    let last_column = columns - 1;
    let first_line = start.line.max(visible.0);
    let last_line = end.line.min(visible.1);
    for line in first_line.0..=last_line.0 {
        let line = Line(line);
        let (first, last) = if block {
            (start.column.0, end.column.0)
        } else {
            (
                if line == start.line {
                    start.column.0
                } else {
                    0
                },
                if line == end.line {
                    end.column.0
                } else {
                    last_column
                },
            )
        };
        let first = first.min(last_column);
        let last = last.min(last_column);
        if first <= last {
            visit(line, first, last);
        }
    }
}

/// 装饰矩形不能只覆盖宽字符的一半：起点落在占位格时左移，终点落在宽字符首格时右扩。
fn expand_to_wide_cells<'a>(
    cell_at: impl Fn(usize) -> &'a Cell,
    start: usize,
    end_inclusive: usize,
    columns: usize,
) -> (usize, usize) {
    let mut start = start.min(columns.saturating_sub(1));
    let mut end = end_inclusive.min(columns.saturating_sub(1));
    if start > 0 && cell_at(start).flags.contains(Flags::WIDE_CHAR_SPACER) {
        start -= 1;
    }
    if cell_at(end).flags.contains(Flags::WIDE_CHAR) {
        end = (end + 1).min(columns.saturating_sub(1));
    }
    (start, (end + 1).min(columns))
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
        RgbColor, cell_width_in_columns, cursor_shape, dim_color, expand_to_wide_cells,
        for_each_row_span, underline_style,
    };
    use alacritty_terminal::{
        index::{Column, Line, Point},
        term::cell::{Cell, Flags},
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
    fn decoration_ranges_cover_both_halves_of_a_wide_character() {
        let mut row: Vec<Cell> = (0..6).map(|_| Cell::default()).collect();
        row[3].flags.insert(Flags::WIDE_CHAR);
        row[4].flags.insert(Flags::WIDE_CHAR_SPACER);

        // 只选中占位格，也要从字形所在列开始绘制。
        assert_eq!(expand_to_wide_cells(|column| &row[column], 4, 4, 6), (3, 5));
        // 只选中首格，也要覆盖占位格。
        assert_eq!(expand_to_wide_cells(|column| &row[column], 1, 3, 6), (1, 5));
        assert_eq!(expand_to_wide_cells(|column| &row[column], 0, 1, 6), (0, 2));
    }

    #[test]
    fn multi_line_ranges_are_split_per_row() {
        let visible = (Line(-5), Line(5));
        let mut spans = Vec::new();
        for_each_row_span(
            Point::new(Line(-1), Column(5)),
            Point::new(Line(1), Column(2)),
            false,
            10,
            visible,
            |line, start, end| spans.push((line.0, start, end)),
        );
        assert_eq!(spans, vec![(-1, 5, 9), (0, 0, 9), (1, 0, 2)]);

        let mut block = Vec::new();
        for_each_row_span(
            Point::new(Line(0), Column(3)),
            Point::new(Line(1), Column(5)),
            true,
            10,
            visible,
            |line, start, end| block.push((line.0, start, end)),
        );
        assert_eq!(block, vec![(0, 3, 5), (1, 3, 5)]);
    }

    #[test]
    fn rows_outside_the_viewport_are_never_visited() {
        let mut spans = Vec::new();
        for_each_row_span(
            Point::new(Line(-1000), Column(0)),
            Point::new(Line(1000), Column(3)),
            false,
            10,
            (Line(-1), Line(1)),
            |line, start, end| spans.push((line.0, start, end)),
        );
        assert_eq!(spans, vec![(-1, 0, 9), (0, 0, 9), (1, 0, 9)]);
    }
}
