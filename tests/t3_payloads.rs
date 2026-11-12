//! T3 v2 listing rows read `payload_json` with serde, so odd payloads keep the
//! values serde gives them (not SQLite's `json_extract` values).
use rusqlite::{Connection, params};
use serde_json::Value;
use std::{fs, process::Command};

fn rows(payloads: &[(&str, &str)]) -> Vec<Value> {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".t3/userdata");
    fs::create_dir_all(&dir).unwrap();
    let con = Connection::open(dir.join("statev2.sqlite")).unwrap();
    con.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT,project_id TEXT,title TEXT,active_provider_thread_id TEXT,created_at TEXT,updated_at TEXT,archived_at TEXT,deleted_at TEXT,payload_json TEXT,provider_instance_id TEXT);
         CREATE TABLE projection_projects(project_id TEXT,workspace_root TEXT);
         CREATE TABLE orchestration_v2_projection_provider_threads(provider_thread_id TEXT,thread_id TEXT,updated_at TEXT,payload_json TEXT,driver TEXT);
         CREATE TABLE orchestration_v2_projection_messages(thread_id TEXT,role TEXT,created_at TEXT);
         CREATE TABLE orchestration_v2_projection_runs(run_id TEXT,thread_id TEXT,ordinal INTEGER,status TEXT);
         INSERT INTO projection_projects VALUES('p','/ws');",
    )
    .unwrap();
    for (id, payload) in payloads {
        con.execute(
            "INSERT INTO orchestration_v2_projection_threads VALUES(?,'p','T',NULL,'2026-11-01T00:00:00Z','2026-11-01T00:00:00Z',NULL,NULL,?,NULL)",
            params![id, payload],
        )
        .unwrap();
    }
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
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn one(payload: &str) -> Value {
    rows(&[("t", payload)]).remove(0)
}
#[test]
fn a_boolean_branch_stays_a_boolean() {
    assert_eq!(one(r#"{"branch":true}"#)["branch"], Value::Bool(true));
}
#[test]
fn an_empty_array_worktree_path_falls_back_to_the_workspace() {
    let r = one(r#"{"worktreePath":[],"branch":"b"}"#);
    assert_eq!(r["cwd"], "/ws");
    assert_eq!(r["branch"], "b");
}
#[test]
fn a_large_integer_keeps_every_digit() {
    let r = one(r#"{"branch":12345678901234567890}"#);
    assert_eq!(r["branch"].as_u64(), Some(12345678901234567890));
}
#[test]
fn a_string_model_selection_is_not_parsed_again() {
    let r = one(r#"{"modelSelection":"{\"model\":\"m\",\"instanceId\":\"i\"}"}"#);
    assert!(r["model"].is_null());
    assert!(r["provider_instance"].is_null());
}
#[test]
fn duplicate_keys_take_the_last_value() {
    assert_eq!(
        one(r#"{"branch":"first","branch":"last"}"#)["branch"],
        "last"
    );
}
#[test]
fn a_payload_serde_rejects_reads_as_empty() {
    let r = one(r#"{"branch":"ok","x":1e999}"#);
    assert!(r["branch"].is_null());
    assert_eq!(r["cwd"], "/ws");
}
#[test]
fn an_ordinary_payload_reads_every_field() {
    let r = one(
        r#"{"branch":"b","worktreePath":"/w","modelSelection":{"model":"m","instanceId":"i"}}"#,
    );
    assert_eq!(r["branch"], "b");
    assert_eq!(r["cwd"], "/w");
    assert_eq!(r["model"], "m");
    assert_eq!(r["provider_instance"], "i");
}
