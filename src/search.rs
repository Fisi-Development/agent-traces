//! Python-reference search ranking, with an in-process streaming candidate scan.
pub use crate::util::event_text;
use crate::{
    context::Context,
    model::{Event, Session},
    util::{label, one_line, parse_ts, s},
};
use anyhow::{Result, bail};
use regex::bytes::{Regex, RegexBuilder};
use rusqlite::params_from_iter;
use std::{
    collections::{HashMap, HashSet},
    io::BufRead,
    path::Path,
};

#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub terms: Vec<String>,
    pub scope: String,
    pub sort: String,
    pub include_self: bool,
    pub limit: usize,
}
impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            terms: vec![],
            scope: "prompts".into(),
            sort: "relevance".into(),
            include_self: false,
            limit: 15,
        }
    }
}
#[derive(Debug)]
pub struct SearchMatch {
    pub row: Session,
    pub hits: Vec<Event>,
    pub score: f64,
    pub match_kind: String,
}
#[derive(Debug)]
pub struct SearchResult {
    pub rows: Vec<SearchMatch>,
    pub total: usize,
}

pub fn roles(scope: &str) -> Result<&'static [&'static str]> {
    Ok(match scope {
        "prompts" => &["prompt", "command"],
        "assistant" => &["assistant"],
        "tools" => &["tool"],
        "results" => &["result"],
        "all" => &["prompt", "command", "assistant", "tool", "result", "system"],
        _ => bail!("unknown search scope: {scope}"),
    })
}
/// Decimal formatting rounds the exact binary float, as Python round(value, 2) does.
pub fn rounded_score(score: f64) -> f64 {
    format!("{score:.2}").parse().unwrap_or(score)
}
pub fn snippet(text: &str, terms: &[String], width: usize) -> String {
    let low = text.to_lowercase();
    let pos = terms
        .iter()
        .filter_map(|t| low.find(&t.to_lowercase()))
        .map(|b| low[..b].chars().count())
        .min()
        .unwrap_or(0);
    let start = pos.saturating_sub(width / 3);
    format!(
        "{}{}",
        if start > 0 { "…" } else { "" },
        one_line(
            &text.chars().skip(start).take(width).collect::<String>(),
            width
        )
    )
}
#[cfg(test)]
thread_local! {
    static PREFILTER_READS: std::cell::RefCell<HashMap<std::path::PathBuf, usize>> =
        std::cell::RefCell::new(HashMap::new());
}
#[cfg(test)]
fn file_matches(ctx: &Context, harness: &str, path: &Path, patterns: &[Regex]) -> bool {
    file_matches_multi(ctx, harness, path, &[patterns.to_vec()])[0]
}
fn file_matches_multi(
    ctx: &Context,
    harness: &str,
    path: &Path,
    queries: &[Vec<Regex>],
) -> Vec<bool> {
    match file_matches_multi_inner(path, queries) {
        Ok(found) => found,
        Err((found, error)) => {
            ctx.diagnostic(
                "store_unreadable",
                Some(harness),
                Some(path),
                error.to_string(),
            );
            found
        }
    }
}
#[cfg(test)]
fn file_matches_inner(path: &Path, patterns: &[Regex]) -> std::io::Result<bool> {
    Ok(file_matches_multi_inner(path, &[patterns.to_vec()]).map_err(|(_, error)| error)?[0])
}
fn file_matches_multi_inner(
    path: &Path,
    queries: &[Vec<Regex>],
) -> std::result::Result<Vec<bool>, (Vec<bool>, std::io::Error)> {
    #[cfg(test)]
    PREFILTER_READS.with(|reads| *reads.borrow_mut().entry(path.to_path_buf()).or_default() += 1);
    let mut reader =
        crate::storage::jsonl::open(path).map_err(|error| (vec![false; queries.len()], error))?;
    let mut seen: Vec<_> = queries
        .iter()
        .map(|patterns| vec![false; patterns.len()])
        .collect();
    let mut remaining: Vec<_> = queries.iter().map(Vec::len).collect();
    let mut line = Vec::new();
    loop {
        line.clear();
        // A query already satisfied before a later read error would have stopped
        // here in a single-query scan, so preserve its successful result.
        if reader
            .read_until(b'\n', &mut line)
            .map_err(|error| (remaining.iter().map(|n| *n == 0).collect(), error))?
            == 0
        {
            return Ok(remaining.iter().map(|n| *n == 0).collect());
        }
        for ((patterns, seen), remaining) in queries.iter().zip(&mut seen).zip(&mut remaining) {
            for (pattern, seen) in patterns.iter().zip(seen) {
                if !*seen && pattern.is_match(&line) {
                    *seen = true;
                    *remaining -= 1;
                }
            }
        }
        if remaining.iter().all(|n| *n == 0) {
            return Ok(vec![true; queries.len()]);
        }
    }
}
// Opening read-only participates in the existing WAL. Never issue journal_mode=WAL,
// which would mutate a live database. SQLite read transactions remain short-lived.
fn sql_candidates(
    ctx: &Context,
    harness: &str,
    terms: &[String],
    roles: &[&str],
) -> Result<HashSet<String>> {
    let path = if harness == "t3" {
        ctx.home.join(".t3").join("userdata").join("state.sqlite")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| ctx.home.join(".local").join("share"))
            .join("opencode")
            .join("opencode.db")
    };
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let con = crate::storage::sqlite::open(ctx, harness, &path)?;
    let query = |sql: &str, args: Vec<String>| -> Result<HashSet<String>> {
        Ok(con
            .prepare(sql)?
            .query_map(params_from_iter(args), |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?)
    };
    let likes: Vec<String> = terms.iter().map(|t| format!("%{t}%")).collect();
    let mut ids = HashSet::new();
    if harness == "t3" {
        let mut msg_roles = Vec::new();
        if roles.iter().any(|r| matches!(*r, "prompt" | "command")) {
            msg_roles.push("user".to_owned());
        }
        if roles.contains(&"assistant") {
            msg_roles.push("assistant".to_owned());
        }
        if !msg_roles.is_empty() {
            let sql = format!(
                "select distinct thread_id from projection_thread_messages where role in ({}) and {}",
                vec!["?"; msg_roles.len()].join(","),
                vec!["text like ?"; terms.len()].join(" and ")
            );
            msg_roles.extend(likes.clone());
            ids.extend(query(&sql, msg_roles)?);
        }
        if roles.iter().any(|r| matches!(*r, "tool" | "result")) {
            ids.extend(query(&format!("select distinct thread_id from projection_thread_activities where kind='tool.completed' and {}",vec!["payload_json like ?";terms.len()].join(" and ")),likes)?);
        }
    } else {
        let types = if roles.iter().all(|r| matches!(*r, "prompt" | "command")) {
            vec!["user"]
        } else {
            vec!["user", "assistant", "synthetic", "system"]
        };
        let sql = format!(
            "select distinct session_id from session_message where type in ({}) and data like ?",
            vec!["?"; types.len()].join(",")
        );
        for (i, like) in likes.into_iter().enumerate() {
            let mut args: Vec<String> = types.iter().map(|t| t.to_string()).collect();
            args.push(like);
            let got = query(&sql, args)?;
            if i == 0 {
                ids = got;
            } else {
                ids.retain(|id| got.contains(id));
            }
        }
    }
    Ok(ids)
}
struct Doc {
    event: Event,
    tf: Vec<f64>,
    length: usize,
    first: bool,
}
fn bm25(tf: &[f64], length: usize, idf: &[f64], avgdl: f64) -> f64 {
    tf.iter()
        .zip(idf)
        .filter(|(f, _)| **f > 0.0)
        .map(|(f, idf)| idf * f * 2.2 / (f + 1.2 * (0.25 + 0.75 * length as f64 / avgdl)))
        .sum()
}
/// A mention of a hyphenated term inside a longer name, such as `k7-latch` in
/// `impl-k7-latch-r1`, counts this much of a whole-name mention.
const EMBEDDED_NAME_WEIGHT: f64 = 0.25;
/// Occurrences of a lowercased term in lowercased text. A plain term keeps the
/// Python reference's substring count. A hyphenated term is a name: an occurrence
/// that a name character (alphanumeric, `_` or `-`) touches on either side is
/// embedded in a longer name and weighs EMBEDDED_NAME_WEIGHT.
fn term_frequency(text: &str, term: &str) -> f64 {
    if !term.contains('-') {
        return text.matches(term).count() as f64;
    }
    let name_char = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    text.match_indices(term)
        .map(|(i, _)| {
            let before = text[..i].chars().next_back();
            let after = text[i + term.len()..].chars().next();
            if before.is_some_and(name_char) || after.is_some_and(name_char) {
                EMBEDDED_NAME_WEIGHT
            } else {
                1.0
            }
        })
        .sum()
}
/// Select sessions whose raw sources contain every term without parsing their events.
/// Keeps harness encounter order and the reference SQL role predicates.
pub fn prefilter_candidates(
    ctx: &Context,
    rows: Vec<Session>,
    terms: &[String],
    scope: &[&str],
) -> Result<Vec<Session>> {
    Ok(prefilter_candidates_multi(ctx, rows, &[terms.to_vec()], scope)?.remove(0))
}
/// Select each query's candidates in one shared pass through transcript sources.
pub fn prefilter_candidates_multi(
    ctx: &Context,
    rows: Vec<Session>,
    queries: &[Vec<String>],
    scope: &[&str],
) -> Result<Vec<Vec<Session>>> {
    let mut candidates: Vec<_> = queries
        .iter()
        .map(|terms| {
            if terms.is_empty() {
                rows.clone()
            } else {
                Vec::new()
            }
        })
        .collect();
    if queries.iter().all(Vec::is_empty) {
        return Ok(candidates);
    }
    let patterns = queries
        .iter()
        .map(|terms| {
            terms
                .iter()
                .map(|t| {
                    RegexBuilder::new(&regex::escape(t))
                        .case_insensitive(true)
                        .build()
                })
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    // Keep harness encounter order and stable ordering within every harness.
    let mut groups: Vec<(String, Vec<Session>)> = Vec::new();
    for row in rows {
        let h = s(&row, "harness").to_owned();
        if let Some((_, rs)) = groups.iter_mut().find(|(key, _)| *key == h) {
            rs.push(row);
        } else {
            groups.push((h, vec![row]));
        }
    }
    let mut memo = HashMap::new();
    for (h, rs) in groups {
        if matches!(h.as_str(), "claude" | "codex" | "grok") {
            for row in rs {
                let p = s(&row, "path");
                let keep = memo
                    .entry(p.to_owned())
                    .or_insert_with(|| file_matches_multi(ctx, &h, &ctx.expand(p), &patterns));
                for (i, terms) in queries.iter().enumerate() {
                    if !terms.is_empty() && keep[i] {
                        candidates[i].push(row.clone());
                    }
                }
            }
        } else if matches!(h.as_str(), "t3" | "opencode") {
            for (i, terms) in queries.iter().enumerate() {
                if terms.is_empty() {
                    continue;
                }
                match sql_candidates(ctx, &h, terms, scope) {
                    Ok(ids) => candidates[i]
                        .extend(rs.iter().filter(|r| ids.contains(s(r, "id"))).cloned()),
                    Err(e) => ctx.diagnostic("store_unreadable", Some(&h), None, e.to_string()),
                }
            }
        }
    }
    Ok(candidates)
}
pub fn search(ctx: &Context, rows: Vec<Session>, options: &SearchOptions) -> Result<SearchResult> {
    let terms: Vec<String> = options
        .terms
        .iter()
        .filter(|t| !t.is_empty())
        .cloned()
        .collect();
    if terms.is_empty() {
        bail!("give at least one search term");
    }
    let scope = roles(&options.scope)?;
    let rows = rows
        .into_iter()
        .filter(|row| {
            options.include_self
                || !["id", "parent", "provider_id"]
                    .iter()
                    .any(|key| ctx.self_ids.contains(s(row, key)))
        })
        .collect();
    let candidates = prefilter_candidates(ctx, rows, &terms, scope)?;
    search_candidates(ctx, candidates, options)
}
/// Score candidates already selected for this query, preserving its BM25 corpus.
pub fn search_candidates(
    ctx: &Context,
    candidates: Vec<Session>,
    options: &SearchOptions,
) -> Result<SearchResult> {
    let terms: Vec<String> = options
        .terms
        .iter()
        .filter(|t| !t.is_empty())
        .cloned()
        .collect();
    if terms.is_empty() {
        bail!("give at least one search term");
    }
    let scope = roles(&options.scope)?;
    let lows: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    let mut df = vec![0usize; lows.len()];
    let mut n_docs = 0usize;
    let mut total_len = 0usize;
    let mut sessions = Vec::new();
    ctx.skipped.set(0);
    for row in candidates {
        let ad = crate::core::adapter(s(&row, "harness"))?;
        let first = one_line(s(&row, "first_prompt"), 80).to_lowercase();
        let events = match ad.events(ctx, &row, Some(&lows)) {
            Ok(e) => e,
            Err(e) => {
                ctx.diagnostic(
                    "store_unreadable",
                    Some(s(&row, "harness")),
                    None,
                    e.to_string(),
                );
                continue;
            }
        };
        let mut docs = Vec::new();
        let mut covered = HashSet::new();
        for event in events {
            if !scope.contains(&event.role.as_str()) {
                continue;
            }
            let low = event_text(&event).to_lowercase();
            let length = low.matches(' ').count() + 1;
            let is_first = !first.is_empty()
                && matches!(event.role.as_str(), "prompt" | "command")
                && one_line(&low, 80) == first;
            n_docs += 1;
            total_len += length;
            let tf: Vec<f64> = lows.iter().map(|t| term_frequency(&low, t)).collect();
            if !tf.iter().any(|n| *n > 0.0) {
                continue;
            }
            // Python increments a dict entry once for each duplicate query term.
            for (i, t) in lows.iter().enumerate() {
                if tf[i] > 0.0 {
                    covered.insert(t.clone());
                    for (j, u) in lows.iter().enumerate() {
                        if u == t {
                            df[j] += 1;
                        }
                    }
                }
            }
            docs.push(Doc {
                event,
                tf,
                length,
                first: is_first,
            });
        }
        if !docs.is_empty() && covered.len() == lows.len() {
            sessions.push((row, docs));
        }
    }
    let avgdl = if n_docs > 0 {
        total_len as f64 / n_docs as f64
    } else {
        1.0
    };
    n_docs += ctx.skipped.get();
    let idf: Vec<f64> = df
        .iter()
        .map(|df| (1.0 + (n_docs as f64 - *df as f64 + 0.5) / (*df as f64 + 0.5)).ln())
        .collect();
    let mut matches = Vec::new();
    for (mut row, docs) in sessions {
        let mut scored: Vec<_> = docs
            .into_iter()
            .map(|d| {
                let weight = match d.event.role.as_str() {
                    "prompt" | "command" => 1.5,
                    "tool" => 0.8,
                    "result" | "system" => 0.5,
                    _ => 1.0,
                };
                (
                    bm25(&d.tf, d.length, &idf, avgdl) * weight * if d.first { 1.2 } else { 1.0 },
                    d,
                )
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        let title = format!("{} {}", label(&row), s(&row, "t3_title")).to_lowercase();
        let ttf: Vec<f64> = lows.iter().map(|t| term_frequency(&title, t)).collect();
        let title_score = 2.0 * bm25(&ttf, title.split_whitespace().count().max(1), &idf, avgdl);
        let score = scored[0].0 + 0.5 * (1.0 + scored.len() as f64).ln() + title_score;
        let match_kind = if ttf.iter().all(|n| *n > 0.0) {
            "title"
        } else if scored
            .iter()
            .any(|(_, d)| d.first && d.tf.iter().all(|n| *n > 0.0))
        {
            "first prompt"
        } else {
            ""
        }
        .to_owned();
        let full = scored.iter().any(|(_, d)| d.tf.iter().all(|n| *n > 0.0));
        let hits = scored
            .into_iter()
            .filter(|(_, d)| !full || d.tf.iter().all(|n| *n > 0.0))
            .map(|(_, d)| d.event)
            .collect();
        row["_score"] = serde_json::json!(rounded_score(score));
        row["_match"] = serde_json::json!(match_kind);
        matches.push(SearchMatch {
            row,
            hits,
            score,
            match_kind,
        });
    }
    sort_matches(&mut matches, &options.sort);
    let total = matches.len();
    matches.truncate(options.limit);
    Ok(SearchResult {
        rows: matches,
        total,
    })
}
fn sort_matches(matches: &mut [SearchMatch], sort: &str) {
    match sort {
        "oldest" => matches.sort_by_key(|m| m.hits.iter().map(|e| e.ts).min().flatten()),
        "recent" => matches.sort_by(|a, b| {
            let rank = |m: &SearchMatch| match m.match_kind.as_str() {
                "title" => 2,
                "first prompt" => 1,
                _ => 0,
            };
            rank(b)
                .cmp(&rank(a))
                .then_with(|| parse_ts(&b.row["updated"]).cmp(&parse_ts(&a.row["updated"])))
        }),
        _ => matches.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| parse_ts(&b.row["updated"]).cmp(&parse_ts(&a.row["updated"])))
        }),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multi_prefilter_matches_single_queries_and_reads_each_file_once() {
        let temp = tempfile::tempdir().unwrap();
        let mut ctx = Context::from_env();
        ctx.home = temp.path().to_path_buf();
        let store = temp.path().join(".claude/projects/test");
        std::fs::create_dir_all(&store).unwrap();
        let mut rows = Vec::new();
        for (id, text) in [
            ("one", "Alpha\nbeta\n"),
            ("two", "beta gamma\n"),
            ("three", "unrelated\n"),
        ] {
            let path = store.join(format!("{id}.jsonl"));
            std::fs::write(&path, text).unwrap();
            rows.push(serde_json::json!({"harness":"claude", "id":id, "path":path}));
        }
        // Duplicate rows must share the same source read without changing order.
        rows.push(rows[0].clone());
        let queries = vec![
            vec!["alpha".into(), "beta".into()],
            vec!["beta".into()],
            vec!["absent".into()],
        ];
        let scope = roles("all").unwrap();
        let expected: Vec<_> = queries
            .iter()
            .map(|q| prefilter_candidates(&ctx, rows.clone(), q, scope).unwrap())
            .collect();
        PREFILTER_READS.with(|reads| reads.borrow_mut().clear());
        let actual = prefilter_candidates_multi(&ctx, rows, &queries, scope).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            actual
                .iter()
                .map(|rows| rows.iter().map(|row| s(row, "id")).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![vec!["one", "one"], vec!["one", "two", "one"], vec![]]
        );
        PREFILTER_READS.with(|reads| {
            let reads = reads.borrow();
            assert_eq!(reads.len(), 3);
            assert!(reads.values().all(|count| *count == 1));
        });
    }
    #[test]
    fn multi_prefilter_preserves_hits_before_a_later_read_error() {
        use ruzstd::encoding::{CompressionLevel, compress_to_vec};
        let ctx = Context::from_env();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("partial.jsonl.zst");
        let text = format!("needle\n{}", "other\n".repeat(50000));
        let mut bytes = compress_to_vec(text.as_bytes(), CompressionLevel::Fastest);
        bytes.extend_from_slice(b"invalid next frame");
        std::fs::write(&path, bytes).unwrap();
        let rows = vec![serde_json::json!({"harness":"codex", "id":"one", "path":path})];
        let queries = vec![vec!["needle".into()], vec!["absent".into()]];
        let scope = roles("all").unwrap();
        let expected: Vec<_> = queries
            .iter()
            .map(|q| prefilter_candidates(&ctx, rows.clone(), q, scope).unwrap())
            .collect();
        ctx.diagnostics.borrow_mut().clear();
        let actual = prefilter_candidates_multi(&ctx, rows.clone(), &queries, scope).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual, vec![rows, vec![]]);
        assert_eq!(ctx.diagnostics.borrow().len(), 1);
    }
    #[test]
    fn shared_prefilter_groups_stably_and_memoizes_source_errors() {
        let ctx = Context::from_env();
        let temp = tempfile::tempdir().unwrap();
        let hit = temp.path().join("hit.jsonl");
        std::fs::write(&hit, b"TARGET.rs\n").unwrap();
        let miss = temp.path().join("miss.jsonl");
        std::fs::write(&miss, b"other.rs\n").unwrap();
        let missing = temp.path().join("missing.jsonl");
        let row = |h: &str, id: &str, p: &Path| serde_json::json!({"harness":h,"id":id,"path":p.to_str().unwrap()});
        let rows = vec![
            row("codex", "one", &hit),
            row("claude", "two", &hit),
            row("codex", "three", &hit),
            row("grok", "miss", &miss),
            row("grok", "absent-a", &missing),
            row("grok", "absent-b", &missing),
        ];
        let selected =
            prefilter_candidates(&ctx, rows, &["target.rs".into()], roles("all").unwrap()).unwrap();
        assert_eq!(
            selected.iter().map(|r| s(r, "id")).collect::<Vec<_>>(),
            vec!["one", "three", "two"]
        );
        assert_eq!(ctx.diagnostics.borrow().len(), 1);
    }
    #[test]
    fn content_search_reaches_a_compressed_rollout() {
        use ruzstd::encoding::{CompressionLevel, compress_to_vec};
        let ctx = Context::from_env();
        let temp = tempfile::tempdir().unwrap();
        let indexed = temp.path().join("rollout-a.jsonl");
        std::fs::write(
            temp.path().join("rollout-a.jsonl.zst"),
            compress_to_vec(
                &b"{\"t\":\"a needle here\"}\n"[..],
                CompressionLevel::Fastest,
            ),
        )
        .unwrap();
        assert!(file_matches(
            &ctx,
            "codex",
            &indexed,
            &[Regex::new("needle").unwrap()]
        ));
        assert!(!file_matches(
            &ctx,
            "codex",
            &indexed,
            &[Regex::new("absent").unwrap()]
        ));
        assert!(ctx.diagnostics.borrow().is_empty());
    }
    #[test]
    fn unreadable_source_reports_partial_coverage_but_no_match_does_not() {
        let ctx = Context::from_env();
        let temp = tempfile::tempdir().unwrap();
        let absent = temp.path().join("missing.jsonl");
        let patterns = vec![Regex::new("needle").unwrap()];
        assert!(!file_matches(&ctx, "claude", &absent, &patterns));
        assert_eq!(ctx.diagnostics.borrow().len(), 1);
        assert_eq!(ctx.diagnostics.borrow()[0].code, "store_unreadable");
        assert_eq!(
            ctx.diagnostics.borrow()[0].store_id.as_deref(),
            absent.to_str()
        );
        let empty = temp.path().join("empty.jsonl");
        std::fs::write(&empty, b"unrelated text\n").unwrap();
        assert!(!file_matches(&ctx, "claude", &empty, &patterns));
        assert_eq!(ctx.diagnostics.borrow().len(), 1);
        let defaults = SearchOptions::default();
        assert_eq!(
            (
                defaults.scope.as_str(),
                defaults.sort.as_str(),
                defaults.limit
            ),
            ("prompts", "relevance", 15)
        );
    }
    #[test]
    fn ordering_uses_full_scores_and_match_priority() {
        let make = |id: &str, score: f64, kind: &str, date: &str| SearchMatch {
            row: serde_json::json!({"id":id,"updated":date}),
            hits: vec![Event::new(
                "prompt",
                "hit",
                parse_ts(&serde_json::json!(date)),
            )],
            score,
            match_kind: kind.into(),
        };
        let mut rows = vec![
            make("a", 1.001, "title", "2020-01-01"),
            make("b", 1.002, "", "2022-01-01"),
            make("c", 1.001, "first prompt", "2021-01-01"),
        ];
        sort_matches(&mut rows, "score");
        assert_eq!(
            rows.iter().map(|m| s(&m.row, "id")).collect::<Vec<_>>(),
            vec!["b", "c", "a"]
        );
        sort_matches(&mut rows, "recent");
        assert_eq!(
            rows.iter().map(|m| s(&m.row, "id")).collect::<Vec<_>>(),
            vec!["a", "c", "b"]
        );
        sort_matches(&mut rows, "oldest");
        assert_eq!(
            rows.iter().map(|m| s(&m.row, "id")).collect::<Vec<_>>(),
            vec!["a", "c", "b"]
        );
    }
    #[test]
    fn exact_binary_decimal_rounding() {
        assert_eq!(rounded_score(2.675), 2.67);
        assert_eq!(rounded_score(2.685), 2.69);
        assert_eq!(rounded_score(1.125), 1.12);
    }
    #[test]
    fn bm25_hand_calculation() {
        let idf = (1.0_f64 + 8.5 / 2.5).ln();
        assert!((bm25(&[1.0], 10, &[idf], 10.0) - idf).abs() < 1e-12);
        assert!(bm25(&[2.0], 10, &[idf], 10.0) > idf);
        assert!(bm25(&[1.0], 20, &[idf], 10.0) < idf);
    }
    #[test]
    fn hyphenated_terms_weigh_embedded_mentions_down() {
        // Plain terms keep the reference substring count.
        assert_eq!(term_frequency("latch k7-latch latches", "latch"), 3.0);
        // Whole names: text edges and punctuation other than `_` and `-` bound them.
        assert_eq!(
            term_frequency("k7-latch, (k7-latch) contracts/k7-latch.md", "k7-latch"),
            3.0
        );
        // Embedded in longer names, on either side.
        assert_eq!(
            term_frequency(
                "impl-k7-latch-r1 xk7-latch k7-latch_2 k7-latch-r2",
                "k7-latch"
            ),
            4.0 * EMBEDDED_NAME_WEIGHT
        );
        assert_eq!(
            term_frequency("é k7-latché", "k7-latch"),
            EMBEDDED_NAME_WEIGHT
        );
        assert_eq!(term_frequency("nothing here", "k7-latch"), 0.0);
    }
    #[test]
    fn snippet_counts_unicode_characters() {
        assert_eq!(snippet("ééé café later", &["café".into()], 6), "…é café");
    }
    #[test]
    fn prefilter_all_terms_across_lines_and_unicode() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        use std::io::Write;
        writeln!(f, "first ALPHA\nsecond KELVIN").unwrap();
        let p = vec![
            RegexBuilder::new("alpha")
                .case_insensitive(true)
                .build()
                .unwrap(),
            RegexBuilder::new("kelvin")
                .case_insensitive(true)
                .build()
                .unwrap(),
        ];
        assert!(file_matches_inner(f.path(), &p).unwrap());
        let p = vec![Regex::new("absent").unwrap()];
        assert!(!file_matches_inner(f.path(), &p).unwrap());
    }
}

