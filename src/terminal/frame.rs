use super::palette::{RgbColor, resolve_color};
use alacritty_terminal::{
    event::EventListener,
    index::{Column, Line, Point},
    selection::SelectionRange,
    term::{Term, TermDamage, cell::Flags},
    vte::ansi::CursorShape,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalCellPatch {
    pub(crate) text: String,
    pub(crate) foreground: RgbColor,
    pub(crate) background: RgbColor,
    pub(crate) column: usize,
    pub(crate) width_in_columns: usize,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline_style: i32,
    pub(crate) strikeout: bool,
    pub(crate) hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowPatch {
    pub(crate) row: usize,
    pub(crate) cells: Vec<TerminalCellPatch>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CursorPatch {
    pub(crate) column: usize,
    pub(crate) row: usize,
    pub(crate) visible: bool,
    pub(crate) shape: i32,
    pub(crate) blinking: bool,
}

#[derive(Debug)]
pub(crate) struct FramePatch {
    pub(crate) generation: u64,
    pub(crate) columns: usize,
    pub(crate) rows: usize,
    pub(crate) full_redraw: bool,
    pub(crate) full_redraw_reason: Option<FullRedrawReason>,
    pub(crate) changed_rows: Vec<RowPatch>,
    pub(crate) cursor: CursorPatch,
    pub(crate) title: Option<String>,
    pub(crate) exit_message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FullRedrawReason {
    TerminalDamage,
    RendererRequest,
    Resize,
}

pub(super) fn capture_frame<T: EventListener>(
    terminal: &mut Term<T>,
    columns: usize,
    rows: usize,
    generation: u64,
    forced_full_redraw: Option<FullRedrawReason>,
    extra_dirty_rows: &[usize],
) -> FramePatch {
    let (full_redraw_reason, mut damaged_rows) = if let Some(reason) = forced_full_redraw {
        (Some(reason), (0..rows).collect())
    } else {
        match terminal.damage() {
            TermDamage::Full => (Some(FullRedrawReason::TerminalDamage), (0..rows).collect()),
            TermDamage::Partial(lines) => {
                let mut rows = lines
                    .filter_map(|damage| (damage.line < rows).then_some(damage.line))
                    .collect::<Vec<_>>();
                rows.sort_unstable();
                rows.dedup();
                (None, rows)
            }
        }
    };
    damaged_rows.extend(extra_dirty_rows.iter().copied().filter(|row| *row < rows));
    damaged_rows.sort_unstable();
    damaged_rows.dedup();
    let full_redraw = full_redraw_reason.is_some();

    let cursor_blinking = terminal.cursor_style().blinking;
    let content = terminal.renderable_content();
    let display_offset = content.display_offset as i32;
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

    let changed_rows = damaged_rows
        .into_iter()
        .map(|row| {
            let grid_line = Line(row as i32 - display_offset);
            let mut cells = Vec::with_capacity(columns);

            for column in 0..columns {
                let cell = &grid[grid_line][Column(column)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }

                let mut foreground = resolve_color(cell.fg, colors, true);
                let mut background = resolve_color(cell.bg, colors, false);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut foreground, &mut background);
                }
                if cell.flags.contains(Flags::DIM) {
                    foreground = dim_color(foreground);
                }
                if selection_contains_cell(selection, grid_line, column, cell.flags) {
                    foreground = RgbColor {
                        red: 0xe6,
                        green: 0xed,
                        blue: 0xf3,
                    };
                    background = RgbColor {
                        red: 0x26,
                        green: 0x4f,
                        blue: 0x78,
                    };
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

            RowPatch { row, cells }
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

fn cursor_shape(shape: CursorShape) -> i32 {
    match shape {
        CursorShape::Block => 0,
        CursorShape::Underline => 1,
        CursorShape::Beam => 2,
        CursorShape::HollowBlock => 3,
        CursorShape::Hidden => 0,
    }
}

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
