//! Rendering.
//!
//! Drawing happens only when [`crate::app::state`] says something changed, so
//! an idle application costs no frames at all - the previous implementation
//! redrew fifty times a second forever.
//!
//! The screen is five stacked rows, and [`layout`] is the one place that
//! decides where they are. It returns them by name because the state machine
//! needs the same rectangles to work out what a mouse click landed on, and a
//! second, independently maintained copy of that arithmetic is exactly how a
//! click ends up one row off.

pub mod history;
pub mod input_line;
pub mod results;
pub mod status;
pub mod theme;

use std::time::{Instant, SystemTime};

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListState, Paragraph};

use crate::app::state::{AppState, Focus};

/// The rows of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunks {
    /// The bordered search box. Its text starts one cell in, on the row below.
    pub input: Rect,
    pub status: Rect,
    /// Results, or the recall panel while browsing history.
    pub results: Rect,
    pub toast: Rect,
    pub help: Rect,
}

impl Chunks {
    /// Column the first character of the search line is drawn at.
    pub fn input_text_x(&self) -> u16 {
        self.input.x + 1
    }

    /// Row the search line is drawn on.
    pub fn input_text_y(&self) -> u16 {
        self.input.y + 1
    }

    /// How many characters of the search line are visible.
    pub fn input_text_width(&self) -> u16 {
        self.input.width.saturating_sub(2)
    }

    /// Row the first list entry is drawn on, inside the border.
    pub fn first_row_y(&self) -> u16 {
        self.results.y + 1
    }
}

/// Splits the screen. The single source of truth for where anything is.
pub fn layout(area: Rect) -> Chunks {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // input
            Constraint::Length(1), // status
            Constraint::Min(1),    // results
            Constraint::Length(1), // toast
            Constraint::Length(1), // help
        ])
        .split(area);

    Chunks {
        input: rows[0],
        status: rows[1],
        results: rows[2],
        toast: rows[3],
        help: rows[4],
    }
}

/// Draws one frame.
pub fn draw(frame: &mut Frame, state: &AppState, viewer_missing: bool) {
    let now = Instant::now();
    let wall = SystemTime::now();
    let chunks = layout(frame.size());

    // 1. Input
    let view = input_line::view(&state.input, chunks.input_text_width());
    frame.render_widget(
        Paragraph::new(view.line).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .title(Span::styled(" File Code Search ", theme::title())),
        ),
        chunks.input,
    );
    // Put the terminal cursor where the caret belongs. The column comes from
    // the same function that drew the text, so the two cannot disagree about
    // how far the line is scrolled.
    frame.set_cursor(
        chunks.input_text_x() + view.caret_column,
        chunks.input_text_y(),
    );

    // 2. Status
    let line = status::render(state, now, wall);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(line.text, theme::tone(line.tone)))),
        chunks.status,
    );

    // 3. Recall, results, or the reason there are none
    if state.focus == Focus::History {
        draw_history(frame, state, chunks.results);
    } else {
        draw_results(frame, state, chunks.results);
    }

    // 4. Toast
    if let Some(toast) = &state.toast {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                toast.text.clone(),
                theme::toast(toast.severity),
            ))),
            chunks.toast,
        );
    } else if state.selection_lost {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "results changed under the selection",
                theme::help(),
            ))),
            chunks.toast,
        );
    }

    // 5. Help. During recall it describes recall, since none of the usual
    // keys mean what they normally do.
    let help = if state.focus == Focus::History {
        history::help_line().to_string()
    } else {
        status::help_line(viewer_missing)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(help, theme::help()))),
        chunks.help,
    );
}

