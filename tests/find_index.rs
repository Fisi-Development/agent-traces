//! `find` over the prompts and assistant scopes answers from a SQLite index of
//! events. Runs the binary against a synthetic store with a cleared environment
//! and a private cache, and compares it with the raw transcript scan
//! (`AGENT_TRACES_NO_INDEX=1`).
//!
//! BM25 scores are computed over the candidate sessions' events. The index
//! scores a session from every in-scope event it stored, the raw scan from the
//! lines that hold a term plus a count of the others, so `_score` and the order
//! of equal-quality sessions may differ. Which sessions match, and each one's
//! hits, must not: every comparison against the raw scan drops the score.
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const NOW: &str = "2026-10-05T12:00:00Z";

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run_with(home: &Path, cache: &Path, args: &[&str], env: &[(&str, &str)]) -> Out {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_TRACES_NOW", NOW)
        .env("AGENT_TRACES_CACHE", cache)
        .env("AGENT_TRACES_OUT", home.join("out"))
        .env("AGENT_TRACES_HOST", "testhost")
        .env("AGENT_TRACES_HOST_ID", "testhost")
        .env("TZ", "UTC")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    Out {
        stdout: String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n"),
        stderr: String::from_utf8(out.stderr).unwrap().replace("\r\n", "\n"),
        code: out.status.code().unwrap(),
    }
}

fn run(home: &Path, args: &[&str]) -> Out {
    run_with(home, &home.join("cache"), args, &[])
}

fn raw(home: &Path, args: &[&str]) -> Out {
    run_with(
        home,
        &home.join("cache-raw"),
        args,
        &[("AGENT_TRACES_NO_INDEX", "1")],
    )
}

fn user(ts: usize, text: &str) -> Value {
    json!({"type":"user","uuid":format!("u{ts}-{text}"),"cwd":"/w","timestamp":format!("2026-10-01T09:{ts:02}:00Z"),
           "message":{"content":text}})
}

fn assistant(ts: usize, text: &str) -> Value {
    json!({"type":"assistant","uuid":format!("a{ts}-{text}"),"cwd":"/w","timestamp":format!("2026-10-01T09:{ts:02}:00Z"),
           "message":{"model":"m","content":[{"type":"text","text":text}]}})
}

fn claude(home: &Path, id: &str, records: &[Value]) -> PathBuf {
    let dir = home.join(".claude/projects/w");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{id}.jsonl"));
    fs::write(&path, lines(records)).unwrap();
    path
}

fn lines(records: &[Value]) -> String {
    records.iter().map(|v| format!("{v}\n")).collect()
}

fn append(path: &Path, records: &[Value]) {
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(lines(records).as_bytes())
        .unwrap();
}

fn codex(home: &Path, id: &str, records: &[Value]) {
    let home_dir = home.join(".codex");
    let dir = home_dir.join("sessions/2026/10/01");
    fs::create_dir_all(&dir).unwrap();
    let mut all = vec![
        json!({"type":"session_meta","timestamp":"2026-10-01T09:00:00Z","payload":{"id":id,"cwd":"/w","timestamp":"2026-10-01T09:00:00Z"}}),
    ];
    all.extend(records.iter().cloned());
    let rollout = dir.join(format!("rollout-2026-10-01T09-00-00-{id}.jsonl"));
    fs::write(&rollout, lines(&all)).unwrap();
    let db = rusqlite::Connection::open(home_dir.join("state_5.sqlite")).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS threads(id TEXT,cwd TEXT,created_at INTEGER,updated_at INTEGER,rollout_path TEXT,source TEXT,first_user_message TEXT);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads VALUES (?1, '/w', 1790000000, 1790000100, ?2, 'cli', 'codex prompt')",
        [id, rollout.to_str().unwrap()],
    )
    .unwrap();
}

fn codex_message(ts: usize, role: &str, text: &str) -> Value {
    let kind = if role == "user" {
        "input_text"
    } else {
        "output_text"
    };
    json!({"type":"response_item","timestamp":format!("2026-10-01T09:{ts:02}:30Z"),
           "payload":{"type":"message","role":role,"content":[{"type":kind,"text":text}]}})
}

const C1: &str = "cccccccc-0000-4000-8000-000000000001";
const C2: &str = "cccccccc-0000-4000-8000-000000000002";
const C3: &str = "cccccccc-0000-4000-8000-000000000003";
const C4: &str = "cccccccc-0000-4000-8000-000000000004";
const C5: &str = "cccccccc-0000-4000-8000-000000000005";
const X1: &str = "dddddddd-0000-4000-8000-000000000001";

