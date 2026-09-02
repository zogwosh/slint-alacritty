//! 鼠标坐标换算、滚轮归一化以及终端鼠标协议编码。

use super::command::{MouseAction, MouseButton, TerminalSize};
use alacritty_terminal::{
    event::EventListener,
    index::{Column, Line, Point},
    term::{Term, TermMode},
};

/// 把视口中的行列坐标换算为包含滚动历史偏移的终端网格坐标。
pub(super) fn visible_point<T: EventListener>(
    terminal: &Term<T>,
    size: TerminalSize,
    column: usize,
    row: usize,
) -> Point {
    let display_offset = terminal.grid().display_offset() as i32;
    Point::new(
        Line(row.min(size.rows.saturating_sub(1)) as i32 - display_offset),
        Column(column.min(size.columns.saturating_sub(1))),
    )
}

/// 累计高精度滚轮输入，只在跨过完整字符行时产生滚动。
pub(super) fn accumulate_scroll_lines(remainder: &mut f32, lines: f32) -> i32 {
    if !lines.is_finite() || lines == 0.0 {
        return 0;
    }

    let accumulated = (*remainder + lines).clamp(-20.0, 20.0);
    let whole_lines = accumulated.trunc() as i32;
    *remainder = accumulated - whole_lines as f32;
    whole_lines
}

/// 按当前终端模式生成 SGR、UTF-8 或传统 X10 鼠标报告。
#[allow(clippy::too_many_arguments)]
pub(super) fn encode_mouse_report(
    button: MouseButton,
    action: MouseAction,
    column: usize,
    row: usize,
    shift: bool,
    alt: bool,
    control: bool,
    mode: TermMode,
) -> Vec<u8> {
    let Some(mut button_code) = (match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Other => None,
    }) else {
        return Vec::new();
    };

    let release = action == MouseAction::Release;
    if release && !mode.contains(TermMode::SGR_MOUSE) {
        button_code = 3;
    } else if action == MouseAction::Move {
        button_code += 32;
    }

    encode_mouse_button_code(button_code, column, row, shift, alt, control, mode, release)
}

/// 为已经确定的按钮编号添加修饰键和坐标编码。
#[allow(clippy::too_many_arguments)]
pub(super) fn encode_mouse_button_code(
    button_code: u8,
    column: usize,
    row: usize,
    shift: bool,
    alt: bool,
    control: bool,
    mode: TermMode,
    release: bool,
) -> Vec<u8> {
    let modifiers = u8::from(shift) * 4 + u8::from(alt) * 8 + u8::from(control) * 16;
    let button_code = button_code.saturating_add(modifiers);
    let column = column.saturating_add(1);
    let row = row.saturating_add(1);

    if mode.contains(TermMode::SGR_MOUSE) {
        let suffix = if release { 'm' } else { 'M' };
        return format!("\x1b[<{button_code};{column};{row}{suffix}").into_bytes();
    }

    let values = [
        u32::from(button_code) + 32,
        column.min(223) as u32 + 32,
        row.min(223) as u32 + 32,
    ];
    if mode.contains(TermMode::UTF8_MOUSE) {
        let mut encoded = b"\x1b[M".to_vec();
        for value in values {
            encoded.extend(
                char::from_u32(value)
                    .unwrap_or('\u{fffd}')
                    .to_string()
                    .as_bytes(),
            );
        }
        encoded
    } else {
        let mut encoded = b"\x1b[M".to_vec();
        encoded.extend(values.map(|value| value as u8));
        encoded
    }
}

#[cfg(test)]
mod tests {
    use super::{accumulate_scroll_lines, encode_mouse_report};
    use crate::terminal::command::{MouseAction, MouseButton};
    use alacritty_terminal::term::TermMode;

    #[test]
    fn sgr_mouse_encodes_press_release_and_modifiers() {
        let mode = TermMode::SGR_MOUSE;
        assert_eq!(
            encode_mouse_report(
                MouseButton::Left,
                MouseAction::Press,
                4,
                2,
                false,
                false,
                true,
                mode,
            ),
            b"\x1b[<16;5;3M"
        );
        assert_eq!(
            encode_mouse_report(
                MouseButton::Left,
                MouseAction::Release,
                4,
                2,
                false,
                false,
                false,
                mode,
            ),
            b"\x1b[<0;5;3m"
        );
    }

    #[test]
    fn legacy_mouse_encodes_motion_and_release() {
        assert_eq!(
            encode_mouse_report(
                MouseButton::Right,
                MouseAction::Move,
                0,
                0,
                false,
                false,
                false,
                TermMode::empty(),
            ),
            b"\x1b[MB!!"
        );
        assert_eq!(
            encode_mouse_report(
                MouseButton::Left,
                MouseAction::Release,
                0,
                0,
                false,
                false,
                false,
                TermMode::empty(),
            ),
            b"\x1b[M#!!"
        );
    }

    #[test]
    fn wheel_delta_accumulates_until_a_whole_line_and_is_bounded() {
        let mut remainder = 0.0;
        assert_eq!(accumulate_scroll_lines(&mut remainder, 0.2), 0);
        assert_eq!(accumulate_scroll_lines(&mut remainder, 0.7), 0);
        assert_eq!(accumulate_scroll_lines(&mut remainder, 0.2), 1);
        assert!((remainder - 0.1).abs() < f32::EPSILON * 4.0);
        assert_eq!(accumulate_scroll_lines(&mut remainder, -0.4), 0);
        assert_eq!(accumulate_scroll_lines(&mut remainder, -0.8), -1);
        assert_eq!(accumulate_scroll_lines(&mut remainder, 50.0), 20);
        assert_eq!(accumulate_scroll_lines(&mut remainder, f32::NAN), 0);
    }
}
