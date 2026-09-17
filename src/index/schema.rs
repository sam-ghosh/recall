use crate::project::project_root;
use crate::session::{SearchResult, Session, SessionSource};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use tantivy::collector::TopDocs;
use tantivy::query::{
    BooleanQuery, BoostQuery, ConstScoreQuery, Occur, PhraseQuery, Query, QueryParser, RangeQuery,
    TermQuery,
};
use tantivy::schema::*;
use tantivy::snippet::SnippetGenerator;
use tantivy::{doc, DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, Term};

/// Bump when the schema or what gets indexed changes. An index written with a
/// different version is deleted and rebuilt on open.
const SCHEMA_VERSION: u32 = 2;
const SCHEMA_VERSION_FILE: &str = "recall-schema-version";

/// `record_type` values: one record per message (searched) and one per session
/// (listed when the query is empty)
const RECORD_MESSAGE: &str = "message";
const RECORD_SESSION: &str = "session";

/// Words shorter than this only match whole words
const MIN_PREFIX_CHARS: usize = 2;
/// How many indexed words one typed word may expand to (most common first)
const MAX_PREFIX_EXPANSIONS: usize = 50;
/// Score multiplier for a whole-word match over a word-start match
const WHOLE_WORD_BOOST: f32 = 2.0;
/// Characters that mean the query uses Tantivy query syntax (quotes, fields, wildcards...)
const QUERY_SYNTAX_CHARS: &[char] = &['"', ':', '*', '(', '^', '~', '[', '{'];

/// Get the default cache directory for the index
pub fn default_index_path() -> PathBuf {
    std::env::var("RECALL_HOME_OVERRIDE")
        .map(|h| PathBuf::from(h).join(".cache").join("recall").join("index"))
        .unwrap_or_else(|_| {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("recall")
                .join("index")
        })
}

/// Restricts search and recent results. Applied inside the index query, so
/// `limit` counts only sessions that pass the filter.
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    /// Project folder (see `project::project_root`): keeps sessions whose
    /// project is this folder or a folder inside it
    pub project: Option<String>,
    pub source: Option<SessionSource>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

/// Wrapper around Tantivy index for session search
pub struct SessionIndex {
    index: Index,
    reader: IndexReader,
    #[allow(dead_code)]
    schema: Schema,
    // Field handles
    session_id: Field,
    source: Field,
    file_path: Field,
    cwd: Field,
    project_root: Field,
    git_branch: Field,
    timestamp: Field,
    record_type: Field,
    content: Field,
    preview: Field,
    message_index: Field,
}

impl SessionIndex {
    /// Open existing index or create a new one at the default path
    pub fn open_default() -> Result<Self> {
        Self::open_or_create(&default_index_path())
    }

    /// Open existing index or create a new one. An index from another schema
    /// version is deleted, together with the `state.json` next to it, so every
    /// session gets indexed again.
    pub fn open_or_create(index_path: &Path) -> Result<Self> {
        std::fs::create_dir_all(index_path)?;

        let version_path = index_path.join(SCHEMA_VERSION_FILE);
        let version_matches = std::fs::read_to_string(&version_path)
            .map(|v| v.trim() == SCHEMA_VERSION.to_string())
            .unwrap_or(false);
        if index_path.join("meta.json").exists() && !version_matches {
            std::fs::remove_dir_all(index_path).context("Failed to delete outdated index")?;
            std::fs::create_dir_all(index_path)?;
            if let Some(cache_dir) = index_path.parent() {
                let _ = std::fs::remove_file(cache_dir.join("state.json"));
            }
        }

        let schema = Self::build_schema();

        let index = if index_path.join("meta.json").exists() {
            Index::open_in_dir(index_path).context("Failed to open existing index")?
        } else {
            let index = Index::create_in_dir(index_path, schema.clone())
                .context("Failed to create new index")?;
            std::fs::write(&version_path, SCHEMA_VERSION.to_string())?;
            index
        };

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .context("Failed to create index reader")?;

        Ok(Self {
            index,
            reader,
            session_id: schema.get_field("session_id").unwrap(),
            source: schema.get_field("source").unwrap(),
            file_path: schema.get_field("file_path").unwrap(),
            cwd: schema.get_field("cwd").unwrap(),
            project_root: schema.get_field("project_root").unwrap(),
            git_branch: schema.get_field("git_branch").unwrap(),
            timestamp: schema.get_field("timestamp").unwrap(),
            record_type: schema.get_field("record_type").unwrap(),
            content: schema.get_field("content").unwrap(),
            preview: schema.get_field("preview").unwrap(),
            message_index: schema.get_field("message_index").unwrap(),
            schema,
        })
    }

