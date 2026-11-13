use std::sync::Arc;

use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use crate::app_event_sender::AppEventSender;
use crate::render::renderable::Renderable;
use crate::terminal_palette::best_color;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::bottom_pane_view::ViewCompletion;

const MAX_VISIBLE_MODELS: usize = 7;
const ACCENT_BLUE_RGB: (u8, u8, u8) = (138, 162, 247);
const EFFORT_TRACK_RGB: (u8, u8, u8) = (204, 68, 51);
const EFFORT_ACTIVE_RGB: (u8, u8, u8) = (235, 95, 80);

pub(crate) type ModelSelectCallback =
    Arc<dyn Fn(&AppEventSender, ReasoningEffortConfig) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct ModelSwitcherEffortOption {
    pub(crate) effort: ReasoningEffortConfig,
    pub(crate) label: String,
    pub(crate) description: String,
}

#[derive(Clone)]
pub(crate) struct ModelSwitcherItem {
    pub(crate) model: String,
    pub(crate) display_name: String,
    pub(crate) is_current: bool,
    pub(crate) efforts: Vec<ModelSwitcherEffortOption>,
    pub(crate) initial_effort_idx: usize,
    pub(crate) on_select: ModelSelectCallback,
}

pub(crate) struct ModelSwitcherView {
    view_id: Option<&'static str>,
    items: Vec<ModelSwitcherItem>,
    filtered_indices: Vec<usize>,
    selected_row: usize,
    scroll_top: usize,
    selected_effort_by_item: Vec<usize>,
    search_query: String,
    completion: Option<ViewCompletion>,
    app_event_tx: AppEventSender,
}

impl ModelSwitcherView {
    pub(crate) fn new(
        view_id: Option<&'static str>,
        items: Vec<ModelSwitcherItem>,
        initial_selected_idx: Option<usize>,
        initial_search_query: Option<String>,
        app_event_tx: AppEventSender,
    ) -> Self {
        let selected_effort_by_item = items
            .iter()
            .map(|item| {
                if item.efforts.is_empty() {
                    0
                } else {
                    item.initial_effort_idx.min(item.efforts.len() - 1)
                }
            })
            .collect::<Vec<_>>();
        let filtered_indices = (0..items.len()).collect::<Vec<_>>();
        let default_idx = initial_selected_idx
            .or_else(|| items.iter().position(|item| item.is_current))
            .unwrap_or(0);
        let selected_row = if items.is_empty() {
            0
        } else {
            default_idx.min(items.len() - 1)
        };
        let mut view = Self {
            view_id,
            items,
            filtered_indices,
            selected_row,
            scroll_top: 0,
            selected_effort_by_item,
            search_query: String::new(),
            completion: None,
            app_event_tx,
        };
        view.ensure_scroll_visible();
        if let Some(query) = initial_search_query.filter(|q| !q.is_empty()) {
            let preferred_item = view.selected_index();
            view.search_query = query;
            view.rebuild_filter(preferred_item);
        }
        view
    }

    pub(crate) fn default_effort_options() -> Vec<ModelSwitcherEffortOption> {
        vec![
            ModelSwitcherEffortOption {
                effort: ReasoningEffortConfig::Custom("instant".to_string()),
                label: "instant".to_string(),
                description: "No reasoning – lowest latency, fastest answers".to_string(),
            },
            ModelSwitcherEffortOption {
                effort: ReasoningEffortConfig::Low,
                label: "low".to_string(),
                description: "Light reasoning for quick, simple work".to_string(),
            },
            ModelSwitcherEffortOption {
                effort: ReasoningEffortConfig::Medium,
                label: "medium".to_string(),
                description: "Balanced reasoning for everyday tasks".to_string(),
            },
            ModelSwitcherEffortOption {
                effort: ReasoningEffortConfig::High,
                label: "high".to_string(),
                description: "Deep reasoning for complex problems".to_string(),
            },
            ModelSwitcherEffortOption {
                effort: ReasoningEffortConfig::Custom(hanzo::effort::DEFAULT.to_string()),
                label: "auto".to_string(),
                description: "Full effort automatically when needed (default)".to_string(),
            },
        ]
    }

