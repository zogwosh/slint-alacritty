//! 将 ANSI 命名色、索引色和应用动态覆盖统一解析为 RGB。

use alacritty_terminal::{
    term::color::{COUNT, Colors},
    vte::ansi::{Color as AnsiColor, NamedColor, Rgb},
};

/// 渲染层使用的紧凑 8 位 RGB 颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RgbColor {
    pub(crate) red: u8,
    pub(crate) green: u8,
    pub(crate) blue: u8,
}

/// 解析终端动态颜色查询使用的索引；覆盖色与默认主题的优先级和实际渲染一致。
pub(super) fn resolve_dynamic_color(
    index: usize,
    overrides: &Colors,
    theme: TerminalTheme,
) -> Option<Rgb> {
    if index >= COUNT {
        return None;
    }
    if let Some(color) = overrides[index] {
        return Some(color);
    }

    Some(match index {
        0..=255 => indexed_color(index as u8),
        256 | 267 => rgb(theme.foreground),
        257 => rgb(theme.background),
        258 => rgb(theme.foreground),
        259..=266 => dim(ANSI_PALETTE[index - 259]),
        268 => dim(rgb(theme.foreground)),
        _ => return None,
    })
}

/// 从 UI DesignTokens 注入的终端基础主题；终端程序仍可通过 ANSI 动态覆盖单元颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalTheme {
    pub(crate) background: RgbColor,
    pub(crate) foreground: RgbColor,
    pub(crate) selection_background: RgbColor,
    pub(crate) selection_foreground: RgbColor,
    pub(crate) search_match_background: RgbColor,
    pub(crate) search_current_background: RgbColor,
}

/// 应用默认的 16 色 ANSI 调色板。
const ANSI_PALETTE: [Rgb; 16] = [
    Rgb {
        r: 0x1f,
        g: 0x24,
        b: 0x2d,
    },
    Rgb {
        r: 0xff,
        g: 0x7b,
        b: 0x72,
    },
    Rgb {
        r: 0x3f,
        g: 0xb9,
        b: 0x50,
    },
    Rgb {
        r: 0xd2,
        g: 0x99,
        b: 0x22,
    },
    Rgb {
        r: 0x58,
        g: 0xa6,
        b: 0xff,
    },
    Rgb {
        r: 0xbc,
        g: 0x8c,
        b: 0xff,
    },
    Rgb {
        r: 0x39,
        g: 0xc5,
        b: 0xcf,
    },
    Rgb {
        r: 0xb1,
        g: 0xba,
        b: 0xc4,
    },
    Rgb {
        r: 0x48,
        g: 0x4f,
        b: 0x58,
    },
    Rgb {
        r: 0xff,
        g: 0xa1,
        b: 0x98,
    },
    Rgb {
        r: 0x56,
        g: 0xd3,
        b: 0x64,
    },
    Rgb {
        r: 0xe3,
        g: 0xb3,
        b: 0x41,
    },
    Rgb {
        r: 0x79,
        g: 0xc0,
        b: 0xff,
    },
    Rgb {
        r: 0xd2,
        g: 0xa8,
        b: 0xff,
    },
    Rgb {
        r: 0x56,
        g: 0xd4,
        b: 0xdd,
    },
    Rgb {
        r: 0xf0,
        g: 0xf6,
        b: 0xfc,
    },
];

/// 优先采用终端应用设置的动态颜色，否则回退到内置调色板。
pub(super) fn resolve_color(
    color: AnsiColor,
    overrides: &Colors,
    foreground: bool,
    theme: TerminalTheme,
) -> RgbColor {
    let rgb = match color {
        AnsiColor::Spec(rgb) => rgb,
        AnsiColor::Indexed(index) => indexed_color(index),
        AnsiColor::Named(named) => {
            overrides[named].unwrap_or_else(|| named_color(named, foreground, theme))
        }
    };
    RgbColor {
        red: rgb.r,
        green: rgb.g,
        blue: rgb.b,
    }
}

