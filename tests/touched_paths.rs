//! Synthetic stores only. `touched` must find a path whose spelling holds a
//! backslash, which transcripts store JSON-escaped (every Windows path).
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

const TIME: &str = "2026-10-20T01:00:00Z";

fn lines(records: &[Value]) -> String {
    records.iter().map(|v| v.to_string() + "\n").collect()
}

fn run(home: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("AGENT_TRACES_NOW", "2026-10-20T01:02:03Z")
        .env("AGENT_TRACES_CACHE", home.join("cache"))
        .env("AGENT_TRACES_OUT", home.join("out"))
        .env("AGENT_TRACES_HOST", "fixture-host")
        .current_dir(home)
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn touched_finds_backslash_paths_in_claude_and_codex_transcripts() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    // On Windows this is a nested folder; elsewhere a file name holding a
    // backslash. Either way the path text has one, as `C:\Users\...` does.
    let target = home.join("memory").join(r"notes\memo.md");
    let spelled = target.to_str().unwrap().to_owned();
    assert!(spelled.contains('\\'));

    // Claude stores the tool input as an object: the path is escaped once.
    let claude = home.join(".claude/projects/synthetic");
    fs::create_dir_all(&claude).unwrap();
    fs::write(
        claude.join("claude-writer.jsonl"),
        lines(&[
            json!({"type":"user","timestamp":TIME,"cwd":home,"message":{"content":"write the memo"}}),
            json!({"type":"assistant","timestamp":TIME,"cwd":home,"message":{"content":[
                {"type":"tool_use","name":"Write","id":"call-1","input":{"file_path":spelled,"content":"x"}}]}}),
        ]),
    )
    .unwrap();
    // Codex stores function arguments as a JSON string: escaped twice on disk.
    let codex = home.join(".codex/sessions/2026/10/20");
    fs::create_dir_all(&codex).unwrap();
    let arguments = json!({"command": format!("echo x > {spelled}")}).to_string();
    let rollout = codex.join("rollout-codex-writer.jsonl");
    let db = rusqlite::Connection::open(home.join(".codex/state_5.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT,cwd TEXT,created_at INTEGER,updated_at INTEGER,rollout_path TEXT,source TEXT);").unwrap();
    db.execute(
        "INSERT INTO threads VALUES('codex-writer',?1,1792458000,1792458005,?2,'cli')",
        [home.to_string_lossy(), rollout.to_string_lossy()],
    )
    .unwrap();
    fs::write(
        &rollout,
        lines(&[
            json!({"type":"session_meta","timestamp":TIME,"payload":{"id":"codex-writer","cwd":home,"timestamp":TIME}}),
            json!({"type":"response_item","timestamp":"2026-10-20T01:00:05Z","payload":{
                "type":"function_call","name":"shell","call_id":"c1","arguments":arguments}}),
        ]),
    )
    .unwrap();

    for exact in [true, false] {
        let mut args = vec!["touched", spelled.as_str(), "--limit", "5"];
        if exact {
            args.push("--exact");
        }
        let (code, stdout, stderr) = run(home, &args);
        assert_eq!(code, 0, "exact={exact}\n{stdout}\n{stderr}");
        assert!(
            stderr.contains("2 write-like calls naming"),
            "exact={exact}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("claude-writer"), "{stdout}");
        assert!(stdout.contains("codex-writer"), "{stdout}");
    }

    // A sibling that shares only the file name is not the exact path.
    let (code, _, stderr) = run(
        home,
        &[
            "touched",
            home.join("elsewhere")
                .join(r"notes\memo.md")
                .to_str()
                .unwrap(),
            "--exact",
        ],
    );
    assert_eq!(code, 1, "{stderr}");
}