fn store() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    // alpha and beta in the assistant reply only: a decoy for a prompts search.
    claude(
        home,
        C1,
        &[
            user(0, "Das ist ärger mit der Heizung"),
            assistant(1, "alpha and beta live in this reply"),
            user(2, "second prompt with zq marker"),
        ],
    );
    // alpha and beta in two different prompts.
    claude(
        home,
        C2,
        &[
            user(0, "alpha comes first"),
            assistant(1, "unrelated reply"),
            user(2, "then beta comes second"),
        ],
    );
    // A term only a tool result holds.
    claude(
        home,
        C3,
        &[
            user(0, "run the thing"),
            json!({"type":"assistant","uuid":"t3a","cwd":"/w","timestamp":"2026-10-01T09:01:00Z",
                   "message":{"content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"ls"}}]}}),
            json!({"type":"user","uuid":"t3r","cwd":"/w","timestamp":"2026-10-01T09:02:00Z",
                   "message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"needle-result-only"}]}}),
        ],
    );
    // A term only JSON metadata holds, and an upper-case non-ASCII prompt.
    let mut meta = user(0, "plain prompt");
    meta["gitBranch"] = json!("metaonly-branch");
    claude(home, C4, &[meta, user(1, "ÄRGER im Büro")]);
    claude(
        home,
        C5,
        &[
            user(0, "he said \"quoted phrase\" loudly"),
            assistant(1, "noted"),
        ],
    );
    codex(
        home,
        X1,
        &[
            codex_message(0, "user", "codex ärger prompt"),
            codex_message(1, "assistant", "codex assistant alpha"),
        ],
    );
    t
}

/// Session ids with their hit snippets, for failure messages.
fn brief(stdout: &str) -> String {
    stdout
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            format!("{} {}\n", v["id"], v["hits"])
        })
        .collect()
}

/// Rows keyed by address with `_score` dropped, to compare matches and hits.
fn matches_only(stdout: &str) -> Vec<Value> {
    let mut rows: Vec<Value> = stdout
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .map(|mut row| {
            row.as_object_mut().unwrap().remove("_score");
            // Hits are ordered by BM25, so their order (and where `--hits N`
            // cuts) may differ between the paths; the set must not.
            if let Some(hits) = row["hits"].as_array_mut() {
                hits.sort_by_key(|h| h.to_string());
            }
            row
        })
        .collect();
    rows.sort_by_key(|r| r.to_string());
    rows
}

#[test]
fn the_index_answers_what_the_raw_scan_answers() {
    let t = store();
    let home = t.path();
    let cases: &[&[&str]] = &[
        &["alpha", "beta", "--in", "prompts"],
        &["alpha", "beta", "--in", "assistant"],
        &["alpha", "--in", "assistant"],
        &["needle-result-only", "--in", "prompts"],
        &["needle-result-only", "--in", "tools"],
        &["needle-result-only", "--in", "results"],
        &["needle-result-only", "--in", "all"],
        &["zq", "--in", "prompts"],
        &["quoted", "phrase", "--in", "prompts"],
        &["metaonly-branch", "--in", "prompts"],
        &["metaonly-branch", "--in", "all"],
        &["codex", "alpha", "--in", "assistant"],
        &["codex", "--in", "prompts"],
    ];
    for args in cases {
        compare_with_raw(home, args, false);
    }
    // Non-ASCII terms: the raw scan folds case with Unicode rules, so it finds
    // `ÄRGER im Büro` and the escaped spelling exactly as the index does.
    for term in ["ärger", "ÄRGER", "Ärger"] {
        compare_with_raw(home, &[term, "--in", "prompts"], false);
        let ids = raw(home, &["find", term, "--in", "prompts", "--ids"]).stdout;
        assert!(ids.contains(C4), "{term}: {ids}");
    }
    // A term with a double quote: the raw scan tests the JSON line, which escapes
    // the quote, so it misses what the index (which stores the text) finds.
    compare_with_raw(home, &["\"quoted phrase\"", "--in", "prompts"], true);
    let named = run(home, &["find", "ärger", "--in", "prompts", "--ids"]);
    assert!(named.stdout.contains(C4), "{}", named.stdout);
    assert_eq!(named.code, 0);
    // The cross-event and 2-character cases really matched something.
    let two = run(
        home,
        &["find", "alpha", "beta", "--in", "prompts", "--json"],
    );
    assert!(
        two.stdout.contains(C2) && !two.stdout.contains(C1),
        "{}",
        two.stdout
    );
    assert!(
        run(home, &["find", "zq", "--in", "prompts", "--ids"])
            .stdout
            .contains(C1)
    );
    assert!(
        run(home, &["find", "needle-result-only", "--in", "tools"]).code == 1,
        "tool input is not the result"
    );
    assert_eq!(
        run(
            home,
            &["find", "needle-result-only", "--in", "results", "--ids"]
        )
        .code,
        0
    );
    assert!(run(home, &["find", "needle-result-only", "--in", "prompts"]).code == 1);
    // The raw scan misses a quoted term (see above); the index finds it, as
    // the prompt really contains it.
    assert_eq!(
        run(home, &["find", "\"quoted phrase\"", "--in", "prompts"]).code,
        0
    );
    assert_eq!(
        raw(home, &["find", "\"quoted phrase\"", "--in", "prompts"]).code,
        1
    );
    assert!(
        run(home, &["find", "quoted phrase", "--in", "prompts", "--ids"])
            .stdout
            .contains(C5)
    );
}

#[test]
fn the_raw_scan_finds_a_word_written_with_json_escapes_as_the_index_does() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    claude(
        home,
        C1,
        &[user(0, "plain prompt"), assistant(1, "unrelated")],
    );
    // serde_json writes UTF-8, so the escaped spellings are written by hand.
    let line = |ts: usize, text: &str| {
        format!(
            "{{\"type\":\"user\",\"uuid\":\"e{ts}\",\"cwd\":\"/w\",\"timestamp\":\"2026-10-01T09:{ts:02}:00Z\",\"message\":{{\"content\":\"{text}\"}}}}\n"
        )
    };
    for (id, text) in [
        (C2, "viel \\u00c4RGER"),
        (C3, "viel \\u00e4rger"),
        (C4, "viel \\u00C4rger"),
    ] {
        fs::write(
            home.join(format!(".claude/projects/w/{id}.jsonl")),
            line(0, text),
        )
        .unwrap();
    }
    for term in ["ärger", "ÄRGER"] {
        compare_with_raw(home, &[term, "--in", "prompts"], false);
        let ids = raw(home, &["find", term, "--in", "prompts", "--ids"]).stdout;
        assert!(
            ids.contains(C2) && ids.contains(C3) && ids.contains(C4) && !ids.contains(C1),
            "{term}: {ids}"
        );
    }
}

#[test]
fn the_raw_scan_keeps_files_whose_lowercase_matches_only_after_expansion() {
    // `İ` lowercases to `i` plus U+0307, which simple case folding does not equate.
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    claude(home, C1, &[user(0, "plain prompt")]);
    claude(home, C2, &[user(0, "nach \u{130}stanbul")]);
    claude(home, C3, &[user(0, "nach i\u{307}stanbul")]);
    for term in ["\u{130}stanbul", "i\u{307}stanbul"] {
        compare_with_raw(home, &[term, "--in", "prompts"], false);
        let ids = raw(home, &["find", term, "--in", "prompts", "--ids"]).stdout;
        assert!(
            ids.contains(C2) && ids.contains(C3) && !ids.contains(C1),
            "{term}: {ids}"
        );
    }
}

#[test]
fn the_raw_scan_keeps_letters_newer_than_the_regex_case_tables() {
    // Rust lowercases U+A7D2 to U+A7D3 (Unicode 17); the regex crate's simple case
    // folding does not pair them yet, so the term's uppercase is an alternative too.
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    claude(home, C1, &[user(0, "plain prompt")]);
    claude(home, C2, &[user(0, "double thorn \u{a7d2}")]);
    for term in ["\u{a7d3}", "\u{a7d2}"] {
        compare_with_raw(home, &[term, "--in", "prompts"], false);
        let ids = raw(home, &["find", term, "--in", "prompts", "--ids"]).stdout;
        assert!(ids.contains(C2) && !ids.contains(C1), "{term}: {ids}");
    }
}

/// Matches and hits equal the raw scan's, scores dropped. With `superset`, the
/// indexed matches need only contain the raw ones.
fn compare_with_raw(home: &Path, args: &[&str], superset: bool) {
    {
        let mut full = vec!["find"];
        full.extend_from_slice(args);
        full.extend(["--json", "--limit", "50", "--hits", "5"]);
        let with = run(home, &full);
        let without = raw(home, &full);
        let (indexed, direct) = (matches_only(&with.stdout), matches_only(&without.stdout));
        let message = format!(
            "{args:?}\nindexed:\n{}raw:\n{}",
            brief(&with.stdout),
            brief(&without.stdout)
        );
        if superset {
            assert!(direct.iter().all(|r| indexed.contains(r)), "{message}");
        } else {
            assert_eq!(indexed, direct, "{message}");
            assert_eq!(with.code, without.code, "{args:?}");
        }
        // The raw scan is never behind the footer's claim.
        let footer = |o: &Out| {
            o.stderr
                .lines()
                .find(|l| l.contains("searched:"))
                .unwrap()
                .to_owned()
        };
        let scope = args[args.iter().position(|a| *a == "--in").unwrap() + 1];
        if matches!(scope, "prompts" | "assistant") {
            assert!(footer(&with).contains("index: "), "{args:?}");
            assert!(!footer(&with).contains("index: off"), "{args:?}");
        } else {
            assert!(footer(&with).ends_with("index: off"), "{args:?}");
        }
        assert!(footer(&without).ends_with("index: off"), "{args:?}");
    }
}

/// Rows are keyed by the path the session listing reports, whose form can differ
/// from the test's own (on Windows, `\` where the test joined with `/`), so the helpers
/// match on the file name, unique in every fixture.
const BY_NAME: &str = "substr(path, -length(?1) - 1) IN ('/' || ?1, '\\' || ?1)";

fn name(path: &Path) -> String {
    path.file_name().unwrap().to_str().unwrap().to_owned()
}

fn count(db: &rusqlite::Connection, path: &Path) -> i64 {
    db.query_row(
        &format!("SELECT count(*) FROM events WHERE {BY_NAME}"),
        [name(path)],
        |r| r.get(0),
    )
    .unwrap()
}

fn offset(db: &rusqlite::Connection, path: &Path) -> i64 {
    db.query_row(
        &format!("SELECT offset FROM files WHERE {BY_NAME}"),
        [name(path)],
        |r| r.get(0),
    )
    .unwrap()
}

fn db(home: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(home.join("cache/find-index-v4.sqlite")).unwrap()
}

#[test]
fn an_append_adds_only_the_new_events() {
    let t = store();
    let home = t.path();
    let path = home.join(format!(".claude/projects/w/{C2}.jsonl"));
    let first = run(home, &["find", "alpha", "--in", "prompts"]);
    assert!(
        first
            .stderr
            .contains("index: 0 indexed, 6 refreshed now, 0 scanned raw"),
        "{}",
        first.stderr
    );
    let (rows, end) = (count(&db(home), &path), offset(&db(home), &path));
    assert_eq!(end as u64, fs::metadata(&path).unwrap().len());
    assert!(run(home, &["find", "appended-marker", "--in", "prompts"]).code == 1);

    append(&path, &[user(5, "an appended-marker prompt")]);
    let again = run(
        home,
        &["find", "appended-marker", "--in", "prompts", "--ids"],
    );
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(again.stdout.contains(C2));
    assert!(
        again
            .stderr
            .contains("5 indexed, 1 refreshed now, 0 scanned raw"),
        "{}",
        again.stderr
    );
    // One new row, none re-added, and the offset moved to the new end.
    assert_eq!(count(&db(home), &path), rows + 1);
    assert_eq!(
        offset(&db(home), &path) as u64,
        fs::metadata(&path).unwrap().len()
    );
    assert!(offset(&db(home), &path) > end);
}

#[test]
fn a_rewritten_file_forgets_its_old_text() {
    let t = store();
    let home = t.path();
    let path = home.join(format!(".claude/projects/w/{C2}.jsonl"));
    assert_eq!(
        run(home, &["find", "comes first", "--in", "prompts"]).code,
        0
    );
    // Shorter than before, so the index sees a shrunk file.
    fs::write(&path, lines(&[user(0, "wholly new words")])).unwrap();
    let old = run(home, &["find", "comes first", "--in", "prompts"]);
    assert_eq!(old.code, 1, "{}", old.stdout);
    let new = run(home, &["find", "wholly new", "--in", "prompts", "--ids"]);
    assert_eq!(new.code, 0);
    assert!(new.stdout.contains(C2));
    assert_eq!(count(&db(home), &path), 1);
}

#[test]
fn a_zero_budget_scans_everything_raw_with_the_same_answers() {
    let t = store();
    let home = t.path();
    let args = ["find", "alpha", "beta", "--in", "prompts", "--json"];
    let budgeted = run_with(
        home,
        &home.join("cache"),
        &args,
        &[("AGENT_TRACES_INDEX_BUDGET_MS", "0")],
    );
    let plain = raw(home, &args);
    assert_eq!(budgeted.stdout, plain.stdout);
    assert!(
        budgeted
            .stderr
            .contains("index: 0 indexed, 0 refreshed now, 6 scanned raw"),
        "{}",
        budgeted.stderr
    );
}

#[test]
fn an_unwritable_cache_falls_back_to_the_raw_scan_with_a_diagnostic() {
    let t = store();
    let home = t.path();
    // A cache path below a regular file cannot be created, even by root.
    fs::write(home.join("blocker"), "not a directory").unwrap();
    let cache = home.join("blocker/cache");
    let args = ["find", "alpha", "beta", "--in", "prompts", "--json"];
    let broken = run_with(home, &cache, &args, &[]);
    let plain = raw(home, &args);
    assert_eq!(broken.stdout, plain.stdout);
    assert_eq!(broken.code, plain.code);
    assert!(
        broken.stderr.contains("index: unavailable ("),
        "{}",
        broken.stderr
    );
    assert!(
        broken.stderr.contains("find index unavailable"),
        "{}",
        broken.stderr
    );
    // Versioned output names it as information and stays complete.
    let v1 = run_with(
        home,
        &cache,
        &[
            "find",
            "alpha",
            "beta",
            "--in",
            "prompts",
            "--json",
            "--schema-version",
            "1",
        ],
        &[("AGENT_TRACES_HOST_ID", "testhost")],
    );
    let envelope: Value = serde_json::from_str(&v1.stdout).unwrap();
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["complete"], true);
    let note = envelope["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "index_unavailable")
        .expect("index_unavailable diagnostic");
    assert_eq!(note["severity"], "info");
    assert!(
        envelope["result"]["index"]
            .as_str()
            .unwrap()
            .starts_with("unavailable (")
    );
}

#[test]
fn a_corrupt_index_falls_back_to_the_raw_scan() {
    let t = store();
    let home = t.path();
    fs::create_dir_all(home.join("cache")).unwrap();
    fs::write(
        home.join("cache/find-index-v4.sqlite"),
        "this is not a database at all",
    )
    .unwrap();
    let args = ["find", "alpha", "beta", "--in", "prompts", "--json"];
    let broken = run(home, &args);
    assert_eq!(broken.stdout, raw(home, &args).stdout);
    assert!(
        broken.stderr.contains("index: unavailable ("),
        "{}",
        broken.stderr
    );
}

#[test]
fn the_index_command_warms_the_index_and_reports_counts() {
    let t = store();
    let home = t.path();
    let first = run(home, &["index"]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(
        first
            .stdout
            .contains("0 files already indexed, 6 refreshed"),
        "{}",
        first.stdout
    );
    assert!(first.stdout.contains("MB on disk"), "{}", first.stdout);
    let second = run(home, &["index"]);
    assert!(
        second
            .stdout
            .contains("6 files already indexed, 0 refreshed"),
        "{}",
        second.stdout
    );
    // A find after the warm-up reads no transcript.
    let find = run(home, &["find", "alpha", "--in", "prompts"]);
    assert!(
        find.stderr
            .contains("index: 6 indexed, 0 refreshed now, 0 scanned raw"),
        "{}",
        find.stderr
    );
}

fn bump_mtime(path: &Path) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    let later =
        fs::metadata(path).unwrap().modified().unwrap() + std::time::Duration::from_secs(10);
    file.set_modified(later).unwrap();
}

fn files_rows(db: &rusqlite::Connection, path: &Path) -> i64 {
    db.query_row(
        &format!("SELECT count(*) FROM files WHERE {BY_NAME}"),
        [name(path)],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn a_same_size_rewrite_is_reindexed_not_trusted() {
    let t = store();
    let home = t.path();
    let path = home.join(format!(".claude/projects/w/{C2}.jsonl"));
    fs::write(&path, lines(&[user(0, "an oldword in a prompt")])).unwrap();
    assert_eq!(run(home, &["find", "oldword", "--in", "prompts"]).code, 0);

    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("oldword", "newword")).unwrap();
    bump_mtime(&path);
    let args = ["find", "newword", "--in", "prompts", "--json"];
    let indexed = run(home, &args);
    assert_eq!(indexed.code, 0, "{}", indexed.stderr);
    assert!(indexed.stdout.contains(C2), "{}", indexed.stdout);
    assert_eq!(
        matches_only(&indexed.stdout),
        matches_only(&raw(home, &args).stdout)
    );
    assert_eq!(
        run(home, &["find", "oldword", "--in", "prompts"]).code,
        1,
        "the old word is gone from the index"
    );
}

#[test]
fn growth_with_a_changed_byte_in_the_last_4k_reindexes_from_the_start() {
    let t = store();
    let home = t.path();
    let path = home.join(format!(".claude/projects/w/{C2}.jsonl"));
    fs::write(
        &path,
        lines(&[user(0, "first oldword"), user(1, "second line")]),
    )
    .unwrap();
    assert_eq!(run(home, &["find", "oldword", "--in", "prompts"]).code, 0);
    let rows = count(&db(home), &path);

    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("oldword", "newword")).unwrap();
    append(&path, &[user(2, "grown tail")]);
    let grown = run(home, &["find", "newword", "--in", "prompts", "--ids"]);
    assert_eq!(grown.code, 0, "{}", grown.stderr);
    assert!(grown.stdout.contains(C2));
    assert_eq!(run(home, &["find", "oldword", "--in", "prompts"]).code, 1);
    assert_eq!(run(home, &["find", "grown", "--in", "prompts"]).code, 0);
    // Reindexed once, not appended to: the three events, each once.
    assert_eq!(count(&db(home), &path), rows + 1);
    assert_eq!(
        offset(&db(home), &path) as u64,
        fs::metadata(&path).unwrap().len()
    );
}

#[test]
fn a_codex_path_that_already_ends_in_zst_is_decompressed_to_index() {
    use ruzstd::encoding::{CompressionLevel, compress_to_vec};
    let t = store();
    let home = t.path();
    let id = "dddddddd-0000-4000-8000-000000000002";
    codex(
        home,
        id,
        &[codex_message(0, "user", "a compressneedle prompt")],
    );
    let rollout = home.join(format!(
        ".codex/sessions/2026/10/01/rollout-2026-10-01T09-00-00-{id}.jsonl"
    ));
    let zst = PathBuf::from(format!("{}.zst", rollout.display()));
    fs::write(
        &zst,
        compress_to_vec(&fs::read(&rollout).unwrap()[..], CompressionLevel::Fastest),
    )
    .unwrap();
    fs::remove_file(&rollout).unwrap();
    let state = rusqlite::Connection::open(home.join(".codex/state_5.sqlite")).unwrap();
    state
        .execute(
            "UPDATE threads SET rollout_path = ?1 WHERE id = ?2",
            [zst.to_str().unwrap(), id],
        )
        .unwrap();
    drop(state);

    let args = ["find", "compressneedle", "--in", "prompts", "--json"];
    let direct = raw(home, &args);
    assert_eq!(direct.code, 0, "{}{}", direct.stdout, direct.stderr);
    assert!(direct.stdout.contains(id), "{}", direct.stdout);
    let indexed = run(home, &args);
    assert_eq!(indexed.code, 0, "{}", indexed.stderr);
    assert_eq!(matches_only(&indexed.stdout), matches_only(&direct.stdout));
    assert!(count(&db(home), &zst) > 0);
}

#[test]
fn a_deleted_transcript_loses_its_rows_on_the_next_index() {
    let t = store();
    let home = t.path();
    let path = home.join(format!(".claude/projects/w/{C3}.jsonl"));
    assert_eq!(run(home, &["index"]).code, 0);
    assert!(count(&db(home), &path) > 0);
    assert_eq!(files_rows(&db(home), &path), 1);

    fs::remove_file(&path).unwrap();
    let again = run(home, &["index"]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    // The sweep's removal is reported, not only done (review round 2).
    assert!(again.stdout.contains(", 1 gone,"), "{}", again.stdout);
    assert_eq!(count(&db(home), &path), 0);
    assert_eq!(files_rows(&db(home), &path), 0);
}

#[test]
fn a_find_drops_the_rows_of_a_harness_whose_last_transcript_was_deleted() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let path = claude(home, C1, &[user(0, "a lonelyword prompt")]);
    assert_eq!(
        run(home, &["find", "lonelyword", "--in", "prompts"]).code,
        0
    );
    assert_eq!(files_rows(&db(home), &path), 1);

    // No claude transcript is left to discover, so only the sweep can see it.
    fs::remove_file(&path).unwrap();
    assert_eq!(
        run(home, &["find", "lonelyword", "--in", "prompts"]).code,
        1
    );
    assert_eq!(files_rows(&db(home), &path), 0);
    assert_eq!(count(&db(home), &path), 0);
}

#[test]
fn a_same_size_rewrite_behind_a_partial_last_line_is_reindexed() {
    let t = store();
    let home = t.path();
    let path = home.join(format!(".claude/projects/w/{C2}.jsonl"));
    // The changed prompt sits more than 4 KB before the offset, and the file ends
    // in a partial line, so the offset stops short of the size.
    let mut records = vec![user(0, "an oldword in a prompt")];
    records.extend((1..12).map(|i| user(i, &"padding ".repeat(60))));
    let text = format!("{}{{\"type\":\"user\",\"mess", lines(&records));
    fs::write(&path, &text).unwrap();
    assert_eq!(run(home, &["find", "oldword", "--in", "prompts"]).code, 0);
    assert!((offset(&db(home), &path) as u64) < fs::metadata(&path).unwrap().len());

    fs::write(&path, text.replace("oldword", "newword")).unwrap();
    bump_mtime(&path);
    let args = ["find", "newword", "--in", "prompts", "--json"];
    let indexed = run(home, &args);
    assert_eq!(indexed.code, 0, "{}", indexed.stderr);
    assert_eq!(
        matches_only(&indexed.stdout),
        matches_only(&raw(home, &args).stdout)
    );
    assert_eq!(run(home, &["find", "oldword", "--in", "prompts"]).code, 1);
}

#[test]
fn older_schema_databases_are_preserved_on_open() {
    let t = store();
    let home = t.path();
    let cache = home.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let old = [
        "find-index-v1.sqlite",
        "find-index-v2.sqlite",
        "find-index-v3.sqlite",
    ];
    for name in old {
        for suffix in ["", "-wal", "-shm"] {
            fs::write(cache.join(format!("{name}{suffix}")), "old").unwrap();
        }
    }
    assert_eq!(run(home, &["find", "alpha", "--in", "prompts"]).code, 0);
    for name in old {
        for suffix in ["", "-wal", "-shm"] {
            assert!(
                fs::read(cache.join(format!("{name}{suffix}"))).unwrap() == b"old",
                "{name}{suffix}"
            );
        }
    }
    assert!(cache.join("find-index-v4.sqlite").exists());
}

#[test]
fn a_deleted_codex_rollout_still_listed_in_its_database_counts_once() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let id = "dddddddd-0000-4000-8000-000000000003";
    codex(home, id, &[codex_message(0, "user", "a countword prompt")]);
    assert_eq!(run(home, &["index"]).code, 0);
    let rollout = home.join(format!(
        ".codex/sessions/2026/10/01/rollout-2026-10-01T09-00-00-{id}.jsonl"
    ));
    // The thread row stays in Codex's database, so discovery still lists it.
    fs::remove_file(&rollout).unwrap();
    let again = run(home, &["index"]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(again.stdout.contains(", 1 gone,"), "{}", again.stdout);
}

fn doc_count(db: &rusqlite::Connection, path: &Path) -> i64 {
    db.query_row(
        &format!("SELECT count(*) FROM docs WHERE {BY_NAME}"),
        [name(path)],
        |r| r.get(0),
    )
    .unwrap()
}

/// Every FTS row has a `docs` row with the same rowid and path, and each file's
/// `ord` runs 0, 1, 2 in file order.
fn docs_mirror_events(db: &rusqlite::Connection) {
    let orphans: i64 = db
        .query_row(
            "SELECT (SELECT count(*) FROM events e LEFT JOIN docs d ON d.id = e.rowid
                         WHERE d.id IS NULL OR d.path <> e.path)
                  + (SELECT count(*) FROM docs d LEFT JOIN events e ON e.rowid = d.id
                         WHERE e.rowid IS NULL)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0, "events and docs rows pair up by rowid");
    let gaps: i64 = db
        .query_row(
            "SELECT count(*) FROM (SELECT path FROM docs GROUP BY path
                                   HAVING count(*) <> max(ord) + 1 OR min(ord) <> 0)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(gaps, 0);
}

