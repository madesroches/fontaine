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

/// Degrees clockwise the back camera's frame must turn to look upright on a display turned
/// `display_quadrant` quarter turns (`Display.getRotation()`); the formula Android documents for a
/// back-facing camera.
pub fn preview_rotation(sensor: i32, display_quadrant: i32) -> u16 {
    let quadrant = if (0..=3).contains(&display_quadrant) {
        display_quadrant
    } else {
        0
    };
    (sensor - 90 * quadrant).rem_euclid(360) as u16
}

/// UV corners, in the order top-left, top-right, bottom-right, bottom-left of a square card, that
/// show the frame turned `rotation` degrees clockwise and centre-cropped to fill the card, so the
/// card stays full whatever the frame's shape. A rotation that is not a right angle counts as 0.
pub fn fit_cover(src: (u32, u32), dst_side: f32, rotation: u16) -> [(f32, f32); 4] {
    let (width, height) = (src.0 as f32, src.1 as f32);
    // A card or frame with no area has nothing to crop; show the whole frame rather than divide
    // by zero.
    let (u0, u1, v0, v1) = if width > 0.0 && height > 0.0 && dst_side.is_finite() && dst_side > 0.0
    {
        let side = width.min(height);
        let u = (width - side) * 0.5 / width;
        let v = (height - side) * 0.5 / height;
        (u, 1.0 - u, v, 1.0 - v)
    } else {
        (0.0, 1.0, 0.0, 1.0)
    };
    let corners = [(u0, v0), (u1, v0), (u1, v1), (u0, v1)];
    let turns = match rotation {
        90 => 1,
        180 => 2,
        270 => 3,
        _ => 0,
    };
    // Turning the image clockwise moves each source corner one screen corner along.
    std::array::from_fn(|screen| corners[(screen + 4 - turns) % 4])
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

    fn close(a: [(f32, f32); 4], b: [(f32, f32); 4]) {
        for (p, q) in a.iter().zip(b) {
            assert!(
                (p.0 - q.0).abs() < 1e-6 && (p.1 - q.1).abs() < 1e-6,
                "{a:?} {b:?}"
            );
        }
    }

    #[test]
    fn a_landscape_frame_is_cropped_to_a_centred_square() {
        // 720 of 1280 columns are kept: 280 px cut from each side.
        close(
            fit_cover((1280, 720), 500.0, 0),
            [
                (0.21875, 0.0),
                (0.78125, 0.0),
                (0.78125, 1.0),
                (0.21875, 1.0),
            ],
        );
    }

    #[test]
    fn a_square_frame_is_shown_whole() {
        close(
            fit_cover((600, 600), 500.0, 0),
            [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
        );
    }

    #[test]
    fn a_quarter_turn_moves_each_corner_along() {
        let (a, b) = (0.21875, 0.78125);
        close(
            fit_cover((1280, 720), 500.0, 90),
            [(a, 1.0), (a, 0.0), (b, 0.0), (b, 1.0)],
        );
        close(
            fit_cover((1280, 720), 500.0, 180),
            [(b, 1.0), (a, 1.0), (a, 0.0), (b, 0.0)],
        );
        close(
            fit_cover((1280, 720), 500.0, 270),
            [(b, 0.0), (b, 1.0), (a, 1.0), (a, 0.0)],
        );
    }

    #[test]
    fn a_bad_rotation_counts_as_none() {
        let upright = fit_cover((1280, 720), 500.0, 0);
        for bad in [1, 45, 91, 360, 450] {
            close(fit_cover((1280, 720), 500.0, bad), upright);
        }
    }

    #[test]
    fn an_empty_frame_or_card_shows_the_whole_frame() {
        let whole = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        close(fit_cover((0, 720), 500.0, 0), whole);
        close(fit_cover((1280, 720), 0.0, 0), whole);
        close(fit_cover((1280, 720), f32::NAN, 0), whole);
    }

    #[test]
    fn preview_rotation_is_sensor_minus_display() {
        for (sensor, expected) in [(90, [90, 0, 270, 180]), (270, [270, 180, 90, 0])] {
            for (quadrant, want) in expected.into_iter().enumerate() {
                assert_eq!(preview_rotation(sensor, quadrant as i32), want);
            }
        }
    }

    #[test]
    fn preview_rotation_ignores_an_unknown_display_rotation() {
        assert_eq!(preview_rotation(90, -1), 90);
        assert_eq!(preview_rotation(90, 4), 90);
        assert_eq!(preview_rotation(270, i32::MAX), 270);
    }
}
