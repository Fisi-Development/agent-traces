//! `find` gives scripts the answer directly: addresses, a count, a hit
//! budget that JSON honors, and a wider snippet. Runs the binary against a
//! synthetic store with a cleared environment, so no real store is read.
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

const NOW: &str = "2026-10-05T12:00:00Z";
const MANY: &str = "bbbbbbbb-0000-4000-8000-000000000001";
const ONE: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const LONG: &str = "bbbbbbbb-0000-4000-8000-000000000003";
const OTHER: &str = "bbbbbbbb-0000-4000-8000-000000000004";

fn write(home: &Path, id: &str, prompts: &[String]) {
    let dir = home.join(".claude/projects/w");
    fs::create_dir_all(&dir).unwrap();
    let body = prompts
        .iter()
        .enumerate()
        .map(|(i, text)| {
            json!({"type": "user", "cwd": "/w", "timestamp": format!("2026-10-0{}T09:{i:02}:00Z", 1 + i % 4),
                   "message": {"content": text}})
            .to_string()
                + "\n"
        })
        .collect::<String>();
    fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
}

fn store() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let many = (0..7)
        .map(|i| format!("pelican run {i}"))
        .collect::<Vec<_>>();
    write(home, MANY, &many);
    write(home, ONE, &["one pelican".into()]);
    // The marker sits 500 characters past the term.
    write(
        home,
        LONG,
        &[format!("pelican {} TAILMARK", "filler ".repeat(72))],
    );
    write(home, OTHER, &["nothing to see".into()]);
    t
}

fn run(home: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_TRACES_NOW", NOW)
        .env("AGENT_TRACES_CACHE", home.join("cache"))
        .env("AGENT_TRACES_OUT", home.join("out"))
        .env("AGENT_TRACES_HOST", "testhost")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    // The binary writes CRLF on Windows; compare lines, not line endings.
    (
        String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n"),
        out.status.code().unwrap(),
    )
}

fn json_rows(home: &Path, args: &[&str]) -> Vec<Value> {
    let (stdout, code) = run(home, args);
    assert_eq!(code, 0, "{args:?}");
    stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn ids_prints_one_address_per_session_in_rank_order() {
    let t = store();
    let (stdout, code) = run(t.path(), &["find", "pelican", "--ids"]);
    assert_eq!(code, 0);
    let ranked = json_rows(t.path(), &["find", "pelican", "--json"])
        .iter()
        .map(|r| format!("claude:{}@testhost", r["id"].as_str().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(ranked.len(), 3);
    assert_eq!(stdout.lines().collect::<Vec<_>>(), ranked);
    let (stdout, _) = run(t.path(), &["find", "pelican", "--ids", "--limit", "1"]);
    assert_eq!(stdout, format!("{}\n", ranked[0]));
}

#[test]
fn an_address_from_ids_feeds_resolve() {
    let t = store();
    let (stdout, _) = run(t.path(), &["find", "one pelican", "--ids"]);
    assert_eq!(stdout.trim(), format!("claude:{ONE}@testhost"));
    let rows = json_rows(t.path(), &["resolve", stdout.trim(), "--json"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], ONE);
}

#[test]
fn count_prints_every_match_not_only_the_shown_page() {
    let t = store();
    assert_eq!(
        run(t.path(), &["find", "pelican", "--count", "--limit", "1"]),
        ("3\n".into(), 0)
    );
    assert_eq!(
        run(t.path(), &["find", "absent-needle", "--count"]),
        ("0\n".into(), 1)
    );
}

#[test]
fn json_honors_an_explicit_hit_budget() {
    let t = store();
    let hits = |args: &[&str]| {
        let rows = json_rows(t.path(), args);
        let row = rows.iter().find(|r| r["id"] == MANY).unwrap().clone();
        row["hits"].as_array().unwrap().len()
    };
    assert_eq!(hits(&["find", "pelican", "--json"]), 5);
    assert_eq!(hits(&["find", "pelican", "--json", "--hits", "0"]), 0);
    assert_eq!(hits(&["find", "pelican", "--json", "--hits", "2"]), 2);
    assert_eq!(hits(&["find", "pelican", "--json", "--hits", "7"]), 7);
}

#[test]
fn width_widens_the_text_around_each_hit() {
    let t = store();
    let snippet = |extra: &[&str]| {
        let mut args = vec!["find", "pelican", "--json", "--hits", "1"];
        args.extend(extra);
        let rows = json_rows(t.path(), &args);
        let row = rows.iter().find(|r| r["id"] == LONG).unwrap().clone();
        row["hits"][0]["snippet"].as_str().unwrap().to_owned()
    };
    assert!(!snippet(&[]).contains("TAILMARK"));
    assert!(snippet(&["--width", "600"]).contains("TAILMARK"));
    let (text, _) = run(
        t.path(),
        &["find", "pelican", "--hits", "1", "--width", "600"],
    );
    assert!(text.contains("TAILMARK"), "{text}");
}

#[test]
fn repeated_queries_return_independent_groups_including_misses() {
    let t = store();
    let rows = json_rows(
        t.path(),
        &[
            "find",
            "-q",
            "one pelican",
            "--query",
            "absent-needle",
            "-q",
            "nothing",
            "--json",
            "--hits",
            "0",
        ],
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["query"], "one pelican");
    assert_eq!(rows[0]["total"], 1);
    assert_eq!(rows[0]["matches"][0]["id"], ONE);
    assert_eq!(rows[0]["matches"][0]["hits"], json!([]));
    assert_eq!(
        rows[1],
        json!({"query":"absent-needle", "total":0, "matches":[]})
    );
    assert_eq!(rows[2]["matches"][0]["id"], OTHER);
    let (text, code) = run(
        t.path(),
        &[
            "find",
            "-q",
            "one pelican",
            "-q",
            "absent-needle",
            "--hits",
            "0",
        ],
    );
    assert_eq!(code, 0);
    assert!(text.contains("Query: one pelican"));
    assert!(text.contains("Query: absent-needle"));
    let counts = json_rows(
        t.path(),
        &["find", "-q", "pelican", "--json", "--limit", "0"],
    );
    assert_eq!(
        counts,
        [json!({"query":"pelican", "total":3, "matches":[]})]
    );
    let (out, code) = run(t.path(), &["find", "-q", "absent-needle", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(
        serde_json::from_str::<Value>(out.trim()).unwrap(),
        json!({"query":"absent-needle", "total":0, "matches":[]})
    );
    for extra in ["--ids", "--count"] {
        assert_eq!(run(t.path(), &["find", "-q", "pelican", extra]).1, 2);
    }
}

#[test]
fn hyphenated_query_matches_the_literal_term_not_its_separate_parts() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), ONE, &["implement p1-guard".into()]);
    write(t.path(), OTHER, &["p1 guard p1 guard p1 guard".into()]);
    let rows = json_rows(t.path(), &["find", "p1-guard", "--json", "--hits", "0"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], ONE);
}
