//! `t3 tasks` looks up delegated tasks (T3's subagent projection) by id without
//! hand-written SQLite: the question agents scripted around statev2.sqlite for.
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn run(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .args(["t3", "tasks"])
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_TRACES_CACHE", home.join("cache"))
        .output()
        .unwrap()
}

const SCHEMA: &str = r#"PRAGMA journal_mode=WAL;
CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT PRIMARY KEY,title TEXT,deleted_at TEXT,payload_json TEXT);
CREATE TABLE orchestration_v2_projection_subagents(subagent_id TEXT PRIMARY KEY,thread_id TEXT NOT NULL,run_id TEXT,
  parent_node_id TEXT NOT NULL,provider TEXT NOT NULL,provider_thread_id TEXT,child_thread_id TEXT,origin TEXT NOT NULL,
  status TEXT NOT NULL,started_at TEXT,completed_at TEXT,updated_at TEXT NOT NULL,payload_json TEXT NOT NULL,
  driver TEXT,provider_instance_id TEXT,completion_delivery_state TEXT);
INSERT INTO orchestration_v2_projection_threads VALUES ('mcp:parent','Parent',NULL,'{}');
INSERT INTO orchestration_v2_projection_subagents VALUES
 ('node:delegated-task:command%3Amcp%3Aaaa%3Adelegate-task%3A7a1c0000','mcp:parent','run-1','node:root','codex_home',NULL,
  'thread:delegated-task:command%3Amcp%3Aaaa%3Adelegate-task%3A7a1c0000','app_owned','running','2026-11-10T10:00:00Z',NULL,
  '2026-11-10T10:05:00Z','{"prompt":"secret brief"}','codex','codex_home',NULL),
 ('node:delegated-task:command%3Amcp%3Aaaa%3Adelegate-task%3Ab2220000','mcp:parent','run-1','node:root','claude_home',NULL,
  'thread:delegated-task:command%3Amcp%3Aaaa%3Adelegate-task%3Ab2220000','app_owned','completed','2026-11-10T09:00:00Z',
  '2026-11-10T09:30:00Z','2026-11-10T09:31:00Z','{"prompt":"other brief"}','claudeAgent','claude_home','delivered'),
 ('node:delegated-task:command%3Amcp%3Aaaa%3Adelegate-task%3Ab2230000','mcp:parent','run-2','node:root','claude_home',NULL,
  NULL,'app_owned','failed','2026-11-10T08:00:00Z','2026-11-10T08:10:00Z','2026-11-10T08:11:00Z','{}','claudeAgent',
  'claude_home','delivered');
PRAGMA wal_checkpoint(TRUNCATE);"#;

fn store() -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".t3/userdata");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("statev2.sqlite");
    Connection::open(&path)
        .unwrap()
        .execute_batch(SCHEMA)
        .unwrap();
    (home, path)
}

#[test]
fn tasks_looks_up_a_delegated_task_by_id_fragment_without_payload_or_writes() {
    let (home, path) = store();
    let before = fs::read(&path).unwrap();
    // The fragment the probe agents matched with `like '%7a1c%'`.
    let out = run(home.path(), &["7a1c", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let task: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        task,
        json!({
            "subagent_id":"node:delegated-task:command%3Amcp%3Aaaa%3Adelegate-task%3A7a1c0000",
            "thread_id":"mcp:parent",
            "run_id":"run-1",
            "child_thread_id":"thread:delegated-task:command%3Amcp%3Aaaa%3Adelegate-task%3A7a1c0000",
            "provider":"codex_home",
            "status":"running",
            "completion_delivery_state":null,
            "started_at":"2026-11-10T10:00:00Z",
            "completed_at":null,
            "updated_at":"2026-11-10T10:05:00Z"
        })
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("secret brief"));
    // The decoded delegate_task id (as T3 tools print it) and the child thread id match too.
    let decoded = run(
        home.path(),
        &["command:mcp:aaa:delegate-task:7a1c0000", "--json"],
    );
    assert!(
        decoded.status.success(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    let child = run(
        home.path(),
        &[
            "thread:delegated-task:command%3Amcp%3Aaaa%3Adelegate-task%3Ab2220000",
            "--json",
        ],
    );
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let child: Value = serde_json::from_slice(&child.stdout).unwrap();
    assert_eq!(child["status"], "completed");
    assert_eq!(child["completion_delivery_state"], "delivered");
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn tasks_resolves_a_full_decoded_id_beside_a_rerun_that_contains_it() {
    let (home, path) = store();
    // A re-run named `<id>-r2` contains the first task's id as a substring.
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "INSERT INTO orchestration_v2_projection_subagents VALUES
             ('node:delegated-task:command%3Amcp%3Aaaa%3Adelegate-task%3A7a1c0000-r2','mcp:parent','run-3','node:root',
              'codex_home',NULL,'thread:delegated-task:command%3Amcp%3Aaaa%3Adelegate-task%3A7a1c0000-r2','app_owned',
              'completed','2026-11-10T11:00:00Z','2026-11-10T11:10:00Z','2026-11-10T11:10:00Z','{}','codex','codex_home',
              'delivered');",
        )
        .unwrap();
    for term in [
        "command:mcp:aaa:delegate-task:7a1c0000",
        "node:delegated-task:command:mcp:aaa:delegate-task:7a1c0000",
        "node:delegated-task:command%3Amcp%3Aaaa%3Adelegate-task%3A7a1c0000",
    ] {
        let out = run(home.path(), &[term, "--json"]);
        assert!(
            out.status.success(),
            "{term}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let task: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(task["status"], "running", "{term}");
    }
    let ambiguous = run(home.path(), &["7a1c", "--json"]);
    assert_eq!(ambiguous.status.code(), Some(2));
}

#[test]
fn tasks_lists_recent_first_and_refuses_ambiguous_or_missing_ids() {
    let (home, _path) = store();
    let out = run(home.path(), &["--limit", "2", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed: Value = serde_json::from_slice(&out.stdout).unwrap();
    let statuses: Vec<_> = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(statuses, ["running", "completed"]);
    let failed = run(home.path(), &["--status", "failed", "--json"]);
    let failed: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(failed["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(failed["tasks"][0]["child_thread_id"], Value::Null);
    let text = run(home.path(), &["7a1c"]);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains("running") && text.contains("mcp:parent"),
        "{text}"
    );
    let ambiguous = run(home.path(), &["b22", "--json"]);
    assert_eq!(ambiguous.status.code(), Some(2));
    assert!(ambiguous.stdout.is_empty());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("ambiguous"));
    let missing = run(home.path(), &["zzzz", "--json"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no T3 task matches"));
}
