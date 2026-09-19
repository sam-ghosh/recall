use crate::index::{
    discover_and_sort_files, index_files, plan_update, IndexProgress, IndexState, SearchFilter,
    SessionIndex,
};
use crate::config::Config;
use crate::parser;
use crate::project::project_root;
use crate::session::{SearchResult, Session, SessionSource};
use crate::time::split_query_dates;
use crate::transcript::{Transcript, TranscriptAction};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// Debounce delay for search (avoid searching on every keystroke during fast typing/paste)
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(50);

/// How many sessions the list shows at most
const RESULT_LIMIT: usize = 200;

/// How long a status bar message (e.g. "Copied session ID") stays
const FLASH_DURATION: Duration = Duration::from_secs(3);

/// Order Ctrl+S steps through the tool filter (None = all tools)
const SOURCE_FILTER_ORDER: &[Option<SessionSource>] = &[
    None,
    Some(SessionSource::ClaudeCode),
    Some(SessionSource::CodexCli),
    Some(SessionSource::Factory),
    Some(SessionSource::OpenCode),
];

/// A parsed session file, kept until the file changes
struct ParsedSession {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    session: Arc<Session>,
}

/// Messages from the indexing thread
pub enum IndexMsg {
    Progress { indexed: usize, total: usize },
    Done { total_sessions: usize },
    NeedsReload,
    Error(String),
}

/// Search scope
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScope {
    /// Search all conversations
    Everything,
    /// Search only conversations from a project: the folder, folders inside
    /// it, and its git worktrees (see `project::project_root`)
    Project(String),
}

/// Whether keys are commands or go into the search box (like nvim's normal
/// and insert modes)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Letters are commands: j/k move, g/G first/last, / starts a search
    Normal,
    /// Letters are typed into the search box, until Enter or Esc
    Search,
}

/// Which side of the session list screen receives movement keys
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    List,
    Preview,
}

pub struct App {
    /// Current search query
    pub query: String,
    /// Cursor position in query (char index)
    pub cursor: usize,
    /// Search results
    pub results: Vec<SearchResult>,
    /// Selected result index
    pub selected: usize,
    /// Results list scroll offset
    pub list_scroll: usize,
    /// Preview scroll offset
    pub preview_scroll: usize,
    /// Currently focused message index in preview (None = auto-focus on matched message)
    pub focused_message: Option<usize>,
    /// Set of expanded message indices (shown in full, not truncated)
    pub expanded_messages: HashSet<usize>,
    /// Total message count in current preview (for navigation bounds)
    pub preview_message_count: usize,
    /// Whether the focused message can be expanded/collapsed
    pub focused_message_expandable: bool,
    /// Line ranges for each message in preview (start_line, end_line) for mouse click mapping
    pub message_line_ranges: Vec<(usize, usize)>,
    /// Preview area bounds (x, y, width, height) for mouse hit testing
    pub preview_area: (u16, u16, u16, u16),
    /// Whether to auto-scroll preview to matched message
    pub pending_auto_scroll: bool,
    /// Whether preview has more content than visible (for scroll hint)
    pub preview_scrollable: bool,
    /// Whether keys are commands or search text
    pub input_mode: InputMode,
    /// Pane that movement keys act on (switched with Tab or Ctrl+W w)
    pub focused_pane: Pane,
    /// Ctrl+W was pressed and the next key picks a pane
    pending_window_key: bool,
    /// Whether the keyboard shortcuts panel is open
    pub show_help: bool,
    /// Lines the shortcuts panel is scrolled down by
    pub help_scroll: usize,
    /// Full-screen view of the selected conversation, when open
    pub transcript: Option<Transcript>,
    /// Should quit
    pub should_quit: bool,
    /// Should execute resume (set on Ctrl+R)
    pub should_resume: Option<Session>,
    /// Text for the main loop to copy, and what to call it in the status bar
    pub pending_copy: Option<(String, &'static str)>,
    /// Short status bar message and when it was set
    flash: Option<(String, Instant)>,
    /// Only show sessions from this tool (None = all)
    pub source_filter: Option<SessionSource>,
    /// Last session file parsed for the preview
    parsed_session: Option<ParsedSession>,
    /// Index for searching
    index: SessionIndex,
    /// Status message (for indexing progress, etc.)
    pub status: Option<String>,
    /// Total sessions indexed
    pub total_sessions: usize,
    /// Channel to receive indexing updates
    index_rx: Option<Receiver<IndexMsg>>,
    /// Is indexing in progress
    pub indexing: bool,
    /// Current search scope
    pub search_scope: SearchScope,
    /// Project of the launch directory (for project-scoped search)
    pub launch_project: String,
    /// Result rows visible in the list, set when rendering (for page up/down)
    pub list_page_size: usize,
    /// Whether a search is pending (for debouncing)
    search_pending: bool,
    /// When the last input occurred (for debouncing)
    last_input: Instant,
    /// Error from indexing thread (shown on exit)
    pub index_error: Option<String>,
}

impl App {
    pub fn new(initial_query: String) -> Result<Self> {
        // Allow override for testing
        let cache_dir = std::env::var("RECALL_HOME_OVERRIDE")
            .map(|h| PathBuf::from(h).join(".cache").join("recall"))
            .unwrap_or_else(|_| {
                dirs::cache_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("recall")
            });

        let index_path = cache_dir.join("index");
        let state_path = cache_dir.join("state.json");

        let index = SessionIndex::open_or_create(&index_path)?;

        // Get launch directory (override for tests)
        let launch_cwd = std::env::var("RECALL_CWD_OVERRIDE").unwrap_or_else(|_| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        });

        // Start background indexing
        let (tx, rx) = mpsc::channel();
        let index_path_clone = index_path.clone();
        thread::spawn(move || {
            background_index(index_path_clone, state_path, tx);
        });

