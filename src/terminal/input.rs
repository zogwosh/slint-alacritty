use alacritty_terminal::term::TermMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyInput {
    Text(String),
    Return,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    Delete,
    PageUp,
    PageDown,
    Insert,
    Backtab,
    Function(u8),
}

pub(super) fn encode_key(
    input: KeyInput,
    control: bool,
    alt: bool,
    shift: bool,
    altgr: bool,
    mode: TermMode,
) -> Vec<u8> {
    let (control, alt) = if altgr {
        (false, false)
    } else {
        (control, alt)
    };
    let modifier = modifier_parameter(control, alt, shift);
    let sequence = match input {
        KeyInput::Text(text) => return encode_text(&text, control, alt),
        KeyInput::Return => "\r".into(),
        KeyInput::Backspace => "\x7f".into(),
        KeyInput::Tab => "\t".into(),
        KeyInput::Escape => "\x1b".into(),
        KeyInput::Up => cursor_sequence('A', mode, modifier),
        KeyInput::Down => cursor_sequence('B', mode, modifier),
        KeyInput::Right => cursor_sequence('C', mode, modifier),
        KeyInput::Left => cursor_sequence('D', mode, modifier),
        KeyInput::Home => cursor_sequence('H', mode, modifier),
        KeyInput::End => cursor_sequence('F', mode, modifier),
        KeyInput::Delete => tilde_sequence(3, modifier),
        KeyInput::PageUp => tilde_sequence(5, modifier),
        KeyInput::PageDown => tilde_sequence(6, modifier),
        KeyInput::Insert => tilde_sequence(2, modifier),
        KeyInput::Backtab if modifier.is_none() || modifier == Some(2) => "\x1b[Z".into(),
        KeyInput::Backtab => format!("\x1b[1;{}Z", modifier.expect("modifier is present")),
        KeyInput::Function(number) => function_sequence(number, modifier),
    };

    let mut bytes = Vec::with_capacity(sequence.len() + usize::from(alt));
    if alt && modifier.is_none() && sequence != "\x1b" {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(sequence.as_bytes());
    bytes
}

fn encode_text(text: &str, control: bool, alt: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + usize::from(alt));
    if alt {
        bytes.push(0x1b);
    }

    if control {
        let mut chars = text.chars();
        if let (Some(character), None) = (chars.next(), chars.next())
            && let Some(control_byte) = control_byte(character)
        {
            bytes.push(control_byte);
            return bytes;
        }
    }

    bytes.extend_from_slice(text.as_bytes());
    bytes
}

fn control_byte(character: char) -> Option<u8> {
    let upper = character.to_ascii_uppercase();
    match upper {
        '@'..='_' => Some((upper as u8) & 0x1f),
        ' ' | '2' => Some(0),
        '3' => Some(27),
        '4' => Some(28),
        '5' => Some(29),
        '6' => Some(30),
        '7' | '-' => Some(31),
        '8' | '?' => Some(127),
        _ => None,
    }
}

fn modifier_parameter(control: bool, alt: bool, shift: bool) -> Option<u8> {
    let value = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(control);
    (value != 1).then_some(value)
}

fn cursor_sequence(key: char, mode: TermMode, modifier: Option<u8>) -> String {
    if let Some(modifier) = modifier {
        return format!("\x1b[1;{modifier}{key}");
    }
    let prefix = if mode.contains(TermMode::APP_CURSOR) {
        "\x1bO"
    } else {
        "\x1b["
    };
    format!("{prefix}{key}")
}

fn tilde_sequence(number: u8, modifier: Option<u8>) -> String {
    modifier.map_or_else(
        || format!("\x1b[{number}~"),
        |modifier| format!("\x1b[{number};{modifier}~"),
    )
}

fn function_sequence(number: u8, modifier: Option<u8>) -> String {
    if let Some(final_byte) = ['P', 'Q', 'R', 'S'].get(number.saturating_sub(1) as usize) {
        return modifier.map_or_else(
            || format!("\x1bO{final_byte}"),
            |modifier| format!("\x1b[1;{modifier}{final_byte}"),
        );
    }
    let code = match number {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return String::new(),
    };
    tilde_sequence(code, modifier)
}

#[cfg(test)]
fn no_modifiers() -> (bool, bool, bool, bool) {
    (false, false, false, false)
}

#[cfg(test)]
fn encode_without_modifiers(input: KeyInput, mode: TermMode) -> Vec<u8> {
    let (control, alt, shift, altgr) = no_modifiers();
    encode_key(input, control, alt, shift, altgr, mode)
}

#[cfg(test)]
mod tests {
    use super::{KeyInput, encode_key, encode_without_modifiers};
    use alacritty_terminal::term::TermMode;

    #[test]
    fn encodes_control_characters() {
        for (text, expected) in [("c", 3), ("[", 27), (" ", 0), ("?", 127), ("8", 127)] {
            assert_eq!(
                encode_key(
                    KeyInput::Text(text.into()),
                    true,
                    false,
                    false,
                    false,
                    TermMode::empty(),
                ),
                vec![expected]
            );
        }
    }

    #[test]
    fn prefixes_alt_text_with_escape() {
        assert_eq!(
            encode_key(
                KeyInput::Text("x".into()),
                false,
                true,
                false,
                false,
                TermMode::empty(),
            ),
            b"\x1bx"
        );
    }

    #[test]
    fn altgr_sends_printable_text_without_control_encoding() {
        assert_eq!(
            encode_key(
                KeyInput::Text("@".into()),
                true,
                true,
                false,
                true,
                TermMode::empty(),
            ),
            b"@"
        );
    }

    #[test]
    fn encodes_xterm_modified_special_keys() {
        assert_eq!(
            encode_key(KeyInput::Left, true, false, false, false, TermMode::empty(),),
            b"\x1b[1;5D"
        );
        assert_eq!(
            encode_key(
                KeyInput::Function(5),
                false,
                false,
                true,
                false,
                TermMode::empty(),
            ),
            b"\x1b[15;2~"
        );
    }

    #[test]
    fn application_cursor_mode_uses_ss3_sequences_without_modifiers() {
        assert_eq!(
            encode_without_modifiers(KeyInput::Up, TermMode::APP_CURSOR),
            b"\x1bOA"
        );
        assert_eq!(
            encode_without_modifiers(KeyInput::Up, TermMode::empty()),
            b"\x1b[A"
        );
    }
}
