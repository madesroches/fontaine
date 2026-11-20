//! Where the UI may draw: the display minus the system bars, cutout and keyboard. Portable so the
//! arithmetic is tested on the host; the Android layer reads the raw numbers.

/// Space the system takes on each edge of the display, in physical pixels (the unit of ImGui
/// coordinates here).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    pub top: f32,
    pub left: f32,
    pub right: f32,
    pub bottom: f32,
}

/// Largest share of an axis one inset may take.
const MAX_EDGE: f32 = 0.4;
/// Largest share of an axis both insets together may take.
const MAX_AXIS: f32 = 0.8;

impl Insets {
    pub const ZERO: Insets = Insets {
        top: 0.0,
        left: 0.0,
        right: 0.0,
        bottom: 0.0,
    };

    /// Bounds a value from the platform so the window never collapses: a `SetNextWindowSize` of
    /// zero means auto-fit, so `safe_rect` must keep a positive size.
    pub fn clamped(self, display: (f32, f32)) -> Insets {
        let (width, height) = display;
        let top = self.top.max(0.0).min(height * MAX_EDGE);
        let left = self.left.max(0.0).min(width * MAX_EDGE);
        // Only the axis total caps the bottom: a soft keyboard in landscape can cover most of the
        // height and the prompt must stay above it.
        let bottom = self.bottom.max(0.0).min((height * MAX_AXIS - top).max(0.0));
        let right = self
            .right
            .max(0.0)
            .min(width * MAX_EDGE)
            .min((width * MAX_AXIS - left).max(0.0));
        Insets {
            top,
            left,
            right,
            bottom,
        }
    }

    /// Converts the content rectangle the activity reports (the part of the display not covered
    /// by the system) into insets. An all-zero rect is what `android-activity` reports before the
    /// first content rect change, so a zero right or bottom means "unknown", not "empty".
    pub fn from_content_rect(
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        display: (f32, f32),
    ) -> Insets {
        if right == 0 || bottom == 0 {
            return Insets::ZERO;
        }
        Insets {
            top: top as f32,
            left: left as f32,
            right: display.0 - right as f32,
            bottom: display.1 - bottom as f32,
        }
    }
}

/// What the window insets API reports, in whole pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowInsets {
    pub top: i32,
    pub left: i32,
    pub right: i32,
    pub bottom: i32,
    /// The soft keyboard is part of the values; before API 30 only the content rect knows it.
    pub includes_keyboard: bool,
}

/// The insets for a frame. The window insets are primary; the content rect covers the time before
/// the first read and, before API 30, the keyboard.
pub fn resolve_insets(
    window: Option<WindowInsets>,
    content_rect: (i32, i32, i32, i32),
    display: (f32, f32),
) -> Insets {
    let (left, top, right, bottom) = content_rect;
    let from_rect = Insets::from_content_rect(left, top, right, bottom, display);
    let insets = match window {
        Some(w) => Insets {
            top: w.top as f32,
            left: w.left as f32,
            right: w.right as f32,
            bottom: if w.includes_keyboard {
                w.bottom as f32
            } else {
                (w.bottom as f32).max(from_rect.bottom)
            },
        },
        None => from_rect,
    };
    insets.clamped(display)
}

/// The window rectangle to draw into, as position and size.
pub fn safe_rect(display: (f32, f32), insets: Insets) -> ((f32, f32), (f32, f32)) {
    (
        (insets.left, insets.top),
        (
            display.0 - insets.left - insets.right,
            display.1 - insets.top - insets.bottom,
        ),
    )
}

/// The outer content margin.
pub fn margin(scale: f32) -> f32 {
    16.0 * scale
}

