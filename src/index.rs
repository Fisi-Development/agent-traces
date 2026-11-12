//! A SQLite full-text index of prompt, command and assistant events, so `find`
//! over the `prompts` and `assistant` scopes stops reading raw transcripts.
//!
//! The index answers one question: for every term, does some in-scope event's
//! lowercased `event_text` contain it? That selects exactly the sessions
//! `search_candidates` keeps, so it can replace the raw-file prefilter, which is
//! only a superset of that rule. A file the index cannot vouch for is scanned raw.
use crate::{
    adapters::{claude, codex, grok},
    context::Context,
    model::{Diagnostic, Session},
    storage::jsonl,
    util::{event_text, s},
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
    time::{Duration, Instant},
};

pub const FILE_NAME: &str = "find-index-v2.sqlite";
/// The previous schema's file, removed on open (it has no `tail_hash`).
const OLD_FILE_NAME: &str = "find-index-v1.sqlite";
/// Bytes before the stored offset whose hash must still match for an append.
const TAIL_BYTES: u64 = 4096;
const DEFAULT_BUDGET_MS: u64 = 8000;
/// Harnesses whose transcripts the index stores. OpenCode and T3 keep SQL
/// stores with their own search paths.
const HARNESSES: [&str; 3] = ["claude", "codex", "grok"];
/// Roles the index stores: the roles of the `prompts` and `assistant` scopes.
const INDEXED_ROLES: [&str; 3] = ["prompt", "command", "assistant"];

/// True when every role of the scope is one the index stores.
pub fn scope_is_indexed(roles: &[&str]) -> bool {
    !roles.is_empty() && roles.iter().all(|r| INDEXED_ROLES.contains(r))
}

