//! User settings, read from `~/.config/recall/config.toml`.
//!
//! ```toml
//! # Session files whose path contains any of these strings are not indexed.
//! skip_paths_containing = ["claude-mem-observer-sessions"]
//! # Sessions whose first message starts with any of these strings are not indexed
//! # (e.g. scheduled or automated runs).
//! skip_sessions_starting_with = ["[IMPORTANT: You are running as a scheduled cron job"]
//! ```

use crate::session::Session;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// claude-mem writes one Claude Code session per background observer run into
/// `~/.claude/projects/<home>--claude-mem-observer-sessions/`. They are not
/// conversations a person had, and there can be thousands of them.
const DEFAULT_SKIP_PATHS_CONTAINING: &[&str] = &["claude-mem-observer-sessions"];

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    /// Session files whose path contains any of these strings are not indexed
    pub skip_paths_containing: Vec<String>,
    /// Sessions whose first message starts with any of these strings are not indexed
    pub skip_sessions_starting_with: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            skip_paths_containing: DEFAULT_SKIP_PATHS_CONTAINING
                .iter()
                .map(|s| s.to_string())
                .collect(),
            skip_sessions_starting_with: Vec::new(),
        }
    }
}

impl Config {
    /// Load from `~/.config/recall/config.toml`, falling back to defaults when
    /// the file is missing or cannot be parsed.
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("recall: ignoring {}: {}", path.display(), e);
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Whether a parsed session should be left out of the index, judged by its
    /// first message
    pub fn should_skip_session(&self, session: &Session) -> bool {
        let Some(first) = session.messages.first() else {
            return false;
        };
        let first = first.content.trim_start();
        self.skip_sessions_starting_with
            .iter()
            .any(|start| !start.is_empty() && first.starts_with(start.as_str()))
    }

    /// Settings that change which parsed sessions get indexed. When this
    /// changes, every session is indexed again.
    pub fn indexing_fingerprint(&self) -> String {
        serde_json::to_string(&self.skip_sessions_starting_with).unwrap_or_default()
    }

    /// Whether a session file should be left out of the index
    pub fn should_skip(&self, path: &Path) -> bool {
        let path = path.to_string_lossy();
        self.skip_paths_containing
            .iter()
            .any(|part| !part.is_empty() && path.contains(part.as_str()))
    }
}

fn config_path() -> Option<PathBuf> {
    let home = std::env::var("RECALL_HOME_OVERRIDE")
        .map(PathBuf::from)
        .ok()
        .or_else(dirs::home_dir)?;
    Some(home.join(".config").join("recall").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_skips_claude_mem_observer_sessions() {
        let config = Config::default();
        assert!(config.should_skip(Path::new(
            "/Users/me/.claude/projects/-Users-me--claude-mem-observer-sessions/abc.jsonl"
        )));
        assert!(!config.should_skip(Path::new(
            "/Users/me/.claude/projects/-Users-me-Programming-xenia/abc.jsonl"
        )));
    }

    #[test]
    fn test_config_file_replaces_default_list() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "skip_paths_containing = [\"scratch\"]\n").unwrap();

        let config = Config::load_from(&path);

        assert!(config.should_skip(Path::new("/x/scratch/a.jsonl")));
        assert!(!config.should_skip(Path::new("/x/claude-mem-observer-sessions/a.jsonl")));
    }

    #[test]
    fn test_empty_list_skips_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "skip_paths_containing = []\n").unwrap();

        let config = Config::load_from(&path);

        assert!(!config.should_skip(Path::new("/x/claude-mem-observer-sessions/a.jsonl")));
    }

    #[test]
    fn test_skip_sessions_by_first_message() {
        use crate::session::{Message, Role, SessionSource};
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "skip_sessions_starting_with = [\"[IMPORTANT: You are running as a scheduled cron job\"]\n",
        )
        .unwrap();
        let config = Config::load_from(&path);
        let session = |first: &str| Session {
            id: "s".to_string(),
            source: SessionSource::ClaudeCode,
            file_path: PathBuf::new(),
            cwd: String::new(),
            git_branch: None,
            title: None,
            timestamp: chrono::Utc::now(),
            messages: vec![Message {
                role: Role::User,
                content: first.to_string(),
                timestamp: chrono::Utc::now(),
            }],
        };

        assert!(config.should_skip_session(&session(
            "  [IMPORTANT: You are running as a scheduled cron job. DELIVER..."
        )));
        assert!(!config.should_skip_session(&session("Can you check the cron job?")));
        // Path defaults still apply when only the session list is set
        assert!(config.should_skip(Path::new("/x/claude-mem-observer-sessions/a.jsonl")));
    }

    #[test]
    fn test_missing_file_uses_defaults() {
        let config = Config::load_from(Path::new("/nonexistent/recall/config.toml"));
        assert_eq!(
            config.skip_paths_containing,
            vec!["claude-mem-observer-sessions"]
        );
    }
}