/// Side of the square scan card: as wide as the window allows, shrunk so the paste section below
/// stays on screen, but never under `min_side`.
pub fn card_side(avail_width: f32, avail_height: f32, below_px: f32, min_side: f32) -> f32 {
    avail_width
        .min(min_side.max(avail_height - below_px))
        .max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: (f32, f32) = (1080.0, 2400.0);

    fn insets(top: f32, left: f32, right: f32, bottom: f32) -> Insets {
        Insets {
            top,
            left,
            right,
            bottom,
        }
    }

    #[test]
    fn negative_insets_clamp_to_zero() {
        assert_eq!(
            insets(-5.0, -1.0, -20.0, -300.0).clamped(DISPLAY),
            Insets::ZERO
        );
    }

    #[test]
    fn oversize_insets_are_capped_per_edge() {
        let c = insets(2000.0, 2000.0, 2000.0, 2000.0).clamped(DISPLAY);
        assert_eq!(c.top, 960.0);
        assert_eq!(c.left, 432.0);
        assert_eq!(c.right, 432.0);
        // 80 % of the height minus the top inset leaves the bottom 960.
        assert_eq!(c.bottom, 960.0);
    }

    #[test]
    fn a_keyboard_height_bottom_inset_survives() {
        let c = insets(120.0, 0.0, 0.0, 1300.0).clamped(DISPLAY);
        assert_eq!(c.bottom, 1300.0);
    }

    #[test]
    fn a_landscape_keyboard_bottom_inset_survives() {
        let landscape = (2400.0, 1080.0);
        let c = insets(48.0, 0.0, 0.0, 700.0).clamped(landscape);
        assert_eq!(c.bottom, 700.0);
    }

    fn window(bottom: i32, includes_keyboard: bool) -> Option<WindowInsets> {
        Some(WindowInsets {
            top: 100,
            left: 0,
            right: 0,
            bottom,
            includes_keyboard,
        })
    }

    #[test]
    fn pre_api_30_bottom_is_the_larger_of_bars_and_content_rect() {
        // The content rect stops 800 px above the display bottom: the keyboard.
        let rect = (0, 100, 1080, 1600);
        let i = resolve_insets(window(48, false), rect, DISPLAY);
        assert_eq!(i.bottom, 800.0);
        let i = resolve_insets(window(900, false), rect, DISPLAY);
        assert_eq!(i.bottom, 900.0);
    }

    #[test]
    fn a_keyboard_inclusive_bottom_ignores_the_content_rect() {
        let i = resolve_insets(window(48, true), (0, 100, 1080, 1600), DISPLAY);
        assert_eq!(i.bottom, 48.0);
        assert_eq!(i.top, 100.0);
    }

    #[test]
    fn without_window_insets_the_content_rect_is_used() {
        let i = resolve_insets(None, (0, 100, 1080, 2300), DISPLAY);
        assert_eq!(i, insets(100.0, 0.0, 0.0, 100.0));
        assert_eq!(resolve_insets(None, (0, 0, 0, 0), DISPLAY), Insets::ZERO);
    }

    #[test]
    fn opposing_insets_leave_a_positive_window() {
        for (display, i) in [
            (DISPLAY, insets(0.4 * 2400.0, 0.0, 0.0, 0.6 * 2400.0)),
            (DISPLAY, insets(0.0, 0.4 * 1080.0, 0.6 * 1080.0, 0.0)),
            (DISPLAY, insets(1e9, 1e9, 1e9, 1e9)),
            ((100.0, 50.0), insets(40.0, 40.0, 60.0, 30.0)),
        ] {
            let (_, size) = safe_rect(display, i.clamped(display));
            assert!(size.0 >= display.0 * 0.2 - 0.01, "{size:?}");
            assert!(size.1 >= display.1 * 0.2 - 0.01, "{size:?}");
        }
    }

    #[test]
    fn zero_insets_are_the_whole_display() {
        assert_eq!(safe_rect(DISPLAY, Insets::ZERO), ((0.0, 0.0), DISPLAY));
    }

    #[test]
    fn safe_rect_skips_the_bars() {
        let (pos, size) = safe_rect(DISPLAY, insets(120.0, 0.0, 0.0, 48.0));
        assert_eq!(pos, (0.0, 120.0));
        assert_eq!(size, (1080.0, 2232.0));
    }

    #[test]
    fn an_empty_content_rect_is_no_insets() {
        assert_eq!(Insets::from_content_rect(0, 0, 0, 0, DISPLAY), Insets::ZERO);
        assert_eq!(
            Insets::from_content_rect(0, 0, 1080, 0, DISPLAY),
            Insets::ZERO
        );
    }

    #[test]
    fn a_content_rect_gives_the_uncovered_edges() {
        let i = Insets::from_content_rect(10, 120, 1060, 2352, DISPLAY);
        assert_eq!(i, insets(120.0, 10.0, 20.0, 48.0));
    }

    #[test]
    fn card_is_width_limited_on_a_tall_screen() {
        assert_eq!(card_side(900.0, 2000.0, 500.0, 160.0), 900.0);
    }

    #[test]
    fn card_shrinks_to_keep_the_paste_section_visible() {
        assert_eq!(card_side(900.0, 1000.0, 500.0, 160.0), 500.0);
    }

    #[test]
    fn card_keeps_its_minimum_in_landscape() {
        assert_eq!(card_side(1800.0, 400.0, 500.0, 160.0), 160.0);
    }

    #[test]
    fn card_never_goes_negative() {
        assert_eq!(card_side(-5.0, 10.0, 500.0, 160.0), 0.0);
        assert_eq!(card_side(0.0, 0.0, 0.0, 0.0), 0.0);
    }
}