        let initial_cursor = initial_query.chars().count();
        let mut app = Self {
            query: initial_query,
            cursor: initial_cursor,
            results: Vec::new(),
            selected: 0,
            list_scroll: 0,
            preview_scroll: 0,
            focused_message: None,
            expanded_messages: HashSet::new(),
            preview_message_count: 0,
            focused_message_expandable: false,
            message_line_ranges: Vec::new(),
            preview_area: (0, 0, 0, 0),
            pending_auto_scroll: false,
            preview_scrollable: false,
            input_mode: InputMode::Normal,
            focused_pane: Pane::List,
            pending_window_key: false,
            show_help: false,
            help_scroll: 0,
            transcript: None,
            should_quit: false,
            should_resume: None,
            pending_copy: None,
            flash: None,
            source_filter: None,
            parsed_session: None,
            index,
            status: None,
            total_sessions: 0,
            index_rx: Some(rx),
            indexing: true,
            search_scope: SearchScope::Project(project_root(&launch_cwd)),
            launch_project: project_root(&launch_cwd),
            list_page_size: 10,
            search_pending: false,
            last_input: Instant::now(),
            index_error: None,
        };

        // If there's an initial query, run the search immediately
        if !app.query.is_empty() {
            let _ = app.search();
        }

        Ok(app)
    }

