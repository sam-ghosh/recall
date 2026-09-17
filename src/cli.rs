//! CLI subcommands for non-interactive mode (JSON output for agents)

use anyhow::Result;
use recall::time::parse_time;
use recall::{
    index::{ensure_index_fresh, SearchFilter, SessionIndex},
    parser,
    project::project_root,
    session::{ListOutput, Message, SearchOutput, SearchResultOutput, SessionSource},
};

const DEFAULT_MESSAGES_PER_SESSION: usize = 5;

/// Run the search subcommand
#[allow(clippy::too_many_arguments)]
pub fn run_search(
    query: &str,
    source: Option<SessionSource>,
    session_id: Option<String>,
    limit: usize,
    context: usize,
    since: Option<String>,
    until: Option<String>,
    cwd: Option<String>,
) -> Result<()> {
    let index = SessionIndex::open_default()?;
    ensure_index_fresh(&index)?;

    // Parse time filters
    let since_dt = since.as_ref().map(|s| parse_time(s)).transpose()?;
    let until_dt = until.as_ref().map(|s| parse_time(s)).transpose()?;

    // If searching within a specific session, handle separately
    if let Some(sid) = session_id {
        return search_in_session(&index, query, &sid, context);
    }

    let filter = SearchFilter {
        project: cwd.as_deref().map(project_root),
        source,
        since: since_dt,
        until: until_dt,
    };
    let results = index.search(query, limit, &filter)?;

    // Pre-compute query terms once (not per-session)
    let query_lower = query.to_lowercase();
    let query_terms: Vec<&str> = query_lower.split_whitespace().collect();

    // Convert to output format
    let output = SearchOutput {
        query: query.to_string(),
        results: results
            .into_iter()
            .map(|r| {
                // Load full session to get messages
                let session = parser::parse_session_file(&r.session.file_path)
                    .unwrap_or(r.session.clone());

                // Filter and score messages in one pass (avoids repeated to_lowercase in sort)
                let mut scored_messages: Vec<(usize, usize, &Message)> = session
                    .messages
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, m)| {
                        let content_lower = m.content.to_lowercase();
                        let score: usize = query_terms
                            .iter()
                            .map(|t| content_lower.matches(t).count())
                            .sum();
                        if score > 0 {
                            Some((idx, score, m))
                        } else {
                            None
                        }
                    })
                    .collect();

                // Sort by pre-computed score (higher first), then recency (higher index first)
                scored_messages.sort_by(|(idx_a, score_a, _), (idx_b, score_b, _)| {
                    score_b.cmp(score_a).then_with(|| idx_b.cmp(idx_a))
                });

                // Get top N messages, with context if requested
                let relevant_messages = if context > 0 {
                    // Convert to format expected by collect_with_context
                    let for_context: Vec<(usize, &Message)> = scored_messages
                        .iter()
                        .map(|(idx, _, m)| (*idx, *m))
                        .collect();
                    collect_with_context(&session.messages, &for_context, context)
                } else {
                    scored_messages
                        .into_iter()
                        .take(DEFAULT_MESSAGES_PER_SESSION)
                        .map(|(_, _, m)| m.clone())
                        .collect()
                };

                let (cmd, args) = r.session.resume_command();
                let resume_command = std::iter::once(cmd)
                    .chain(args)
                    .collect::<Vec<_>>()
                    .join(" ");

                SearchResultOutput {
                    session_id: r.session.id,
                    source: r.session.source,
                    cwd: r.session.cwd,
                    timestamp: r.session.timestamp,
                    relevant_messages,
                    resume_command,
                }
            })
            .collect(),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Search within a specific session (returns all matches)
fn search_in_session(
    index: &SessionIndex,
    query: &str,
    session_id: &str,
    context: usize,
) -> Result<()> {
    let file_path = index
        .get_by_id(session_id)?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

    let session = parser::parse_session_file(&file_path)?;

    let query_lower = query.to_lowercase();
    let query_terms: Vec<&str> = query_lower.split_whitespace().collect();

    // Filter and score messages in one pass (avoids repeated to_lowercase in sort)
    let mut scored_messages: Vec<(usize, usize, &Message)> = session
        .messages
        .iter()
        .enumerate()
        .filter_map(|(idx, m)| {
            let content_lower = m.content.to_lowercase();
            let score: usize = query_terms
                .iter()
                .map(|t| content_lower.matches(t).count())
                .sum();
            if score > 0 {
                Some((idx, score, m))
            } else {
                None
            }
        })
        .collect();

    // Sort by pre-computed score (higher first), then recency (higher index first)
    scored_messages.sort_by(|(idx_a, score_a, _), (idx_b, score_b, _)| {
        score_b.cmp(score_a).then_with(|| idx_b.cmp(idx_a))
    });

    // Return all matches (no limit for single session search)
    let relevant_messages = if context > 0 {
        let for_context: Vec<(usize, &Message)> = scored_messages
            .iter()
            .map(|(idx, _, m)| (*idx, *m))
            .collect();
        collect_with_context(&session.messages, &for_context, context)
    } else {
        scored_messages
            .into_iter()
            .map(|(_, _, m)| m.clone())
            .collect()
    };

    let (cmd, args) = session.resume_command();
    let resume_command = std::iter::once(cmd)
        .chain(args)
        .collect::<Vec<_>>()
        .join(" ");

    let output = SearchOutput {
        query: query.to_string(),
        results: vec![SearchResultOutput {
            session_id: session.id,
            source: session.source,
            cwd: session.cwd,
            timestamp: session.timestamp,
            relevant_messages,
            resume_command,
        }],
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Collect messages with context around matches, deduplicating overlaps
fn collect_with_context(
    all_messages: &[Message],
    scored: &[(usize, &Message)],
    context: usize,
) -> Vec<Message> {
    let mut indices: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();

    for (idx, _) in scored.iter().take(DEFAULT_MESSAGES_PER_SESSION) {
        let start = idx.saturating_sub(context);
        let end = (*idx + context + 1).min(all_messages.len());
        for i in start..end {
            indices.insert(i);
        }
    }

    indices
        .into_iter()
        .map(|i| all_messages[i].clone())
        .collect()
}

/// Run the list subcommand
pub fn run_list(
    limit: usize,
    source: Option<SessionSource>,
    since: Option<String>,
    until: Option<String>,
    cwd: Option<String>,
) -> Result<()> {
    let index = SessionIndex::open_default()?;
    ensure_index_fresh(&index)?;

    // Parse time filters
    let since_dt = since.as_ref().map(|s| parse_time(s)).transpose()?;
    let until_dt = until.as_ref().map(|s| parse_time(s)).transpose()?;

    let filter = SearchFilter {
        project: cwd.as_deref().map(project_root),
        source,
        since: since_dt,
        until: until_dt,
    };
    let results = index.recent(limit, &filter)?;

    let output = ListOutput {
        sessions: results
            .iter()
            .map(|r| r.session.to_summary())
            .collect(),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Run the read subcommand
pub fn run_read(session_id: &str) -> Result<()> {
    let index = SessionIndex::open_default()?;
    ensure_index_fresh(&index)?;

    // Find the session by ID
    let file_path = index
        .get_by_id(session_id)?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

    // Parse full session
    let session = parser::parse_session_file(&file_path)?;
    let output = session.to_read_output();

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
