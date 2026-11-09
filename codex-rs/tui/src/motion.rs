//! Centralized motion primitives for the TUI.
//!
//! Callers choose an explicit reduced-motion fallback here instead of reaching
//! directly for time-varying spinner or shimmer helpers.

use std::time::Duration;
use std::time::Instant;

use crate::tui::FrameRequester;
use ratatui::style::Stylize;
use ratatui::text::Span;

#[path = "shimmer.rs"]
mod shimmer;

use shimmer::shimmer_spans;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MotionMode {
    Animated,
    Reduced,
}

impl MotionMode {
    pub(crate) fn from_animations_enabled(animations_enabled: bool) -> Self {
        if animations_enabled {
            Self::Animated
        } else {
            Self::Reduced
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReducedMotionIndicator {
    Hidden,
    StaticBullet,
}

/// Show the current loading glyph immediately and schedule the next frame.
pub(crate) fn loading_glyph(
    started_at: Instant,
    mode: MotionMode,
    requester: &FrameRequester,
) -> &'static str {
    loading_glyph_with_delay(started_at, Duration::ZERO, mode, requester)
}

/// Show nothing during the animation delay, or a static glyph in reduced motion.
/// Schedule the frame that makes the spinner visible and each animation frame.
pub(crate) fn loading_glyph_with_delay(
    started_at: Instant,
    delay: Duration,
    mode: MotionMode,
    requester: &FrameRequester,
) -> &'static str {
    const FRAME_DURATION: Duration = Duration::from_millis(100);
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    if mode == MotionMode::Reduced {
        return "◌";
    }
    let elapsed = started_at.elapsed();
    if elapsed < delay {
        requester.schedule_frame_in(delay - elapsed);
        return "";
    }
    requester.schedule_frame_in(FRAME_DURATION);
    let frame = elapsed.as_millis() / FRAME_DURATION.as_millis();
    FRAMES[usize::try_from(frame).unwrap_or_default() % FRAMES.len()]
}

pub(crate) fn activity_indicator(
    start_time: Option<Instant>,
    motion_mode: MotionMode,
    reduced_motion_indicator: ReducedMotionIndicator,
) -> Option<Span<'static>> {
    match motion_mode {
        MotionMode::Animated => Some(animated_activity_indicator(start_time)),
        MotionMode::Reduced => match reduced_motion_indicator {
            ReducedMotionIndicator::Hidden => None,
            ReducedMotionIndicator::StaticBullet => Some("•".dim()),
        },
    }
}

pub(crate) fn shimmer_text(text: &str, motion_mode: MotionMode) -> Vec<Span<'static>> {
    match motion_mode {
        MotionMode::Animated => shimmer_spans(text),
        MotionMode::Reduced => {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![text.to_string().into()]
            }
        }
    }
}

/// The running mark is two Braille cells, a grid of four dots by four. A short comet of dots goes
/// round the edge of the grid, and every few turns the dots gather into an H and let go again.
pub(crate) const INDICATOR_STEP_MS: u64 = 90;
const ORBIT_STEP_MS: u128 = INDICATOR_STEP_MS as u128;
const ORBIT_TURNS: u128 = 3;
const HOLD_H_MS: u128 = 700;
/// The edge of the grid, clockwise from the top left: (column, row).
const ORBIT: [(u8, u8); 12] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (3, 1),
    (3, 2),
    (3, 3),
    (2, 3),
    (1, 3),
    (0, 3),
    (0, 2),
    (0, 1),
];
/// The H: both sides, and the bar across the middle two rows.
const H: [(u8, u8); 12] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (0, 3),
    (3, 0),
    (3, 1),
    (3, 2),
    (3, 3),
    (1, 1),
    (2, 1),
    (1, 2),
    (2, 2),
];

/// Two Braille cells holding the dots at these (column, row) places of the four-by-four grid.
fn braille_pair(dots: &[(u8, u8)]) -> String {
    const LEFT: [u32; 4] = [0x01, 0x02, 0x04, 0x40];
    const RIGHT: [u32; 4] = [0x08, 0x10, 0x20, 0x80];
    let mut cells = [0x2800u32; 2];
    for &(column, row) in dots {
        let bits = if column % 2 == 0 { LEFT } else { RIGHT };
        let bit = bits[usize::from(row)];
        cells[usize::from(column / 2)] |= bit;
    }
    cells
        .iter()
        .filter_map(|code| char::from_u32(*code))
        .collect()
}

fn indicator_dots(elapsed: Duration) -> String {
    let orbit_ms = ORBIT_STEP_MS * ORBIT.len() as u128 * ORBIT_TURNS;
    let t = elapsed.as_millis() % (orbit_ms + HOLD_H_MS);
    if t >= orbit_ms {
        return braille_pair(&H);
    }
    let head = usize::try_from(t / ORBIT_STEP_MS).unwrap_or_default() % ORBIT.len();
    let comet: Vec<(u8, u8)> = (0..3)
        .map(|tail| ORBIT[(head + ORBIT.len() - tail) % ORBIT.len()])
        .collect();
    braille_pair(&comet)
}

fn animated_activity_indicator(start_time: Option<Instant>) -> Span<'static> {
    let elapsed = start_time.map(|st| st.elapsed()).unwrap_or_default();
    let dots = indicator_dots(elapsed);
    if supports_color::on_cached(supports_color::Stream::Stdout)
        .map(|level| level.has_16m)
        .unwrap_or(false)
    {
        let style = shimmer_spans("•")
            .into_iter()
            .next()
            .map(|span| span.style)
            .unwrap_or_default();
        Span::styled(dots, style)
    } else {
        dots.into()
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn reduced_motion_activity_indicator_uses_explicit_fallback() {
        assert_eq!(
            activity_indicator(
                /*start_time*/ None,
                MotionMode::Reduced,
                ReducedMotionIndicator::Hidden,
            ),
            None
        );
        assert_eq!(
            activity_indicator(
                /*start_time*/ None,
                MotionMode::Reduced,
                ReducedMotionIndicator::StaticBullet,
            ),
            Some("•".dim())
        );
    }

    #[test]
    fn reduced_motion_shimmer_text_is_plain_text() {
        assert_eq!(
            shimmer_text("Loading", MotionMode::Reduced),
            vec!["Loading".into()]
        );
        assert_eq!(
            shimmer_text("", MotionMode::Reduced),
            Vec::<Span<'static>>::new()
        );
    }
}