fn named_color(color: NamedColor, foreground: bool, theme: TerminalTheme) -> Rgb {
    match color {
        NamedColor::Foreground | NamedColor::BrightForeground => rgb(theme.foreground),
        NamedColor::Background => rgb(theme.background),
        NamedColor::Cursor => rgb(theme.foreground),
        NamedColor::DimForeground => dim(rgb(theme.foreground)),
        NamedColor::DimBlack => dim(ANSI_PALETTE[0]),
        NamedColor::DimRed => dim(ANSI_PALETTE[1]),
        NamedColor::DimGreen => dim(ANSI_PALETTE[2]),
        NamedColor::DimYellow => dim(ANSI_PALETTE[3]),
        NamedColor::DimBlue => dim(ANSI_PALETTE[4]),
        NamedColor::DimMagenta => dim(ANSI_PALETTE[5]),
        NamedColor::DimCyan => dim(ANSI_PALETTE[6]),
        NamedColor::DimWhite => dim(ANSI_PALETTE[7]),
        named => ANSI_PALETTE
            .get(named as usize)
            .copied()
            .unwrap_or(if foreground {
                rgb(theme.foreground)
            } else {
                rgb(theme.background)
            }),
    }
}

fn rgb(color: RgbColor) -> Rgb {
    Rgb {
        r: color.red,
        g: color.green,
        b: color.blue,
    }
}

/// 生成 xterm 256 色中的 6×6×6 色立方与灰阶区间。
fn indexed_color(index: u8) -> Rgb {
    if index < 16 {
        return ANSI_PALETTE[index as usize];
    }
    if index < 232 {
        let value = index - 16;
        let component = |part: u8| if part == 0 { 0 } else { 55 + part * 40 };
        return Rgb {
            r: component(value / 36),
            g: component((value / 6) % 6),
            b: component(value % 6),
        };
    }
    let gray = 8 + (index - 232) * 10;
    Rgb {
        r: gray,
        g: gray,
        b: gray,
    }
}

fn dim(color: Rgb) -> Rgb {
    Rgb {
        r: (u16::from(color.r) * 2 / 3) as u8,
        g: (u16::from(color.g) * 2 / 3) as u8,
        b: (u16::from(color.b) * 2 / 3) as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::{RgbColor, TerminalTheme, resolve_color, resolve_dynamic_color};
    use alacritty_terminal::{
        term::color::Colors,
        vte::ansi::{Color, NamedColor},
    };

    #[test]
    fn bright_dim_colors_do_not_overflow() {
        let color = resolve_color(
            Color::Named(NamedColor::DimRed),
            &Colors::default(),
            true,
            TerminalTheme {
                background: RgbColor {
                    red: 1,
                    green: 2,
                    blue: 3,
                },
                foreground: RgbColor {
                    red: 4,
                    green: 5,
                    blue: 6,
                },
                selection_background: RgbColor {
                    red: 7,
                    green: 8,
                    blue: 9,
                },
                selection_foreground: RgbColor {
                    red: 10,
                    green: 11,
                    blue: 12,
                },
                search_match_background: RgbColor {
                    red: 13,
                    green: 14,
                    blue: 15,
                },
                search_current_background: RgbColor {
                    red: 19,
                    green: 20,
                    blue: 21,
                },
            },
        );
        assert_eq!((color.red, color.green, color.blue), (170, 82, 76));
    }

    #[test]
    fn dynamic_queries_use_overrides_then_theme_defaults() {
        let theme = TerminalTheme {
            background: RgbColor {
                red: 1,
                green: 2,
                blue: 3,
            },
            foreground: RgbColor {
                red: 4,
                green: 5,
                blue: 6,
            },
            selection_background: RgbColor {
                red: 7,
                green: 8,
                blue: 9,
            },
            selection_foreground: RgbColor {
                red: 10,
                green: 11,
                blue: 12,
            },
            search_match_background: RgbColor {
                red: 13,
                green: 14,
                blue: 15,
            },
            search_current_background: RgbColor {
                red: 19,
                green: 20,
                blue: 21,
            },
        };
        let mut colors = Colors::default();
        colors[NamedColor::Foreground] = Some(alacritty_terminal::vte::ansi::Rgb {
            r: 20,
            g: 21,
            b: 22,
        });

        assert_eq!(
            resolve_dynamic_color(NamedColor::Foreground as usize, &colors, theme),
            colors[NamedColor::Foreground]
        );
        assert_eq!(
            resolve_dynamic_color(NamedColor::Background as usize, &colors, theme),
            Some(alacritty_terminal::vte::ansi::Rgb { r: 1, g: 2, b: 3 })
        );
        assert!(resolve_dynamic_color(usize::MAX, &colors, theme).is_none());
    }
}
