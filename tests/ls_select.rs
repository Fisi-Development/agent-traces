//! `ls` answers the questions agents used to pipe its JSON into Python for:
//! keyword and T3 filters, a field projection with addresses, counts, and a
//! `--since` that `--all` does not discard. Runs the binary against a
//! synthetic Claude store with a cleared environment.
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

const NOW: &str = "2026-10-05T12:00:00Z";
const OLD: &str = "bbbbbbbb-0000-4000-8000-000000000001";
const LEDGER: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const RECENT: &str = "bbbbbbbb-0000-4000-8000-000000000003";

fn write(home: &Path, id: &str, cwd: &str, ts: &str, prompt: &str, title: Option<&str>) {
    let dir = home.join(".claude/projects").join(id);
    fs::create_dir_all(&dir).unwrap();
    let mut records = vec![json!({"type": "user", "cwd": cwd, "gitBranch": "main",
        "timestamp": ts, "message": {"content": prompt}})];
    if let Some(t) = title {
        records.push(json!({"type": "ai-title", "aiTitle": t}));
    }
    let body = records
        .iter()
        .map(|r| r.to_string() + "\n")
        .collect::<String>();
    fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
}

fn store() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    write(
        home,
        OLD,
        "/w/harbor",
        "2026-08-01T10:00:00Z",
        "old work",
        None,
    );
    write(
        home,
        LEDGER,
        "/w/payments",
        "2026-10-03T10:00:00Z",
        "line one\n\tline two",
        Some("Fix the Ledger"),
    );
    write(
        home,
        RECENT,
        "/w/harbor",
        "2026-10-05T10:00:00Z",
        "newest work",
        None,
    );
    t
}

fn run(home: &Path, args: &[&str]) -> (i32, String, String) {
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
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn json_ids(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[test]
fn all_keeps_an_explicit_since() {
    let t = store();
    let (_, out, _) = run(t.path(), &["ls", "--all", "--json"]);
    assert_eq!(json_ids(&out), [RECENT, LEDGER, OLD]);
    let (_, out, _) = run(t.path(), &["ls", "--all", "--since", "30d", "--json"]);
    assert_eq!(json_ids(&out), [RECENT, LEDGER]);
}

#[test]
fn grep_filters_before_the_limit_and_matches_any_term() {
    let t = store();
    // RECENT is newest but matches neither term, so --limit 1 must still find LEDGER.
    let (_, out, _) = run(
        t.path(),
        &[
            "ls", "--all", "--grep", "ledger", "--grep", "nomatch", "--limit", "1", "--json",
        ],
    );
    assert_eq!(json_ids(&out), [LEDGER]);
    // A cwd term keeps both harbor sessions, newest first.
    let (_, out, _) = run(t.path(), &["ls", "--all", "--grep", "HARBOR", "--json"]);
    assert_eq!(json_ids(&out), [RECENT, OLD]);
    // Every session here is outside T3.
    let (_, out, _) = run(t.path(), &["ls", "--all", "--t3", "--json"]);
    assert_eq!(out, "");
}

#[test]
fn fields_print_one_tab_separated_line_per_session() {
    let t = store();
    let (code, out, _) = run(
        t.path(),
        &[
            "ls",
            "--all",
            "--grep",
            "ledger",
            "--fields",
            "address,cwd,first_prompt,parent",
        ],
    );
    assert_eq!(code, 0);
    // The prompt's newline and tab cannot split the row; a null field is an empty cell.
    assert_eq!(
        out,
        format!("claude:{LEDGER}@testhost\t/w/payments\tline one line two\t\n")
    );
}

#[test]
fn fields_with_json_keep_only_the_named_keys() {
    let t = store();
    let (_, out, _) = run(
        t.path(),
        &[
            "ls",
            "--all",
            "--limit",
            "1",
            "--fields",
            "id,address,updated",
            "--json",
        ],
    );
    let row: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(
        row,
        json!({"id": RECENT, "address": format!("claude:{RECENT}@testhost"),
            "updated": "2026-10-05T10:00:00+00:00"})
    );
}

#[test]
fn count_by_counts_every_match_regardless_of_limit() {
    let t = store();
    let (_, out, err) = run(
        t.path(),
        &["ls", "--all", "--count-by", "cwd", "--limit", "1"],
    );
    assert_eq!(out, "     2  /w/harbor\n     1  /w/payments\n");
    assert!(err.contains("3 sessions in 2 groups by cwd"), "{err}");
    let (_, out, _) = run(
        t.path(),
        &["ls", "--since", "30d", "--count-by", "cwd", "--json"],
    );
    let rows: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        rows,
        [
            json!({"cwd": "/w/harbor", "count": 1}),
            json!({"cwd": "/w/payments", "count": 1})
        ]
    );
}

#[test]
fn an_unknown_field_is_an_error_not_an_empty_column() {
    let t = store();
    for flag in ["--fields", "--count-by"] {
        let (code, out, err) = run(t.path(), &["ls", flag, "updated_at"]);
        assert_eq!(code, 2, "{flag}");
        assert_eq!(out, "");
        assert!(err.contains("unknown field \"updated_at\""), "{err}");
    }
}
