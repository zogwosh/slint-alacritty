use alacritty_terminal::{
    term::color::Colors,
    vte::ansi::{Color as AnsiColor, NamedColor, Rgb},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RgbColor {
    pub(crate) red: u8,
    pub(crate) green: u8,
    pub(crate) blue: u8,
}

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

pub(super) fn resolve_color(color: AnsiColor, overrides: &Colors, foreground: bool) -> RgbColor {
    let rgb = match color {
        AnsiColor::Spec(rgb) => rgb,
        AnsiColor::Indexed(index) => indexed_color(index),
        AnsiColor::Named(named) => {
            overrides[named].unwrap_or_else(|| named_color(named, foreground))
        }
    };
    RgbColor {
        red: rgb.r,
        green: rgb.g,
        blue: rgb.b,
    }
}

fn named_color(color: NamedColor, foreground: bool) -> Rgb {
    match color {
        NamedColor::Foreground | NamedColor::BrightForeground => Rgb {
            r: 0xe6,
            g: 0xed,
            b: 0xf3,
        },
        NamedColor::Background => Rgb {
            r: 0x0d,
            g: 0x11,
            b: 0x17,
        },
        NamedColor::Cursor => Rgb {
            r: 0xe6,
            g: 0xed,
            b: 0xf3,
        },
        NamedColor::DimForeground => Rgb {
            r: 0x7d,
            g: 0x85,
            b: 0x90,
        },
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
                Rgb {
                    r: 0xe6,
                    g: 0xed,
                    b: 0xf3,
                }
            } else {
                Rgb {
                    r: 0x0d,
                    g: 0x11,
                    b: 0x17,
                }
            }),
    }
}

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
    use super::resolve_color;
    use alacritty_terminal::{
        term::color::Colors,
        vte::ansi::{Color, NamedColor},
    };

    #[test]
    fn bright_dim_colors_do_not_overflow() {
        let color = resolve_color(Color::Named(NamedColor::DimRed), &Colors::default(), true);
        assert_eq!((color.red, color.green, color.blue), (170, 82, 76));
    }
}