    /// Check for indexing updates (call this in the main loop)
    pub fn poll_index_updates(&mut self) {
        use std::sync::mpsc::TryRecvError;

        let Some(rx) = &self.index_rx else {
            return;
        };

        // Collect messages, tracking if channel was disconnected
        let mut messages = Vec::new();
        let mut channel_disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(msg) => messages.push(msg),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    channel_disconnected = true;
                    break;
                }
            }
        }

        let mut should_close_rx = false;
        let mut needs_reload = false;
        let mut needs_search = false;

        for msg in messages {
            match msg {
                IndexMsg::Progress { indexed, total } => {
                    self.status = Some(format!("Indexing {}/{}...", indexed, total));
                    self.total_sessions = indexed;
                }
                IndexMsg::NeedsReload => {
                    needs_reload = true;
                    needs_search = true;
                }
                IndexMsg::Done { total_sessions } => {
                    self.total_sessions = total_sessions;
                    self.status = None;
                    self.indexing = false;
                    should_close_rx = true;
                    needs_reload = true;
                    needs_search = true;
                }
                IndexMsg::Error(err) => {
                    self.index_error = Some(err);
                    self.status = Some("Index error • Ctrl+C for details".to_string());
                    self.indexing = false;
                    should_close_rx = true;
                }
            }
        }

        // Detect unexpected indexer death (channel closed without Done/Error)
        if channel_disconnected && self.indexing {
            self.index_error = Some("Indexer stopped unexpectedly (possible crash)".to_string());
            self.status = Some("Index error • Ctrl+C for details".to_string());
            self.indexing = false;
            should_close_rx = true;
        }

        if needs_reload {
            let _ = self.index.reload();
        }
        if needs_search {
            let _ = self.search();
        }
        if should_close_rx {
            self.index_rx = None;
        }
    }

    /// Perform a search (or show recent sessions if query is empty)
    pub fn search(&mut self) -> Result<()> {
        // Remember currently selected session to preserve selection
        let selected_session_id = self.results.get(self.selected).map(|r| r.session.id.clone());

        let dates = split_query_dates(&self.query);
        let filter = SearchFilter {
            project: match &self.search_scope {
                SearchScope::Project(root) => Some(root.clone()),
                SearchScope::Everything => None,
            },
            source: self.source_filter,
            since: dates.since,
            until: dates.until,
        };

        self.results = if dates.text.is_empty() {
            self.index.recent(RESULT_LIMIT, &filter)?
        } else {
            self.index.search(&dates.text, RESULT_LIMIT, &filter)?
        };

        // Try to preserve selection on the same session
        if let Some(ref id) = selected_session_id {
            if let Some(pos) = self.results.iter().position(|r| &r.session.id == id) {
                self.selected = pos;
                // Scroll to keep selection visible (at top of list area)
                self.list_scroll = pos;
            } else {
                self.selected = 0;
                self.list_scroll = 0;
            }
        } else {
            self.selected = 0;
            self.list_scroll = 0;
        }
        self.update_preview_scroll();

        Ok(())
    }

    /// Toggle search scope between everything and the current project
    pub fn toggle_scope(&mut self) {
        self.search_scope = match self.search_scope {
            SearchScope::Everything => SearchScope::Project(self.launch_project.clone()),
            SearchScope::Project(_) => SearchScope::Everything,
        };
        let _ = self.search();
    }

    /// Step the tool filter: all → Claude → Codex → Factory → OpenCode → all
    pub fn cycle_source_filter(&mut self) {
        let position = SOURCE_FILTER_ORDER
            .iter()
            .position(|s| *s == self.source_filter)
            .unwrap_or(0);
        self.source_filter = SOURCE_FILTER_ORDER[(position + 1) % SOURCE_FILTER_ORDER.len()];
        let _ = self.search();
    }

    /// The search box text without date words (`since:2w`), for highlighting
    pub fn search_words(&self) -> String {
        split_query_dates(&self.query).text
    }

    /// Show a short message in the status bar
    pub fn set_flash(&mut self, message: impl Into<String>) {
        self.flash = Some((message.into(), Instant::now()));
    }

    /// The status bar message, while it is recent
    pub fn flash_message(&self) -> Option<&str> {
        self.flash
            .as_ref()
            .filter(|(_, at)| at.elapsed() < FLASH_DURATION)
            .map(|(message, _)| message.as_str())
    }

    /// Parse a session file, reusing the last result while the file is unchanged
    pub fn load_session(&mut self, path: &Path) -> Option<Arc<Session>> {
        let metadata = std::fs::metadata(path).ok();
        let modified = metadata.as_ref().and_then(|m| m.modified().ok());
        let len = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        if let Some(parsed) = &self.parsed_session {
            if parsed.path == path && parsed.modified == modified && parsed.len == len {
                return Some(parsed.session.clone());
            }
        }
        let session = Arc::new(parser::parse_session_file(path).ok()?);
        self.parsed_session = Some(ParsedSession {
            path: path.to_path_buf(),
            modified,
            len,
            session: session.clone(),
        });
        Some(session)
    }

    /// Open the selected conversation full screen: at the message focused in
    /// the preview when the preview has focus, at the matched message when
    /// searching, otherwise at the top
    pub fn open_transcript(&mut self) {
        let Some(result) = self.results.get(self.selected) else {
            return;
        };
        let path = result.session.file_path.clone();
        let matched = result.matched_message_index;
        let words = self.search_words();
        let open_at = if self.focused_pane == Pane::Preview {
            Some(self.focused_message.unwrap_or(matched))
        } else {
            (!words.is_empty()).then_some(matched)
        };
        match self.load_session(&path) {
            Some(session) => self.transcript = Some(Transcript::new(session, words, open_at)),
            None => self.set_flash("Could not read this session file"),
        }
    }

    /// The session shown in the transcript view, or else the selected one
    fn current_session(&self) -> Option<Session> {
        match &self.transcript {
            Some(transcript) => Some((*transcript.session).clone()),
            None => self.results.get(self.selected).map(|r| r.session.clone()),
        }
    }

    fn copy_session_id(&mut self) {
        if let Some(session) = self.current_session() {
            self.pending_copy = Some((session.id, "session ID"));
        }
    }

    fn copy_resume_command(&mut self) {
        if let Some(session) = self.current_session() {
            self.pending_copy = Some((session.resume_shell_command(), "resume command"));
        }
    }

    fn on_transcript_key(&mut self, key: KeyEvent) {
        let Some(transcript) = self.transcript.as_mut() else {
            return;
        };
        match transcript.on_key(key) {
            TranscriptAction::None => {}
            TranscriptAction::Close => self.transcript = None,
            TranscriptAction::Quit => self.should_quit = true,
            TranscriptAction::Resume => {
                self.should_resume = Some((*transcript.session).clone());
            }
            TranscriptAction::CopySessionId => self.copy_session_id(),
            TranscriptAction::CopyResumeCommand => self.copy_resume_command(),
            TranscriptAction::ShowHelp => self.open_help(),
            TranscriptAction::NotFound(search) => self.set_flash(format!("Not found: {}", search)),
        }
    }

    fn open_help(&mut self) {
        self.show_help = true;
        self.help_scroll = 0;
    }

    /// Get the folder name for display (last component of path)
    pub fn scope_folder_name(&self) -> Option<&str> {
        match &self.search_scope {
            SearchScope::Everything => None,
            SearchScope::Project(path) => {
                path.rsplit(std::path::MAIN_SEPARATOR).next()
            }
        }
    }

    /// Get a compact display path for the scope
    /// - Replaces home dir with ~
    /// - If short enough, shows full path
    /// - Otherwise shows ~/.../<dir> or /.../<dir>
    pub fn scope_display_path(&self) -> Option<String> {
        let path = match &self.search_scope {
            SearchScope::Everything => return None,
            SearchScope::Project(path) => path.as_str(),
        };

        // Replace home dir with ~ (HOME on Unix, USERPROFILE on Windows)
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        let display_path = if !home.is_empty() && path.starts_with(&home) {
            format!("~{}", &path[home.len()..])
        } else {
            path.to_string()
        };

        // If short enough, show full path
        const MAX_LEN: usize = 25;
        if display_path.len() <= MAX_LEN {
            return Some(display_path);
        }

        // Otherwise show prefix/.../<last_dir>
        let last_component = path.rsplit(std::path::MAIN_SEPARATOR).next().unwrap_or(path);
        let prefix = if display_path.starts_with('~') { "~" } else { "" };
        Some(format!("{}/.../{}", prefix, last_component))
    }

    /// Handle a key press. The shortcuts panel (`ui::SHORTCUTS`) lists these.
    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if ctrl && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        if self.show_help {
            match key.code {
                KeyCode::Esc
                | KeyCode::Enter
                | KeyCode::F(1)
                | KeyCode::Char('?')
                | KeyCode::Char('q') => self.show_help = false,
                KeyCode::Down | KeyCode::Char('j') => self.help_scroll += 1,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                _ => {}
            }
            return;
        }

        if self.transcript.is_some() {
            self.on_transcript_key(key);
            return;
        }

        // Keys that do the same in every mode and pane
        match key.code {
            KeyCode::F(1) => return self.open_help(),
            KeyCode::Char('r') if ctrl => return self.on_resume(),
            KeyCode::Char('y') if ctrl => return self.copy_resume_command(),
            KeyCode::Char('s') if ctrl => return self.cycle_source_filter(),
            _ => {}
        }

        match self.input_mode {
            InputMode::Search => self.on_search_key(key),
            InputMode::Normal => self.on_normal_key(key),
        }
    }

    /// Search mode: keys edit the search box; arrows still move the selection
    fn on_search_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.input_mode = InputMode::Normal,
            KeyCode::Char('a') if ctrl => self.on_home(),
            KeyCode::Char('e') if ctrl => self.on_end(),
            KeyCode::Char('u') if ctrl => self.delete_to_start(),
            KeyCode::Char('w') if ctrl => self.delete_previous_word(),
            KeyCode::Char('n') if ctrl => self.on_down(),
            KeyCode::Char('p') if ctrl => self.on_up(),
            KeyCode::Up => self.on_up(),
            KeyCode::Down => self.on_down(),
            KeyCode::Left => self.on_left(),
            KeyCode::Right => self.on_right(),
            KeyCode::Home => self.on_home(),
            KeyCode::End => self.on_end(),
            KeyCode::Delete => self.on_delete(),
            KeyCode::Backspace => self.on_backspace(),
            KeyCode::Char(c) if !ctrl => self.on_char(c),
            _ => {}
        }
    }

    /// Normal mode: letters are commands, acting on the focused pane
    fn on_normal_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if self.pending_window_key {
            self.pending_window_key = false;
            match key.code {
                KeyCode::Char('w') | KeyCode::Char('p') => self.switch_pane(),
                KeyCode::Char('l') | KeyCode::Right => self.focused_pane = Pane::Preview,
                KeyCode::Char('h') | KeyCode::Left => self.focused_pane = Pane::List,
                _ => {}
            }
            return;
        }

        // Keys that do the same in both panes
        match key.code {
            KeyCode::Char('w') if ctrl => return self.pending_window_key = true,
            KeyCode::Tab | KeyCode::BackTab => return self.switch_pane(),
            KeyCode::Char('?') => return self.open_help(),
            KeyCode::Char('/') | KeyCode::Char('i') | KeyCode::Char('a') if !ctrl => {
                self.focused_pane = Pane::List;
                self.input_mode = InputMode::Search;
                self.on_end();
                return;
            }
            KeyCode::Enter => return self.open_transcript(),
            KeyCode::Char('s') if !ctrl => return self.toggle_scope(),
            KeyCode::Char('t') => return self.cycle_source_filter(),
            KeyCode::Char('y') if !ctrl => return self.copy_session_id(),
            KeyCode::Char('Y') => return self.copy_resume_command(),
            KeyCode::Up if shift => return self.focus_prev_message(),
            KeyCode::Down if shift => return self.focus_next_message(),
            _ => {}
        }

        match self.focused_pane {
            Pane::List => match key.code {
                KeyCode::Esc => self.on_escape(),
                KeyCode::Char('j') | KeyCode::Down => self.on_down(),
                KeyCode::Char('k') | KeyCode::Up => self.on_up(),
                KeyCode::Char('g') | KeyCode::Home => self.select_first(),
                KeyCode::Char('G') | KeyCode::End => self.select_last(),
                KeyCode::Char('d') if ctrl => self.on_half_page_down(),
                KeyCode::Char('u') if ctrl => self.on_half_page_up(),
                KeyCode::Char('f') if ctrl => self.on_page_down(),
                KeyCode::Char('b') if ctrl => self.on_page_up(),
                KeyCode::PageDown => self.on_page_down(),
                KeyCode::PageUp => self.on_page_up(),
                KeyCode::Char('e') if ctrl => self.toggle_focused_expansion(),
                KeyCode::Char(']') | KeyCode::Char('J') => self.focus_next_message(),
                KeyCode::Char('[') | KeyCode::Char('K') => self.focus_prev_message(),
                _ => {}
            },
            Pane::Preview => {
                let height = (self.preview_area.3 as usize).max(1);
                match key.code {
                    KeyCode::Esc => self.focused_pane = Pane::List,
                    KeyCode::Char('j') | KeyCode::Down => self.scroll_preview_down(1),
                    KeyCode::Char('k') | KeyCode::Up => self.scroll_preview_up(1),
                    KeyCode::Char('d') if ctrl => self.scroll_preview_down(height / 2),
                    KeyCode::Char('u') if ctrl => self.scroll_preview_up(height / 2),
                    KeyCode::Char('f') if ctrl => self.scroll_preview_down(height.saturating_sub(2)),
                    KeyCode::Char('b') if ctrl => self.scroll_preview_up(height.saturating_sub(2)),
                    KeyCode::PageDown => self.scroll_preview_down(height.saturating_sub(2)),
                    KeyCode::PageUp => self.scroll_preview_up(height.saturating_sub(2)),
                    KeyCode::Char('g') | KeyCode::Home => self.focus_message(0),
                    KeyCode::Char('G') | KeyCode::End => {
                        self.focus_message(self.preview_message_count.saturating_sub(1))
                    }
                    KeyCode::Char(']') | KeyCode::Char('J') => self.focus_next_message(),
                    KeyCode::Char('[') | KeyCode::Char('K') => self.focus_prev_message(),
                    KeyCode::Char('e') if ctrl => self.toggle_focused_expansion(),
                    KeyCode::Char('o') => self.toggle_focused_expansion(),
                    _ => {}
                }
            }
        }
    }

    /// Move focus between the session list and the preview
    pub fn switch_pane(&mut self) {
        self.focused_pane = match self.focused_pane {
            Pane::List if !self.results.is_empty() => Pane::Preview,
            _ => Pane::List,
        };
    }

    /// Focus a message in the preview and scroll to it
    pub fn focus_message(&mut self, index: usize) {
        if self.preview_message_count == 0 {
            return;
        }
        self.focused_message = Some(index.min(self.preview_message_count - 1));
        self.pending_auto_scroll = true;
    }

    /// Delete the search text before the cursor (Ctrl+U)
    fn delete_to_start(&mut self) {
        let byte_pos = self.cursor_byte_pos();
        if byte_pos > 0 {
            self.query.replace_range(..byte_pos, "");
            self.cursor = 0;
            self.mark_search_pending();
        }
    }

    /// Delete the word before the cursor (Ctrl+W)
    fn delete_previous_word(&mut self) {
        let byte_pos = self.cursor_byte_pos();
        let before = &self.query[..byte_pos];
        let word_start = before.trim_end().rfind(' ').map(|i| i + 1).unwrap_or(0);
        if word_start < byte_pos {
            let removed = self.query[word_start..byte_pos].chars().count();
            self.query.replace_range(word_start..byte_pos, "");
            self.cursor -= removed;
            self.mark_search_pending();
        }
    }

    /// Handle character input
    pub fn on_char(&mut self, c: char) {
        // Insert at cursor position
        let byte_pos = self.cursor_byte_pos();
        self.query.insert(byte_pos, c);
        self.cursor += 1;
        self.mark_search_pending();
    }

    /// Handle backspace
    pub fn on_backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let byte_pos = self.cursor_byte_pos();
            self.query.remove(byte_pos);
            self.mark_search_pending();
        }
    }

    /// Handle delete key
    pub fn on_delete(&mut self) {
        let char_count = self.query.chars().count();
        if self.cursor < char_count {
            let byte_pos = self.cursor_byte_pos();
            self.query.remove(byte_pos);
            self.mark_search_pending();
        }
    }

    /// Clear search
    /// Clear the search. Never quits: only Ctrl+C does.
    pub fn on_escape(&mut self) {
        if !self.query.is_empty() {
            self.query.clear();
            self.cursor = 0;
            self.mark_search_pending();
        }
    }

    /// Move cursor left
    pub fn on_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move cursor right
    pub fn on_right(&mut self) {
        let char_count = self.query.chars().count();
        if self.cursor < char_count {
            self.cursor += 1;
        }
    }

    /// Move cursor to start
    pub fn on_home(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to end
    pub fn on_end(&mut self) {
        self.cursor = self.query.chars().count();
    }

    /// Convert cursor (char index) to byte position
    fn cursor_byte_pos(&self) -> usize {
        self.query.char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.query.len())
    }

    /// Mark that a search is needed (debounced)
    fn mark_search_pending(&mut self) {
        self.search_pending = true;
        self.last_input = Instant::now();
    }

    /// Check if debounce period has elapsed and trigger search if needed
    pub fn maybe_search(&mut self) {
        if self.search_pending && self.last_input.elapsed() >= SEARCH_DEBOUNCE {
            self.search_pending = false;
            let _ = self.search();
        }
    }

    /// Force any pending search to run immediately (for tests)
    pub fn flush_pending_search(&mut self) {
        if self.search_pending {
            self.search_pending = false;
            let _ = self.search();
        }
    }

    /// Move selection up
    pub fn on_up(&mut self) {
        self.move_selection(-1);
    }

    /// Move selection down
    pub fn on_down(&mut self) {
        self.move_selection(1);
    }

    /// Move selection up by one screen of results
    pub fn on_page_up(&mut self) {
        self.move_selection(-(self.list_page_size.max(1) as isize));
    }

    /// Move selection down by one screen of results
    pub fn on_page_down(&mut self) {
        self.move_selection(self.list_page_size.max(1) as isize);
    }

    /// Move selection up by half a screen of results
    pub fn on_half_page_up(&mut self) {
        self.move_selection(-((self.list_page_size / 2).max(1) as isize));
    }

    /// Move selection down by half a screen of results
    pub fn on_half_page_down(&mut self) {
        self.move_selection((self.list_page_size / 2).max(1) as isize);
    }

    /// Select the first result
    pub fn select_first(&mut self) {
        self.move_selection(isize::MIN);
    }

    /// Select the last result
    pub fn select_last(&mut self) {
        self.move_selection(isize::MAX);
    }

    /// Move selection by `delta` rows, stopping at the first and last result
    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let last = self.results.len() - 1;
        let target = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta as usize).min(last)
        };
        if target != self.selected {
            self.selected = target;
            self.update_preview_scroll();
        }
    }

    /// Resume the selected conversation
    pub fn on_resume(&mut self) {
        let Some(path) = self.results.get(self.selected).map(|r| r.session.file_path.clone())
        else {
            return;
        };
        match self.load_session(&path) {
            Some(session) => self.should_resume = Some((*session).clone()),
            None => self.set_flash("Could not read this session file"),
        }
    }

    /// Update preview scroll to show the matched message
    fn update_preview_scroll(&mut self) {
        // Signal that we need to auto-scroll to the matched message
        // The actual scroll position is calculated in render_preview
        // since it depends on wrapped line counts
        self.pending_auto_scroll = true;
        self.preview_scroll = 0;
        // Reset focus and expansions when switching sessions
        self.focused_message = None;
        self.expanded_messages.clear();
    }

    /// Scroll preview up
    pub fn scroll_preview_up(&mut self, lines: usize) {
        self.preview_scroll = self.preview_scroll.saturating_sub(lines);
    }

    /// Scroll preview down
    pub fn scroll_preview_down(&mut self, lines: usize) {
        self.preview_scroll = self.preview_scroll.saturating_add(lines);
    }

    /// Navigate to previous message in preview
    pub fn focus_prev_message(&mut self) {
        if self.preview_message_count == 0 {
            return;
        }
        let matched_idx = self
            .selected_result()
            .map(|r| r.matched_message_index)
            .unwrap_or(0);
        let current = self.focused_message.unwrap_or(matched_idx);
        if current > 0 {
            self.focused_message = Some(current - 1);
            self.pending_auto_scroll = true;
        }
    }

    /// Navigate to next message in preview
    pub fn focus_next_message(&mut self) {
        if self.preview_message_count == 0 {
            return;
        }
        let matched_idx = self
            .selected_result()
            .map(|r| r.matched_message_index)
            .unwrap_or(0);
        let current = self.focused_message.unwrap_or(matched_idx);
        if current + 1 < self.preview_message_count {
            self.focused_message = Some(current + 1);
            self.pending_auto_scroll = true;
        }
    }

    /// Toggle expansion of the focused message
    pub fn toggle_focused_expansion(&mut self) {
        if self.preview_message_count == 0 {
            return;
        }
        let matched_idx = self
            .selected_result()
            .map(|r| r.matched_message_index)
            .unwrap_or(0);
        let focused = self.focused_message.unwrap_or(matched_idx);
        if self.expanded_messages.contains(&focused) {
            self.expanded_messages.remove(&focused);
        } else {
            self.expanded_messages.insert(focused);
        }
    }

    /// Get the currently selected result
    pub fn selected_result(&self) -> Option<&SearchResult> {
        self.results.get(self.selected)
    }

    /// Handle mouse click in preview area - returns true if a message was clicked
    pub fn click_preview_message(&mut self, x: u16, y: u16) -> bool {
        let (px, py, pw, ph) = self.preview_area;

        // Check if click is within preview bounds
        if x < px || x >= px + pw || y < py || y >= py + ph {
            return false;
        }

        // Calculate which line was clicked (accounting for scroll)
        let clicked_line = (y - py) as usize + self.preview_scroll;

        // Find which message contains this line
        for (msg_idx, &(start, end)) in self.message_line_ranges.iter().enumerate() {
            if clicked_line >= start && clicked_line < end {
                self.focused_message = Some(msg_idx);
                return true;
            }
        }

        false
    }
}