/// `AGENT_TRACES_NO_INDEX=1` makes every query scan raw transcripts.
pub fn disabled() -> bool {
    std::env::var_os("AGENT_TRACES_NO_INDEX").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Wall-clock allowance for refreshing files inside one query. The override
/// exists for tests, which force it to 0 to exercise the raw fallback.
fn budget() -> Duration {
    Duration::from_millis(
        std::env::var("AGENT_TRACES_INDEX_BUDGET_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BUDGET_MS),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The index already holds the file's current content.
    Current,
    /// This call brought the file up to date.
    Refreshed,
    /// The transcript no longer exists.
    Gone,
    /// Not current in the index and not refreshed now: scan it raw.
    Skipped,
}

pub struct Index {
    conn: Connection,
}

/// What an adapter can say about its transcript lines. All three adapters turn
/// each record into events without state carried across lines (Claude's uuid
/// dedup only removes duplicates, and the index keeps them), so a tail parsed
/// from a byte offset yields exactly the tail of a full parse.
fn mode_for(harness: &str) -> &'static str {
    match harness {
        "claude" | "codex" | "grok" => "append",
        _ => "full",
    }
}

fn record_events(harness: &str, record: &Value) -> Vec<crate::model::Event> {
    match harness {
        "claude" => claude::record_events(record),
        "codex" => codex::codex_event(record).into_iter().collect(),
        "grok" => grok::event_records(record),
        _ => vec![],
    }
}

/// Lowercased `(role, event_text)` rows for the roles the index stores, read
/// from `start` to the end of the last complete line. Returns the rows, the
/// offset after that line and the hash of the up to `TAIL_BYTES` bytes ending
/// there (empty for a compressed file, which is always reindexed whole). A final
/// line with no newline counts only when it parses, as the raw readers do.
/// Rows, end offset and tail hash of one `read_events` pass.
type Parsed = (Vec<(String, String)>, u64, String);

fn read_events(
    harness: &str,
    real: &Path,
    compressed: bool,
    start: u64,
) -> std::io::Result<Parsed> {
    let mut tail: Vec<u8> = Vec::new();
    let mut reader: Box<dyn BufRead> = if compressed {
        jsonl::open(real)?
    } else {
        let mut file = File::open(real)?;
        // The bytes before `start` seed the hash window; they are read in the
        // same pass as the new lines, so the hash describes what was parsed.
        let from = start.saturating_sub(TAIL_BYTES);
        file.seek(SeekFrom::Start(from))?;
        file.by_ref().take(start - from).read_to_end(&mut tail)?;
        file.seek(SeekFrom::Start(start))?;
        Box::new(BufReader::with_capacity(128 * 1024, file))
    };
    let mut consumed = if compressed { 0 } else { start };
    let mut rows = Vec::new();
    let mut raw = Vec::new();
    loop {
        raw.clear();
        let n = reader.read_until(b'\n', &mut raw)?;
        if n == 0 {
            break;
        }
        match jsonl::decode(&raw) {
            Ok(record) => {
                for event in record_events(harness, &record) {
                    if INDEXED_ROLES.contains(&event.role.as_str()) {
                        let body = event_text(&event).to_lowercase();
                        if !body.is_empty() {
                            rows.push((event.role.clone(), body));
                        }
                    }
                }
            }
            // A malformed complete line is skipped for good, as the raw readers do.
            Err(_) if raw.last() == Some(&b'\n') => {}
            // A writer may be mid-line: leave it for the next refresh.
            Err(_) => break,
        }
        consumed += n as u64;
        if !compressed {
            tail.extend_from_slice(&raw);
            if tail.len() > 2 * TAIL_BYTES as usize {
                tail.drain(..tail.len() - TAIL_BYTES as usize);
            }
        }
    }
    let hash = if compressed {
        String::new()
    } else {
        hex_sha256(&tail[tail.len().saturating_sub(TAIL_BYTES as usize)..])
    };
    Ok((rows, consumed, hash))
}

/// True only when neither `path` nor its compressed sibling exists. Any other
/// stat error (permissions, a store being remounted) keeps the rows. The plain
/// path is checked first, so a live transcript costs one stat.
fn missing(path: &Path) -> bool {
    let gone = |p: &Path| matches!(std::fs::metadata(p), Err(e) if e.kind() == std::io::ErrorKind::NotFound);
    if !gone(path) {
        return false;
    }
    jsonl::COMPRESSED_EXTENSIONS.iter().all(|ext| {
        let mut sibling = path.as_os_str().to_owned();
        sibling.push(format!(".{ext}"));
        gone(Path::new(&sibling))
    })
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// True when the bytes of `real` ending at `offset` (up to `TAIL_BYTES`) hash to
/// `want`: the file still begins with what was indexed, as far as that window
/// can tell.
fn tail_matches(real: &Path, offset: u64, want: &str) -> bool {
    let read = || -> std::io::Result<Vec<u8>> {
        let mut file = File::open(real)?;
        let from = offset.saturating_sub(TAIL_BYTES);
        file.seek(SeekFrom::Start(from))?;
        let mut buf = Vec::new();
        file.take(offset - from).read_to_end(&mut buf)?;
        Ok(buf)
    };
    matches!(read(), Ok(buf) if buf.len() as u64 == offset.min(TAIL_BYTES) && hex_sha256(&buf) == want)
}

struct Stored {
    size: i64,
    mtime_ns: i64,
    offset: i64,
    mode: String,
    tail_hash: String,
}

impl Index {
    pub fn open(ctx: &Context) -> Result<Self> {
        let path = crate::core::ensure_output_path(ctx, &ctx.cache_dir.join(FILE_NAME))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Best effort: the old schema's database is dead weight now.
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(ctx.cache_dir.join(format!("{OLD_FILE_NAME}{suffix}")));
        }
        let conn = Connection::open(&path)?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS files(
                 path TEXT PRIMARY KEY, harness TEXT NOT NULL, size INTEGER NOT NULL,
                 mtime_ns INTEGER NOT NULL, offset INTEGER NOT NULL, mode TEXT NOT NULL,
                 tail_hash TEXT NOT NULL);
             CREATE VIRTUAL TABLE IF NOT EXISTS events USING fts5(
                 body, path UNINDEXED, role UNINDEXED, tokenize='trigram');",
        )?;
        Ok(Self { conn })
    }

    fn stored(conn: &Connection, key: &str) -> rusqlite::Result<Option<Stored>> {
        conn.query_row(
            "SELECT size, mtime_ns, offset, mode, tail_hash FROM files WHERE path = ?1",
            [key],
            |r| {
                Ok(Stored {
                    size: r.get(0)?,
                    mtime_ns: r.get(1)?,
                    offset: r.get(2)?,
                    mode: r.get(3)?,
                    tail_hash: r.get(4)?,
                })
            },
        )
        .optional()
    }

    /// Bring one transcript up to date. `allow_work` false only reports whether
    /// the file is already current.
    pub fn refresh(
        &mut self,
        harness: &str,
        key: &str,
        path: &Path,
        allow_work: bool,
    ) -> Result<Status> {
        let real = jsonl::locate(path);
        let meta = match std::fs::metadata(&real) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Status::Gone),
            Err(_) => return Ok(Status::Skipped),
        };
        let size = meta.len() as i64;
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as i64);
        let before = Self::stored(&self.conn, key)?;
        if before
            .as_ref()
            .is_some_and(|b| b.size == size && b.mtime_ns == mtime_ns)
        {
            return Ok(Status::Current);
        }
        if !allow_work {
            return Ok(Status::Skipped);
        }
        // A compressed rollout cannot be read from an offset: reindex it whole.
        // Decided from the resolved file's own extension, never from its path.
        let compressed = jsonl::is_compressed(&real);
        let mode = if compressed {
            "full"
        } else {
            mode_for(harness)
        };
        // A changed (size, mtime) is an append only when the file grew past its
        // stored size and the bytes ending at the stored offset are the ones
        // indexed. Comparing with the stored size, not the offset, matters when a
        // partial final line leaves the offset short of the size: a same-size
        // rewrite would otherwise pass. Anything else (same size with a new
        // mtime, shrunk, rewritten) reindexes from 0.
        // Limit: an edit before the last 4 KB together with growth is not
        // detected, since transcripts are append-only.
        let (start, replace) = match &before {
            None => (0, false),
            Some(b)
                if b.mode == "append"
                    && !compressed
                    && size > b.size
                    && size > b.offset
                    && tail_matches(&real, b.offset as u64, &b.tail_hash) =>
            {
                (b.offset as u64, false)
            }
            Some(_) => (0, true),
        };
        let (rows, offset, tail_hash) = match read_events(harness, &real, compressed, start) {
            Ok(read) => read,
            // The raw scan reports an unreadable transcript as it always has.
            Err(_) => return Ok(Status::Skipped),
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Another process may have refreshed the file while this one parsed it.
        let now = Self::stored(&tx, key)?;
        let same = match (&before, &now) {
            (None, None) => true,
            (Some(a), Some(b)) => (a.size, a.mtime_ns, a.offset) == (b.size, b.mtime_ns, b.offset),
            _ => false,
        };
        if !same {
            return Ok(Status::Skipped);
        }
        if replace {
            tx.execute("DELETE FROM events WHERE path = ?1", [key])?;
        }
        {
            let mut insert =
                tx.prepare_cached("INSERT INTO events(body, path, role) VALUES (?1, ?2, ?3)")?;
            for (role, body) in &rows {
                insert.execute(params![body, key, role])?;
            }
        }
        tx.execute(
            "INSERT INTO files(path, harness, size, mtime_ns, offset, mode, tail_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path) DO UPDATE SET harness = excluded.harness, size = excluded.size,
                 mtime_ns = excluded.mtime_ns, offset = excluded.offset, mode = excluded.mode,
                 tail_hash = excluded.tail_hash",
            params![key, harness, size, mtime_ns, offset as i64, mode, tail_hash],
        )?;
        tx.commit()?;
        Ok(Status::Refreshed)
    }

    /// Drop the rows of every stored transcript of `harnesses` whose file no
    /// longer exists, discovered or not: a deleted transcript leaves discovery
    /// first, so only the stored paths can name it. Returns the keys it dropped.
    pub fn sweep(&mut self, ctx: &Context) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare("SELECT path, harness FROM files")?;
        let stored = statement
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let gone: Vec<String> = stored
            .into_iter()
            .filter(|(_, harness)| HARNESSES.contains(&harness.as_str()))
            .filter(|(path, _)| missing(&ctx.expand(path)))
            .map(|(path, _)| path)
            .collect();
        self.forget(&gone)?;
        Ok(gone)
    }

    /// Drop the rows of transcripts that no longer exist.
    pub fn forget(&mut self, keys: &[String]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for chunk in keys.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(",");
            tx.execute(
                &format!("DELETE FROM events WHERE path IN ({marks})"),
                params_from_iter(chunk),
            )?;
            tx.execute(
                &format!("DELETE FROM files WHERE path IN ({marks})"),
                params_from_iter(chunk),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Paths with an in-scope event whose lowercased text contains `term`
    /// (already lowercased, never empty).
    pub fn paths_with(&self, term: &str, roles: &[&str]) -> Result<HashSet<String>> {
        let marks = vec!["?"; roles.len()].join(",");
        let (sql, needle) = if term.chars().count() >= 3 {
            (
                format!(
                    "SELECT DISTINCT path FROM events WHERE events MATCH ?1 AND role IN ({marks})"
                ),
                format!("\"{}\"", term.replace('"', "\"\"")),
            )
        } else {
            (
                format!(
                    "SELECT DISTINCT path FROM events WHERE instr(body, ?1) > 0 AND role IN ({marks})"
                ),
                term.to_owned(),
            )
        };
        let mut statement = self.conn.prepare(&sql)?;
        let args = std::iter::once(needle.as_str()).chain(roles.iter().copied());
        Ok(statement
            .query_map(params_from_iter(args), |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn event_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))?
            as u64)
    }
}

