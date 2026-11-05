//! A Codex index row whose rollout file was deleted is a missing session, not an
//! unreadable store: a no-hit `find` names no such file on stderr and stays
//! complete, and the skipped sessions are counted once per harness.
use serde_json::Value;
use std::{fs, path::Path, process::Command};

const NOW: &str = "2026-10-05T12:00:00Z";

fn run(home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_TRACES_NOW", NOW)
        .env("AGENT_TRACES_CACHE", home.join("cache"))
        .env("AGENT_TRACES_OUT", home.join("out"))
        .env("AGENT_TRACES_HOST", "testhost")
        .env("AGENT_TRACES_HOST_ID", "testhost")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (
        String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n"),
        String::from_utf8(out.stderr).unwrap().replace("\r\n", "\n"),
        out.status.code().unwrap(),
    )
}

/// One live rollout and two index rows whose rollout files are gone.
fn store() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let codex = t.path().join(".codex");
    let sessions = codex.join("sessions/2026/10/04");
    fs::create_dir_all(&sessions).unwrap();
    let con = rusqlite::Connection::open(codex.join("state_5.sqlite")).unwrap();
    con.execute_batch(
        "PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT,cwd TEXT,created_at INTEGER,updated_at INTEGER,rollout_path TEXT,source TEXT,archived INTEGER,name TEXT);",
    )
    .unwrap();
    for (i, live) in [(1, true), (2, false), (3, false)] {
        let id = format!("019eb7f2-526c-7d73-b4db-e76f861c32a{i}");
        let rollout = sessions.join(format!("rollout-2026-10-04T10-00-0{i}-{id}.jsonl"));
        if live {
            fs::write(
                &rollout,
                format!(
                    "{{\"type\":\"session_meta\",\"timestamp\":\"2026-10-04T10:00:00Z\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/repo\"}}}}\n\
                     {{\"type\":\"response_item\",\"timestamp\":\"2026-10-04T10:00:01Z\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"live pelican\"}}]}}}}\n"
                ),
            )
            .unwrap();
        }
        con.execute(
            "INSERT INTO threads VALUES(?,'/repo',1759572000,1759572001,?,'cli',0,NULL)",
            [id.as_str(), rollout.to_string_lossy().as_ref()],
        )
        .unwrap();
    }
    t
}

#[test]
fn a_deleted_rollout_is_not_reported_as_an_unreadable_store() {
    let t = store();
    for args in [
        &["find", "zzqqxx1"][..],
        &["find", "-q", "zzqqxx1", "-q", "zzqqxx2"][..],
    ] {
        let (_, stderr, code) = run(t.path(), args);
        // No hits exits 1; a partial search would exit 3 in schema mode.
        assert_eq!(code, 1, "{args:?}: {stderr}");
        assert!(!stderr.contains("os error"), "{args:?}: {stderr}");
        assert!(!stderr.contains("rollout-"), "{args:?}: {stderr}");
        // One counted summary line for the harness, not one per missing file.
        let lines: Vec<_> = stderr.lines().filter(|l| l.starts_with("codex:")).collect();
        assert_eq!(lines.len(), 1, "{args:?}: {stderr}");
        assert!(lines[0].contains('2'), "{args:?}: {stderr}");
    }
}

#[test]
fn a_deleted_rollout_leaves_the_v1_envelope_complete() {
    let t = store();
    let (stdout, stderr, code) = run(
        t.path(),
        &["find", "zzqqxx1", "--json", "--schema-version", "1"],
    );
    let out: Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|_| panic!("{stdout}{stderr}"));
    assert_eq!(code, 1, "{out}");
    assert_eq!(out["status"], "not_found", "{out}");
    assert_eq!(out["complete"], true, "{out}");
    assert!(
        !out["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "store_unreadable"),
        "{out}"
    );
}

#[test]
fn a_live_rollout_still_matches_beside_deleted_ones() {
    let t = store();
    let (stdout, stderr, code) = run(t.path(), &["find", "pelican", "--ids"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("c32a1"), "{stdout}{stderr}");
}
