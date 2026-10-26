//! The colours the interface draws with, by meaning. Call sites add their own `BOLD`/`DIM`;
//! selection is `REVERSED` and not a token.

use fontaine_core::config::ThemeName;
use ratatui::style::Color;

/// The marks that start a notice, with an ASCII set for terminals and fonts without `⚠`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyphs {
    pub info: &'static str,
    pub warning: &'static str,
}

impl Glyphs {
    pub const UNICODE: Glyphs = Glyphs {
        info: "· ",
        warning: "⚠ ",
    };
    pub const ASCII: Glyphs = Glyphs {
        info: "- ",
        warning: "* ",
    };

    /// ASCII when `FONTAINE_ASCII_GLYPHS` is set to anything but empty or `0`.
    pub fn from_env(value: Option<&str>) -> Glyphs {
        match value {
            Some(v) if !v.is_empty() && v != "0" => Glyphs::ASCII,
            _ => Glyphs::UNICODE,
        }
    }
}

impl Default for Glyphs {
    fn default() -> Self {
        Glyphs::UNICODE
    }
}

pub const ASCII_GLYPHS_VAR: &str = "FONTAINE_ASCII_GLYPHS";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// Your `>` messages.
    pub user: Color,
    /// A tool call that finished.
    pub success: Color,
    /// A failed or denied call, error entries, the guardian-unavailable heading.
    pub error: Color,
    pub warning: Color,
    /// Dialog headings.
    pub heading: Color,
    /// Manual and dontAsk.
    pub mode_manual: Color,
    pub mode_accept: Color,
    pub mode_plan: Color,
    pub mode_auto: Color,
    /// The `!` input border.
    pub shell: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::preset(ThemeName::default())
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

impl Theme {
    pub fn preset(name: ThemeName) -> Theme {
        match name {
            ThemeName::DarkAnsi => Theme {
                user: Color::Cyan,
                success: Color::Green,
                error: Color::Red,
                warning: Color::Yellow,
                heading: Color::Yellow,
                mode_manual: Color::Gray,
                mode_accept: Color::Green,
                mode_plan: Color::Cyan,
                mode_auto: Color::Yellow,
                shell: Color::Magenta,
            },
            ThemeName::LightAnsi => Theme {
                user: Color::Blue,
                success: Color::Green,
                error: Color::Red,
                warning: Color::Magenta,
                heading: Color::Magenta,
                mode_manual: Color::DarkGray,
                mode_accept: Color::Green,
                mode_plan: Color::Blue,
                mode_auto: Color::Magenta,
                shell: Color::Magenta,
            },
            ThemeName::Dark => Theme {
                user: rgb(122, 180, 232),
                success: rgb(78, 186, 101),
                error: rgb(255, 107, 128),
                warning: rgb(255, 193, 7),
                heading: rgb(255, 193, 7),
                mode_manual: rgb(153, 153, 153),
                mode_accept: rgb(175, 135, 255),
                mode_plan: rgb(72, 150, 140),
                mode_auto: rgb(255, 193, 7),
                shell: rgb(253, 93, 177),
            },
            ThemeName::Light => Theme {
                user: rgb(0, 95, 175),
                success: rgb(44, 122, 57),
                error: rgb(171, 43, 63),
                warning: rgb(150, 108, 30),
                heading: rgb(150, 108, 30),
                mode_manual: rgb(102, 102, 102),
                mode_accept: rgb(135, 0, 255),
                mode_plan: rgb(0, 102, 102),
                mode_auto: rgb(150, 108, 30),
                shell: rgb(255, 0, 135),
            },
            ThemeName::DarkDaltonized => Theme {
                user: rgb(122, 180, 232),
                success: rgb(51, 153, 255),
                error: rgb(255, 153, 51),
                warning: rgb(255, 204, 0),
                heading: rgb(255, 204, 0),
                mode_manual: rgb(153, 153, 153),
                mode_accept: rgb(175, 135, 255),
                mode_plan: rgb(72, 150, 140),
                mode_auto: rgb(255, 204, 0),
                shell: rgb(253, 93, 177),
            },
            ThemeName::LightDaltonized => Theme {
                user: rgb(0, 95, 175),
                success: rgb(0, 102, 153),
                error: rgb(204, 102, 0),
                warning: rgb(150, 108, 30),
                heading: rgb(150, 108, 30),
                mode_manual: rgb(102, 102, 102),
                mode_accept: rgb(135, 0, 255),
                mode_plan: rgb(0, 102, 102),
                mode_auto: rgb(150, 108, 30),
                shell: rgb(255, 0, 135),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_distinct() {
        for (i, a) in ThemeName::ALL.iter().enumerate() {
            for b in &ThemeName::ALL[i + 1..] {
                assert_ne!(Theme::preset(*a), Theme::preset(*b), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn daltonized_success_and_error_differ_in_hue_family() {
        for name in [ThemeName::DarkDaltonized, ThemeName::LightDaltonized] {
            let theme = Theme::preset(name);
            let Color::Rgb(sr, _, sb) = theme.success else {
                panic!("{name:?} success is not rgb");
            };
            let Color::Rgb(er, _, eb) = theme.error else {
                panic!("{name:?} error is not rgb");
            };
            assert!(sb > sr, "{name:?} success should be bluish");
            assert!(er > eb, "{name:?} error should be orangish");
        }
    }

    #[test]
    fn the_default_is_the_ansi_palette_the_interface_always_used() {
        let theme = Theme::default();
        assert_eq!(theme.success, Color::Green);
        assert_eq!(theme.error, Color::Red);
        assert_eq!(theme.user, Color::Cyan);
        assert_eq!(theme.heading, Color::Yellow);
    }
}