    fn build_schema() -> Schema {
        let mut builder = Schema::builder();

        // Stored metadata fields
        builder.add_text_field("session_id", STRING | STORED);
        builder.add_text_field("source", STRING | STORED);
        builder.add_text_field("file_path", STRING | STORED);
        builder.add_text_field("cwd", STRING | STORED);
        builder.add_text_field("project_root", STRING | STORED);
        builder.add_text_field("git_branch", STRING | STORED);

        // Timestamp for recency sorting (stored as i64 unix timestamp)
        builder.add_i64_field("timestamp", INDEXED | STORED | FAST);

        // "message" or "session"
        builder.add_text_field("record_type", STRING);

        // Message index within the session (for match-recency)
        builder.add_u64_field("message_index", STORED);

        // Searchable message text (message records only)
        builder.add_text_field("content", TEXT | STORED);

        // Text shown for a session in the recent list (session records only)
        builder.add_text_field("preview", STORED);

        builder.build()
    }

    /// Get a writer for indexing operations
    pub fn writer(&self) -> Result<IndexWriter> {
        self.index
            .writer(50_000_000) // 50MB heap
            .context("Failed to create index writer")
    }

    /// Index a session: one record per message plus one record for the session
    pub fn index_session(&self, writer: &mut IndexWriter, session: &Session) -> Result<()> {
        let timestamp_secs = session.timestamp.timestamp();
        let project = project_root(&session.cwd);
        let file_path = session.file_path.to_string_lossy().to_string();
        let git_branch = session.git_branch.clone().unwrap_or_default();

        // Index each message separately for match-recency ranking
        for (idx, message) in session.messages.iter().enumerate() {
            writer.add_document(doc!(
                self.session_id => session.id.clone(),
                self.source => session.source.as_str(),
                self.file_path => file_path.clone(),
                self.cwd => session.cwd.clone(),
                self.project_root => project.clone(),
                self.git_branch => git_branch.clone(),
                self.timestamp => timestamp_secs,
                self.record_type => RECORD_MESSAGE,
                self.message_index => idx as u64,
                self.content => message.content.clone(),
            ))?;
        }

        writer.add_document(doc!(
            self.session_id => session.id.clone(),
            self.source => session.source.as_str(),
            self.file_path => file_path,
            self.cwd => session.cwd.clone(),
            self.project_root => project,
            self.git_branch => git_branch,
            self.timestamp => timestamp_secs,
            self.record_type => RECORD_SESSION,
            self.message_index => 0u64,
            self.preview => session_preview(session),
        ))?;

        Ok(())
    }

    /// Delete all documents for a session (by file path)
    pub fn delete_session(&self, writer: &mut IndexWriter, file_path: &Path) {
        let term = Term::from_field_text(self.file_path, &file_path.to_string_lossy());
        writer.delete_term(term);
    }

    /// Reload the reader to see recent changes
    pub fn reload(&self) -> Result<()> {
        self.reader.reload().context("Failed to reload reader")
    }