/// What a query learned from the index about one transcript path.
pub enum Answer {
    /// Per query: every term matched an in-scope event of this file.
    Keep(Vec<bool>),
    Gone,
    /// Not covered by the index in this query: scan the raw transcript.
    Raw,
}

enum Mode {
    Off,
    /// Enabled, not yet opened.
    Pending,
    Ready(Index),
    Unavailable(String),
}

/// One query's use of the index, from refresh to the footer line.
pub struct Run {
    mode: Mode,
    scope: Vec<String>,
    started: Instant,
    indexed: usize,
    refreshed: usize,
    raw: usize,
    answers: HashMap<String, Answer>,
}

impl Run {
    pub fn new(queries: &[Vec<String>], scope: &[&str]) -> Self {
        let usable = scope_is_indexed(scope)
            && !disabled()
            && queries.iter().any(|q| !q.is_empty())
            // An empty term matches every file in the raw scan: leave that to it.
            && queries.iter().flatten().all(|t| !t.is_empty());
        Self {
            mode: if usable { Mode::Pending } else { Mode::Off },
            scope: scope.iter().map(|r| (*r).to_owned()).collect(),
            started: Instant::now(),
            indexed: 0,
            refreshed: 0,
            raw: 0,
            answers: HashMap::new(),
        }
    }

