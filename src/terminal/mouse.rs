use super::command::{MouseAction, MouseButton, TerminalSize};
use alacritty_terminal::{
    event::EventListener,
    index::{Column, Line, Point},
    term::{Term, TermMode},
};

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

pub(super) fn scroll_lines(lines: f32) -> i32 {
    if !lines.is_finite() || lines == 0.0 {
        return 0;
    }

    let rounded = lines.round() as i32;
    if rounded == 0 {
        lines.signum() as i32
    } else {
        rounded.clamp(-20, 20)
    }
}

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
    use super::{encode_mouse_report, scroll_lines};
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
    fn wheel_delta_is_never_lost_and_is_bounded() {
        assert_eq!(scroll_lines(0.2), 1);
        assert_eq!(scroll_lines(-0.2), -1);
        assert_eq!(scroll_lines(50.0), 20);
        assert_eq!(scroll_lines(f32::NAN), 0);
    }
}
