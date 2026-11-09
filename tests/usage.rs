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
    let token = |total, input, cached, output| json!({"type":"event_msg","timestamp":if total == 100 {"2026-11-03T12:00:10Z"} else {"2026-11-03T12:01:00Z"},"payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":if total == 100 {80} else {120},"cached_input_tokens":if total == 100 {30} else {90},"output_tokens":if total == 100 {20} else {30},"reasoning_output_tokens":if total == 100 {7} else {0}},"total_token_usage":{"input_tokens":input,"cached_input_tokens":cached,"output_tokens":output,"reasoning_output_tokens":7,"total_tokens":total}}}});
    let rollout = home.join(".codex/sessions/2026/11/03/rollout-usage-codex.jsonl");
    write(
        &rollout,
        vec![
            json!({"type":"session_meta","payload":{"id":CODEX,"cwd":"/fixture","timestamp":"2026-11-03T12:00:00Z"}}),
            json!({"type":"turn_context","payload":{"model":"fixture-codex"}}),
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
        "launch",
        "duplicates_dropped",
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
    assert_eq!(got[0]["input_tokens"], 6);
    assert_eq!(got[0]["cache_read_tokens"], 12);
    assert_eq!(got[0]["cache_write_tokens"], 18);
    assert_eq!(got[0]["output_tokens"], 24);
    assert_eq!(got[0]["reasoning_tokens"], Value::Null);
    assert_eq!(got[0]["total_tokens"], 60);
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
    assert_eq!(lines[1], "usage-claude\tclaude\t2\t6\t12\t18\t24\t\t60\t90");
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
        "usage is read for claude, codex and opencode sessions only"
    );
    assert!(got[2]["calls"].is_null());
}

#[test]
fn per_call_rows_and_windows() {
    let home = fixture();
    for (id, expected) in [
        (CLAUDE, [(1, 2, 3, 4, 10), (5, 10, 15, 20, 50)]),
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
        "id\tharness\tts\tmodel\tinput\tcache_read\tcache_write\toutput\treasoning\ttotal\ttools\tdedupe_key\tspeed\treported_cost_usd"
    );
}

#[test]
fn undated_claude_records_are_skipped_and_notes_go_to_stderr() {
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
    assert_eq!(calls[0]["ts"], "2026-11-03T12:01:00Z");
    assert_eq!(calls[0]["input_tokens"], 9);
    assert_eq!(
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
        .len(),
        1
    );
    let window = rows(run(
        home.path(),
        &["usage", CLAUDE, "--since", "2026-11-01", "--json"],
    ));
    assert_eq!(window[0]["calls"], 1);
    assert_eq!(window[0]["input_tokens"], 9);
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
        // Windows writes stderr lines with CRLF.
        String::from_utf8(out.stderr).unwrap().replace("\r\n", "\n"),
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
        String::from_utf8(out.stderr).unwrap().replace("\r\n", "\n"),
        "thread-missing: provider transcript not found\n"
    );
}

