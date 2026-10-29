//! The Hanzo welcome animation for onboarding and fresh conversations.
//! Visible time pauses while hidden; first-screen motion stops on interaction.

mod lighting;
mod policy;
mod renderer;
mod sequence;

use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::motion::MotionMode;
use crate::terminal_palette;
use lighting::Lighting;
pub(crate) use policy::Presentation;
pub(crate) use policy::is_startup_cell;
use renderer::MAX_COLUMNS;
use renderer::MAX_ROWS;
use renderer::Renderer;

pub(crate) const FRAME_INTERVAL: Duration = Duration::from_millis(/*millis*/ 50);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerState {
    Empty,
    Draft,
}

#[derive(Default)]
pub(crate) struct EmptyStateAnimation {
    eligible: bool,
    spin_elapsed: Duration,
    last_frame: Option<Instant>,
    fade_elapsed: Duration,
    static_mark: Option<bool>,
    opacity: f32,
    fade_from: f32,
    renderer: Option<Renderer>,
    stage: Option<Rect>,
    replaying: bool,
    // Once the user starts interacting, only the static mark is allowed.
    interacted: bool,
}

impl EmptyStateAnimation {
    #[cfg(test)]
    pub(crate) fn set_test_phase(&mut self, phase: f64) {
        self.spin_elapsed = Duration::from_secs_f64(phase * sequence::LOOP_SECONDS);
        self.last_frame = None;
    }

    pub(crate) fn is_eligible(&self) -> bool {
        self.eligible
    }

    pub(crate) fn start_fresh(&mut self) {
        self.cancel_replay();
        self.eligible = true;
        self.interacted = false;
        self.spin_elapsed = Duration::ZERO;
        self.last_frame = None;
        self.fade_elapsed = Duration::ZERO;
        self.static_mark = None;
        self.opacity = 1.0;
    }

    /// Keep the provisional pose when handing off to the live thread.
    pub(crate) fn continue_from(&mut self, source: &mut Self) {
        let mut previous = std::mem::take(source);
        if !previous.is_eligible() {
            previous.start_fresh();
        }
        *self = previous;
    }

    pub(crate) fn dismiss(&mut self) {
        self.eligible = false;
        self.cancel_replay();
    }

    pub(crate) fn cancel_replay(&mut self) {
        self.stage = None;
        self.replaying = false;
        self.pause_clock();
    }