fn draw_results(frame: &mut Frame, state: &AppState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(Span::styled(
            results::title(&state.hits, state.matched),
            theme::title(),
        ));

    if state.hits.is_empty() {
        let message = state
            .empty_reason
            .as_ref()
            .map(results::empty_message)
            .unwrap_or_else(|| "No results.".to_string());
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {message}"),
                theme::help(),
            )))
            .block(block),
            area,
        );
        return;
    }

    let query_len = state.input.chars().count();
    let items: Vec<_> = state
        .hits
        .iter()
        .map(|h| results::row_with_query(h, query_len))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(theme::selection())
        .highlight_symbol(">> ");
    let mut list_state = ListState::default();
    list_state.select(state.selected_row());
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_history(frame: &mut Frame, state: &AppState, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(Span::styled(history::title(&state.history), theme::title()));

    if state.history.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                history::empty_message(),
                theme::help(),
            )))
            .block(block),
            area,
        );
        return;
    }

    let list = List::new(history::rows(&state.history))
        .block(block)
        .highlight_style(history::highlight())
        .highlight_symbol(">> ");
    let mut list_state = ListState::default();
    list_state.select(state.history.cursor());
    frame.render_stateful_widget(list, area, &mut list_state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::{AppEvent, IndexMsg, SearchMsg};
    use crate::config::Settings;
    use crate::index::errors::EnumError;
    use crate::index::store::{FlatStatus, Health};
    use crate::search::matcher::{Hit, SearchOutcome};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::sync::Arc;
    use std::time::Duration;

    fn render_to_text(state: &AppState) -> String {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, state, false)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.get(x, y).symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn state() -> AppState {
        AppState::new(Settings::default(), Instant::now())
    }

    fn type_code(s: &mut AppState) {
        let now = Instant::now();
        for c in "11-D-0704".chars() {
            s.update(
                AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                now,
            );
        }
    }

    fn hit(name: &str, match_pos: u32) -> Hit {
        Hit {
            path: Arc::from(format!("V:\\{name}").as_str()),
            name: Arc::from(name),
            match_pos,
            index: 0,
        }
    }

    fn results(s: &mut AppState, hits: Vec<Hit>, matched: u32, total: u32) {
        s.update(
            AppEvent::Search(SearchMsg {
                epoch: s.query_epoch(),
                query: s.input.text().to_string(),
                elapsed: Duration::from_micros(300),
                result: Ok(SearchOutcome {
                    hits,
                    matched,
                    total,
                    cancelled: false,
                    unicode_fallback: false,
                }),
            }),
            Instant::now(),
        );
    }

    #[test]
    fn an_empty_app_renders_its_prompt() {
        let text = render_to_text(&state());
        assert!(text.contains("File Code Search"));
        assert!(text.contains("Type a job code to search."));
        assert!(text.contains("Enter open"));
    }

    #[test]
    fn results_are_listed_with_the_selection_marked() {
        let mut s = state();
        type_code(&mut s);
        results(
            &mut s,
            vec![hit("11d_alpha.pdf", 0), hit("11d_beta.pdf", 0)],
            2,
            500,
        );

        let text = render_to_text(&s);
        assert!(text.contains("11d_alpha.pdf"));
        assert!(text.contains("11d_beta.pdf"));
        assert!(text.contains(">>"), "the selected row should be marked");
        assert!(text.contains("Results (2)"));
    }

    /// The regression that motivated `EmptyReason`: an unreachable drive used
    /// to render as an empty list with no explanation.
    #[test]
    fn an_unreachable_drive_is_explained_on_screen() {
        let mut s = state();
        let now = Instant::now();
        let status = FlatStatus {
            health: Health::Unreachable {
                err: EnumError::Transient(53),
                since: now,
                attempt: 1,
                next_retry_at: now + Duration::from_secs(30),
            },
            ..Default::default()
        };
        s.update(AppEvent::Index(IndexMsg::Status(Arc::new(status))), now);

        let text = render_to_text(&s);
        assert!(text.contains("unreachable"), "{text}");
        assert!(text.contains("os error 53"), "{text}");
        assert!(text.contains("F5"), "{text}");
    }

    #[test]
    fn a_no_match_result_says_how_much_was_searched() {
        let mut s = state();
        type_code(&mut s);
        results(&mut s, vec![], 0, 1_284_551);
        let text = render_to_text(&s);
        assert!(text.contains("No matches among 1,284,551 files."), "{text}");
    }

    #[test]
    fn an_unrecognised_code_offers_the_expected_forms() {
        let mut s = state();
        let now = Instant::now();
        for c in "!!!!".chars() {
            s.update(
                AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                now,
            );
        }
        let text = render_to_text(&s);
        assert!(text.contains("does not look like a job code"), "{text}");
    }

    #[test]
    fn a_capped_result_list_reports_the_true_total() {
        let mut s = state();
        type_code(&mut s);
        let hits: Vec<Hit> = (0..15).map(|i| hit(&format!("f{i:02}.pdf"), 0)).collect();
        results(&mut s, hits, 4321, 9000);
        let text = render_to_text(&s);
        assert!(text.contains("Results (15 of 4,321)"), "{text}");
    }

    #[test]
    fn a_toast_is_shown() {
        let mut s = state();
        s.update(
            AppEvent::Open(crate::app::event::OpenMsg::Failed {
                path: Arc::from("V:\\a.pdf"),
                detail: "avwin.exe not found on PATH".into(),
            }),
            Instant::now(),
        );
        let text = render_to_text(&s);
        assert!(text.contains("avwin.exe not found"), "{text}");
    }

    #[test]
    fn a_missing_viewer_is_warned_about_in_the_help_line() {
        let backend = TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let s = state();
        terminal.draw(|f| draw(f, &s, true)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.get(x, y).symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("avwin.exe not found"), "{text}");
    }

    #[test]
    fn a_pinned_selection_that_moved_is_flagged() {
        let mut s = state();
        let now = Instant::now();
        type_code(&mut s);
        results(&mut s, vec![hit("a", 0), hit("b", 0), hit("c", 0)], 3, 3);
        s.update(
            AppEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            now,
        );
        results(&mut s, vec![hit("a", 0), hit("c", 0)], 2, 2);

        let text = render_to_text(&s);
        assert!(
            text.contains("results changed under the selection"),
            "{text}"
        );
    }

    #[test]
    fn rendering_a_very_narrow_terminal_does_not_panic() {
        let mut s = state();
        type_code(&mut s);
        results(
            &mut s,
            vec![hit("a_very_long_filename_indeed.pdf", 2)],
            1,
            10,
        );
        let backend = TestBackend::new(12, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &s, false)).unwrap();
    }

    #[test]
    fn rendering_non_ascii_names_does_not_panic() {
        let mut s = state();
        type_code(&mut s);
        results(
            &mut s,
            vec![hit("Écoles-Été.pdf", 0), hit("ПРИВЕТ.txt", 0)],
            2,
            2,
        );
        let text = render_to_text(&s);
        assert!(text.contains("cole") || text.contains("É"), "{text}");
    }
}
