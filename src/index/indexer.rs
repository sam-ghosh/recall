//! Shared indexing logic for both background (TUI) and synchronous (CLI) modes

use super::state::IndexState;
use super::SessionIndex;
use crate::config::Config;
use crate::parser;
use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;
use tantivy::IndexWriter;

/// Progress information during indexing
pub struct IndexProgress {
    pub indexed: usize,
    pub total: usize,
}

/// Callback for reporting indexing progress
pub type ProgressCallback = Box<dyn FnMut(IndexProgress) + Send>;

/// Callback for notifying that the index should be reloaded
pub type ReloadCallback = Box<dyn FnMut() + Send>;

/// Discovers session files and sorts them by modification time (most recent first)
pub fn discover_and_sort_files(config: &Config) -> Vec<PathBuf> {
    let mut files = parser::discover_session_files(config);
    files.sort_by(|a, b| {
        let mtime_a = std::fs::metadata(a)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let mtime_b = std::fs::metadata(b)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        mtime_b.cmp(&mtime_a) // Descending (most recent first)
    });
    files
}

/// What an index run has to do
pub struct IndexUpdate {
    /// New or changed session files, most recent first
    pub to_index: Vec<PathBuf>,
    /// Indexed files that were deleted, or are now skipped by config
    pub to_remove: Vec<PathBuf>,
}

impl IndexUpdate {
    pub fn is_empty(&self) -> bool {
        self.to_index.is_empty() && self.to_remove.is_empty()
    }
}

/// Compare discovered files with what the index already holds. When the
/// config's indexing settings changed, every discovered file is indexed again.
pub fn plan_update(state: &IndexState, discovered: &[PathBuf], config: &Config) -> IndexUpdate {
    let discovered_set: HashSet<&PathBuf> = discovered.iter().collect();
    let config_changed = state.config_fingerprint != config.indexing_fingerprint();
    IndexUpdate {
        to_index: discovered
            .iter()
            .filter(|f| config_changed || state.needs_reindex(f))
            .cloned()
            .collect(),
        to_remove: state
            .indexed_files
            .keys()
            .filter(|f| !discovered_set.contains(f))
            .cloned()
            .collect(),
    }
}

/// Apply an update: remove records for `to_remove`, then index `to_index`, calling progress callbacks as work proceeds.
///
/// - `on_progress`: Called every 50 files with current progress
/// - `on_reload`: Called every 200 files after a commit (for incremental updates)
///
/// Returns the number of files successfully indexed.
pub fn index_files(
    index: &SessionIndex,
    writer: &mut IndexWriter,
    state: &mut IndexState,
    config: &Config,
    update: &IndexUpdate,
    mut on_progress: Option<ProgressCallback>,
    mut on_reload: Option<ReloadCallback>,
) -> Result<usize> {
    for file_path in &update.to_remove {
        index.delete_session(writer, file_path);
        state.remove(file_path);
    }

    let files = &update.to_index;
    let total = files.len();
    let mut indexed = 0;

    for (i, file_path) in files.iter().enumerate() {
        // Delete existing documents for this file (in case of update)
        index.delete_session(writer, file_path);

        // Parse and index
        match parser::parse_session_file(file_path) {
            Ok(session) => {
                if !session.messages.is_empty() && !config.should_skip_session(&session) {
                    let _ = index.index_session(writer, &session);
                }
                // Mark as indexed even if empty (so we don't reprocess it)
                state.mark_indexed(file_path);
                indexed += 1;
            }
            Err(_) => {
                // Skip failed files (they might be incomplete/corrupted)
                // Don't mark as indexed so we retry next time
            }
        }

        // Progress update every 50 files or at the end
        if (i + 1) % 50 == 0 || i + 1 == total {
            if let Some(ref mut callback) = on_progress {
                callback(IndexProgress {
                    indexed: i + 1,
                    total,
                });
            }
        }

        // Commit and notify for reload every 200 files
        if (i + 1) % 200 == 0 {
            writer.commit()?;
            if let Some(ref mut callback) = on_reload {
                callback();
            }
        }
    }

    // Final commit
    state.config_fingerprint = config.indexing_fingerprint();
    writer.commit()?;

    Ok(indexed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_update_removes_files_no_longer_discovered() {
        let dir = tempfile::TempDir::new().unwrap();
        let kept = dir.path().join("kept.jsonl");
        let gone = dir.path().join("gone.jsonl");
        let new = dir.path().join("new.jsonl");
        for f in [&kept, &gone, &new] {
            std::fs::write(f, "{}").unwrap();
        }
        let mut state = IndexState::default();
        state.mark_indexed(&kept);
        state.mark_indexed(&gone);

        state.config_fingerprint = Config::default().indexing_fingerprint();

        let update = plan_update(&state, &[kept.clone(), new.clone()], &Config::default());

        assert_eq!(update.to_index, vec![new]);
        assert_eq!(update.to_remove, vec![gone]);
    }

    #[test]
    fn test_plan_update_reindexes_everything_when_config_changes() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("a.jsonl");
        std::fs::write(&file, "{}").unwrap();
        let mut state = IndexState::default();
        state.mark_indexed(&file);
        state.config_fingerprint = Config::default().indexing_fingerprint();
        let mut config = Config::default();
        config.skip_sessions_starting_with = vec!["[cron]".to_string()];

        let update = plan_update(&state, &[file.clone()], &config);

        assert_eq!(update.to_index, vec![file]);
    }
}