    /// Search for sessions matching the query
    /// Returns results grouped by session, ranked by match-recency
    pub fn search(
        &self,
        query_str: &str,
        limit: usize,
        filter: &SearchFilter,
    ) -> Result<Vec<SearchResult>> {
        if query_str.trim().is_empty() {
            return Ok(Vec::new());
        }

        let searcher = self.reader.searcher();
        let Some(text_query) = self.build_text_query(&searcher, query_str)? else {
            return Ok(Vec::new());
        };

        // Snippets come from the text query only, so filter terms are not highlighted
        let mut snippet_generator =
            SnippetGenerator::create(&searcher, &*text_query, self.content)?;
        snippet_generator.set_max_num_chars(200);

        let query = self.apply_filter(text_query, filter);

        // Get more results than limit to group by session
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit * 10))?;

        // Group by session, keeping the best message per session.
        // Prefer more recent message indices (higher = more recent) if scores are similar.
        struct BestMatch {
            score: f32,
            ranking_score: f32,
            message_index: usize,
            doc: TantivyDocument,
        }
        let mut best_by_session: HashMap<String, BestMatch> = HashMap::new();

        for (score, doc_addr) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_addr)?;
            let session_id = self.text(&doc, self.session_id).to_string();
            let message_index = doc
                .get_first(self.message_index)
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let ranking_score = score + (message_index as f32) * 0.01;

            match best_by_session.get_mut(&session_id) {
                Some(best) => {
                    if ranking_score > best.ranking_score {
                        *best = BestMatch {
                            score,
                            ranking_score,
                            message_index,
                            doc,
                        };
                    }
                }
                None => {
                    best_by_session.insert(
                        session_id,
                        BestMatch {
                            score,
                            ranking_score: score,
                            message_index,
                            doc,
                        },
                    );
                }
            }
        }

        let mut best: Vec<BestMatch> = best_by_session.into_values().collect();

        // Sort by combined relevance + recency score
        // Recency boost: exponential decay with ~7 day half-life
        let now = Utc::now().timestamp() as f64;
        let half_life_secs = 7.0 * 24.0 * 3600.0; // 7 days
        let final_score = |m: &BestMatch| {
            let timestamp = m
                .doc
                .get_first(self.timestamp)
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let age = (now - timestamp as f64).max(0.0);
            // Exponential decay: recent sessions get boost up to 2x
            (m.score as f64) * (1.0 + (-age / half_life_secs).exp())
        };
        best.sort_by(|a, b| {
            final_score(b)
                .partial_cmp(&final_score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        best.truncate(limit);

        // Snippets only for the results we return
        let results = best
            .into_iter()
            .map(|m| {
                let tantivy_snippet = snippet_generator.snippet_from_doc(&m.doc);
                let match_fragment = tantivy_snippet.fragment().to_string();
                let snippet = match_fragment.replace('\n', " ");
                let match_spans = tantivy_snippet
                    .highlighted()
                    .iter()
                    .map(|r| (r.start, r.end))
                    .collect();
                SearchResult {
                    session: self.session_from_doc(&m.doc),
                    score: m.score,
                    matched_message_index: m.message_index,
                    snippet,
                    match_spans,
                    match_fragment,
                }
            })
            .collect();

        Ok(results)
    }

    /// Get recent sessions sorted by timestamp (most recent first)
    pub fn recent(&self, limit: usize, filter: &SearchFilter) -> Result<Vec<SearchResult>> {
        let searcher = self.reader.searcher();

        let sessions_only: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.record_type, RECORD_SESSION),
            IndexRecordOption::Basic,
        ));
        let query = self.apply_filter(sessions_only, filter);

        let top_docs: Vec<(i64, DocAddress)> = searcher.search(
            &query,
            &TopDocs::with_limit(limit)
                .order_by_fast_field::<i64>("timestamp", tantivy::Order::Desc),
        )?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (_timestamp, doc_addr) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_addr)?;
            results.push(SearchResult {
                session: self.session_from_doc(&doc),
                score: 0.0,
                matched_message_index: 0,
                snippet: self.text(&doc, self.preview).to_string(),
                match_spans: Vec::new(),
                match_fragment: String::new(),
            });
        }

        Ok(results)
    }

    /// Look up a session by ID and return its file path
    pub fn get_by_id(&self, session_id: &str) -> Result<Option<PathBuf>> {
        let searcher = self.reader.searcher();

        let term = Term::from_field_text(self.session_id, session_id);
        let query = TermQuery::new(term, IndexRecordOption::Basic);

        let top_docs = searcher.search(&query, &TopDocs::with_limit(1))?;

        if let Some((_score, doc_addr)) = top_docs.first() {
            let doc: TantivyDocument = searcher.doc(*doc_addr)?;
            Ok(doc
                .get_first(self.file_path)
                .and_then(|v| v.as_str())
                .map(PathBuf::from))
        } else {
            Ok(None)
        }
    }

    /// Build the query that matches message text.
    ///
    /// Plain words: every word must match the start of a word in the message
    /// ("xeni" finds "xenia"); whole-word matches score higher, and the words
    /// appearing together as a phrase score higher still. Queries using Tantivy
    /// syntax (quotes, `field:`, `*`...) go to Tantivy's query parser instead.
    fn build_text_query(
        &self,
        searcher: &Searcher,
        query_str: &str,
    ) -> Result<Option<Box<dyn Query>>> {
        if uses_query_syntax(query_str) {
            let mut parser = QueryParser::for_index(&self.index, vec![self.content]);
            parser.set_conjunction_by_default();
            if let Ok(query) = parser.parse_query(query_str) {
                return Ok(Some(query));
            }
            // Not valid syntax (e.g. a pasted URL): treat it as plain words
        }

        let words = self.tokenize(query_str);
        if words.is_empty() {
            return Ok(None);
        }

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for (_, word) in &words {
            clauses.push((Occur::Must, self.word_query(searcher, word)?));
        }

        if words.len() > 1 {
            let phrase_terms = words
                .iter()
                .map(|(position, word)| (*position, Term::from_field_text(self.content, word)))
                .collect();
            let phrase = PhraseQuery::new_with_offset(phrase_terms);
            clauses.push((
                Occur::Should,
                Box::new(BoostQuery::new(Box::new(phrase), 10.0)),
            ));
        }

        Ok(Some(Box::new(BooleanQuery::new(clauses))))
    }

    /// Split text into (position, word) the same way message content was indexed
    fn tokenize(&self, text: &str) -> Vec<(usize, String)> {
        let mut words = Vec::new();
        if let Some(mut tokenizer) = self.index.tokenizers().get("default") {
            let mut stream = tokenizer.token_stream(text);
            stream.process(&mut |token| words.push((token.position, token.text.clone())));
        }
        words
    }

    /// Match one typed word: the whole word, or any indexed word starting with it
    fn word_query(&self, searcher: &Searcher, word: &str) -> Result<Box<dyn Query>> {
        let term_query = |text: &str| -> Box<dyn Query> {
            Box::new(TermQuery::new(
                Term::from_field_text(self.content, text),
                IndexRecordOption::WithFreqs,
            ))
        };

        let mut alternatives: Vec<(Occur, Box<dyn Query>)> = vec![(
            Occur::Should,
            Box::new(BoostQuery::new(term_query(word), WHOLE_WORD_BOOST)),
        )];

        if word.chars().count() >= MIN_PREFIX_CHARS {
            for longer_word in self.words_starting_with(searcher, word)? {
                if longer_word != word {
                    alternatives.push((Occur::Should, term_query(&longer_word)));
                }
            }
        }

        Ok(Box::new(BooleanQuery::new(alternatives)))
    }

    /// Indexed words that start with `prefix`, most common first
    fn words_starting_with(&self, searcher: &Searcher, prefix: &str) -> Result<Vec<String>> {
        let mut doc_counts: HashMap<String, u32> = HashMap::new();
        for segment in searcher.segment_readers() {
            let inverted_index = segment.inverted_index(self.content)?;
            let mut stream = inverted_index
                .terms()
                .range()
                .ge(prefix.as_bytes())
                .into_stream()?;
            while stream.advance() {
                if !stream.key().starts_with(prefix.as_bytes()) {
                    break;
                }
                if let Ok(word) = std::str::from_utf8(stream.key()) {
                    *doc_counts.entry(word.to_string()).or_default() += stream.value().doc_freq;
                }
            }
        }

        let mut words: Vec<(String, u32)> = doc_counts.into_iter().collect();
        words.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        words.truncate(MAX_PREFIX_EXPANSIONS);
        Ok(words.into_iter().map(|(word, _)| word).collect())
    }

    /// Add the filter to a query. Filter clauses do not change the score.
    fn apply_filter(&self, query: Box<dyn Query>, filter: &SearchFilter) -> Box<dyn Query> {
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, query)];
        let mut require = |q: Box<dyn Query>| {
            clauses.push((Occur::Must, Box::new(ConstScoreQuery::new(q, 0.0))));
        };

        if let Some(root) = &filter.project {
            let root = root.trim_end_matches('/');
            let exact: Box<dyn Query> = Box::new(TermQuery::new(
                Term::from_field_text(self.project_root, root),
                IndexRecordOption::Basic,
            ));
            // Everything under "<root>/": '0' is the character after '/'
            let lower = format!("{}/", root);
            let upper = format!("{}0", root);
            let inside: Box<dyn Query> = Box::new(RangeQuery::new_str_bounds(
                "project_root".to_string(),
                Bound::Included(&lower),
                Bound::Excluded(&upper),
            ));
            require(Box::new(BooleanQuery::new(vec![
                (Occur::Should, exact),
                (Occur::Should, inside),
            ])));
        }

        if let Some(source) = filter.source {
            require(Box::new(TermQuery::new(
                Term::from_field_text(self.source, source.as_str()),
                IndexRecordOption::Basic,
            )));
        }

        if filter.since.is_some() || filter.until.is_some() {
            let lower = filter
                .since
                .map_or(Bound::Unbounded, |t| Bound::Included(t.timestamp()));
            let upper = filter
                .until
                .map_or(Bound::Unbounded, |t| Bound::Included(t.timestamp()));
            require(Box::new(RangeQuery::new_i64_bounds(
                "timestamp".to_string(),
                lower,
                upper,
            )));
        }

        if clauses.len() == 1 {
            return clauses.pop().unwrap().1;
        }
        Box::new(BooleanQuery::new(clauses))
    }

    fn text<'a>(&self, doc: &'a TantivyDocument, field: Field) -> &'a str {
        doc.get_first(field).and_then(|v| v.as_str()).unwrap_or("")
    }

    /// Session metadata from a stored record (messages are not loaded)
    fn session_from_doc(&self, doc: &TantivyDocument) -> Session {
        let timestamp_secs = doc
            .get_first(self.timestamp)
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        Session {
            id: self.text(doc, self.session_id).to_string(),
            source: SessionSource::parse(self.text(doc, self.source))
                .unwrap_or(SessionSource::ClaudeCode),
            file_path: PathBuf::from(self.text(doc, self.file_path)),
            cwd: self.text(doc, self.cwd).to_string(),
            git_branch: Some(self.text(doc, self.git_branch).to_string()).filter(|s| !s.is_empty()),
            timestamp: DateTime::from_timestamp(timestamp_secs, 0).unwrap_or_default(),
            messages: Vec::new(),
        }
    }
}