#[test]
fn opencode_usage_counts_calls_and_windows_without_message_text() {
    if std::env::var_os("XDG_DATA_HOME").is_some() {
        return;
    }
    let home = fixture();
    let path = home.path().join(".local/share/opencode/opencode.db");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let con = Connection::open(path).unwrap();
    con.execute_batch(r#"PRAGMA journal_mode=WAL;
        CREATE TABLE session_v2(id TEXT,parent_id TEXT,directory TEXT,title TEXT,model TEXT,agent TEXT,time_created INTEGER,time_updated INTEGER,time_archived INTEGER);
        CREATE TABLE session_message(id TEXT,session_id TEXT,type TEXT,seq INTEGER,time_created INTEGER,time_updated INTEGER,data TEXT);
        INSERT INTO session_v2 VALUES('usage-opencode',NULL,'/fixture','Usage','{"id":"session-model"}','agent',1793707200000,1793707290000,NULL);
        INSERT INTO session_v2 VALUES('empty-opencode',NULL,'/fixture','Empty',NULL,'agent',1793707200000,1793707290000,NULL);
    "#).unwrap();
    let messages = [
        (
            "assistant",
            json!({"tokens":{"input":10,"output":20,"reasoning":5,"cache":{"read":30,"write":40}},"time":{"created":1793707210000_i64},"model":{"id":"call-model"}}),
        ),
        (
            "assistant",
            json!({"tokens":{"input":2,"output":3,"cache":{"read":4,"write":5}},"model":{"id":"call-model"}}),
        ),
        ("compaction", json!({"tokens":{"input":7,"output":11}})),
        ("user", json!({"text":"SECRET-MARKER"})),
        ("assistant", json!({"content":[{"text":"SECRET-MARKER"}]})),
    ];
    for (seq, (kind, data)) in messages.iter().enumerate() {
        con.execute(
            "INSERT INTO session_message VALUES(?1,'usage-opencode',?2,?3,?4,?4,?5)",
            rusqlite::params![
                format!("message-{seq}"),
                kind,
                seq as i64,
                1793707210000_i64 + seq as i64 * 10000,
                data.to_string()
            ],
        )
        .unwrap();
    }
    // A malformed row is skipped with a diagnostic and must not fail the query.
    con.execute(
        "INSERT INTO session_message VALUES('message-bad','usage-opencode','assistant',99,1793707280000,1793707280000,'{not json')",
        [],
    )
    .unwrap();
    let out = run(home.path(), &["usage", "usage-opencode", "--json"]);
    assert!(!String::from_utf8_lossy(&out.stdout).contains("SECRET-MARKER"));
    let session = rows(out).remove(0);
    assert_eq!(session["calls"], 3);
    for (key, expected) in [
        ("input_tokens", 19),
        ("cache_read_tokens", 34),
        ("cache_write_tokens", 45),
        ("output_tokens", 39),
        ("reasoning_tokens", 5),
        ("total_tokens", 137),
    ] {
        assert_eq!(session[key], expected, "{key}");
    }
    let out = run(
        home.path(),
        &["usage", "usage-opencode", "--calls", "--json"],
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("SECRET-MARKER"));
    let calls = rows(out);
    assert_eq!(calls.len(), 3);
    for (index, call) in calls.iter().enumerate() {
        assert_eq!(call["id"], "usage-opencode");
        assert_eq!(call["harness"], "opencode");
        assert_eq!(call["tools"], json!([]));
        assert!(call["dedupe_key"].is_null());
        assert_eq!(call["speed"], "standard");
        assert!(call["reported_cost_usd"].is_null());
        assert_eq!(
            call["model"],
            if index == 2 {
                "session-model"
            } else {
                "call-model"
            }
        );
        let ts = call["ts"].as_str().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(ts).is_ok());
        assert_eq!(
            ts,
            [
                "2026-11-03T12:00:10Z",
                "2026-11-03T12:00:20Z",
                "2026-11-03T12:00:30Z"
            ][index]
        );
    }
    assert_eq!(calls[0]["output_tokens"], 25);
    assert_eq!(calls[0]["total_tokens"], 105);
    let filtered = rows(run(
        home.path(),
        &[
            "usage",
            "usage-opencode",
            "--calls",
            "--since",
            "2026-11-03T12:00:20Z",
            "--json",
        ],
    ));
    assert_eq!(filtered, calls[1..]);
    let window = rows(run(
        home.path(),
        &[
            "usage",
            "usage-opencode",
            "--since",
            "2026-11-03T12:00:20Z",
            "--until",
            "2026-11-03T12:00:20Z",
            "--json",
        ],
    ));
    assert_eq!(window[0]["calls"], 1);
    assert_eq!(window[0]["total_tokens"], 14);
    let empty = rows(run(home.path(), &["usage", "empty-opencode", "--json"]));
    for key in [
        "calls",
        "input_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "output_tokens",
        "reasoning_tokens",
        "total_tokens",
    ] {
        assert_eq!(empty[0][key], 0);
    }
    t3(home.path());
    let t3_con = Connection::open(home.path().join(".t3/userdata/state.sqlite")).unwrap();
    t3_con.execute("UPDATE provider_session_runtime SET provider_name='opencode',provider_instance_id='opencode',resume_cursor_json='{\"sessionId\":\"usage-opencode\"}' WHERE thread_id='thread-usage'", []).unwrap();
    let provider = rows(run(home.path(), &["usage", "thread-usage", "--json"]));
    assert_eq!(provider[0]["harness"], "opencode");
    assert_eq!(provider[0]["t3_thread"], "thread-usage");
    assert_eq!(provider[0]["total_tokens"], 137);
    con.execute("DROP TABLE session_message", []).unwrap();
    let unreadable = rows(run(home.path(), &["usage", "usage-opencode", "--json"]));
    assert_eq!(unreadable[0]["note"], "store unreadable");
    assert!(unreadable[0]["calls"].is_null());
    assert!(unreadable[0]["total_tokens"].is_null());
}

