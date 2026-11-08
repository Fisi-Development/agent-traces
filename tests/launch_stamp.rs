use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

const STAMPED: &str = "launch-stamped";
const PLAIN: &str = "launch-plain";
const CODEX: &str = "launch-codex";
const DEEP: &str = "launch-deep";
const DEEP_CODEX: &str = "launch-deep-codex";
const FAR: &str = "launch-far";
// A Telepathica delivery header runs ~600 characters before the handoff body.
fn header(n: usize) -> String {
    format!(
        "[telepathica] Message m_1 (handoff) from claude:x@h.\n{}\n```text\n",
        "h".repeat(n)
    )
}
fn fixture() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join(".claude/projects/fixture");
    fs::create_dir_all(&dir).unwrap();
    for (id, prompt, ts) in [
        (
            STAMPED,
            "[harbor-launch v1 surface=bosun subject=case:example launcher=tick parent=claude:parent@host] needle",
            "2026-11-03T12:00:00Z",
        ),
        (PLAIN, "needle plain", "2026-11-03T13:00:00Z"),
    ] {
        fs::write(
            dir.join(format!("{id}.jsonl")),
            format!(
                "{}\n",
                json!({"type":"user","cwd":"/fixture","timestamp":ts,"message":{"content":prompt}})
            ),
        )
        .unwrap();
    }
    fs::create_dir_all(t.path().join(".codex")).unwrap();
    let db = Connection::open(t.path().join(".codex/state_5.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT,cwd TEXT,created_at INTEGER,updated_at INTEGER,rollout_path TEXT,source TEXT,first_user_message TEXT);").unwrap();
    db.execute("INSERT INTO threads VALUES(?,'/fixture',1793707200,1793707200,'missing.jsonl','cli',?)", [CODEX, "leading line\n[harbor-launch v1 surface=lore subject=goal:fix%20it launcher=handoff] needle"]).unwrap();
    t
}
fn deep_fixture() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join(".claude/projects/fixture");
    fs::create_dir_all(&dir).unwrap();
    let deep = format!(
        "{}[harbor-launch v1 surface=bosun subject=case:deep launcher=bosun-tick] needle",
        header(700)
    );
    let far = format!(
        "{}[harbor-launch v1 surface=bosun subject=case:far launcher=bosun-tick] needle",
        header(4100)
    );
    for (id, prompt) in [(DEEP, deep), (FAR, far)] {
        fs::write(
            dir.join(format!("{id}.jsonl")),
            format!(
                "{}\n",
                json!({"type":"user","cwd":"/fixture","timestamp":"2026-11-03T14:00:00Z","message":{"content":prompt}})
            ),
        )
        .unwrap();
    }
    fs::create_dir_all(t.path().join(".codex")).unwrap();
    let db = Connection::open(t.path().join(".codex/state_5.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT,cwd TEXT,created_at INTEGER,updated_at INTEGER,rollout_path TEXT,source TEXT,first_user_message TEXT);").unwrap();
    let deep_codex = format!(
        "{}[harbor-launch v1 surface=lore subject=none launcher=handoff] needle",
        header(700)
    );
    db.execute(
        "INSERT INTO threads VALUES(?,'/fixture',1793707200,1793707200,'missing.jsonl','cli',?)",
        [DEEP_CODEX, deep_codex.as_str()],
    )
    .unwrap();
    t
}
fn run(home: &Path, args: &[&str]) -> Vec<Value> {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_TRACES_NOW", "2026-11-04T12:00:00Z")
        .env("AGENT_TRACES_CACHE", home.join("cache"))
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
#[test]
fn stamp_past_the_first_prompt_excerpt_is_read_from_the_full_message() {
    let t = deep_fixture();
    let rows = run(t.path(), &["ls", "--all", "--json"]);
    let row = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap().clone();
    // first_prompt stops at 600 characters, but the stamp after a 700-character header still counts.
    assert_eq!(
        row(DEEP)["first_prompt"].as_str().unwrap().chars().count(),
        600
    );
    assert_eq!(row(DEEP)["launch_subject"], "case:deep");
    assert_eq!(row(DEEP_CODEX)["launch_launcher"], "handoff");
    // Past launch::WINDOW (4,000 characters) a stamp is not a launch stamp.
    assert_eq!(row(FAR)["launch_surface"], Value::Null);
}
#[test]
fn rows_filters_counts_and_usage_share_launch_metadata() {
    let t = fixture();
    let rows = run(t.path(), &["ls", "--all", "--json"]);
    let stamped = rows.iter().find(|r| r["id"] == STAMPED).unwrap();
    assert_eq!(stamped["launch_surface"], "bosun");
    assert_eq!(stamped["launch_subject"], "case:example");
    assert_eq!(stamped["launch_launcher"], "tick");
    assert_eq!(stamped["launch_parent"], "claude:parent@host");
    for key in ["surface", "subject", "launcher", "parent"] {
        assert_eq!(
            rows.iter().find(|r| r["id"] == PLAIN).unwrap()[format!("launch_{key}")],
            Value::Null
        );
    }
    assert_eq!(
        rows.iter().find(|r| r["id"] == CODEX).unwrap()["launch_subject"],
        "goal:fix it"
    );
    assert_eq!(
        run(
            t.path(),
            &[
                "ls",
                "--all",
                "--surface",
                "bosun",
                "--limit",
                "1",
                "--json"
            ]
        )[0]["id"],
        STAMPED
    );
    let plain = run(t.path(), &["ls", "--all", "--surface", "none", "--json"]);
    assert_eq!(plain.len(), 1);
    assert_eq!(plain[0]["id"], PLAIN);
    let counts = run(
        t.path(),
        &[
            "ls",
            "--all",
            "--harness",
            "claude",
            "--count-by",
            "launch_launcher",
            "--json",
        ],
    );
    assert_eq!(counts.len(), 2);
    assert!(counts.iter().all(|r| r["count"] == 1));
    let usage = run(t.path(), &["usage", STAMPED, PLAIN, "--json"]);
    assert_eq!(
        usage[0]["launch"],
        json!({"surface":"bosun","subject":"case:example","launcher":"tick","parent":"claude:parent@host"})
    );
    assert_eq!(
        usage[1]["launch"],
        json!({"surface":null,"subject":null,"launcher":null,"parent":null})
    );
    for args in [
        vec![
            "find",
            "needle",
            "--surface",
            "bosun",
            "--limit",
            "1",
            "--json",
        ],
        vec!["find", "needle", "--launcher", "tick", "--json"],
        vec![
            "ls",
            "--all",
            "--surface",
            "absent",
            "--surface",
            "bosun",
            "--launcher",
            "tick",
            "--json",
        ],
    ] {
        let found = run(t.path(), &args);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["id"], STAMPED);
    }
    let grouped = run(
        t.path(),
        &["find", "-q", "needle", "--surface", "bosun", "--json"],
    );
    assert_eq!(grouped[0]["total"], 1);
    assert_eq!(grouped[0]["matches"][0]["id"], STAMPED);
    assert_eq!(
        run(t.path(), &["resolve", STAMPED, "--json"])[0]["launch_surface"],
        "bosun"
    );
}