#[test]
fn docs_rows_follow_events_rows_through_append_rewrite_delete_and_forget() {
    let t = store();
    let home = t.path();
    let path = home.join(format!(".claude/projects/w/{C2}.jsonl"));
    let other = home.join(format!(".claude/projects/w/{C3}.jsonl"));
    assert_eq!(run(home, &["index"]).code, 0);
    let rows = count(&db(home), &path);
    assert!(rows > 0);
    assert_eq!(doc_count(&db(home), &path), rows);
    docs_mirror_events(&db(home));

    // An append adds one row to each table and continues `ord`.
    append(&path, &[user(5, "an appended-marker prompt")]);
    assert_eq!(run(home, &["index"]).code, 0);
    assert_eq!(count(&db(home), &path), rows + 1);
    assert_eq!(doc_count(&db(home), &path), rows + 1);
    docs_mirror_events(&db(home));

    // A rewrite replaces both, not adds to them.
    fs::write(&path, lines(&[user(0, "wholly new words")])).unwrap();
    assert_eq!(run(home, &["index"]).code, 0);
    assert_eq!(count(&db(home), &path), 1);
    assert_eq!(doc_count(&db(home), &path), 1);
    docs_mirror_events(&db(home));

    // A deleted transcript loses both when the sweep forgets it.
    assert!(doc_count(&db(home), &other) > 0);
    fs::remove_file(&other).unwrap();
    assert_eq!(run(home, &["index"]).code, 0);
    assert_eq!(count(&db(home), &other), 0);
    assert_eq!(doc_count(&db(home), &other), 0);
    docs_mirror_events(&db(home));
}

