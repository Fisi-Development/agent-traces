use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};
const TIME: &str = "2026-11-03T12:00:00Z";
fn jsonl(path: &Path, records: Vec<Value>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        records
            .iter()
            .map(|r| r.to_string() + "\n")
            .collect::<String>(),
    )
    .unwrap();
}
fn run(home: &Path, args: &[&str]) -> Vec<Value> {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .arg("prompts")
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_TRACES_NOW", "2026-11-04T12:00:00Z")
        .env("AGENT_TRACES_HOST", "fixture-host")
        .env("AGENT_TRACES_CACHE", home.join("cache"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
fn fixture(harness: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let texts = [
        "normal typed turn",
        "[telepathica] handoff",
        "<system-reminder>injected</system-reminder>",
        "next typed turn",
    ];
    match harness {
        "claude" => {
            let mut records: Vec<_> = texts.iter().map(|t| json!({"type":"user","cwd":"/fixture","timestamp":TIME,"message":{"content":t}})).collect();
            records.push(json!({"type":"user","timestamp":TIME,"message":{"content":[{"type":"tool_result","tool_use_id":"tool","content":"tool result"}]}}));
            jsonl(
                &home.join(".claude/projects/fixture/session-claude.jsonl"),
                records,
            );
        }
        "codex" => {
            let mut records = vec![
                json!({"type":"session_meta","payload":{"id":"session-codex","cwd":"/fixture","timestamp":TIME}}),
            ];
            records.extend(texts.iter().map(|t| json!({"type":"response_item","timestamp":TIME,"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":t}]}})));
            records.push(json!({"type":"response_item","timestamp":TIME,"payload":{"type":"function_call_output","output":"tool result","call_id":"tool"}}));
            let path = home.join(".codex/sessions/2026/11/03/rollout-session-codex.jsonl");
            jsonl(&path, records);
            let con = Connection::open(home.join(".codex/state_5.sqlite")).unwrap();
            con.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT,cwd TEXT,created_at INTEGER,updated_at INTEGER,rollout_path TEXT,source TEXT);").unwrap();
            con.execute("INSERT INTO threads VALUES('session-codex','/fixture',1793707200,1793707200,?,'cli')", [path.to_string_lossy().to_string()]).unwrap();
        }
        "grok" => {
            let path = home.join(".grok/sessions/%2Ffixture/session-grok/chat_history.jsonl");
            let mut records: Vec<_> = texts
                .iter()
                .enumerate()
                .map(|(i, t)| json!({"type":"user","timestamp":TIME,"prompt_index":i,"content":t}))
                .collect();
            records.push(json!({"type":"tool_result","timestamp":TIME,"content":"tool result"}));
            jsonl(&path, records);
            fs::write(
                path.parent().unwrap().join("summary.json"),
                json!({"info":{"cwd":"/fixture"},"created_at":TIME,"last_active_at":TIME})
                    .to_string(),
            )
            .unwrap();
        }
        "opencode" => {
            let path = home.join(".local/share/opencode/opencode.db");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let con = Connection::open(path).unwrap();
            con.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE session_v2(id TEXT,parent_id TEXT,directory TEXT,title TEXT,model TEXT,agent TEXT,time_created INTEGER,time_updated INTEGER,time_archived INTEGER); CREATE TABLE session_message(session_id TEXT,type TEXT,data TEXT,time_created INTEGER,seq INTEGER);").unwrap();
            con.execute("INSERT INTO session_v2 VALUES('session-opencode',NULL,'/fixture','Test','{}',NULL,1793707200000,1793707200000,NULL)",[]).unwrap();
            for (i, t) in texts.iter().enumerate() {
                con.execute("INSERT INTO session_message VALUES('session-opencode','user',?,1793707200000,?)",params![json!({"text":t}).to_string(),i as i64]).unwrap();
            }
            con.execute("INSERT INTO session_message VALUES('session-opencode','assistant',?,1793707200000,4)",
                [json!({"content":[{"type":"tool","name":"read","id":"call","state":{"input":{},"content":"tool result","status":"completed"}}]}).to_string()]).unwrap();
        }
        _ => unreachable!(),
    }
    dir
}
#[test]
fn all_harnesses_filter_machine_turns_and_keep_audit_indices() {
    for harness in ["claude", "codex", "grok", "opencode"] {
        let dir = fixture(harness);
        let rows = run(dir.path(), &["--harness", harness]);
        assert_eq!(rows.len(), 2, "{harness}: {rows:?}");
        assert_eq!(rows[0]["text"], "normal typed turn");
        assert_eq!(rows[1]["text"], "next typed turn");
        assert_eq!(rows[1]["turn"], 3);
        assert_eq!(rows[0]["origin"], "heuristic");
        assert_eq!(rows[0]["t3_thread"], Value::Null);
        let audit = run(dir.path(), &["--harness", harness, "--include-machine"]);
        assert_eq!(audit.len(), 4, "{harness}: {audit:?}");
        assert_eq!(audit[1]["origin"], "machine");
        assert_eq!(audit[2]["origin"], "machine");
        let clipped = run(dir.path(), &["--harness", harness, "--max-chars", "6"]);
        assert_eq!(clipped[0]["text"], "norma…");
        assert_eq!(clipped[0]["chars"], 17);
        assert!(
            run(
                dir.path(),
                &["--harness", harness, "--since", "2026-11-04T00:00:00Z"]
            )
            .is_empty()
        );
    }
}
#[test]
fn t3_metadata_overrides_provider_heuristics_without_duplicates() {
    for harness in ["claude", "codex", "grok", "opencode"] {
        let dir = fixture(harness);
        if harness == "claude" {
            jsonl(
                &dir.path()
                    .join(".claude/projects/fixture/session-claude.jsonl"),
                vec![
                    json!({"type":"user","cwd":"/fixture","entrypoint":"sdk-cli","timestamp":TIME,"message":{"content":"SDK provider copy"}}),
                ],
            );
        }
        let path = dir.path().join(".t3/userdata/statev2.sqlite");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let con = Connection::open(path).unwrap();
        con.execute_batch("PRAGMA journal_mode=WAL;
CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT,project_id TEXT,title TEXT,active_provider_thread_id TEXT,created_at TEXT,updated_at TEXT,archived_at TEXT,deleted_at TEXT,payload_json TEXT,provider_instance_id TEXT);
CREATE TABLE projection_projects(project_id TEXT,workspace_root TEXT);
CREATE TABLE orchestration_v2_projection_provider_threads(provider_thread_id TEXT,thread_id TEXT,updated_at TEXT,payload_json TEXT,driver TEXT);
CREATE TABLE orchestration_v2_projection_runs(thread_id TEXT,ordinal INTEGER,status TEXT);
CREATE TABLE orchestration_v2_projection_messages(thread_id TEXT,role TEXT,created_at TEXT,payload_json TEXT);
INSERT INTO projection_projects VALUES('project','/fixture');
INSERT INTO orchestration_v2_projection_threads VALUES('thread','project','Test','provider','2026-11-03T12:00:00Z','2026-11-03T12:00:00Z',NULL,NULL,'{}',NULL);
INSERT INTO orchestration_v2_projection_provider_threads VALUES('provider','thread','2026-11-03T12:00:00Z','{\"nativeThreadRef\":{\"nativeId\":\"session-claude\"}}','claudeAgent');").unwrap();
        let driver = if harness == "claude" {
            "claudeAgent"
        } else {
            harness
        };
        con.execute(
            "UPDATE orchestration_v2_projection_provider_threads SET driver=?,payload_json=?",
            params![
                driver,
                json!({"nativeThreadRef":{"nativeId":format!("session-{harness}")}}).to_string()
            ],
        )
        .unwrap();
        for (text, by, source) in [
            ("typed via client", "user", "web"),
            ("injected via MCP", "user", "mcp"),
            ("agent launch", "agent", "web"),
        ] {
            con.execute(
                "INSERT INTO orchestration_v2_projection_messages VALUES('thread','user',?,?)",
                params![
                    TIME,
                    json!({"text":text,"createdBy":by,"creationSource":source}).to_string()
                ],
            )
            .unwrap();
        }
        let rows = run(dir.path(), &[]);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["origin"], "t3");
        assert_eq!(rows[0]["text"], "typed via client");
        assert_eq!(rows[0]["address"], "t3:thread@fixture-host");
        assert_eq!(run(dir.path(), &["--include-machine"]).len(), 3);
        assert_eq!(run(dir.path(), &["--harness", harness]).len(), 1);
        assert_eq!(run(dir.path(), &["--harness", "t3"]).len(), 1);
        con.execute(
            "UPDATE orchestration_v2_projection_provider_threads SET payload_json=?",
            [json!({"nativeThreadRef":{"nativeId":"replacement-provider"}}).to_string()],
        )
        .unwrap();
        con.execute("INSERT INTO orchestration_v2_projection_provider_threads VALUES('historical','thread',?,?,?)",params![TIME,json!({"nativeThreadRef":{"nativeId":format!("session-{harness}")}}).to_string(),driver]).unwrap();
        let historical = run(dir.path(), &["--harness", harness]);
        assert_eq!(historical.len(), 1);
        assert_eq!(historical[0]["origin"], "t3");
    }
}