fn uses_query_syntax(query: &str) -> bool {
    query.contains(QUERY_SYNTAX_CHARS)
        || query
            .split_whitespace()
            .any(|word| word.len() > 1 && (word.starts_with('-') || word.starts_with('+')))
}

/// The first user message (or first message), shortened, for the recent list
fn session_preview(session: &Session) -> String {
    let message = session
        .messages
        .iter()
        .find(|m| m.role == crate::session::Role::User)
        .or_else(|| session.messages.first());
    message
        .map(|m| {
            m.content
                .chars()
                .take(200)
                .collect::<String>()
                .replace('\n', " ")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Message, Role};
    use chrono::Duration;

    fn session(id: &str, cwd: &str, age_days: i64, messages: &[&str]) -> Session {
        let timestamp = Utc::now() - Duration::days(age_days);
        Session {
            id: id.to_string(),
            source: SessionSource::ClaudeCode,
            file_path: PathBuf::from(format!("/sessions/{}.jsonl", id)),
            cwd: cwd.to_string(),
            git_branch: None,
            timestamp,
            messages: messages
                .iter()
                .enumerate()
                .map(|(i, text)| Message {
                    role: if i % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    content: text.to_string(),
                    timestamp,
                })
                .collect(),
        }
    }

    fn index_with(sessions: &[Session]) -> (tempfile::TempDir, SessionIndex) {
        let dir = tempfile::TempDir::new().unwrap();
        let index = SessionIndex::open_or_create(&dir.path().join("index")).unwrap();
        let mut writer = index.writer().unwrap();
        for s in sessions {
            index.index_session(&mut writer, s).unwrap();
        }
        writer.commit().unwrap();
        index.reload().unwrap();
        (dir, index)
    }

    fn ids(results: &[SearchResult]) -> Vec<String> {
        let mut ids: Vec<String> = results.iter().map(|r| r.session.id.clone()).collect();
        ids.sort();
        ids
    }

    fn project(root: &str) -> SearchFilter {
        SearchFilter {
            project: Some(root.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn test_partial_word_matches_word_start() {
        let (_dir, index) =
            index_with(&[session("a", "/p/xenia", 1, &["deploy the xenia webserver"])]);

        assert_eq!(
            ids(&index.search("xeni", 10, &SearchFilter::default()).unwrap()),
            vec!["a"]
        );
        assert_eq!(
            ids(&index
                .search("webser dep", 10, &SearchFilter::default())
                .unwrap()),
            vec!["a"]
        );
    }

    #[test]
    fn test_partial_word_does_not_match_middle_of_word() {
        let (_dir, index) =
            index_with(&[session("a", "/p/xenia", 1, &["deploy the xenia webserver"])]);

        assert!(index
            .search("enia", 10, &SearchFilter::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_all_words_must_match() {
        let (_dir, index) = index_with(&[
            session("a", "/p/x", 1, &["deploy xenia"]),
            session("b", "/p/x", 1, &["deploy aegis"]),
        ]);

        assert_eq!(
            ids(&index
                .search("deploy xen", 10, &SearchFilter::default())
                .unwrap()),
            vec!["a"]
        );
    }

    #[test]
    fn test_snippet_highlights_partial_word_match() {
        let (_dir, index) = index_with(&[session("a", "/p/x", 1, &["the webserver restarted"])]);

        let results = index.search("webs", 10, &SearchFilter::default()).unwrap();

        let result = &results[0];
        let highlighted: Vec<&str> = result
            .match_spans
            .iter()
            .map(|&(s, e)| &result.match_fragment[s..e])
            .collect();
        assert_eq!(highlighted, vec!["webserver"]);
    }

    #[test]
    fn test_query_syntax_still_supported() {
        let (_dir, index) = index_with(&[
            session("a", "/p/x", 1, &["gunicorn restart failed"]),
            session("b", "/p/x", 1, &["restart gunicorn later"]),
        ]);

        let results = index
            .search("\"gunicorn restart\"", 10, &SearchFilter::default())
            .unwrap();

        assert_eq!(ids(&results), vec!["a"]);
    }

    #[test]
    fn test_pasted_url_is_searched_as_words() {
        let (_dir, index) = index_with(&[session(
            "a",
            "/p/x",
            1,
            &["see https://nhc.sentry.io/issues/42"],
        )]);

        let results = index
            .search("https://nhc.sentry.io", 10, &SearchFilter::default())
            .unwrap();

        assert_eq!(ids(&results), vec!["a"]);
    }

    #[test]
    fn test_project_filter_includes_worktrees_and_subfolders() {
        let (_dir, index) = index_with(&[
            session("main", "/p/xenia", 1, &["deploy"]),
            session("worktree", "/p/xenia__worktrees/fix-bug", 1, &["deploy"]),
            session("subfolder", "/p/xenia/frontend", 1, &["deploy"]),
            session("other", "/p/xenia2", 1, &["deploy"]),
            session("dash", "/p/xenia-node24", 1, &["deploy"]),
        ]);

        let expected = vec!["main", "subfolder", "worktree"];
        assert_eq!(
            ids(&index.search("deploy", 10, &project("/p/xenia")).unwrap()),
            expected
        );
        assert_eq!(
            ids(&index.recent(10, &project("/p/xenia")).unwrap()),
            expected
        );
    }

    #[test]
    fn test_project_filter_applies_before_limit() {
        // Many newer sessions elsewhere must not push the project's session out
        let mut sessions: Vec<Session> = (0..30)
            .map(|i| session(&format!("noise{}", i), "/p/other", 0, &["deploy"]))
            .collect();
        sessions.push(session("mine", "/p/xenia", 20, &["deploy"]));
        let (_dir, index) = index_with(&sessions);

        assert_eq!(
            ids(&index.recent(5, &project("/p/xenia")).unwrap()),
            vec!["mine"]
        );
        assert_eq!(
            ids(&index.search("deploy", 5, &project("/p/xenia")).unwrap()),
            vec!["mine"]
        );
    }

    #[test]
    fn test_recent_returns_one_result_per_session_newest_first() {
        let (_dir, index) = index_with(&[
            session(
                "old",
                "/p/x",
                5,
                &["first question", "answer", "second question"],
            ),
            session("new", "/p/x", 1, &["hello", "hi"]),
        ]);

        let results = index.recent(10, &SearchFilter::default()).unwrap();

        let ordered: Vec<&str> = results.iter().map(|r| r.session.id.as_str()).collect();
        assert_eq!(ordered, vec!["new", "old"]);
        assert_eq!(results[1].snippet, "first question");
    }

    #[test]
    fn test_time_and_source_filters() {
        let (_dir, index) = index_with(&[
            session("recent", "/p/x", 1, &["deploy"]),
            session("old", "/p/x", 40, &["deploy"]),
        ]);

        let since = SearchFilter {
            since: Some(Utc::now() - Duration::days(7)),
            ..Default::default()
        };
        assert_eq!(ids(&index.recent(10, &since).unwrap()), vec!["recent"]);
        assert_eq!(
            ids(&index.search("deploy", 10, &since).unwrap()),
            vec!["recent"]
        );

        let codex = SearchFilter {
            source: Some(SessionSource::CodexCli),
            ..Default::default()
        };
        assert!(index.recent(10, &codex).unwrap().is_empty());
    }

    #[test]
    fn test_outdated_index_is_rebuilt() {
        let dir = tempfile::TempDir::new().unwrap();
        let index_path = dir.path().join("index");
        {
            let index = SessionIndex::open_or_create(&index_path).unwrap();
            let mut writer = index.writer().unwrap();
            index
                .index_session(&mut writer, &session("a", "/p/x", 1, &["deploy"]))
                .unwrap();
            writer.commit().unwrap();
        }
        std::fs::write(index_path.join(SCHEMA_VERSION_FILE), "1").unwrap();
        std::fs::write(dir.path().join("state.json"), "{}").unwrap();

        let index = SessionIndex::open_or_create(&index_path).unwrap();

        assert!(index
            .recent(10, &SearchFilter::default())
            .unwrap()
            .is_empty());
        assert!(!dir.path().join("state.json").exists());
    }
}