/// The point of the stored events: a find over files the index holds current
/// reads no transcript. After warming, each transcript's bytes are replaced with
/// different text of the same length, and its size and mtime restored, so the
/// index still believes the file is current. If the find read the transcript it
/// would report the new text; it reports the old, from `docs`.
#[test]
fn an_indexed_find_reads_no_transcript_for_a_current_file() {
    let t = store();
    let home = t.path();
    assert_eq!(run(home, &["index"]).code, 0);
    let args = ["find", "alpha", "--in", "prompts", "--json", "--hits", "5"];
    let before = run(home, &args);
    assert_eq!(before.code, 0, "{}", before.stderr);
    assert!(before.stdout.contains("alpha comes first"));

    let mut changed = 0;
    for entry in walk(&home.join(".claude"))
        .into_iter()
        .chain(walk(&home.join(".codex")))
    {
        if entry.extension().is_some_and(|e| e == "jsonl") {
            let modified = fs::metadata(&entry).unwrap().modified().unwrap();
            let text = fs::read_to_string(&entry).unwrap();
            // Same length: "alpha" and "omega" are both five bytes.
            fs::write(&entry, text.replace("alpha", "omega")).unwrap();
            fs::OpenOptions::new()
                .write(true)
                .open(&entry)
                .unwrap()
                .set_modified(modified)
                .unwrap();
            changed += 1;
        }
    }
    assert!(changed >= 6);
    // The raw scan sees the edit, proving a transcript read would show it.
    assert_eq!(raw(home, &args).code, 1, "alpha is gone from the files");

    let after = run(home, &args);
    assert!(
        after
            .stderr
            .contains("index: 6 indexed, 0 refreshed now, 0 scanned raw"),
        "{}",
        after.stderr
    );
    assert_eq!(after.code, 0, "{}", after.stderr);
    assert_eq!(after.stdout, before.stdout);
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(read) = fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walk(&path));
            } else {
                found.push(path);
            }
        }
    }
    found
}

