//! Full-screen view of one conversation, moved through with vim-style keys.
//!
//! This module holds the view's position, search and key handling. `ui.rs`
//! builds the styled lines and hands them over with `Transcript::set_lines`.

use crate::session::{Role, Session};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use std::sync::Arc;

/// What the app should do after a key press in the transcript view
#[derive(Debug, PartialEq, Eq)]
pub enum TranscriptAction {
    None,
    Close,
    Quit,
    Resume,
    CopySessionId,
    CopyResumeCommand,
    ShowHelp,
    /// A search found nothing
    NotFound(String),
}

pub struct Transcript {
    pub session: Arc<Session>,
    /// First visible line
    pub top: usize,
    /// Rows the transcript area shows (set when rendering)
    pub height: usize,
    /// Styled lines (built by `ui.rs`)
    pub lines: Vec<Line<'static>>,
    /// Width and highlighted words the lines were built for
    pub lines_built_for: Option<(u16, String)>,
    /// Line where each message starts
    pub message_starts: Vec<usize>,
    /// Search words from the session list, highlighted until a transcript search replaces them
    pub list_query: String,
    /// Text being typed after `/`
    pub search_input: Option<String>,
    /// Last search made in the transcript
    pub search: String,
    /// Lines containing the last search
    pub match_lines: Vec<usize>,
    /// Position in `match_lines` of the match last jumped to
    pub current_match: Option<usize>,
    /// Message to scroll to once the lines are built
    open_at_message: Option<usize>,
    /// Plain text of each line, for searching
    line_texts: Vec<String>,
}

impl Transcript {
    pub fn new(session: Arc<Session>, list_query: String, open_at_message: Option<usize>) -> Self {
        Self {
            session,
            top: 0,
            height: 1,
            lines: Vec::new(),
            lines_built_for: None,
            message_starts: Vec::new(),
            list_query,
            search_input: None,
            search: String::new(),
            match_lines: Vec::new(),
            current_match: None,
            open_at_message,
            line_texts: Vec::new(),
        }
    }

    /// Words to highlight: the transcript search, or else the list search
    pub fn highlight_words(&self) -> &str {
        if self.search.is_empty() {
            &self.list_query
        } else {
            &self.search
        }
    }