    /// A click on the idle logo is interaction, not a request to restart motion.
    /// Do not consume the mouse event; the normal input owner still receives it.
    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        let inside = self
            .stage
            .is_some_and(|stage| stage.contains(Position::new(mouse.column, mouse.row)))
            && mouse.modifiers.is_empty();
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) && inside {
            self.interacted = true;
            self.cancel_replay();
        }
        false
    }

    /// Stop visible time at the last painted frame, including when no hidden frame was drawn.
    /// Resume (after process suspension) may call this after the interruption; phase is retained.
    pub(crate) fn pause_clock(&mut self) {
        self.last_frame = None;
    }

    /// Paint the shared logo sequence inside a caller-owned, reserved rectangle.
    /// The caller clears the stage and keeps its layout stable after motion finishes.
    pub(crate) fn render_in(
        &mut self,
        area: Rect,
        buffer: &mut Buffer,
        presentation: Presentation,
    ) -> Option<Duration> {
        let now = Instant::now();
        self.render_in_at(area, buffer, presentation, now)
    }

    fn render_in_at(
        &mut self,
        area: Rect,
        buffer: &mut Buffer,
        presentation: Presentation,
        now: Instant,
    ) -> Option<Duration> {
        if !self.eligible
            || presentation == Presentation::Hidden
            || area.is_empty()
            || area.width > MAX_COLUMNS
            || area.height > MAX_ROWS
            || area.intersection(buffer.area) != area
        {
            self.pause_clock();
            return None;
        }
        let static_mark = presentation == Presentation::Faded;
        let previous_frame = self.last_frame.replace(now);
        if self.static_mark != Some(static_mark) {
            self.fade_elapsed = if self.static_mark.is_none() {
                sequence::STATIC_FADE
            } else {
                Duration::ZERO
            };
            self.fade_from = self.opacity;
        } else if let Some(previous) = previous_frame {
            let elapsed = now.saturating_duration_since(previous);
            self.fade_elapsed += elapsed;
            if !static_mark {
                self.spin_elapsed += elapsed;
            }
        }
        self.static_mark = Some(static_mark);
        let finished = self.spin_elapsed >= sequence::SPIN_DURATION;
        let phase =
            self.spin_elapsed.min(sequence::SPIN_DURATION).as_secs_f64() / sequence::LOOP_SECONDS;
        let settling = !finished && static_mark && self.fade_elapsed < sequence::STATIC_FADE;
        self.opacity = if finished && self.replaying {
            sequence::static_opacity(
                self.spin_elapsed - sequence::SPIN_DURATION,
                /*from*/ 1.0,
            )
        } else if finished {
            1.0
        } else if static_mark {
            sequence::static_opacity(self.fade_elapsed, self.fade_from)
        } else {
            1.0
        };
        if !static_mark && !finished {
            self.opacity = self.fade_from
                + (self.opacity - self.fade_from)
                    * sequence::progress(self.fade_elapsed, sequence::STATIC_FADE) as f32;
        }
        self.paint_frame(area, buffer, phase, self.opacity);
        ((!finished && (!static_mark || settling))
            || (self.replaying
                && self.spin_elapsed < sequence::SPIN_DURATION + sequence::STATIC_FADE))
            .then_some(FRAME_INTERVAL)
    }

    /// Render only inside a reserved, unobstructed rectangle. At 80 terminal
    /// columns the stage is at most 60 wide; a short 80x24 viewport scales
    /// the mark down rather than displacing the input composer.
    ///
    /// Continuous rotation belongs only to a truly empty, untouched
    /// composer. Typing or clicking stops it for this fresh conversation.
    pub(crate) fn render_first_screen(
        &mut self,
        available: Rect,
        buffer: &mut Buffer,
        composer: Option<ComposerState>,
        motion: MotionMode,
    ) -> Option<Duration> {
        const IDEAL_ROWS: u16 = 21;
        const MIN_STAGE_ROWS: u16 = 12;
        if composer != Some(ComposerState::Empty) || available.is_empty() {
            if composer == Some(ComposerState::Draft) {
                self.interacted = true;
            }
            self.cancel_replay();
            return None;
        }
        let max_width = buffer
            .area
            .width
            .saturating_sub(4)
            .min(available.width)
            .min(MAX_COLUMNS);
        let width_for_height =
            (u32::from(available.height) * u32::from(MAX_COLUMNS) / u32::from(IDEAL_ROWS)) as u16;
        let width = max_width.min(width_for_height);
        let height = width * IDEAL_ROWS / MAX_COLUMNS;
        if height < MIN_STAGE_ROWS || height > available.height {
            self.cancel_replay();
            return None;
        }
        let stage = Rect::new(
            available.x + (available.width - width) / 2,
            available.y + (available.height - height) / 2,
            width,
            height,
        );
        if stage.intersection(buffer.area) != stage {
            self.cancel_replay();
            return None;
        }
        self.stage = Some(stage);
        if self.interacted || motion == MotionMode::Reduced {
            self.pause_clock();
            self.paint_frame(stage, buffer, /*phase*/ 0.0, /*opacity*/ 1.0);
            return None;
        }
        let now = Instant::now();
        if let Some(last) = self.last_frame.replace(now) {
            self.spin_elapsed += now.saturating_duration_since(last);
        }
        let phase = (self.spin_elapsed.as_secs_f64() / sequence::LOOP_SECONDS).fract();
        self.paint_frame(stage, buffer, phase, /*opacity*/ 1.0);
        Some(FRAME_INTERVAL)
    }

    fn paint_frame(&mut self, area: Rect, buffer: &mut Buffer, phase: f64, opacity: f32) {
        let background = terminal_palette::default_bg();
        // Color capability does not depend on whether the terminal answered OSC 11.
        let color_level = terminal_palette::effective_stdout_color_level();
        let background = background.unwrap_or((15, 20, 37));
        let light = Lighting::terminal(
            terminal_palette::default_fg().unwrap_or((210, 221, 235)),
            background,
        );
        let cells = self.renderer.get_or_insert_with(Renderer::default).frame(
            area.width,
            area.height,
            phase,
            &light,
        );
        for (i, cell) in cells.iter().enumerate().filter(|(_, cell)| cell.dots != 0) {
            let [_, r, g, b] = cell.rgb.to_be_bytes();
            let target = &mut buffer[(
                area.x + i as u16 % area.width,
                area.y + i as u16 / area.width,
            )];
            target.set_char(cell.glyph());
            let color = crate::color::blend((r, g, b), background, opacity);
            let color = if color_level == terminal_palette::StdoutColorLevel::Ansi256 {
                // Four bits per channel bound the cache and avoid palette searches each frame.
                static COLORS: [OnceLock<Color>; 4096] = [const { OnceLock::new() }; 4096];
                let (r, g, b) = (color.0 >> 4, color.1 >> 4, color.2 >> 4);
                let index = usize::from(r) * 256 + usize::from(g) * 16 + usize::from(b);
                *COLORS[index].get_or_init(|| {
                    terminal_palette::best_color_for_level(
                        (r * 16 + 8, g * 16 + 8, b * 16 + 8),
                        color_level,
                    )
                })
            } else {
                terminal_palette::best_color_for_level(color, color_level)
            };
            target.set_fg(color);
            // Default-color terminals can only step between normal and dim intensity.
            if opacity < 0.5
                && matches!(
                    color_level,
                    terminal_palette::StdoutColorLevel::Ansi16
                        | terminal_palette::StdoutColorLevel::Unknown
                )
            {
                target.set_style(target.style().dim());
            }
        }
    }
}

#[cfg(test)]
#[path = "empty_state_animation_tests.rs"]
mod tests;