/// Raw `find` drops a Claude record whose `uuid` an earlier record in the file
/// already used, whole. The index must serve the same events: alpha and beta sit
/// in two prompts sharing one uuid, so only the first counts and no session has both.
#[test]
fn indexed_find_keeps_the_claude_uuid_dedup() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let dup = |ts: usize, text: &str| {
        let mut v = user(ts, text);
        v["uuid"] = json!("shared-uuid");
        v
    };
    claude(home, C1, &[dup(0, "alpha first"), dup(1, "beta second")]);
    let args = ["find", "alpha", "beta", "--in", "prompts", "--json"];
    let direct = raw(home, &args);
    assert_eq!(
        direct.code, 1,
        "raw drops the second record: {}",
        direct.stdout
    );
    let indexed = run(home, &args);
    assert_eq!(indexed.code, 1, "{}", indexed.stdout);
    // The first record alone still matches on its own term.
    assert_eq!(run(home, &["find", "alpha", "--in", "prompts"]).code, 0);
    assert_eq!(run(home, &["find", "beta", "--in", "prompts"]).code, 1);
}

/// A `docs` table with fewer rows than the file's stored event count cannot be
/// trusted: the find falls back to the transcript instead of scoring a part.
#[test]
fn missing_docs_rows_fall_back_to_the_transcript() {
    let t = store();
    let home = t.path();
    let path = home.join(format!(".claude/projects/w/{C2}.jsonl"));
    assert_eq!(run(home, &["index"]).code, 0);
    let args = ["find", "alpha", "beta", "--in", "prompts", "--json"];
    let want = matches_only(&raw(home, &args).stdout);
    assert!(want.iter().any(|r| r["id"] == C2));
    // Lose the docs row holding `alpha` while the file is unchanged and current.
    let n = db(home)
        .execute(
            &format!("DELETE FROM docs WHERE {BY_NAME} AND event LIKE '%alpha comes first%'"),
            [name(&path)],
        )
        .unwrap();
    assert_eq!(n, 1);
    let indexed = run(home, &args);
    assert!(
        indexed.stderr.contains("6 indexed, 0 refreshed now"),
        "{}",
        indexed.stderr
    );
    assert_eq!(matches_only(&indexed.stdout), want);
}