    /// Refresh every listed transcript, within the budget, then answer the
    /// queries from the index. Any index error turns the whole query raw.
    pub fn prepare(
        &mut self,
        ctx: &Context,
        harness_paths: &[(String, Vec<String>)],
        queries: &[Vec<String>],
    ) {
        if matches!(self.mode, Mode::Off | Mode::Unavailable(_)) {
            return;
        }
        // No early return on an empty listing: the sweep in `fill` must still
        // drop the rows of a harness whose last transcript was deleted.
        if matches!(self.mode, Mode::Pending) {
            self.mode = match Index::open(ctx) {
                Ok(index) => Mode::Ready(index),
                Err(e) => Mode::Unavailable(format!("{e:#}")),
            };
        }
        if !matches!(self.mode, Mode::Ready(_)) {
            return;
        }
        let Mode::Ready(mut index) = std::mem::replace(&mut self.mode, Mode::Off) else {
            return;
        };
        match self.fill(ctx, &mut index, harness_paths, queries) {
            Ok(()) => self.mode = Mode::Ready(index),
            Err(e) => {
                self.answers.clear();
                self.mode = Mode::Unavailable(format!("{e:#}"));
            }
        }
    }

    fn fill(
        &mut self,
        ctx: &Context,
        index: &mut Index,
        harness_paths: &[(String, Vec<String>)],
        queries: &[Vec<String>],
    ) -> Result<()> {
        let limit = budget();
        index.sweep(ctx)?;
        let mut current = Vec::new();
        let mut gone = Vec::new();
        let mut seen = HashSet::new();
        for (harness, paths) in harness_paths {
            for key in paths {
                if !seen.insert(key.clone()) {
                    continue;
                }
                let allow = self.started.elapsed() < limit;
                match index.refresh(harness, key, &ctx.expand(key), allow)? {
                    Status::Current => {
                        self.indexed += 1;
                        current.push(key.clone());
                    }
                    Status::Refreshed => {
                        self.refreshed += 1;
                        current.push(key.clone());
                    }
                    Status::Gone => gone.push(key.clone()),
                    Status::Skipped => self.raw += 1,
                }
            }
        }
        index.forget(&gone)?;
        let roles: Vec<&str> = self.scope.iter().map(String::as_str).collect();
        let mut sets: HashMap<String, HashSet<String>> = HashMap::new();
        for term in queries.iter().flatten() {
            let low = term.to_lowercase();
            if let std::collections::hash_map::Entry::Vacant(slot) = sets.entry(low.clone()) {
                slot.insert(index.paths_with(&low, &roles)?);
            }
        }
        for key in current {
            let keep = queries
                .iter()
                .map(|terms| {
                    !terms.is_empty()
                        && terms
                            .iter()
                            .all(|t| sets[&t.to_lowercase()].contains(key.as_str()))
                })
                .collect();
            self.answers.insert(key, Answer::Keep(keep));
        }
        for key in gone {
            self.answers.insert(key, Answer::Gone);
        }
        Ok(())
    }

