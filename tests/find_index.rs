//! `find` over the prompts and assistant scopes answers from a SQLite index of
//! events. Runs the binary against a synthetic store with a cleared environment
//! and a private cache, and compares it with the raw transcript scan
//! (`AGENT_TRACES_NO_INDEX=1`).
//!
//! BM25 scores are computed over the candidate sessions' events. The raw scan
//! keeps every session whose file text holds the terms, the index only those
//! whose in-scope events do, so a decoy session can shift a score without
//! changing which sessions match or their hits. Cases without a decoy compare
//! byte for byte; the others compare matches and hits with scores dropped.
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

/// Rows keyed by address with `_score` dropped, to compare matches and hits.
fn matches_only(stdout: &str) -> Vec<Value> {
    let mut rows: Vec<Value> = stdout
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .map(|mut row| {
            row.as_object_mut().unwrap().remove("_score");
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
    // (args, scores comparable): a case with a decoy session may shift scores.
    let cases: &[(&[&str], bool)] = &[
        (&["alpha", "beta", "--in", "prompts"], false),
        (&["alpha", "beta", "--in", "assistant"], false),
        (&["alpha", "--in", "assistant"], false),
        (&["needle-result-only", "--in", "prompts"], true),
        (&["needle-result-only", "--in", "tools"], true),
        (&["needle-result-only", "--in", "results"], true),
        (&["needle-result-only", "--in", "all"], true),
        (&["zq", "--in", "prompts"], true),
        (&["ärger", "--in", "prompts"], true),
        (&["ÄRGER", "--in", "prompts"], true),
        (&["Ärger", "--in", "prompts"], true),
        (&["\"quoted phrase\"", "--in", "prompts"], true),
        (&["quoted", "phrase", "--in", "prompts"], true),
        (&["metaonly-branch", "--in", "prompts"], true),
        (&["metaonly-branch", "--in", "all"], true),
        (&["codex", "alpha", "--in", "assistant"], true),
        (&["codex", "--in", "prompts"], true),
    ];
    for (args, same_scores) in cases {
        let mut full = vec!["find"];
        full.extend_from_slice(args);
        full.extend(["--json", "--limit", "50", "--hits", "5"]);
        let with = run(home, &full);
        let without = raw(home, &full);
        assert_eq!(with.code, without.code, "{args:?}");
        assert_eq!(
            matches_only(&with.stdout),
            matches_only(&without.stdout),
            "{args:?}"
        );
        if *same_scores {
            assert_eq!(with.stdout, without.stdout, "{args:?}");
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
    // A term holding a double quote cannot occur in a JSON line, which escapes it,
    // so both scans miss it; the index must not invent a hit the search then drops.
    assert_eq!(
        run(home, &["find", "\"quoted phrase\"", "--in", "prompts"]).code,
        1
    );
    assert!(
        run(home, &["find", "quoted phrase", "--in", "prompts", "--ids"])
            .stdout
            .contains(C5)
    );
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
    rusqlite::Connection::open(home.join("cache/find-index-v2.sqlite")).unwrap()
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
        home.join("cache/find-index-v2.sqlite"),
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
    assert_eq!(indexed.stdout, raw(home, &args).stdout);
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
    assert_eq!(indexed.stdout, direct.stdout);
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
    assert_eq!(indexed.stdout, raw(home, &args).stdout);
    assert_eq!(run(home, &["find", "oldword", "--in", "prompts"]).code, 1);
}

#[test]
fn the_old_schema_database_is_removed_on_open() {
    let t = store();
    let home = t.path();
    let cache = home.join("cache");
    fs::create_dir_all(&cache).unwrap();
    for suffix in ["", "-wal", "-shm"] {
        fs::write(cache.join(format!("find-index-v1.sqlite{suffix}")), "old").unwrap();
    }
    assert_eq!(run(home, &["find", "alpha", "--in", "prompts"]).code, 0);
    for suffix in ["", "-wal", "-shm"] {
        assert!(!cache.join(format!("find-index-v1.sqlite{suffix}")).exists());
    }
    assert!(cache.join("find-index-v2.sqlite").exists());
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