#[test]
fn a_half_rebuilt_v4_index_is_rebuilt_not_left_broken() {
    // An interrupted rebuild could leave current `docs` beside an old `files`
    // without `n_events`; every later run then failed (review round 2 of #1315).
    let t = store();
    let home = t.path();
    let cache = home.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let db = rusqlite::Connection::open(cache.join("find-index-v4.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE files(path TEXT PRIMARY KEY, harness TEXT NOT NULL, size INTEGER NOT NULL,
             mtime_ns INTEGER NOT NULL, offset INTEGER NOT NULL, mode TEXT NOT NULL,
             tail_hash TEXT NOT NULL);
         CREATE TABLE docs(id INTEGER PRIMARY KEY, path TEXT NOT NULL, ord INTEGER NOT NULL,
             event TEXT NOT NULL, rec INTEGER NOT NULL, uuid TEXT NOT NULL);",
    )
    .unwrap();
    drop(db);
    for _ in 0..2 {
        let out = run(home, &["index"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
    }
    let args = ["find", "alpha", "--in", "prompts", "--json"];
    assert_eq!(
        matches_only(&run(home, &args).stdout),
        matches_only(&raw(home, &args).stdout)
    );
}

#[test]
fn malformed_escapes_keep_neighbors_and_partial_coverage_across_searches() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let path = claude(
        home,
        C1,
        &[
            user(0, "beforemarker"),
            assistant(1, ""),
            json!({"type":"custom-title","customTitle":"Synthetic search"}),
        ],
    );
    let mut original = fs::read(&path).unwrap();
    // A lone surrogate reproduces a serde diagnostic also seen for a truncated
    // hex escape. Neither it nor an unknown escape may discard adjacent records.
    for escape in [r"\ud83d", r"\u12", r"\q"] {
        original.extend_from_slice(
            format!(r#"{{"type":"user","message":{{"content":"lostmarker {escape}"}}}}"#)
                .as_bytes(),
        );
        original.push(b'\n');
    }
    // Keep corruption outside metadata's initial probe and 256 KiB tail.
    // Otherwise metadata warnings would conceal a silent find-index failure.
    original.extend_from_slice(
        lines(&[json!({"type":"system","content":"padding".repeat(50_000)})]).as_bytes(),
    );
    original.extend_from_slice(lines(&[user(2, "aftermarker")]).as_bytes());
    fs::write(&path, &original).unwrap();

    // Build first, then search twice: diagnostics must survive the cache and
    // appear even for an absent term, where no candidate transcript is rendered.
    let warm = run(home, &["index"]);
    assert_eq!(warm.code, 0, "{}", warm.stderr);
    assert!(warm.stderr.contains("search coverage is incomplete"));
    assert_eq!(count(&db(home), &path), 2);
    for indexed in [false, true, true] {
        for term in ["beforemarker", "aftermarker", "absentmarker", "lostmarker"] {
            let args = ["find", term, "--json", "--schema-version", "1"];
            let out = if indexed {
                run(home, &args)
            } else {
                raw(home, &args)
            };
            let value: Value = serde_json::from_str(&out.stdout).unwrap();
            assert_eq!(value["complete"], false, "{}", out.stdout);
            assert_eq!(value["status"], "partial", "{}", out.stdout);
            let issues: Vec<_> = value["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| d["code"] == "malformed_record")
                .collect();
            assert_eq!(issues.len(), 3, "{}", out.stdout);
            if term == "beforemarker" || term == "aftermarker" {
                assert_eq!(
                    value["result"]["matches"][0]["session"]["identity"]["id"], C1,
                    "{}",
                    out.stdout
                );
                assert!(
                    value["result"]["matches"][0]["hits"]
                        .to_string()
                        .contains(term),
                    "{}",
                    out.stdout
                );
            } else {
                assert_eq!(value["result"]["total"], 0, "{}", out.stdout);
                assert_eq!(value["result"]["matches"], json!([]), "{}", out.stdout);
            }
        }
    }
    assert_eq!(
        fs::read(&path).unwrap(),
        original,
        "source bytes are unchanged"
    );

    append(&path, &[user(2, "appendedmarker")]);
    let appended = run(
        home,
        &["find", "appendedmarker", "--json", "--schema-version", "1"],
    );
    let value: Value = serde_json::from_str(&appended.stdout).unwrap();
    assert_eq!(value["complete"], false);
    assert_eq!(
        value["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["code"] == "malformed_record")
            .count(),
        3,
        "{}",
        appended.stdout
    );
    assert!(appended.stdout.contains("appendedmarker"));
    assert_eq!(count(&db(home), &path), 3);

    // A source rewrite that removes corruption clears the stored warning.
    fs::write(&path, lines(&[user(0, "repairedmarker")])).unwrap();
    let fixed = run(
        home,
        &["find", "repairedmarker", "--json", "--schema-version", "1"],
    );
    let value: Value = serde_json::from_str(&fixed.stdout).unwrap();
    assert_eq!(value["complete"], true, "{}", fixed.stdout);
    assert!(!fixed.stdout.contains("malformed_record"));
}

#[test]
fn an_indexed_partial_escape_is_retried_and_its_warning_clears_when_completed() {
    use std::io::Write;
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let path = claude(home, C1, &[user(0, "beforemarker")]);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(br#"{"type":"user","message":{"content":"tailmarker \u12"#)
        .unwrap();
    let args = ["find", "tailmarker", "--json", "--schema-version", "1"];
    for _ in 0..2 {
        let out = run(home, &args);
        let value: Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(value["complete"], false, "{}", out.stdout);
        assert_eq!(
            value["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| d["code"] == "malformed_record")
                .count(),
            1,
            "{}",
            out.stdout
        );
        assert!(out.stdout.contains("unterminated final record"));
    }
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"34\"}}\n")
        .unwrap();
    let out = run(home, &args);
    let value: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(value["complete"], true, "{}", out.stdout);
    assert!(out.stdout.contains(C1));
    assert_eq!(count(&db(home), &path), 2);
}

#[test]
fn metadata_tail_raw_and_index_diagnostics_count_records_once() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let path = claude(home, C1, &[user(0, "beforemarker")]);
    let mut original = fs::read(&path).unwrap();
    original.extend_from_slice(b"broken\nbroken\n");
    original.extend_from_slice(lines(&[user(1, "aftermarker")]).as_bytes());
    fs::write(&path, &original).unwrap();
    for indexed in [false, true, true] {
        for term in ["beforemarker", "aftermarker", "absentmarker"] {
            let args = ["find", term, "--json", "--schema-version", "1"];
            let out = if indexed {
                run(home, &args)
            } else {
                raw(home, &args)
            };
            let value: Value = serde_json::from_str(&out.stdout).unwrap();
            assert_eq!(value["complete"], false, "{}", out.stdout);
            assert_eq!(
                value["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|d| d["code"] == "malformed_record")
                    .count(),
                2,
                "{}",
                out.stdout
            );
            assert_eq!(
                value["result"]["total"],
                if term == "absentmarker" { 0 } else { 1 }
            );
        }
    }
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn legacy_writes_coexist_without_certifying_new_coverage() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let path = claude(home, C1, &[user(0, "compatmarker")]);
    append(
        &path,
        &[json!({"type":"system","content":"padding".repeat(50_000)})],
    );
    let mut source = fs::read(&path).unwrap();
    source.extend_from_slice(b"broken\n");
    // Keep the malformed record out of metadata's initial and tail probes.
    source.extend_from_slice(
        lines(&[json!({"type":"system","content":"padding".repeat(50_000)})]).as_bytes(),
    );
    source.extend_from_slice(lines(&[user(1, "tailmarker")]).as_bytes());
    fs::write(&path, &source).unwrap();
    let cache = home.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let legacy = rusqlite::Connection::open(cache.join("find-index-v3.sqlite")).unwrap();
    // The v3 writer has no issues column. Its unchanged source metadata and
    // empty event set must never be accepted as validated v4 coverage.
    legacy.execute_batch(
        "CREATE TABLE files(path TEXT PRIMARY KEY, harness TEXT NOT NULL, size INTEGER NOT NULL,
             mtime_ns INTEGER NOT NULL, offset INTEGER NOT NULL, mode TEXT NOT NULL,
             tail_hash TEXT NOT NULL, n_events INTEGER NOT NULL);",
    ).unwrap();
    let meta = fs::metadata(&path).unwrap();
    let mtime = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    let key = format!(
        "~{}{}",
        std::path::MAIN_SEPARATOR,
        path.strip_prefix(home).unwrap().display()
    );
    let write_old = || {
        legacy
            .execute(
                "INSERT INTO files(path,harness,size,mtime_ns,offset,mode,tail_hash,n_events)
         VALUES (?1,'claude',?2,?3,?2,'append','',0)
         ON CONFLICT(path) DO UPDATE SET size=excluded.size,mtime_ns=excluded.mtime_ns",
                rusqlite::params![key, meta.len() as i64, mtime],
            )
            .unwrap()
    };
    write_old();
    for _ in 0..2 {
        let out = run(
            home,
            &["find", "compatmarker", "--json", "--schema-version", "1"],
        );
        assert_eq!(out.code, 3, "{}", out.stderr);
        let value: Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(value["complete"], false, "{}", out.stdout);
        assert_eq!(value["result"]["total"], 1, "{}", out.stdout);
        assert!(!out.stderr.contains("index_unavailable"), "{}", out.stderr);
        assert_eq!(count(&db(home), &path), 2);
        // An older process can keep writing after the newer one has opened.
        assert_eq!(write_old(), 1);
        assert_eq!(
            legacy
                .query_row("SELECT n_events FROM files", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(fs::read(&path).unwrap(), source);
}

#[test]
fn malformed_persisted_issues_are_stale_and_replaced_without_duplicate_events() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let path = claude(home, C1, &[user(0, "repairmarker")]);
    let source = fs::read(&path).unwrap();
    assert_eq!(run(home, &["index"]).code, 0);
    for issues in ["not json", "null", "{}", "[[0,1]]"] {
        db(home)
            .execute("UPDATE files SET issues=?1", [issues])
            .unwrap();
        let out = run_with(
            home,
            &home.join("cache"),
            &["find", "repairmarker", "--json", "--schema-version", "1"],
            &[("AGENT_TRACES_INDEX_BUDGET_MS", "0")],
        );
        let value: Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(value["complete"], true, "{}", out.stdout);
        assert_eq!(value["result"]["total"], 1);
        assert!(
            value["result"]["index"]
                .as_str()
                .unwrap()
                .contains("1 scanned raw")
        );
        assert!(!out.stderr.contains("index_unavailable"), "{}", out.stderr);
        let out = run(
            home,
            &["find", "repairmarker", "--json", "--schema-version", "1"],
        );
        let value: Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(value["complete"], true, "{}", out.stdout);
        assert_eq!(value["result"]["total"], 1);
        assert!(
            value["result"]["index"]
                .as_str()
                .unwrap()
                .contains("1 refreshed now")
        );
        assert_eq!(count(&db(home), &path), 1);
        assert_eq!(
            db(home)
                .query_row("SELECT count(*) FROM docs", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db(home)
                .query_row("SELECT issues FROM files", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "[]"
        );
    }
    assert_eq!(fs::read(&path).unwrap(), source);
}