/// Background indexing function
fn background_index(index_path: PathBuf, state_path: PathBuf, tx: Sender<IndexMsg>) {
    let index = match SessionIndex::open_or_create(&index_path) {
        Ok(idx) => idx,
        Err(e) => {
            let _ = tx.send(IndexMsg::Error(format!("Failed to open index: {}", e)));
            return;
        }
    };
    let mut state = match IndexState::load(&state_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(IndexMsg::Error(format!("Failed to load index state: {}", e)));
            return;
        }
    };

    // Discover and sort files by mtime (most recent first)
    let config = Config::load();
    let files = discover_and_sort_files(&config);

    let update = plan_update(&state, &files, &config);

    if update.is_empty() {
        let _ = tx.send(IndexMsg::Done {
            total_sessions: files.len(),
        });
        return;
    }

    let mut writer = match index.writer() {
        Ok(w) => w,
        Err(e) => {
            let _ = tx.send(IndexMsg::Error(format!("Failed to create index writer: {}", e)));
            return;
        }
    };

    // Progress callback sends to channel
    let tx_progress = tx.clone();
    let on_progress = Box::new(move |p: IndexProgress| {
        let _ = tx_progress.send(IndexMsg::Progress {
            indexed: p.indexed,
            total: p.total,
        });
    });

    // Reload callback sends to channel
    let tx_reload = tx.clone();
    let on_reload = Box::new(move || {
        let _ = tx_reload.send(IndexMsg::NeedsReload);
    });

    let result = index_files(
        &index,
        &mut writer,
        &mut state,
        &config,
        &update,
        Some(on_progress),
        Some(on_reload),
    );

    if let Err(e) = result {
        let _ = tx.send(IndexMsg::Error(format!("Indexing failed: {}", e)));
        return;
    }

    let _ = state.save(&state_path);

    let _ = tx.send(IndexMsg::Done {
        total_sessions: files.len(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a minimal App for testing navigation/expansion features
    /// This bypasses the index initialization for unit tests
    fn test_app() -> App {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);
        let test_id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let index_path = std::env::temp_dir().join(format!("recall_test_index_{}", test_id));

        App {
            query: String::new(),
            cursor: 0,
            results: Vec::new(),
            selected: 0,
            list_scroll: 0,
            preview_scroll: 0,
            focused_message: None,
            expanded_messages: HashSet::new(),
            preview_message_count: 0,
            focused_message_expandable: false,
            message_line_ranges: Vec::new(),
            preview_area: (0, 0, 0, 0),
            pending_auto_scroll: false,
            preview_scrollable: false,
            input_mode: InputMode::Normal,
            focused_pane: Pane::List,
            pending_window_key: false,
            show_help: false,
            help_scroll: 0,
            transcript: None,
            should_quit: false,
            should_resume: None,
            pending_copy: None,
            flash: None,
            source_filter: None,
            parsed_session: None,
            index: SessionIndex::open_or_create(&index_path).unwrap(),
            status: None,
            total_sessions: 0,
            index_rx: None,
            indexing: false,
            search_scope: SearchScope::Everything,
            launch_project: String::new(),
            list_page_size: 10,
            search_pending: false,
            last_input: Instant::now(),
            index_error: None,
        }
    }

    // ==================== focus_prev_message tests ====================

    #[test]
    fn test_focus_prev_at_first_message_stays() {
        let mut app = test_app();
        app.preview_message_count = 5;
        app.focused_message = Some(0);

        app.focus_prev_message();

        assert_eq!(app.focused_message, Some(0));
    }

    #[test]
    fn test_focus_prev_moves_up() {
        let mut app = test_app();
        app.preview_message_count = 5;
        app.focused_message = Some(3);

        app.focus_prev_message();

        assert_eq!(app.focused_message, Some(2));
    }

    #[test]
    fn test_focus_prev_from_none_at_zero_stays_none() {
        let mut app = test_app();
        app.preview_message_count = 5;
        app.focused_message = None;
        // When focused_message is None and no result, defaults to 0
        // Moving prev from 0 does nothing (already at first)

        app.focus_prev_message();

        // Stays None because we couldn't move (already at first message)
        assert_eq!(app.focused_message, None);
    }

    #[test]
    fn test_focus_prev_no_messages_noop() {
        let mut app = test_app();
        app.preview_message_count = 0;
        app.focused_message = Some(2);

        app.focus_prev_message();

        // Should not change when no messages
        assert_eq!(app.focused_message, Some(2));
    }

    // ==================== focus_next_message tests ====================

    #[test]
    fn test_focus_next_at_last_message_stays() {
        let mut app = test_app();
        app.preview_message_count = 5;
        app.focused_message = Some(4);

        app.focus_next_message();

        assert_eq!(app.focused_message, Some(4));
    }

    #[test]
    fn test_focus_next_moves_down() {
        let mut app = test_app();
        app.preview_message_count = 5;
        app.focused_message = Some(2);

        app.focus_next_message();

        assert_eq!(app.focused_message, Some(3));
    }

    #[test]
    fn test_focus_next_from_none_uses_matched_index() {
        let mut app = test_app();
        app.preview_message_count = 5;
        app.focused_message = None;
        // When focused_message is None and no result, defaults to 0
        // So moving next from 0 goes to 1

        app.focus_next_message();

        assert_eq!(app.focused_message, Some(1));
    }

    #[test]
    fn test_focus_next_no_messages_noop() {
        let mut app = test_app();
        app.preview_message_count = 0;
        app.focused_message = Some(2);

        app.focus_next_message();

        // Should not change when no messages
        assert_eq!(app.focused_message, Some(2));
    }

    // ==================== toggle_focused_expansion tests ====================

    #[test]
    fn test_toggle_expands_collapsed_message() {
        let mut app = test_app();
        app.preview_message_count = 5;
        app.focused_message = Some(2);

        app.toggle_focused_expansion();

        assert!(app.expanded_messages.contains(&2));
    }

    #[test]
    fn test_toggle_collapses_expanded_message() {
        let mut app = test_app();
        app.preview_message_count = 5;
        app.focused_message = Some(2);
        app.expanded_messages.insert(2);

        app.toggle_focused_expansion();

        assert!(!app.expanded_messages.contains(&2));
    }

    #[test]
    fn test_toggle_no_messages_noop() {
        let mut app = test_app();
        app.preview_message_count = 0;
        app.focused_message = Some(2);

        app.toggle_focused_expansion();

        assert!(app.expanded_messages.is_empty());
    }

    #[test]
    fn test_multiple_messages_can_be_expanded() {
        let mut app = test_app();
        app.preview_message_count = 5;

        app.focused_message = Some(1);
        app.toggle_focused_expansion();

        app.focused_message = Some(3);
        app.toggle_focused_expansion();

        assert!(app.expanded_messages.contains(&1));
        assert!(app.expanded_messages.contains(&3));
        assert_eq!(app.expanded_messages.len(), 2);
    }

    // ==================== click_preview_message tests ====================

    #[test]
    fn test_click_inside_preview_selects_message() {
        let mut app = test_app();
        app.preview_area = (50, 5, 60, 20); // x, y, width, height
        app.preview_scroll = 0;
        app.message_line_ranges = vec![
            (0, 5),   // Message 0: lines 0-4
            (5, 12),  // Message 1: lines 5-11
            (12, 18), // Message 2: lines 12-17
        ];

        // Click on line 7 (y=5+7=12), which is in message 1
        let clicked = app.click_preview_message(55, 12);

        assert!(clicked);
        assert_eq!(app.focused_message, Some(1));
    }

    #[test]
    fn test_click_outside_preview_returns_false() {
        let mut app = test_app();
        app.preview_area = (50, 5, 60, 20);
        app.message_line_ranges = vec![(0, 5), (5, 12)];

        // Click outside preview area (x too small)
        let clicked = app.click_preview_message(10, 10);

        assert!(!clicked);
        assert_eq!(app.focused_message, None);
    }

    #[test]
    fn test_click_accounts_for_scroll() {
        let mut app = test_app();
        app.preview_area = (50, 5, 60, 20);
        app.preview_scroll = 10; // Scrolled down 10 lines
        app.message_line_ranges = vec![
            (0, 5),   // Message 0: lines 0-4
            (5, 12),  // Message 1: lines 5-11
            (12, 25), // Message 2: lines 12-24
        ];

        // Click at y=5 (top of preview), with scroll=10, actual line = 0 + 10 = 10
        // Line 10 is in message 1 (lines 5-11)
        let clicked = app.click_preview_message(55, 5);

        assert!(clicked);
        assert_eq!(app.focused_message, Some(1));
    }

    #[test]
    fn test_click_on_empty_area_returns_false() {
        let mut app = test_app();
        app.preview_area = (50, 5, 60, 20);
        app.preview_scroll = 0;
        app.message_line_ranges = vec![
            (0, 3),  // Message 0: lines 0-2
            (4, 8),  // Message 1: lines 4-7 (gap at line 3)
        ];

        // Click on line 3 which is between messages
        let clicked = app.click_preview_message(55, 8); // y=5+3=8 -> line 3

        assert!(!clicked);
    }

    // ==================== list paging tests ====================

    fn app_with_results(count: usize) -> App {
        let mut app = test_app();
        app.results = (0..count)
            .map(|i| SearchResult {
                session: Session {
                    id: format!("s{}", i),
                    source: crate::session::SessionSource::ClaudeCode,
                    file_path: PathBuf::new(),
                    cwd: String::new(),
                    git_branch: None,
                    title: None,
                    timestamp: chrono::Utc::now(),
                    messages: Vec::new(),
                },
                message_count: 0,
                duration_secs: 0,
                score: 0.0,
                matched_message_index: 0,
                snippet: String::new(),
                match_spans: Vec::new(),
                match_fragment: String::new(),
            })
            .collect();
        app.list_page_size = 10;
        app
    }

    #[test]
    fn test_page_down_moves_one_screen_and_stops_at_last() {
        let mut app = app_with_results(25);

        app.on_page_down();
        assert_eq!(app.selected, 10);
        app.on_page_down();
        app.on_page_down();
        assert_eq!(app.selected, 24);
    }

    #[test]
    fn test_page_up_stops_at_first() {
        let mut app = app_with_results(25);
        app.selected = 13;

        app.on_page_up();
        assert_eq!(app.selected, 3);
        app.on_page_up();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn test_half_page_moves() {
        let mut app = app_with_results(25);

        app.on_half_page_down();
        assert_eq!(app.selected, 5);
        app.on_half_page_up();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn test_first_and_last() {
        let mut app = app_with_results(25);

        app.select_last();
        assert_eq!(app.selected, 24);
        app.select_first();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn test_paging_with_no_results_is_noop() {
        let mut app = test_app();

        app.on_page_down();
        app.select_last();

        assert_eq!(app.selected, 0);
    }

    // ==================== shortcuts panel tests ====================

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn test_question_mark_opens_help_when_search_is_empty() {
        let mut app = test_app();

        press(&mut app, KeyCode::Char('?'));

        assert!(app.show_help);
        assert_eq!(app.query, "");
    }

    #[test]
    fn test_question_mark_is_typed_in_search_mode() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('a'));

        press(&mut app, KeyCode::Char('?'));

        assert!(!app.show_help);
        assert_eq!(app.query, "a?");
    }

    #[test]
    fn test_f1_opens_help_with_search_text() {
        let mut app = test_app();
        app.on_char('a');

        press(&mut app, KeyCode::F(1));

        assert!(app.show_help);
    }

    #[test]
    fn test_esc_closes_help_without_quitting_or_clearing() {
        let mut app = test_app();
        app.show_help = true;
        app.query = "deploy".to_string();

        press(&mut app, KeyCode::Esc);

        assert!(!app.show_help);
        assert!(!app.should_quit);
        assert_eq!(app.query, "deploy");
    }

    #[test]
    fn test_other_keys_ignored_while_help_open() {
        let mut app = test_app();
        app.show_help = true;

        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Enter);

        assert_eq!(app.query, "");
        assert!(app.should_resume.is_none());
    }

    // ==================== transcript view and copy keys ====================

    /// App with one result backed by a real session file
    fn app_with_session_file() -> (App, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let folder = dir.path().join(".claude").join("projects").join("-p-xenia");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("abc.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"user","sessionId":"abc","cwd":"/p/xenia","timestamp":"2026-01-01T10:00:00Z","message":{"role":"user","content":"deploy the app"}}"#,
                "\n",
                r#"{"type":"assistant","sessionId":"abc","cwd":"/p/xenia","timestamp":"2026-01-01T10:01:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Deployed."}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        let mut app = app_with_results(1);
        app.results[0].session.file_path = path;
        app.results[0].session.cwd = "/p/xenia".to_string();
        (app, dir)
    }

    #[test]
    fn test_enter_opens_transcript_and_q_closes_it() {
        let (mut app, _dir) = app_with_session_file();

        press(&mut app, KeyCode::Enter);

        let transcript = app.transcript.as_ref().expect("transcript open");
        assert_eq!(transcript.session.messages.len(), 2);
        assert!(app.should_resume.is_none());

        press(&mut app, KeyCode::Char('q'));
        assert!(app.transcript.is_none());
        assert!(!app.should_quit);
        assert_eq!(app.query, "", "q is not typed into the search box");
    }

    #[test]
    fn test_ctrl_r_in_transcript_resumes_and_enter_does_not() {
        let (mut app, _dir) = app_with_session_file();
        press(&mut app, KeyCode::Enter);

        press(&mut app, KeyCode::Enter);
        assert!(app.should_resume.is_none());
        app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));

        assert_eq!(app.should_resume.as_ref().map(|s| s.messages.len()), Some(2));
    }

    #[test]
    fn test_ctrl_r_resumes_from_list() {
        let (mut app, _dir) = app_with_session_file();

        app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));

        assert!(app.should_resume.is_some());
        assert!(app.transcript.is_none());
    }

    #[test]
    fn test_y_and_ctrl_y_copy_without_quitting() {
        let mut app = app_with_results(3);
        app.results[0].session.cwd = "/p/xenia".to_string();

        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.pending_copy, Some(("s0".to_string(), "session ID")));
        assert!(!app.should_quit);

        app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        let (text, what) = app.pending_copy.clone().unwrap();
        assert_eq!(what, "resume command");
        assert!(text.starts_with("cd /p/xenia && "), "{}", text);
        assert!(text.ends_with(" s0"), "{}", text);
    }

    #[test]
    fn test_help_from_transcript_returns_to_transcript() {
        let (mut app, _dir) = app_with_session_file();
        press(&mut app, KeyCode::Enter);

        press(&mut app, KeyCode::Char('?'));
        assert!(app.show_help);
        press(&mut app, KeyCode::Esc);

        assert!(!app.show_help);
        assert!(app.transcript.is_some());
    }

    #[test]
    fn test_ctrl_s_cycles_source_filter() {
        let mut app = test_app();
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);

        app.on_key(ctrl_s);
        assert_eq!(app.source_filter, Some(SessionSource::ClaudeCode));
        app.on_key(ctrl_s);
        assert_eq!(app.source_filter, Some(SessionSource::CodexCli));
        for _ in 0..3 {
            app.on_key(ctrl_s);
        }
        assert_eq!(app.source_filter, None);
    }

    #[test]
    fn test_search_words_leave_out_dates() {
        let mut app = test_app();
        app.query = "deploy since:2w".to_string();

        assert_eq!(app.search_words(), "deploy");
    }

    #[test]
    fn test_parsed_session_reused_until_file_changes() {
        let (mut app, _dir) = app_with_session_file();
        let path = app.results[0].session.file_path.clone();

        let first = app.load_session(&path).unwrap();
        let second = app.load_session(&path).unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(r#"{"type":"user","sessionId":"abc","cwd":"/p/xenia","timestamp":"2026-01-01T10:02:00Z","message":{"role":"user","content":"thanks"}}"#);
        text.push('\n');
        std::fs::write(&path, text).unwrap();

        assert_eq!(app.load_session(&path).unwrap().messages.len(), 3);
    }

    // ==================== normal and search modes, panes ====================

    #[test]
    fn test_starts_in_normal_mode_where_letters_are_commands() {
        let mut app = app_with_results(25);

        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected, 2);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.selected, 24);
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.selected, 0);
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(app.selected, 5);
        assert_eq!(app.query, "");
    }

    #[test]
    fn test_slash_types_a_search_until_enter_or_esc() {
        let mut app = app_with_results(5);

        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.input_mode, InputMode::Search);
        for c in "jgq".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.query, "jgq");
        assert!(!app.should_quit);

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.transcript.is_none(), "Enter ends the search, doesn't open");
        assert_eq!(app.query, "jgq");

        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.query, "jgq");
    }

    #[test]
    fn test_search_mode_editing_keys() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('/'));
        for c in "deploy the app".chars() {
            press(&mut app, KeyCode::Char(c));
        }

        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.query, "deploy the ");
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.query, "");
    }

    #[test]
    fn test_normal_mode_command_keys() {
        let mut app = app_with_results(3);

        press(&mut app, KeyCode::Char('Y'));
        assert_eq!(app.pending_copy.as_ref().map(|c| c.1), Some("resume command"));
        // s and t search again (the test index is empty)
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.search_scope, SearchScope::Project(String::new()));
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.source_filter, Some(SessionSource::ClaudeCode));
        press(&mut app, KeyCode::Char('?'));
        assert!(app.show_help);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Esc);
        assert!(!app.should_quit, "only Ctrl+C quits");
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn test_tab_and_ctrl_w_switch_panes() {
        let mut app = app_with_results(3);

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focused_pane, Pane::Preview);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focused_pane, Pane::List);

        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.focused_pane, Pane::Preview);
        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.focused_pane, Pane::List);

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focused_pane, Pane::List, "Esc returns to the list");
        assert!(!app.should_quit);
    }

    #[test]
    fn test_preview_pane_keys_move_through_messages() {
        let mut app = app_with_results(3);
        app.preview_message_count = 10;
        press(&mut app, KeyCode::Tab);

        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.focused_message, Some(9));
        press(&mut app, KeyCode::Char('['));
        assert_eq!(app.focused_message, Some(8));
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.focused_message, Some(0));
        press(&mut app, KeyCode::Char(']'));
        assert_eq!(app.focused_message, Some(1));
        assert_eq!(app.selected, 0, "session selection doesn't move");

        app.preview_scroll = 5;
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.preview_scroll, 4);
    }

    #[test]
    fn test_enter_in_preview_opens_transcript_at_focused_message() {
        let (mut app, _dir) = app_with_session_file();
        app.preview_message_count = 2;
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('G'));

        press(&mut app, KeyCode::Enter);

        let transcript = app.transcript.as_mut().expect("transcript open");
        transcript.height = 1;
        transcript.set_lines(
            (0..6).map(|i| ratatui::text::Line::raw(i.to_string())).collect(),
            vec![0, 3],
        );
        assert_eq!(transcript.top, 3);
    }

    // ==================== State reset tests ====================

    #[test]
    fn test_navigation_sets_pending_auto_scroll() {
        let mut app = test_app();
        app.preview_message_count = 5;
        app.focused_message = Some(2);
        app.pending_auto_scroll = false;

        app.focus_next_message();

        assert!(app.pending_auto_scroll);
    }
}
