use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn run(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .arg("t3")
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_TRACES_CACHE", home.join("cache"))
        .output()
        .unwrap()
}
fn value(home: &Path, args: &[&str]) -> Value {
    let out = run(home, args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
#[test]
fn snapshot_and_summary_are_bounded_and_read_only() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".t3/userdata");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("statev2.sqlite");
    let con = Connection::open(&path).unwrap();
    con.execute_batch(r#"PRAGMA journal_mode=WAL;
      CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT PRIMARY KEY,title TEXT,deleted_at TEXT);
      CREATE TABLE orchestration_v2_projection_runs(run_id TEXT PRIMARY KEY,thread_id TEXT,ordinal INTEGER,status TEXT);
      CREATE TABLE orchestration_v2_projection_runtime_requests(thread_id TEXT,kind TEXT,status TEXT);
      CREATE TABLE orchestration_v2_projection_turn_items(thread_id TEXT,run_id TEXT,type TEXT,status TEXT,updated_at TEXT,payload_json TEXT);
      CREATE TABLE orchestration_v2_projection_messages(message_id TEXT,thread_id TEXT,role TEXT,updated_at TEXT,payload_json TEXT);
      INSERT INTO orchestration_v2_projection_threads VALUES ('thread:a','A',NULL),('child:b','B',NULL),('deleted','D','yesterday');
      INSERT INTO orchestration_v2_projection_runs VALUES ('old','thread:a',1,'running'),('new-a','thread:a',2,'failed'),('new-z','thread:a',2,'completed');
      INSERT INTO orchestration_v2_projection_runtime_requests VALUES ('thread:a','command','pending'),('thread:a','command','pending'),('thread:a','permission','resolved'),('thread:a','file-read','pending'),('deleted','command','pending');
      INSERT INTO orchestration_v2_projection_turn_items VALUES
       ('thread:a','old','secret_request','waiting','now','{"private":"never export"}'),
       ('child:b','new-z','secret_request','waiting','now','{}'),
       ('child:b','absent','secret_request','waiting','now','{}'),
       ('thread:a','new-z','secret_request','waiting','now','{}');
      INSERT INTO orchestration_v2_projection_messages VALUES
       ('old','thread:a','user','1','{"text":"old"}'),
       ('a','thread:a','assistant','2','{"text":"tie loser"}'),
       ('tool','thread:a','tool','9','{"text":"tool payload"}');
    "#).unwrap();
    let text = format!(" \n sk-{} \t {} ", "a".repeat(30), "界".repeat(600));
    con.execute("INSERT INTO orchestration_v2_projection_messages VALUES ('z','thread:a','assistant','2',?)",
        [json!({"text":text,"private":"never export"}).to_string()]).unwrap();
    // Keep the writer open and leave committed data in the WAL.
    let before = fs::read(&path).unwrap();
    let wal_path = dir.join("statev2.sqlite-wal");
    let wal = fs::read(&wal_path).unwrap();
    assert!(!wal.is_empty());
    let snapshot = value(home.path(), &["snapshot", "--json"]);
    assert_eq!(
        snapshot,
        json!({
          "threads":[
           {"thread_id":"child:b","title":"B","status":"idle","latest_run_id":null,"link":"t3-thread://v1/child:b"},
           {"thread_id":"thread:a","title":"A","status":"completed","latest_run_id":"new-z","link":"t3-thread://v1/thread:a"}],
          "requests":{"thread:a":["command","file-read","user_input"]}
        })
    );
    let summary = value(home.path(), &["summary", "thread:a", "--json"]);
    let text = summary["text"].as_str().unwrap();
    assert_eq!(text.chars().count(), 500);
    assert!(text.starts_with("<redacted> 界"));
    assert!(!text.contains("sk-"));
    assert_eq!(summary.as_object().unwrap().len(), 2);
    assert_eq!(value(home.path(), &["summary", "child:b"])["text"], "");
    for id in ["missing", "deleted", "a"] {
        let out = run(home.path(), &["summary", id, "--json"]);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
    }
    assert_eq!(value(home.path(), &["snapshot"]), snapshot);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read(&wal_path).unwrap(), wal);
    con.execute_batch("DROP TABLE orchestration_v2_projection_runtime_requests")
        .unwrap();
    let out = run(home.path(), &["snapshot", "--json"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}
#[test]
fn absent_store_is_an_error() {
    let home = tempfile::tempdir().unwrap();
    assert!(!run(home.path(), &["snapshot", "--json"]).status.success());
    assert!(
        !run(home.path(), &["summary", "missing", "--json"])
            .status
            .success()
    );
}
