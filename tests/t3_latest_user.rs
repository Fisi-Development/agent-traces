//! The T3 v2 listing reports each thread's newest user message as `updated`,
//! falling back to the thread's own `updated_at` when it has none.
use rusqlite::Connection;
use serde_json::Value;
use std::{fs, process::Command};

#[test]
fn updated_is_the_newest_user_message_per_thread() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".t3/userdata");
    fs::create_dir_all(&dir).unwrap();
    let con = Connection::open(dir.join("statev2.sqlite")).unwrap();
    con.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT,project_id TEXT,title TEXT,active_provider_thread_id TEXT,created_at TEXT,updated_at TEXT,archived_at TEXT,deleted_at TEXT,payload_json TEXT,provider_instance_id TEXT);
         CREATE TABLE projection_projects(project_id TEXT,workspace_root TEXT);
         CREATE TABLE orchestration_v2_projection_provider_threads(provider_thread_id TEXT,thread_id TEXT,updated_at TEXT,payload_json TEXT,driver TEXT);
         CREATE TABLE orchestration_v2_projection_messages(message_id TEXT,thread_id TEXT,role TEXT,created_at TEXT,updated_at TEXT);
         CREATE TABLE orchestration_v2_projection_runs(run_id TEXT,thread_id TEXT,ordinal INTEGER,status TEXT);
         INSERT INTO projection_projects VALUES('p','/ws');
         INSERT INTO orchestration_v2_projection_threads VALUES
           ('many','p','T',NULL,'2026-11-01T00:00:00Z','2026-11-09T00:00:00Z',NULL,NULL,'{}',NULL),
           ('none','p','T',NULL,'2026-11-01T00:00:00Z','2026-11-09T00:00:00Z',NULL,NULL,'{}',NULL),
           ('only_assistant','p','T',NULL,'2026-11-01T00:00:00Z','2026-11-08T00:00:00Z',NULL,NULL,'{}',NULL),
           ('one','p','T',NULL,'2026-11-01T00:00:00Z','2026-11-09T00:00:00Z',NULL,NULL,'{}',NULL);
         -- 'many' is inserted out of time order, with a newer assistant message and another thread's newer user message
         INSERT INTO orchestration_v2_projection_messages VALUES
           ('m1','many','user','2026-11-03T00:00:00Z','2026-11-03T00:00:00Z'),
           ('m2','many','user','2026-11-05T00:00:00Z','2026-11-05T00:00:00Z'),
           ('m3','many','user','2026-11-02T00:00:00Z','2026-11-02T00:00:00Z'),
           ('m4','many','assistant','2026-11-07T00:00:00Z','2026-11-07T00:00:00Z'),
           ('m5','only_assistant','assistant','2026-11-06T00:00:00Z','2026-11-06T00:00:00Z'),
           ('m6','one','user','2026-11-04T00:00:00Z','2026-11-04T00:00:00Z'),
           ('m7','gone','user','2026-11-10T00:00:00Z','2026-11-10T00:00:00Z');",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .args(["ls", "--all", "--harness", "t3", "--json"])
        .env_clear()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("AGENT_TRACES_CACHE", home.path().join("cache"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let updated = |id: &str| -> String {
        String::from_utf8(out.stdout.clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|r| r["id"] == id)
            .unwrap()["updated"]
            .as_str()
            .unwrap()
            .to_string()
    };
    // Compare as instants: the listing may reformat the timestamp.
    assert!(updated("many").starts_with("2026-11-05"));
    assert!(updated("one").starts_with("2026-11-04"));
    assert!(updated("none").starts_with("2026-11-09"));
    assert!(updated("only_assistant").starts_with("2026-11-08"));
}