#[test]
fn prefixes_redaction_unicode_and_resumed_session_window() {
    let dir = fixture("claude");
    let path = dir
        .path()
        .join(".claude/projects/fixture/session-claude.jsonl");
    let mut records = vec![
        json!({"type":"user","cwd":"/fixture","timestamp":"2026-11-01T00:00:00Z","message":{"content":"old typed turn"}}),
    ];
    for text in [
        "[telepathica] job",
        "[agent-mailbox] job",
        "[lookout-nudge] job",
        "<task-notification>job</task-notification>",
        "Delegated task job",
        "<command-name>test</command-name>",
        "Base directory for this skill: test",
        "This session is being continued from a previous conversation",
    ] {
        records.push(json!({"type":"user","timestamp":TIME,"message":{"content":text}}));
    }
    records.push(json!({"type":"user","timestamp":TIME,"message":{"content":"é🙂 typed sk-abcdefghijklmnopqrstuvwxyz"}}));
    jsonl(&path, records);
    let rows = run(dir.path(), &["--since", TIME]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["turn"], 9);
    assert_eq!(rows[0]["text"], "é🙂 typed <redacted>");
    assert_eq!(rows[0]["chars"], 38);
    let clipped = run(dir.path(), &["--since", TIME, "--max-chars", "3"]);
    assert_eq!(clipped[0]["text"], "é🙂…");
    assert_eq!(
        run(dir.path(), &["--since", TIME, "--max-chars", "0"])[0]["text"],
        ""
    );
    assert_eq!(run(dir.path(), &["--include-machine"]).len(), 10);
    assert!(run(dir.path(), &["--cwd", "another"]).is_empty());
    assert!(run(dir.path(), &["--until", "2026-10-01"]).is_empty());
}