    /// Replace the rendered lines (after a resize or a new search)
    pub fn set_lines(&mut self, lines: Vec<Line<'static>>, message_starts: Vec<usize>) {
        self.line_texts = lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        self.lines = lines;
        self.message_starts = message_starts;
        self.find_matches();
        if let Some(message) = self.open_at_message.take() {
            if let Some(&start) = self.message_starts.get(message) {
                self.top = start;
            }
        }
        self.clamp();
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    fn max_top(&self) -> usize {
        self.line_count().saturating_sub(self.height)
    }

    fn clamp(&mut self) {
        self.top = self.top.min(self.max_top());
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.top = if delta < 0 {
            self.top.saturating_sub(delta.unsigned_abs())
        } else {
            self.top.saturating_add(delta as usize)
        };
        self.clamp();
    }

    fn half_page(&self) -> isize {
        (self.height / 2).max(1) as isize
    }

    /// A full page keeps two lines of the previous page in view, as vim does
    fn full_page(&self) -> isize {
        self.height.saturating_sub(2).max(1) as isize
    }

    pub fn to_top(&mut self) {
        self.top = 0;
    }

    pub fn to_bottom(&mut self) {
        self.top = self.max_top();
    }

    /// Index of the message at the top of the screen
    pub fn current_message(&self) -> usize {
        self.message_starts
            .iter()
            .rposition(|&start| start <= self.top)
            .unwrap_or(0)
    }

    /// Scroll so the next message (optionally only your own) starts at the top
    fn next_message(&mut self, only_user: bool) {
        let target = self
            .message_starts
            .iter()
            .enumerate()
            .filter(|(i, _)| !only_user || self.is_user_message(*i))
            .map(|(_, &start)| start)
            .find(|&start| start > self.top);
        if let Some(start) = target {
            self.top = start;
            self.clamp();
        }
    }

    /// Scroll so the previous message (optionally only your own) starts at the top
    fn previous_message(&mut self, only_user: bool) {
        let target = self
            .message_starts
            .iter()
            .enumerate()
            .filter(|(i, _)| !only_user || self.is_user_message(*i))
            .map(|(_, &start)| start)
            .rfind(|&start| start < self.top);
        if let Some(start) = target {
            self.top = start;
        }
    }

    fn is_user_message(&self, index: usize) -> bool {
        self.session
            .messages
            .get(index)
            .is_some_and(|m| m.role == Role::User)
    }

    fn find_matches(&mut self) {
        let search = self.search.to_lowercase();
        self.match_lines = if search.is_empty() {
            Vec::new()
        } else {
            self.line_texts
                .iter()
                .enumerate()
                .filter(|(_, text)| text.to_lowercase().contains(&search))
                .map(|(i, _)| i)
                .collect()
        };
        if self.current_match.is_some_and(|m| m >= self.match_lines.len()) {
            self.current_match = None;
        }
    }

    /// Jump to the next (or previous) match, wrapping around the ends.
    /// Starts from the last match jumped to while it is on screen, otherwise
    /// from the top of the screen.
    fn jump_to_match(&mut self, forward: bool) -> bool {
        if self.match_lines.is_empty() {
            return false;
        }
        let visible = self.top..self.top + self.height;
        let from = self
            .current_match
            .map(|m| self.match_lines[m])
            .filter(|line| visible.contains(line));
        let found = if forward {
            self.match_lines
                .iter()
                .position(|&line| from.map_or(line >= self.top, |f| line > f))
                .unwrap_or(0)
        } else {
            self.match_lines
                .iter()
                .rposition(|&line| from.map_or(line < self.top, |f| line < f))
                .unwrap_or(self.match_lines.len() - 1)
        };
        self.current_match = Some(found);
        // Show the match a third of the way down, with context above it
        self.top = self.match_lines[found].saturating_sub(self.height / 3);
        self.clamp();
        true
    }

    pub fn on_key(&mut self, key: KeyEvent) -> TranscriptAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if ctrl && key.code == KeyCode::Char('c') {
            return TranscriptAction::Quit;
        }

        if let Some(input) = self.search_input.as_mut() {
            match key.code {
                KeyCode::Esc => self.search_input = None,
                KeyCode::Backspace => {
                    if input.pop().is_none() {
                        self.search_input = None;
                    }
                }
                KeyCode::Enter => {
                    let input = self.search_input.take().unwrap_or_default();
                    if !input.is_empty() {
                        self.search = input;
                        self.current_match = None;
                        // Lines are rebuilt with the new highlight when rendering;
                        // the text doesn't change, so matches can be found now
                        self.find_matches();
                        if !self.jump_to_match(true) {
                            return TranscriptAction::NotFound(self.search.clone());
                        }
                    }
                }
                KeyCode::Char(c) if !ctrl => input.push(c),
                _ => {}
            }
            return TranscriptAction::None;
        }

        match key.code {
            KeyCode::Esc if !self.search.is_empty() => {
                self.search.clear();
                self.find_matches();
            }
            KeyCode::Esc | KeyCode::Char('q') => return TranscriptAction::Close,
            KeyCode::Char('?') | KeyCode::F(1) => return TranscriptAction::ShowHelp,
            KeyCode::Enter => return TranscriptAction::Resume,
            KeyCode::Char('r') if ctrl => return TranscriptAction::Resume,
            KeyCode::Char('y') if ctrl => return TranscriptAction::CopyResumeCommand,
            KeyCode::Char('Y') => return TranscriptAction::CopyResumeCommand,
            KeyCode::Char('y') => return TranscriptAction::CopySessionId,

            KeyCode::Char('d') | KeyCode::Char('u') if ctrl => {
                let down = key.code == KeyCode::Char('d');
                self.scroll_by(if down { self.half_page() } else { -self.half_page() });
            }
            KeyCode::Char('f') | KeyCode::Char('b') if ctrl => {
                let down = key.code == KeyCode::Char('f');
                self.scroll_by(if down { self.full_page() } else { -self.full_page() });
            }
            KeyCode::Char('e') if ctrl => self.scroll_by(1),

            KeyCode::Up if shift => self.previous_message(false),
            KeyCode::Down if shift => self.next_message(false),
            KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('d') => self.scroll_by(self.half_page()),
            KeyCode::Char('u') => self.scroll_by(-self.half_page()),
            KeyCode::Char('f') | KeyCode::Char(' ') | KeyCode::PageDown => {
                self.scroll_by(self.full_page())
            }
            KeyCode::Char('b') | KeyCode::PageUp => self.scroll_by(-self.full_page()),
            KeyCode::Char('g') | KeyCode::Home => self.to_top(),
            KeyCode::Char('G') | KeyCode::End => self.to_bottom(),
            KeyCode::Char(']') | KeyCode::Char('J') => self.next_message(false),
            KeyCode::Char('[') | KeyCode::Char('K') => self.previous_message(false),
            KeyCode::Char('}') => self.next_message(true),
            KeyCode::Char('{') => self.previous_message(true),
            KeyCode::Char('/') => self.search_input = Some(String::new()),
            KeyCode::Char('n') | KeyCode::Char('N') => {
                let forward = key.code == KeyCode::Char('n');
                if self.search.is_empty() {
                    return TranscriptAction::None;
                }
                if !self.jump_to_match(forward) {
                    return TranscriptAction::NotFound(self.search.clone());
                }
            }
            _ => {}
        }
        TranscriptAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Message, SessionSource};
    use std::path::PathBuf;