    pub fn answer(&mut self, key: &str) -> Answer {
        self.answers.remove(key).unwrap_or(Answer::Raw)
    }

    /// The footer text, and the `index_unavailable` diagnostic when it applies.
    pub fn finish(self, ctx: &Context) {
        let summary = match &self.mode {
            Mode::Off => "off".to_owned(),
            Mode::Pending | Mode::Ready(_) => format!(
                "{} indexed, {} refreshed now, {} scanned raw",
                self.indexed, self.refreshed, self.raw
            ),
            Mode::Unavailable(reason) => {
                ctx.diagnostics.borrow_mut().push(Diagnostic {
                    code: "index_unavailable".into(),
                    severity: "info".into(),
                    harness: None,
                    store_id: None,
                    message: format!(
                        "find index unavailable, transcripts scanned directly: {reason}"
                    ),
                    retryable: false,
                });
                format!("unavailable ({reason})")
            }
        };
        *ctx.index_summary.borrow_mut() = Some(summary);
    }
}

pub struct Stats {
    pub current: usize,
    pub refreshed: usize,
    pub gone: usize,
    pub failed: usize,
    pub events: u64,
    pub seconds: f64,
    pub bytes: u64,
}

/// Refresh every claude, codex and grok transcript in `rows`, with no budget.
pub fn build(ctx: &Context, rows: &[Session]) -> Result<Stats> {
    let started = Instant::now();
    let mut index = Index::open(ctx)?;
    let swept = index.sweep(ctx)?;
    let (mut current, mut refreshed, mut failed) = (0, 0, 0);
    let mut gone = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        let harness = s(row, "harness");
        if !HARNESSES.contains(&harness) {
            continue;
        }
        let key = s(row, "path");
        if key.is_empty() || !seen.insert(key.to_owned()) {
            continue;
        }
        match index.refresh(harness, key, &ctx.expand(key), true)? {
            Status::Current => current += 1,
            Status::Refreshed => refreshed += 1,
            Status::Gone => gone.push(key.to_owned()),
            Status::Skipped => failed += 1,
        }
    }
    index.forget(&gone)?;
    let _ = index
        .conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    let path = ctx.cache_dir.join(FILE_NAME);
    let bytes = ["", "-wal"]
        .iter()
        .filter_map(|suffix| {
            let mut name = path.clone().into_os_string();
            name.push(suffix);
            std::fs::metadata(name).ok()
        })
        .map(|m| m.len())
        .sum();
    let events = index.event_count()?;
    Ok(Stats {
        current,
        refreshed,
        // A discovered transcript the sweep already dropped counts once.
        gone: swept.into_iter().chain(gone).collect::<HashSet<_>>().len(),
        failed,
        events,
        seconds: started.elapsed().as_secs_f64(),
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn memory_index() -> Index {
        Index::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn the_bundled_sqlite_has_fts5_and_the_trigram_tokenizer() {
        // Index::init fails on a build without either.
        let index = memory_index();
        index
            .conn
            .execute(
                "INSERT INTO events(body, path, role) VALUES ('das ist ärger mit \"zitat\"', 'p', 'prompt')",
                [],
            )
            .unwrap();
        for (term, hits) in [("ärger", 1), ("rger", 1), ("\"zitat\"", 1), ("absent", 0)] {
            assert_eq!(
                index.paths_with(term, &["prompt"]).unwrap().len(),
                hits,
                "{term}"
            );
        }
        // Two characters take the instr path, and a role outside the scope never matches.
        assert_eq!(index.paths_with("är", &["prompt"]).unwrap().len(), 1);
        assert!(
            index
                .paths_with("ärger", &["assistant"])
                .unwrap()
                .is_empty()
        );
    }

    fn lines(values: &[Value]) -> String {
        values.iter().map(|v| format!("{v}\n")).collect()
    }

    fn fixtures() -> Vec<(&'static str, Vec<Value>)> {
        vec![
            (
                "claude",
                vec![
                    json!({"type":"user","uuid":"a","timestamp":"2026-10-01T09:00:00Z","message":{"content":"Erste Frage zu Ärger"}}),
                    json!({"type":"assistant","uuid":"b","timestamp":"2026-10-01T09:01:00Z","message":{"model":"m","content":[{"type":"text","text":"Antwort eins"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}),
                    json!({"type":"user","uuid":"c","timestamp":"2026-10-01T09:02:00Z","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"file list"}]}}),
                    json!({"type":"user","uuid":"d","timestamp":"2026-10-01T09:03:00Z","message":{"content":"<command-name>/foo</command-name><command-args>bar</command-args>"}}),
                    json!({"type":"assistant","uuid":"e","timestamp":"2026-10-01T09:04:00Z","message":{"content":[{"type":"text","text":"Antwort zwei"}]}}),
                ],
            ),
            (
                "codex",
                vec![
                    json!({"type":"session_meta","timestamp":"2026-10-01T09:00:00Z","payload":{"id":"x","cwd":"/w"}}),
                    json!({"type":"response_item","timestamp":"2026-10-01T09:00:01Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Erste Frage"}]}}),
                    json!({"type":"response_item","timestamp":"2026-10-01T09:00:02Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Antwort eins"}]}}),
                    json!({"type":"response_item","timestamp":"2026-10-01T09:00:03Z","payload":{"type":"function_call","name":"shell","arguments":"{}","call_id":"c1"}}),
                    json!({"type":"response_item","timestamp":"2026-10-01T09:00:04Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Antwort zwei"}]}}),
                ],
            ),
            (
                "grok",
                vec![
                    json!({"type":"system","content":"rules"}),
                    json!({"type":"user","prompt_index":0,"content":"<user_query>Erste Frage</user_query>"}),
                    json!({"type":"assistant","content":"Antwort eins","tool_calls":[{"id":"g1","function":{"name":"run","arguments":"{}"}}]}),
                    json!({"type":"tool_result","tool_call_id":"g1","content":"output"}),
                    json!({"type":"assistant","content":"Antwort zwei"}),
                ],
            ),
        ]
    }

    #[test]
    fn a_tail_parsed_at_an_offset_is_the_tail_of_a_full_parse_for_every_adapter() {
        for (harness, records) in fixtures() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.jsonl");
            let all = lines(&records);
            std::fs::write(&path, &all).unwrap();
            let (full, end, _) = read_events(harness, &path, false, 0).unwrap();
            assert_eq!(end as usize, all.len(), "{harness}");
            assert!(full.len() >= 3, "{harness} fixture yields events: {full:?}");
            // Split after every record: prefix rows then tail rows equal the full parse.
            for cut in 1..records.len() {
                let prefix = lines(&records[..cut]);
                std::fs::write(&path, &prefix).unwrap();
                let (head, offset, _) = read_events(harness, &path, false, 0).unwrap();
                assert_eq!(offset as usize, prefix.len());
                std::fs::write(&path, &all).unwrap();
                let (tail, _, _) = read_events(harness, &path, false, offset).unwrap();
                let joined: Vec<_> = head.into_iter().chain(tail).collect();
                assert_eq!(joined, full, "{harness} cut at record {cut}");
            }
        }
    }

    #[test]
    fn the_index_rows_are_the_adapter_events_of_the_indexed_roles() {
        let ctx = Context::from_env();
        for (harness, records) in fixtures() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.jsonl");
            std::fs::write(&path, lines(&records)).unwrap();
            let (rows, _, _) = read_events(harness, &path, false, 0).unwrap();
            let row = json!({"harness": harness, "path": path.to_str().unwrap()});
            let expected: Vec<_> = crate::core::adapter(harness)
                .unwrap()
                .events(&ctx, &row, None)
                .unwrap()
                .filter(|e| INDEXED_ROLES.contains(&e.role.as_str()))
                .map(|e| (e.role.clone(), event_text(&e).to_lowercase()))
                .filter(|(_, body)| !body.is_empty())
                .collect();
            assert_eq!(rows, expected, "{harness}");
        }
    }

    #[test]
    fn a_partial_last_line_waits_and_a_complete_unterminated_one_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let first =
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"one"}]}});
        let second =
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"two"}]}});
        let text = format!("{first}\n{{\"type\":\"assistant\",\"mess");
        std::fs::write(&path, &text).unwrap();
        let (rows, offset, _) = read_events("claude", &path, false, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(offset as usize, first.to_string().len() + 1);
        std::fs::write(&path, format!("{first}\n{second}")).unwrap();
        let (rows, offset, _) = read_events("claude", &path, false, 0).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            offset as usize,
            first.to_string().len() + 1 + second.to_string().len()
        );
    }
}
