//! Synchronous indexing for CLI mode

use super::indexer::{discover_and_sort_files, index_files, plan_update, IndexProgress};
use super::schema::default_index_path;
use super::state::IndexState;
use super::SessionIndex;
use anyhow::Result;
use std::io::Write;

/// Ensure index is up-to-date before running CLI queries.
/// Discovers new/modified session files and indexes them synchronously.
/// Progress is printed to stderr.
pub fn ensure_index_fresh(index: &SessionIndex) -> Result<()> {
    // state.json lives alongside the index directory
    let index_path = default_index_path();
    let state_path = index_path
        .parent()
        .map(|p| p.join("state.json"))
        .unwrap_or_else(|| index_path.join("state.json"));

    let mut state = IndexState::load(&state_path)?;

    // Discover all session files
    let files = discover_and_sort_files();

    // Find files that need indexing or removing
    let update = plan_update(&state, &files);
    let total = update.to_index.len();
    if update.is_empty() {
        // Nothing to index, we're fresh
        return Ok(());
    }

    if total > 0 {
        eprintln!(
            "Indexing {} session{}...",
            total,
            if total == 1 { "" } else { "s" }
        );
    }

    let mut writer = index.writer()?;

    // Progress callback prints to stderr
    let on_progress = Box::new(|p: IndexProgress| {
        eprint!("\rIndexing {}/{}...", p.indexed, p.total);
        let _ = std::io::stderr().flush();
    });

    index_files(
        index,
        &mut writer,
        &mut state,
        &update,
        Some(on_progress),
        None, // No reload callback for sync mode
    )?;

    state.save(&state_path)?;

    // Clear progress line and print completion
    if total > 0 {
        eprintln!(
            "\rIndexed {} session{}.    ",
            total,
            if total == 1 { "" } else { "s" }
        );
    }

    // Reload index to see new data
    index.reload()?;

    Ok(())
}