fn claude_record(id: Option<&str>, request: Option<&str>, output: u64) -> Value {
    json!({"type":"assistant","timestamp":"2026-11-03T12:00:10Z","requestId":request,
        "message":{"id":id,"model":"executor","usage":{"input_tokens":10,"output_tokens":output}}})
}
fn claude_path(home: &Path, id: &str) -> std::path::PathBuf {
    home.join(format!(".claude/projects/fixture/{id}.jsonl"))
}
#[test]
fn claude_cross_file_first_wins_in_argument_order_and_windows() {
    let home = fixture();
    let a = "shared-claude-a";
    let b = "shared-claude-b";
    write(
        &claude_path(home.path(), a),
        vec![claude_record(Some("shared"), Some("request"), 3)],
    );
    write(
        &claude_path(home.path(), b),
        vec![claude_record(Some("shared"), Some("request"), 99)],
    );
    for window in [
        vec![],
        vec![
            "--since",
            "2026-11-03T12:00:10Z",
            "--until",
            "2026-11-03T12:00:10Z",
        ],
    ] {
        let mut args = vec!["usage", a, b, "--json"];
        args.extend(&window);
        let sessions = rows(run(home.path(), &args));
        assert_eq!(sessions[0]["output_tokens"], 3);
        assert_eq!(sessions[1]["calls"], 0);
        assert_eq!(sessions[1]["total_tokens"], 0);
        assert_eq!(sessions[1]["duplicates_dropped"], 1);
        args.push("--calls");
        let calls = rows(run(home.path(), &args));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["dedupe_key"], "shared:request");
    }
    let reverse = rows(run(home.path(), &["usage", b, a, "--calls", "--json"]));
    assert_eq!(reverse[0]["output_tokens"], 99);
    // --keep-duplicates leaves cross-id dedupe to a caller that batches (front-door spend.py).
    let kept = rows(run(
        home.path(),
        &["usage", a, b, "--json", "--keep-duplicates"],
    ));
    assert_eq!(kept[1]["output_tokens"], 99);
    assert_eq!(kept[1]["duplicates_dropped"], 0);
    let kept_calls = rows(run(
        home.path(),
        &["usage", a, b, "--calls", "--json", "--keep-duplicates"],
    ));
    assert_eq!(kept_calls.len(), 2);
    assert_eq!(kept_calls[1]["dedupe_key"], "shared:request");
}
#[test]
fn claude_first_wins_request_only_null_keys_and_tool_names() {
    let home = fixture();
    let mut first = claude_record(Some("message"), Some("request"), 3);
    first["costUSD"] = json!(0.25);
    first["message"]["usage"]["speed"] = json!("fast");
    first["message"]["content"] = json!([{"type":"tool_use","id":"a","name":"Read"}]);
    let mut repeated = claude_record(Some("message"), Some("request"), 99);
    repeated["message"]["content"] = json!([{"type":"tool_use","id":"b","name":"Read"},{"type":"tool_use","id":"c","name":"Edit"}]);
    let mut invalid = claude_record(Some("invalid"), None, 1000);
    invalid["message"]["model"] = json!("");
    write(
        &claude_path(home.path(), CLAUDE),
        vec![
            first,
            repeated,
            claude_record(None, Some("request-only"), 4),
            claude_record(Some("message"), Some("another-request"), 5),
            claude_record(None, None, 6),
            claude_record(None, None, 6),
            invalid,
        ],
    );
    let calls = rows(run(home.path(), &["usage", CLAUDE, "--calls", "--json"]));
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0]["output_tokens"], 3);
    assert_eq!(calls[0]["tools"], json!(["Read", "Edit"]));
    assert_eq!(calls[0]["speed"], "fast");
    assert_eq!(calls[0]["reported_cost_usd"], 0.25);
    assert_eq!(calls[1]["dedupe_key"], ":request-only");
    assert_eq!(calls[1]["speed"], "standard");
    assert!(calls[1]["reported_cost_usd"].is_null());
    assert!(calls[3]["dedupe_key"].is_null());
    assert!(calls[4]["dedupe_key"].is_null());
}
#[test]
fn claude_advisor_is_a_separate_model_call_and_dedupes() {
    let home = fixture();
    let mut record = claude_record(Some("executor"), Some("request"), 3);
    record["message"]["usage"]["iterations"] = json!([
        {"type":"advisor_message","model":"advisor","input_tokens":7,"output_tokens":11},
        {"type":"advisor_message","model":"<synthetic>","output_tokens":1000},
        {"type":"advisor_message","model":"","output_tokens":1000}]);
    record["costUSD"] = json!(0.5);
    write(
        &claude_path(home.path(), CLAUDE),
        vec![record.clone(), record],
    );
    let calls = rows(run(home.path(), &["usage", CLAUDE, "--calls", "--json"]));
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1]["model"], "advisor");
    assert_eq!(calls[1]["dedupe_key"], "executor:request:advisor:0");
    assert_eq!(calls[1]["input_tokens"], 7);
    assert_eq!(calls[1]["output_tokens"], 11);
    assert!(calls[1]["reported_cost_usd"].is_null());
    assert_eq!(calls[1]["tools"], json!([]));
}
fn codex_usage(input: u64, cached: u64, write: u64, output: u64, reasoning: u64) -> Value {
    json!({"input_tokens":input,"cached_input_tokens":cached,"cache_write_input_tokens":write,
        "output_tokens":output,"reasoning_output_tokens":reasoning,"total_tokens":input+output})
}
fn codex_event(ts: &str, last: Value, cumulative: Value) -> Value {
    json!({"type":"event_msg","timestamp":ts,"payload":{"type":"token_count",
        "info":{"last_token_usage":last,"total_token_usage":cumulative}}})
}
fn codex_path(home: &Path) -> std::path::PathBuf {
    home.join(".codex/sessions/2026/11/03/rollout-usage-codex.jsonl")
}
fn codex_meta() -> Value {
    json!({"type":"session_meta","timestamp":"2026-11-03T12:00:00Z","payload":{"id":CODEX,"cwd":"/fixture"}})
}
fn codex_context(model: &str) -> Value {
    json!({"type":"turn_context","payload":{"model":model}})
}
#[test]
fn codex_fork_burst_baseline_only_bills_real_child_request() {
    for subagent in [false, true] {
        let home = fixture();
        let mut meta = codex_meta();
        if subagent {
            meta["payload"]["source"] =
                json!({"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}});
        } else {
            meta["payload"]["forked_from_id"] = json!("parent");
        }
        write(
            &codex_path(home.path()),
            vec![
                meta,
                codex_context("child-model"),
                codex_event(
                    "2026-11-03T12:00:00.010Z",
                    codex_usage(1000, 300, 0, 200, 10),
                    codex_usage(1000, 300, 0, 200, 10),
                ),
                codex_event(
                    "2026-11-03T12:00:00.040Z",
                    codex_usage(1000, 300, 0, 200, 10),
                    codex_usage(2000, 600, 0, 400, 20),
                ),
                json!({"type":"session_meta","payload":{"id":"ancestor"}}),
                codex_event(
                    "2026-11-03T12:00:05Z",
                    codex_usage(100, 30, 10, 20, 7),
                    codex_usage(2100, 630, 10, 420, 27),
                ),
            ],
        );
        let calls = rows(run(home.path(), &["usage", CODEX, "--calls", "--json"]));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["model"], "child-model");
        assert_eq!(calls[0]["input_tokens"], 60);
        assert_eq!(calls[0]["cache_write_tokens"], 10);
        assert_eq!(calls[0]["reasoning_tokens"], 7);
        assert_eq!(calls[0]["total_tokens"], 120);
        assert_eq!(
            calls[0]["dedupe_key"],
            "codex:usage-codex:1793707205000:child-model:60:30:10:20:1"
        );
        let session = rows(run(home.path(), &["usage", CODEX, "--json"]));
        assert_eq!(session[0]["total_tokens"], 120);
    }
}
#[test]
fn codex_moved_copy_dedupes_using_meta_id_and_occurrence() {
    let home = fixture();
    let a = codex_usage(100, 30, 0, 20, 7);
    let b = codex_usage(200, 60, 0, 40, 14);
    let records = vec![
        codex_meta(),
        codex_context("model"),
        codex_event("2026-11-03T12:00:10Z", a.clone(), a.clone()),
        codex_event("2026-11-03T12:00:10Z", a, b),
    ];
    write(&codex_path(home.path()), records.clone());
    let moved = home
        .path()
        .join(".codex/sessions/2026/11/03/rollout-moved-codex.jsonl");
    write(&moved, records);
    let con = Connection::open(home.path().join(".codex/state_5.sqlite")).unwrap();
    con.execute(
        "INSERT INTO threads VALUES('moved-codex','/fixture',1793707200,1793707290,?,'cli')",
        [moved.to_str().unwrap()],
    )
    .unwrap();
    let calls = rows(run(
        home.path(),
        &["usage", CODEX, "moved-codex", "--calls", "--json"],
    ));
    assert_eq!(calls.len(), 2);
    assert!(calls[0]["dedupe_key"].as_str().unwrap().ends_with(":1"));
    assert!(calls[1]["dedupe_key"].as_str().unwrap().ends_with(":2"));
    let sessions = rows(run(home.path(), &["usage", CODEX, "moved-codex", "--json"]));
    assert_eq!(sessions[0]["total_tokens"], 240);
    assert_eq!(sessions[1]["duplicates_dropped"], 2);
    assert_eq!(sessions[1]["total_tokens"], 0);
}
#[test]
fn codex_unchanged_cumulative_and_zero_component_events_are_skipped() {
    let home = fixture();
    let a = codex_usage(100, 30, 0, 20, 7);
    let mut zero = codex_usage(0, 0, 0, 0, 0);
    zero["total_tokens"] = json!(900);
    write(
        &codex_path(home.path()),
        vec![
            codex_meta(),
            codex_context("model"),
            codex_event("2026-11-03T12:00:10Z", a.clone(), a.clone()),
            codex_event("2026-11-03T12:00:11Z", a.clone(), a),
            codex_event("2026-11-03T12:00:12Z", zero, codex_usage(0, 0, 0, 0, 0)),
        ],
    );
    let calls = rows(run(home.path(), &["usage", CODEX, "--calls", "--json"]));
    assert_eq!(calls.len(), 1);
    let session = rows(run(home.path(), &["usage", CODEX, "--json"]));
    assert_eq!(session[0]["total_tokens"], 120);
}

fn codex_speed(tier: &str) -> Value {
    json!({"type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"service_tier":tier}}})
}
#[test]
fn codex_bridges_unreported_requests_with_original_model_and_speed() {
    let home = fixture();
    let base = codex_usage(100, 30, 10, 20, 7);
    let interrupted = codex_usage(40, 10, 5, 10, 3);
    let final_usage = codex_usage(60, 20, 5, 20, 4);
    write(
        &codex_path(home.path()),
        vec![
            codex_meta(),
            codex_context("base-model"),
            codex_event("2026-11-03T12:00:10Z", base.clone(), base),
            codex_context("interrupted-model"),
            codex_speed("priority"),
            json!({"type":"token_usage_record","timestamp":"2026-11-03T12:00:11Z","payload":{"usage":interrupted}}),
            codex_context("final-model"),
            codex_speed("ultrafast"),
            json!({"type":"token_usage_record","timestamp":"2026-11-03T12:00:12Z","payload":{"usage":final_usage.clone()}}),
            codex_event(
                "2026-11-03T12:00:13Z",
                final_usage,
                codex_usage(200, 60, 20, 50, 14),
            ),
        ],
    );
    let calls = rows(run(home.path(), &["usage", CODEX, "--calls", "--json"]));
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0]["speed"], "standard");
    assert_eq!(calls[1]["model"], "interrupted-model");
    assert_eq!(calls[1]["speed"], "fast");
    assert_eq!(calls[1]["ts"], "2026-11-03T12:00:11Z");
    assert_eq!(calls[1]["total_tokens"], 50);
    assert_eq!(calls[2]["model"], "final-model");
    assert_eq!(calls[2]["speed"], "ultrafast");
    assert_eq!(calls[2]["total_tokens"], 80);
}
#[test]
fn codex_rebases_malformed_snapshots_and_rejects_invalid_components() {
    let home = fixture();
    let a = codex_usage(100, 30, 0, 20, 7);
    let mut invalid = a.clone();
    invalid["cached_input_tokens"] = json!(101);
    let mut invalid_reasoning = a.clone();
    invalid_reasoning["reasoning_output_tokens"] = json!(21);
    let mut invalid_total = a.clone();
    invalid_total["total_tokens"] = json!(121);
    write(
        &codex_path(home.path()),
        vec![
            codex_meta(),
            codex_context("model"),
            codex_event("2026-11-03T12:00:10Z", a.clone(), a.clone()),
            // A valid cumulative snapshot rebases even when the request is malformed.
            codex_event(
                "2026-11-03T12:00:11Z",
                invalid.clone(),
                codex_usage(200, 60, 0, 40, 14),
            ),
            codex_event(
                "2026-11-03T12:00:12Z",
                a.clone(),
                codex_usage(300, 90, 0, 60, 21),
            ),
            codex_event("2026-11-03T12:00:13Z", invalid, Value::Null),
            codex_event("2026-11-03T12:00:14Z", invalid_reasoning, Value::Null),
            codex_event("2026-11-03T12:00:15Z", invalid_total, Value::Null),
            codex_event(
                "2026-11-03T12:00:16Z",
                a.clone(),
                json!({"input_tokens":-1,"output_tokens":20}),
            ),
            codex_event("2026-11-03T12:00:17Z", a, codex_usage(400, 120, 0, 80, 28)),
        ],
    );
    let calls = rows(run(home.path(), &["usage", CODEX, "--calls", "--json"]));
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[1]["ts"], "2026-11-03T12:00:12Z");
    assert_eq!(calls[2]["ts"], "2026-11-03T12:00:17Z");
}
#[test]
fn codex_legacy_signatures_restarts_and_sequence_replays() {
    let home = fixture();
    let a = codex_usage(100, 30, 0, 20, 7);
    let b = codex_usage(40, 10, 0, 10, 3);
    write(
        &codex_path(home.path()),
        vec![
            codex_meta(),
            codex_context("model"),
            codex_event("2026-11-03T12:00:10Z", a.clone(), a.clone()),
            codex_event("2026-11-03T12:00:11Z", b.clone(), Value::Null),
            codex_event("2026-11-03T12:00:12Z", b.clone(), Value::Null),
            codex_event(
                "2026-11-03T12:00:13Z",
                b.clone(),
                codex_usage(140, 40, 0, 30, 10),
            ),
            // Replaying the first sequence start restores the previous baseline.
            codex_event("2026-11-03T12:00:10Z", a.clone(), a.clone()),
            codex_event(
                "2026-11-03T12:00:14Z",
                b.clone(),
                codex_usage(180, 50, 0, 40, 13),
            ),
            codex_event("2026-11-03T12:00:15Z", a.clone(), a.clone()),
            codex_event("2026-11-03T12:00:16Z", b, codex_usage(140, 40, 0, 30, 10)),
        ],
    );
    let calls = rows(run(home.path(), &["usage", CODEX, "--calls", "--json"]));
    assert_eq!(calls.len(), 5);
    assert_eq!(
        calls
            .iter()
            .map(|c| c["total_tokens"].as_u64().unwrap())
            .sum::<u64>(),
        390
    );
    assert_eq!(calls[2]["ts"], "2026-11-03T12:00:14Z");
    assert_eq!(calls[3]["ts"], "2026-11-03T12:00:15Z");
}
