use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const CLAUDE: &str = "usage-claude";
const CODEX: &str = "usage-codex";
fn write(path: &Path, records: Vec<Value>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        records.iter().map(|r| format!("{r}\n")).collect::<String>(),
    )
    .unwrap();
}
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let assistant = |id, model, n| json!({"type":"assistant","timestamp":if n == 1 {"2026-11-03T12:00:10Z"} else if n == 5 {"2026-11-03T12:01:00Z"} else {"2026-11-03T12:01:30Z"},"message":{"id":id,"model":model,"content":if n == 2 {json!([{"type":"tool_use","id":format!("{id}-tool"),"name":"Read"},{"type":"tool_use","id":"extra-tool","name":"Edit"}])} else {json!([{"type":"tool_use","id":format!("{id}-tool"),"name":"Read"}])},"usage":{"input_tokens":n,"cache_read_input_tokens":2*n,"cache_creation_input_tokens":3*n,"output_tokens":4*n}}});
    write(
        &home.join(format!(".claude/projects/fixture/{CLAUDE}.jsonl")),
        vec![
            json!({"type":"user","cwd":"/fixture","timestamp":"2026-11-03T12:00:00Z","message":{"content":"hello"}}),
            assistant("msg-one", "fixture-model", 1),
            assistant("msg-one", "fixture-model", 2),
            assistant("msg-one", "fixture-model", 3),
            assistant("msg-two", "fixture-model", 5),
            assistant("msg-synthetic", "<synthetic>", 1000),
        ],
    );
    let token = |total, input, cached, output| json!({"type":"event_msg","timestamp":if total == 100 {"2026-11-03T12:00:10Z"} else {"2026-11-03T12:01:00Z"},"payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":input,"cached_input_tokens":cached,"output_tokens":output,"reasoning_output_tokens":7,"total_tokens":total}}}});
    let rollout = home.join(".codex/sessions/2026/11/03/rollout-usage-codex.jsonl");
    write(
        &rollout,
        vec![
            json!({"type":"session_meta","payload":{"id":CODEX,"cwd":"/fixture","timestamp":"2026-11-03T12:00:00Z"}}),
            json!({"type":"event_msg","payload":{"type":"token_count","info":null}}),
            json!({"type":"response_item","payload":{"type":"function_call","name":"exec_command"}}),
            token(100, 80, 30, 20),
            json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch"}}),
            json!({"type":"response_item","payload":{"type":"local_shell_call"}}),
            token(250, 200, 120, 50),
            token(250, 200, 120, 50),
        ],
    );
    let con = Connection::open(home.join(".codex/state_5.sqlite")).unwrap();
    con.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT,cwd TEXT,created_at INTEGER,updated_at INTEGER,rollout_path TEXT,source TEXT);").unwrap();
    con.execute(
        "INSERT INTO threads VALUES(?,'/fixture',1793707200,1793707290,?,'cli')",
        [CODEX, &rollout.to_string_lossy()],
    )
    .unwrap();
    dir
}
fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_TRACES_HOME", home)
        .env("AGENT_TRACES_CACHE", home.join("cache"))
        .output()
        .unwrap()
}
fn rows(out: Output) -> Vec<Value> {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
#[test]
fn usage_deduplicates_and_preserves_order_and_columns() {
    let home = fixture();
    let got = rows(run(home.path(), &["usage", CLAUDE, CODEX, "--json"]));
    assert_eq!(got.len(), 2);
    let keys = [
        "harness",
        "id",
        "t3_thread",
        "model",
        "path",
        "started",
        "updated",
        "wall_s",
        "calls",
        "input_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "output_tokens",
        "reasoning_tokens",
        "total_tokens",
    ];
    for row in &got {
        assert_eq!(
            row.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            keys
        );
        assert_eq!(row["t3_thread"], Value::Null);
        assert_eq!(row["wall_s"], 90, "{row}");
        assert_eq!(row["calls"], 2);
    }
    assert_eq!(got[0]["id"], CLAUDE);
    assert_eq!(got[0]["input_tokens"], 8);
    assert_eq!(got[0]["cache_read_tokens"], 16);
    assert_eq!(got[0]["cache_write_tokens"], 24);
    assert_eq!(got[0]["output_tokens"], 32);
    assert_eq!(got[0]["reasoning_tokens"], Value::Null);
    assert_eq!(got[0]["total_tokens"], 80);
    assert_eq!(got[1]["id"], CODEX);
    assert_eq!(got[1]["input_tokens"], 80);
    assert_eq!(got[1]["cache_read_tokens"], 120);
    assert_eq!(got[1]["cache_write_tokens"], 0);
    assert_eq!(got[1]["output_tokens"], 50);
    assert_eq!(got[1]["reasoning_tokens"], 7);
    assert_eq!(got[1]["total_tokens"], 250);
    let reverse = rows(run(home.path(), &["usage", CODEX, CLAUDE, "--json"]));
    assert_eq!(reverse[0]["id"], CODEX);
    assert_eq!(reverse[1]["id"], CLAUDE);
    let out = run(home.path(), &["usage", CLAUDE, CODEX]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[0],
        "id\tharness\tcalls\tinput\tcache_read\tcache_write\toutput\treasoning\ttotal\twall_s"
    );
    assert_eq!(lines[1], "usage-claude\tclaude\t2\t8\t16\t24\t32\t\t80\t90");
    assert_eq!(
        lines[2],
        "usage-codex\tcodex\t2\t80\t120\t0\t50\t7\t250\t90"
    );
}
#[test]
fn missing_id_fails_atomically() {
    let home = fixture();
    let out = run(home.path(), &["usage", CLAUDE, "unknown-id", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
}

fn t3(home: &Path) {
    let path = home.join(".t3/userdata/state.sqlite");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let con = Connection::open(path).unwrap();
    con.execute_batch(r#"PRAGMA journal_mode=WAL;
        CREATE TABLE projection_projects(project_id TEXT, workspace_root TEXT, title TEXT);
        CREATE TABLE projection_threads(thread_id TEXT,title TEXT,branch TEXT,worktree_path TEXT,created_at TEXT,updated_at TEXT,latest_user_message_at TEXT,model_selection_json TEXT,archived_at TEXT,deleted_at TEXT,project_id TEXT);
        CREATE TABLE provider_session_runtime(thread_id TEXT,provider_name TEXT,provider_instance_id TEXT,resume_cursor_json TEXT,status TEXT);
        INSERT INTO projection_projects VALUES('project','/fixture','Fixture');
        INSERT INTO projection_threads VALUES('thread-usage','Usage',NULL,'/fixture','2026-11-03T12:00:00Z','2026-11-03T12:01:30Z',NULL,'{}',NULL,NULL,'project');
        INSERT INTO projection_threads VALUES('thread-missing','Missing',NULL,'/fixture',NULL,NULL,NULL,'{}',NULL,NULL,'project');
        INSERT INTO provider_session_runtime VALUES('thread-usage','codex','codex','{"threadId":"usage-codex"}','idle');
        INSERT INTO provider_session_runtime VALUES('thread-missing','codex','codex','{"threadId":"absent-provider"}','idle');
    "#).unwrap();
}
#[test]
fn t3_follows_provider_and_missing_provider_is_explicit() {
    let home = fixture();
    t3(home.path());
    let got = rows(run(
        home.path(),
        &["usage", "thread-usage", "thread-missing", CODEX, "--json"],
    ));
    assert_eq!(got[0]["harness"], "codex");
    assert_eq!(got[0]["id"], CODEX);
    assert_eq!(got[0]["t3_thread"], "thread-usage");
    assert_eq!(got[0]["calls"], 2);
    assert_eq!(got[1]["id"], "thread-missing");
    assert_eq!(got[1]["note"], "provider transcript not found");
    assert_eq!(got[1]["wall_s"], Value::Null);
    for key in [
        "calls",
        "input_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "output_tokens",
        "reasoning_tokens",
        "total_tokens",
    ] {
        assert_eq!(got[1][key], Value::Null);
    }
    assert_eq!(got[2]["t3_thread"], Value::Null);
    let ambiguous = run(home.path(), &["usage", CLAUDE, "thread-", "--json"]);
    assert_eq!(
        ambiguous.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&ambiguous.stderr)
    );
    assert!(ambiguous.stdout.is_empty());
}
#[test]
fn missing_provider_file_returns_the_thread_with_a_note() {
    let home = fixture();
    t3(home.path());
    fs::remove_file(
        home.path()
            .join(".codex/sessions/2026/11/03/rollout-usage-codex.jsonl"),
    )
    .unwrap();
    let got = rows(run(home.path(), &["usage", "thread-usage", "--json"]));
    assert_eq!(got[0]["id"], "thread-usage");
    assert_eq!(got[0]["harness"], "t3");
    assert_eq!(got[0]["note"], "provider transcript not found");
    assert!(got[0]["calls"].is_null());
}

#[test]
fn missing_transcript_is_null_not_zero() {
    let home = fixture();
    fs::remove_file(
        home.path()
            .join(".codex/sessions/2026/11/03/rollout-usage-codex.jsonl"),
    )
    .unwrap();
    let got = rows(run(home.path(), &["usage", CODEX, "--json"]));
    assert_eq!(got[0]["id"], CODEX);
    assert_eq!(got[0]["note"], "transcript not found");
    for key in ["calls", "input_tokens", "output_tokens", "total_tokens"] {
        assert!(got[0][key].is_null(), "{key} should be null");
    }
}

#[test]
fn absent_usage_is_zero_and_unsupported_harness_is_null() {
    let home = fixture();
    write(
        &home
            .path()
            .join(".claude/projects/fixture/empty-claude.jsonl"),
        vec![json!({"type":"user","message":{"content":"empty"}})],
    );
    let rollout = home
        .path()
        .join(".codex/sessions/2026/11/03/rollout-usage-codex.jsonl");
    write(
        &rollout,
        vec![json!({"type":"event_msg","payload":{"type":"token_count","info":null}})],
    );
    let grok = home
        .path()
        .join(".grok/sessions/%2Ffixture/usage-grok/chat_history.jsonl");
    write(&grok, vec![json!({"type":"user","content":"empty"})]);
    fs::write(grok.parent().unwrap().join("summary.json"), "{}").unwrap();
    let got = rows(run(
        home.path(),
        &["usage", "empty-claude", CODEX, "usage-grok", "--json"],
    ));
    for row in &got[..2] {
        for key in [
            "calls",
            "input_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "output_tokens",
            "total_tokens",
        ] {
            assert_eq!(row[key], 0);
        }
    }
    assert!(got[0]["wall_s"].is_null());
    assert!(got[0]["reasoning_tokens"].is_null());
    assert_eq!(got[1]["reasoning_tokens"], 0);
    assert_eq!(
        got[2]["note"],
        "usage is read for claude and codex sessions only"
    );
    assert!(got[2]["calls"].is_null());
}

#[test]
fn per_call_rows_and_windows() {
    let home = fixture();
    for (id, expected) in [
        (CLAUDE, [(3, 6, 9, 12, 30), (5, 10, 15, 20, 50)]),
        (CODEX, [(50, 30, 0, 20, 100), (30, 90, 0, 30, 150)]),
    ] {
        let calls = rows(run(home.path(), &["usage", id, "--calls", "--json"]));
        assert_eq!(calls.len(), 2);
        for (call, (input, read, write, output, total)) in calls.iter().zip(expected) {
            assert_eq!(call["id"], id);
            assert_eq!(call["input_tokens"], input);
            assert_eq!(call["cache_read_tokens"], read);
            assert_eq!(call["cache_write_tokens"], write);
            assert_eq!(call["output_tokens"], output);
            assert_eq!(call["total_tokens"], total);
        }
        assert_eq!(calls[0]["ts"], "2026-11-03T12:00:10Z");
        assert_eq!(calls[1]["ts"], "2026-11-03T12:01:00Z");
        if id == CLAUDE {
            assert_eq!(calls[0]["tools"], json!(["Read", "Edit"]));
            assert!(calls[0]["reasoning_tokens"].is_null());
        } else {
            assert_eq!(calls[0]["tools"], json!(["exec_command"]));
            assert_eq!(
                calls[1]["tools"],
                json!(["apply_patch", "local_shell_call"])
            );
            assert_eq!(calls[0]["reasoning_tokens"], 7);
            assert_eq!(calls[1]["reasoning_tokens"], 0);
        }
        let session = rows(run(home.path(), &["usage", id, "--json"])).remove(0);
        for key in [
            "input_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "output_tokens",
            "total_tokens",
        ] {
            assert_eq!(
                calls.iter().map(|c| c[key].as_u64().unwrap()).sum::<u64>(),
                session[key].as_u64().unwrap()
            );
        }
        let window = rows(run(
            home.path(),
            &[
                "usage",
                id,
                "--since",
                "2026-11-03T12:01:00Z",
                "--until",
                "2026-11-03T12:01:00Z",
                "--json",
            ],
        ))
        .remove(0);
        assert_eq!(window["calls"], 1);
        for key in [
            "input_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "output_tokens",
            "reasoning_tokens",
            "total_tokens",
        ] {
            assert_eq!(window[key], calls[1][key]);
        }
        assert_eq!(
            window["window"],
            json!({"since":"2026-11-03T12:01:00Z","until":"2026-11-03T12:01:00Z"})
        );
        assert_eq!(window["wall_s"], session["wall_s"]);
        let empty = rows(run(
            home.path(),
            &["usage", id, "--until", "2026-11-02", "--json"],
        ));
        assert_eq!(empty[0]["calls"], 0);
        let filtered = rows(run(
            home.path(),
            &[
                "usage",
                id,
                "--calls",
                "--since",
                "2026-11-03T12:01:00Z",
                "--json",
            ],
        ));
        assert_eq!(filtered, calls[1..]);
    }
    let out = run(home.path(), &["usage", CLAUDE, "--calls"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
        "id\tharness\tts\tmodel\tinput\tcache_read\tcache_write\toutput\treasoning\ttotal\ttools"
    );
}

#[test]
fn undated_calls_are_only_dropped_in_windows_and_notes_go_to_stderr() {
    let home = fixture();
    let transcript = home
        .path()
        .join(format!(".claude/projects/fixture/{CLAUDE}.jsonl"));
    write(
        &transcript,
        vec![
            json!({"type":"assistant","message":{"id":"undated","model":"fixture-model","usage":{"input_tokens":4}}}),
            json!({"type":"assistant","timestamp":"2026-11-03T12:01:00Z","message":{"id":"undated","model":"fixture-model","usage":{"input_tokens":9}}}),
            json!({"type":"assistant","message":{"id":"no-usage","model":"fixture-model"}}),
        ],
    );
    let calls = rows(run(home.path(), &["usage", CLAUDE, "--calls", "--json"]));
    assert_eq!(calls.len(), 1);
    assert!(calls[0]["ts"].is_null());
    assert_eq!(calls[0]["input_tokens"], 9);
    assert!(
        rows(run(
            home.path(),
            &[
                "usage",
                CLAUDE,
                "--calls",
                "--since",
                "2026-11-01",
                "--json"
            ]
        ))
        .is_empty()
    );
    let window = rows(run(
        home.path(),
        &["usage", CLAUDE, "--since", "2026-11-01", "--json"],
    ));
    assert_eq!(window[0]["calls"], 0);
    assert_eq!(window[0]["input_tokens"], 0);
    assert_eq!(window[0]["window"]["until"], Value::Null);
    fs::remove_file(
        home.path()
            .join(".codex/sessions/2026/11/03/rollout-usage-codex.jsonl"),
    )
    .unwrap();
    let out = run(home.path(), &["usage", CODEX, "--calls", "--json"]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "usage-codex: transcript not found\n"
    );
    t3(home.path());
    let out = run(
        home.path(),
        &["usage", "thread-missing", "--calls", "--json"],
    );
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "thread-missing: provider transcript not found\n"
    );
}