#[test]
fn folded_t3_rows_use_the_provider_prompt() {
    let t = fixture();
    let path = t.path().join(".t3/userdata/state.sqlite");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let db = Connection::open(path).unwrap();
    db.execute_batch(r#"
        PRAGMA journal_mode=WAL;
        CREATE TABLE projection_projects(project_id TEXT, workspace_root TEXT, title TEXT);
        CREATE TABLE projection_threads(thread_id TEXT,title TEXT,branch TEXT,worktree_path TEXT,created_at TEXT,updated_at TEXT,latest_user_message_at TEXT,model_selection_json TEXT,archived_at TEXT,deleted_at TEXT,project_id TEXT);
        CREATE TABLE provider_session_runtime(thread_id TEXT,provider_name TEXT,provider_instance_id TEXT,resume_cursor_json TEXT,status TEXT);
        INSERT INTO projection_projects VALUES('project','/fixture','Fixture');
        INSERT INTO projection_threads VALUES('thread-launch','Launch',NULL,'/fixture','2026-11-03T12:00:00Z','2026-11-03T12:00:00Z',NULL,'{}',NULL,NULL,'project');
        INSERT INTO provider_session_runtime VALUES('thread-launch','claude','claude','{"sessionId":"launch-stamped"}','idle');
    "#).unwrap();
    let rows = run(
        t.path(),
        &["ls", "--all", "--t3", "--surface", "bosun", "--json"],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], STAMPED);
    assert_eq!(rows[0]["t3_thread"], "thread-launch");
    assert_eq!(rows[0]["launch_launcher"], "tick");
    let usage = run(t.path(), &["usage", "thread-launch", "--json"]);
    assert_eq!(usage[0]["id"], STAMPED);
    assert_eq!(usage[0]["launch"]["surface"], "bosun");
}
