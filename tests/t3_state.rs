use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn run(home: &Path, id: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .args(["t3", "state", id, "--json"])
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_TRACES_CACHE", home.join("cache"))
        .output()
        .unwrap()
}

#[test]
fn state_reads_thread_latest_run_and_checkpoint_without_changing_the_store() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".t3/userdata");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("statev2.sqlite");
    let con = Connection::open(&path).unwrap();
    con.execute_batch(r#"PRAGMA journal_mode=WAL;
        CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT PRIMARY KEY,title TEXT,deleted_at TEXT,payload_json TEXT);
        CREATE TABLE orchestration_v2_projection_runs(run_id TEXT PRIMARY KEY,thread_id TEXT,ordinal INTEGER,status TEXT);
        CREATE TABLE orchestration_v2_projection_checkpoints(checkpoint_id TEXT PRIMARY KEY,thread_id TEXT,status TEXT,captured_at TEXT,payload_json TEXT);
        INSERT INTO orchestration_v2_projection_threads VALUES ('thread:example','Example',NULL,'{"branch":"feature","worktreePath":"/work/example"}');
        INSERT INTO orchestration_v2_projection_threads VALUES ('thread:other','Other',NULL,'{}');
        INSERT INTO orchestration_v2_projection_runs VALUES ('run-old','thread:example',1,'completed'),('run-new','thread:example',2,'running');
        INSERT INTO orchestration_v2_projection_checkpoints VALUES
          ('cp-old','thread:example','ready','2026-10-01T00:00:00Z','{"ref":"refs/old"}'),
          ('cp-new','thread:example','ready','2026-10-02T00:00:00Z','{"ref":"refs/new","files":[{"private":"not exported"}]}'),
          ('cp-other','thread:other','failed','2026-10-03T00:00:00Z','{"ref":"refs/other"}');
        PRAGMA wal_checkpoint(TRUNCATE);"#).unwrap();
    let before = fs::read(&path).unwrap();
    let wal = fs::read(path.with_file_name("statev2.sqlite-wal")).unwrap();
    let out = run(home.path(), "example");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let state: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        state,
        json!({
            "thread_id":"thread:example", "title":"Example", "branch":"feature", "worktree_path":"/work/example",
            "latest_run":{"run_id":"run-new","status":"running"},
            "latest_checkpoint":{"checkpoint_id":"cp-new","status":"ready","captured_at":"2026-10-02T00:00:00Z","ref":"refs/new"}
        })
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        fs::read(path.with_file_name("statev2.sqlite-wal")).unwrap(),
        wal
    );
    assert!(!run(home.path(), "missing").status.success());
    let other = run(home.path(), "thread:other");
    let state: Value = serde_json::from_slice(&other.stdout).unwrap();
    assert!(state["latest_run"].is_null());
    con.execute_batch("INSERT INTO orchestration_v2_projection_threads VALUES ('import:example','Collision',NULL,'{}');
        INSERT INTO orchestration_v2_projection_threads VALUES ('thread:deleted','Deleted','2026-10-01','{}');").unwrap();
    let ambiguous = run(home.path(), "example");
    assert_eq!(ambiguous.status.code(), Some(2));
    assert!(ambiguous.stdout.is_empty());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("ambiguous"));
    assert!(run(home.path(), "thread:example").status.success());
    assert!(!run(home.path(), "thread:deleted").status.success());
    con.execute_batch("DROP TABLE orchestration_v2_projection_checkpoints")
        .unwrap();
    let absent = run(home.path(), "thread:other");
    assert!(absent.status.success());
    let state: Value = serde_json::from_slice(&absent.stdout).unwrap();
    assert!(state["latest_checkpoint"].is_null());
}