#[test]
fn marked_agent_launch_excludes_only_the_first_user_turn() {
    let dir = fixture("codex");
    let con = Connection::open(dir.path().join(".codex/state_5.sqlite")).unwrap();
    con.execute("UPDATE threads SET source='exec'", []).unwrap();
    let rows = run(dir.path(), &["--harness", "codex"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["text"], "next typed turn");
    assert_eq!(
        run(dir.path(), &["--include-machine"])[0]["origin"],
        "machine"
    );
    con.execute_batch("CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT); INSERT INTO thread_spawn_edges VALUES('parent','session-codex');").unwrap();
    assert!(run(dir.path(), &[]).is_empty());
    assert!(
        run(dir.path(), &["--include-machine"])
            .iter()
            .all(|r| r["origin"] == "machine")
    );
}

#[test]
fn claude_headless_entrypoints_exclude_every_turn() {
    for entrypoint in ["sdk-cli", "sdk-ts", "sdk-py", "cli", ""] {
        let dir = fixture("claude");
        jsonl(
            &dir.path()
                .join(".claude/projects/fixture/session-claude.jsonl"),
            vec![
                json!({"type":"user","cwd":"/fixture","timestamp":TIME,"message":{"content":"first typed turn"}}),
                json!({"type":"user","entrypoint":entrypoint,"timestamp":TIME,"message":{"content":"later typed turn"}}),
            ],
        );
        let headless = entrypoint.starts_with("sdk");
        let rows = run(dir.path(), &[]);
        assert_eq!(rows.len(), if headless { 0 } else { 2 }, "{entrypoint}");
        let audit = run(dir.path(), &["--include-machine"]);
        assert_eq!(audit.len(), 2);
        assert!(
            audit
                .iter()
                .all(|r| r["origin"] == if headless { "machine" } else { "heuristic" })
        );
    }
}

#[test]
fn codex_exec_metadata_excludes_every_turn() {
    for source in ["exec", "cli", "vscode", ""] {
        let dir = fixture("codex");
        let path = dir
            .path()
            .join(".codex/sessions/2026/11/03/rollout-session-codex.jsonl");
        jsonl(
            &path,
            vec![
                json!({"type":"session_meta","payload":{"id":"session-codex","cwd":"/fixture","timestamp":TIME,"source":source}}),
                json!({"type":"response_item","timestamp":TIME,"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"first typed turn"}]}}),
                json!({"type":"response_item","timestamp":TIME,"payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"later typed turn"}]}}),
            ],
        );
        let headless = source == "exec";
        assert_eq!(
            run(dir.path(), &[]).len(),
            if headless { 0 } else { 2 },
            "{source}"
        );
        let audit = run(dir.path(), &["--include-machine"]);
        assert_eq!(audit.len(), 2);
        assert!(
            audit
                .iter()
                .all(|r| r["origin"] == if headless { "machine" } else { "heuristic" })
        );
    }
}