#[cfg(test)]
mod sql_tests {
    use super::*;
    #[test]
    fn t3_prefilter_preserves_same_message_and_role_constraints() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp
            .path()
            .join(".t3")
            .join("userdata")
            .join("state.sqlite");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let writer = rusqlite::Connection::open(&db).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE projection_thread_messages(thread_id TEXT,role TEXT,text TEXT); CREATE TABLE projection_thread_activities(thread_id TEXT,kind TEXT,payload_json TEXT); INSERT INTO projection_thread_messages VALUES ('same','user','alpha beta'),('split','user','alpha'),('split','user','beta'),('reply','assistant','alpha beta'); INSERT INTO projection_thread_activities VALUES ('tool','tool.completed','alpha beta');").unwrap();
        let mut ctx = Context::from_env();
        ctx.home = temp.path().to_owned();
        let terms = vec!["alpha".into(), "beta".into()];
        assert_eq!(
            sql_candidates(&ctx, "t3", &terms, &["prompt"]).unwrap(),
            HashSet::from(["same".into()])
        );
        assert_eq!(
            sql_candidates(&ctx, "t3", &terms, &["assistant"]).unwrap(),
            HashSet::from(["reply".into()])
        );
        assert_eq!(
            sql_candidates(&ctx, "t3", &terms, &["tool"]).unwrap(),
            HashSet::from(["tool".into()])
        );
        // The reader must coexist with an uncommitted WAL writer and see only committed data.
        writer.execute_batch("BEGIN IMMEDIATE; INSERT INTO projection_thread_messages VALUES ('uncommitted','user','alpha beta');").unwrap();
        assert_eq!(
            sql_candidates(&ctx, "t3", &terms, &["prompt"]).unwrap(),
            HashSet::from(["same".into()])
        );
        writer.execute_batch("ROLLBACK").unwrap();
    }
}
