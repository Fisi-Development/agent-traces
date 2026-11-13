//! Synthetic store only. Agents grepped, sed-ed and cut `show`'s Markdown to
//! pull one turn's tool calls and results; `--tools-only` and `--json` give
//! them that directly.
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

fn session(home: &Path) {
    let dir = home.join(".claude/projects/synthetic");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("show-tools-fixture.jsonl"),
        lines(&[
            json!({"type":"user","timestamp":TIME,"cwd":home,"message":{"content":"PROMPTMARK list the files"}}),
            json!({"type":"assistant","timestamp":"2026-10-20T01:00:01Z","cwd":home,"message":{"content":[
                {"type":"text","text":"ASSISTANTMARK I will list them."},
                {"type":"tool_use","name":"Bash","id":"call-1","input":{"command":"ls -la"}}]}}),
            json!({"type":"user","timestamp":"2026-10-20T01:00:02Z","cwd":home,"message":{"content":[
                {"type":"tool_result","tool_use_id":"call-1","content":"RESULTMARK a.txt"}]}}),
            json!({"type":"assistant","timestamp":"2026-10-20T01:00:03Z","cwd":home,"message":{"content":[
                {"type":"tool_use","name":"Read","id":"call-2","input":{"file_path":"/nope"}}]}}),
            json!({"type":"user","timestamp":"2026-10-20T01:00:04Z","cwd":home,"message":{"content":[
                {"type":"tool_result","tool_use_id":"call-2","is_error":true,"content":"ERRMARK no such file"}]}}),
            json!({"type":"assistant","timestamp":"2026-10-20T01:00:05Z","cwd":home,"message":{"content":[
                {"type":"text","text":"ASSISTANTMARK done"}]}}),
        ]),
    )
    .unwrap();
}

#[test]
fn tools_only_prints_calls_and_results_without_prose() {
    let temp = tempfile::tempdir().unwrap();
    session(temp.path());
    let (code, out, err) = run(temp.path(), &["show", "show-tools-fixture", "--tools-only"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("tool Bash") && out.contains("ls -la"), "{out}");
    assert!(
        out.contains("RESULTMARK"),
        "results are part of the tools view: {out}"
    );
    assert!(out.contains("ERRMARK"), "{out}");
    // The header may name the session by its first prompt; the turns must not.
    let body = out.split_once("---- turn").map_or(out.as_str(), |(_, b)| b);
    assert!(!body.contains("PROMPTMARK"), "{out}");
    assert!(!body.contains("ASSISTANTMARK"), "{out}");
    // Compact mode pairs each result with its call by id, so a reader can match them.
    assert!(out.contains("call-1"), "{out}");
    let (code, _, err) = run(
        temp.path(),
        &["show", "show-tools-fixture", "--tools-only", "--no-tools"],
    );
    assert_ne!(
        code, 0,
        "--tools-only with --no-tools is a contradiction: {err}"
    );
}

#[test]
fn json_gives_turns_and_paired_tool_events() {
    let temp = tempfile::tempdir().unwrap();
    session(temp.path());
    let (code, out, err) = run(
        temp.path(),
        &["show", "show-tools-fixture", "--json", "--results"],
    );
    assert_eq!(code, 0, "{err}");
    let doc: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(doc["session"]["id"], "show-tools-fixture");
    assert_eq!(doc["turns_total"], 1);
    let turn = &doc["turns"][0];
    assert_eq!(turn["turn"], 1);
    let events = turn["events"].as_array().unwrap();
    let roles: Vec<_> = events.iter().map(|e| e["role"].as_str().unwrap()).collect();
    assert_eq!(
        roles,
        [
            "prompt",
            "assistant",
            "tool",
            "result",
            "tool",
            "result",
            "assistant"
        ]
    );
    assert_eq!(events[2]["name"], "Bash");
    assert_eq!(events[2]["call_id"], "call-1");
    assert_eq!(events[3]["call_id"], "call-1");
    assert!(events[3]["text"].as_str().unwrap().contains("RESULTMARK"));
    assert_eq!(events[5]["error"], true);
    assert_eq!(events[0]["ts"], "2026-10-20T01:00:00Z");

    // --tools-only narrows the JSON the same way.
    let (code, out, _) = run(
        temp.path(),
        &["show", "show-tools-fixture", "--json", "--tools-only"],
    );
    assert_eq!(code, 0);
    let doc: Value = serde_json::from_str(&out).unwrap();
    let roles: Vec<_> = doc["turns"][0]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["role"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(roles, ["tool", "result", "tool", "result"]);
    // --no-tools drops tool calls and their results.
    let (_, out, _) = run(
        temp.path(),
        &["show", "show-tools-fixture", "--json", "--no-tools"],
    );
    let doc: Value = serde_json::from_str(&out).unwrap();
    assert!(
        doc["turns"][0]["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["role"] != "tool" && e["role"] != "result")
    );
}