    fn current_item_index(&self) -> Option<usize> {
        self.filtered_indices.get(self.selected_row).copied()
    }

    fn ensure_scroll_visible(&mut self) {
        let len = self.filtered_indices.len();
        if len <= MAX_VISIBLE_MODELS {
            self.scroll_top = 0;
            return;
        }
        if self.selected_row < self.scroll_top {
            self.scroll_top = self.selected_row;
        } else if self.selected_row >= self.scroll_top + MAX_VISIBLE_MODELS {
            self.scroll_top = self.selected_row + 1 - MAX_VISIBLE_MODELS;
        }
        let max_scroll = len.saturating_sub(MAX_VISIBLE_MODELS);
        if self.scroll_top > max_scroll {
            self.scroll_top = max_scroll;
        }
    }

    fn rebuild_filter(&mut self, preferred_item_idx: Option<usize>) {
        let query = self.search_query.trim().to_ascii_lowercase();
        if query.is_empty() {
            self.filtered_indices = (0..self.items.len()).collect();
        } else {
            self.filtered_indices = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(idx, item)| {
                    let name_match = item.display_name.to_ascii_lowercase().contains(&query);
                    let slug_match = item.model.to_ascii_lowercase().contains(&query);
                    (name_match || slug_match).then_some(idx)
                })
                .collect();
        }
        if self.filtered_indices.is_empty() {
            self.selected_row = 0;
            self.scroll_top = 0;
            return;
        }
        if let Some(pref) = preferred_item_idx
            && let Some(pos) = self.filtered_indices.iter().position(|&idx| idx == pref)
        {
            self.selected_row = pos;
        } else {
            self.selected_row = 0;
        }
        self.ensure_scroll_visible();
    }

    fn move_up(&mut self) {
        let len = self.filtered_indices.len();
        if len == 0 {
            return;
        }
        if self.selected_row > 0 {
            self.selected_row -= 1;
        } else {
            self.selected_row = len - 1;
        }
        self.ensure_scroll_visible();
    }

    fn move_down(&mut self) {
        let len = self.filtered_indices.len();
        if len == 0 {
            return;
        }
        if self.selected_row + 1 < len {
            self.selected_row += 1;
        } else {
            self.selected_row = 0;
        }
        self.ensure_scroll_visible();
    }

    fn move_effort_left(&mut self) {
        if let Some(item_idx) = self.current_item_index()
            && let Some(effort_idx) = self.selected_effort_by_item.get_mut(item_idx)
        {
            *effort_idx = effort_idx.saturating_sub(1);
        }
    }

    fn move_effort_right(&mut self) {
        if let Some(item_idx) = self.current_item_index() {
            let max_idx = self.items[item_idx].efforts.len().saturating_sub(1);
            if let Some(effort_idx) = self.selected_effort_by_item.get_mut(item_idx) {
                *effort_idx = (*effort_idx + 1).min(max_idx);
            }
        }
    }

    fn accept_selection(&mut self) {
        let Some(item_idx) = self.current_item_index() else {
            return;
        };
        let item = &self.items[item_idx];
        let effort_idx = self
            .selected_effort_by_item
            .get(item_idx)
            .copied()
            .unwrap_or(0);
        let effort = item
            .efforts
            .get(effort_idx)
            .map(|opt| opt.effort.clone())
            .unwrap_or(ReasoningEffortConfig::High);
        (item.on_select)(&self.app_event_tx, effort);
        self.completion = Some(ViewCompletion::Accepted);
    }

    fn build_lines(&self) -> Vec<Line<'static>> {
        let accent_blue = best_color(ACCENT_BLUE_RGB);
        let track_coral = best_color(EFFORT_TRACK_RGB);
        let active_coral = best_color(EFFORT_ACTIVE_RGB);

        let mut lines: Vec<Line<'static>> = Vec::with_capacity(18);

        // 1. Header
        lines.push(Line::from("Switch Model".bold()));
        lines.push(Line::default());

        // 2. Search input + underline
        lines.push(Line::from(vec![
            Span::from("  Search: ").dim(),
            Span::from(self.search_query.clone()),
            Span::from("█"),
        ]));
        lines.push(Line::from(vec![
            Span::from("          "),
            Span::from("──────────────────────────").dim(),
        ]));
        lines.push(Line::default());

        // 3. Model list (up to MAX_VISIBLE_MODELS rows)
        let name_col_width = self
            .items
            .iter()
            .map(|item| item.display_name.chars().count())
            .max()
            .unwrap_or(16)
            .max(19)
            + 4;

        if self.filtered_indices.is_empty() {
            lines.push(Line::from("  No matching models".dim()));
        } else {
            let visible_end =
                (self.scroll_top + MAX_VISIBLE_MODELS).min(self.filtered_indices.len());
            for (row_idx, &item_idx) in self.filtered_indices[self.scroll_top..visible_end]
                .iter()
                .enumerate()
            {
                let actual_row = self.scroll_top + row_idx;
                let is_selected = actual_row == self.selected_row;
                let item = &self.items[item_idx];

                let mut spans = Vec::with_capacity(4);
                if is_selected {
                    spans.push(Span::styled("> ", Style::default().fg(accent_blue)));
                    spans.push(Span::styled(
                        item.display_name.clone(),
                        Style::default().fg(accent_blue),
                    ));
                } else {
                    spans.push(Span::from("  "));
                    spans.push(Span::from(item.display_name.clone()));
                }

                if item.is_current {
                    let name_len = item.display_name.chars().count();
                    let pad = name_col_width.saturating_sub(name_len).max(2);
                    spans.push(Span::from(" ".repeat(pad)));
                    spans.push(Span::from("(current)").dim());
                }

                lines.push(Line::from(spans));
            }
        }

        lines.push(Line::default());

        // 4. Effort slider
        let default_efforts = Self::default_effort_options();
        let default_idx = default_efforts.len().saturating_sub(1);
        let (efforts, effort_idx) = if let Some(item_idx) = self.current_item_index() {
            let item = &self.items[item_idx];
            let idx = self
                .selected_effort_by_item
                .get(item_idx)
                .copied()
                .unwrap_or(default_idx);
            if item.efforts.is_empty() {
                (default_efforts.as_slice(), default_idx)
            } else {
                (item.efforts.as_slice(), idx.min(item.efforts.len() - 1))
            }
        } else {
            (default_efforts.as_slice(), default_idx)
        };

        let num_efforts = efforts.len().max(1);
        let can_go_left = effort_idx > 0;
        let can_go_right = effort_idx + 1 < num_efforts;
        let seg_width = if num_efforts >= 5 { 9usize } else { 15usize };
        let seg_bar = "─".repeat(seg_width);
        let side_pad = if num_efforts >= 5 { "    " } else { "        " };
        let first_node_col = 12 + side_pad.len();
        let step_cols = seg_width + 1;

        let mut track_spans = vec![
            Span::from("  Effort").dim(),
            Span::from("   "),
            if can_go_left {
                Span::styled("◀", Style::default().fg(accent_blue))
            } else {
                Span::from("◀").dim()
            },
            Span::from(side_pad),
        ];

        for (idx, opt) in efforts.iter().enumerate() {
            if idx > 0 {
                let seg_active = idx <= effort_idx;
                let seg_span = if seg_active {
                    Span::styled(seg_bar.clone(), Style::default().fg(track_coral))
                } else {
                    Span::from(seg_bar.clone()).dim()
                };
                track_spans.push(seg_span);
            }
            let glyph = hanzo::effort::symbol(Some(opt.label.as_str()));
            let node_span = if idx == effort_idx {
                Span::styled(glyph, Style::default().fg(active_coral).bold())
            } else if idx < effort_idx {
                Span::styled(glyph, Style::default().fg(track_coral))
            } else {
                Span::from(glyph).dim()
            };
            track_spans.push(node_span);
        }

        track_spans.push(Span::from(side_pad));
        track_spans.push(if can_go_right {
            Span::styled("▶", Style::default().fg(accent_blue))
        } else {
            Span::from("▶").dim()
        });
        lines.push(Line::from(track_spans));

        // Effort labels line aligned under nodes
        let mut label_spans: Vec<Span<'static>> = Vec::new();
        let mut cursor_col: usize = 0;
        for (idx, opt) in efforts.iter().enumerate() {
            let node_col = first_node_col + idx * step_cols;
            let label_len = opt.label.chars().count();
            let target_start = node_col.saturating_sub(label_len / 2);
            let pad = target_start.saturating_sub(cursor_col);
            if pad > 0 {
                label_spans.push(Span::from(" ".repeat(pad)));
                cursor_col += pad;
            }
            let span = if idx == effort_idx {
                Span::styled(opt.label.clone(), Style::default().fg(active_coral).bold())
            } else {
                Span::from(opt.label.clone()).dim()
            };
            label_spans.push(span);
            cursor_col += label_len;
        }
        lines.push(Line::from(label_spans));

        // Effort description line
        let desc = efforts
            .get(effort_idx)
            .map(|opt| opt.description.as_str())
            .unwrap_or("Full effort automatically when needed (default)");
        lines.push(Line::from(vec![
            Span::from("            "),
            Span::from(desc.to_string()).dim().italic(),
        ]));

        lines.push(Line::default());

        // 5. Keyboard footer
        lines.push(Line::from(vec![
            Span::from("Keyboard: ").bold(),
            Span::styled("↑/↓", Style::default().fg(accent_blue)),
            Span::from(" Navigate  ").dim(),
            Span::styled("←/→", Style::default().fg(accent_blue)),
            Span::from(" Effort  ").dim(),
            Span::styled("enter", Style::default().fg(accent_blue)),
            Span::from(" Select  ").dim(),
            Span::styled("esc", Style::default().fg(accent_blue)),
            Span::from(" Go Back").dim(),
        ]));

        lines
    }
}