    fn press(t: &mut Transcript, code: KeyCode) -> TranscriptAction {
        t.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn press_ctrl(t: &mut Transcript, c: char) -> TranscriptAction {
        t.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    /// 10 messages alternating You/Claude, each 10 lines long; screen of 20 rows.
    /// Message 7 contains "needle" on its third line.
    fn transcript() -> Transcript {
        let messages = (0..10)
            .map(|i| Message {
                role: if i % 2 == 0 { Role::User } else { Role::Assistant },
                content: String::new(),
                timestamp: chrono::Utc::now(),
            })
            .collect();
        let session = Session {
            id: "id".to_string(),
            source: SessionSource::ClaudeCode,
            file_path: PathBuf::new(),
            cwd: String::new(),
            git_branch: None,
            title: None,
            timestamp: chrono::Utc::now(),
            messages,
        };
        let mut t = Transcript::new(Arc::new(session), String::new(), None);
        t.height = 20;
        let lines = (0..100)
            .map(|i| {
                if i == 72 {
                    Line::raw("a Needle here")
                } else {
                    Line::raw(format!("line {}", i))
                }
            })
            .collect();
        t.set_lines(lines, (0..10).map(|i| i * 10).collect());
        t
    }

    #[test]
    fn test_line_and_page_movement() {
        let mut t = transcript();

        press(&mut t, KeyCode::Char('j'));
        assert_eq!(t.top, 1);
        press(&mut t, KeyCode::Char('k'));
        press(&mut t, KeyCode::Char('k'));
        assert_eq!(t.top, 0);

        press(&mut t, KeyCode::Char('d'));
        assert_eq!(t.top, 10);
        press_ctrl(&mut t, 'u');
        assert_eq!(t.top, 0);

        press(&mut t, KeyCode::Char(' '));
        assert_eq!(t.top, 18);
        press_ctrl(&mut t, 'b');
        assert_eq!(t.top, 0);
        press(&mut t, KeyCode::PageDown);
        assert_eq!(t.top, 18);
    }

    #[test]
    fn test_top_and_bottom() {
        let mut t = transcript();

        press(&mut t, KeyCode::Char('G'));
        assert_eq!(t.top, 80);
        press(&mut t, KeyCode::Char('j'));
        assert_eq!(t.top, 80, "can't scroll past the last screen");
        press(&mut t, KeyCode::Char('g'));
        assert_eq!(t.top, 0);
        press(&mut t, KeyCode::End);
        assert_eq!(t.top, 80);
        press(&mut t, KeyCode::Home);
        assert_eq!(t.top, 0);
    }

    #[test]
    fn test_message_jumps() {
        let mut t = transcript();
        t.top = 15;

        press(&mut t, KeyCode::Char(']'));
        assert_eq!(t.top, 20);
        assert_eq!(t.current_message(), 2);
        press(&mut t, KeyCode::Char('['));
        assert_eq!(t.top, 10);
        t.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(t.top, 20);

        // { } skip to your own messages (even-numbered here)
        t.top = 0;
        press(&mut t, KeyCode::Char('}'));
        assert_eq!(t.top, 20);
        press(&mut t, KeyCode::Char('}'));
        assert_eq!(t.top, 40);
        press(&mut t, KeyCode::Char('{'));
        assert_eq!(t.top, 20);
    }

    #[test]
    fn test_search_jumps_to_match_and_wraps() {
        let mut t = transcript();

        press(&mut t, KeyCode::Char('/'));
        for c in "needle".chars() {
            press(&mut t, KeyCode::Char(c));
        }
        assert_eq!(t.top, 0, "doesn't move while typing");
        press(&mut t, KeyCode::Enter);

        assert_eq!(t.search, "needle");
        assert_eq!(t.match_lines, vec![72]);
        assert_eq!(t.top, 72 - 20 / 3);
        assert_eq!(press(&mut t, KeyCode::Char('n')), TranscriptAction::None);
        assert_eq!(t.top, 72 - 20 / 3, "only match: wraps to itself");
    }

    #[test]
    fn test_search_not_found() {
        let mut t = transcript();
        press(&mut t, KeyCode::Char('/'));
        press(&mut t, KeyCode::Char('z'));

        assert_eq!(
            press(&mut t, KeyCode::Enter),
            TranscriptAction::NotFound("z".to_string())
        );
        assert_eq!(t.top, 0);
    }

    #[test]
    fn test_typing_search_doesnt_trigger_keys() {
        let mut t = transcript();
        press(&mut t, KeyCode::Char('/'));

        assert_eq!(press(&mut t, KeyCode::Char('q')), TranscriptAction::None);
        press(&mut t, KeyCode::Char('G'));
        assert_eq!(t.top, 0);
        assert_eq!(t.search_input.as_deref(), Some("qG"));

        press(&mut t, KeyCode::Esc);
        assert_eq!(t.search_input, None);
        assert_eq!(t.search, "");
    }

    #[test]
    fn test_esc_clears_search_before_closing() {
        let mut t = transcript();
        t.search = "needle".to_string();

        assert_eq!(press(&mut t, KeyCode::Esc), TranscriptAction::None);
        assert_eq!(t.search, "");
        assert_eq!(press(&mut t, KeyCode::Esc), TranscriptAction::Close);
    }

    #[test]
    fn test_actions() {
        let mut t = transcript();
        assert_eq!(press(&mut t, KeyCode::Char('q')), TranscriptAction::Close);
        assert_eq!(press(&mut t, KeyCode::Enter), TranscriptAction::Resume);
        assert_eq!(press(&mut t, KeyCode::Char('y')), TranscriptAction::CopySessionId);
        assert_eq!(press(&mut t, KeyCode::Char('Y')), TranscriptAction::CopyResumeCommand);
        assert_eq!(press_ctrl(&mut t, 'y'), TranscriptAction::CopyResumeCommand);
        assert_eq!(press(&mut t, KeyCode::Char('?')), TranscriptAction::ShowHelp);
        assert_eq!(press_ctrl(&mut t, 'c'), TranscriptAction::Quit);
    }

    #[test]
    fn test_opens_at_message() {
        let session = transcript().session.clone();
        let mut t = Transcript::new(session, String::new(), Some(3));
        t.height = 20;
        t.set_lines((0..100).map(|i| Line::raw(i.to_string())).collect(), (0..10).map(|i| i * 10).collect());

        assert_eq!(t.top, 30);
    }

    #[test]
    fn test_short_transcript_does_not_scroll() {
        let mut t = transcript();
        t.height = 200;
        t.clamp();

        press(&mut t, KeyCode::Char('G'));
        assert_eq!(t.top, 0);
    }
}