impl Renderable for ModelSwitcherView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let lines = self.build_lines();
        Paragraph::new(lines).render(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        let model_rows = if self.filtered_indices.is_empty() {
            1
        } else {
            self.filtered_indices.len().min(MAX_VISIBLE_MODELS)
        };
        // 5 header/search lines + model_rows + 1 blank + 3 effort lines + 1 blank + 1 footer
        (11 + model_rows) as u16
    }
}

impl BottomPaneView for ModelSwitcherView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if key_event.kind == KeyEventKind::Release {
            return;
        }
        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.move_up(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.move_down(),
            KeyEvent {
                code: KeyCode::Left,
                ..
            } => self.move_effort_left(),
            KeyEvent {
                code: KeyCode::Right,
                ..
            } => self.move_effort_right(),
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => self.accept_selection(),
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.completion = Some(ViewCompletion::Cancelled);
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                let preferred = self.current_item_index();
                self.search_query.pop();
                self.rebuild_filter(preferred);
            }
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                let preferred = self.current_item_index();
                self.search_query.clear();
                self.rebuild_filter(preferred);
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                let preferred = self.current_item_index();
                self.search_query.push(c);
                self.rebuild_filter(preferred);
            }
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn view_id(&self) -> Option<&'static str> {
        self.view_id
    }

    fn selected_index(&self) -> Option<usize> {
        self.current_item_index()
    }

    fn search_query(&self) -> Option<&str> {
        Some(&self.search_query)
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.completion = Some(ViewCompletion::Cancelled);
        CancellationEvent::Handled
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }
}
