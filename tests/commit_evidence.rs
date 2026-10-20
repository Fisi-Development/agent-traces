//! Synthetic stores only. Git and the CLI run against disposable repositories.
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const NOW: &str = "2026-10-20T01:02:03Z";
const TIME: &str = "2026-10-20T01:00:00Z";
const LAST: &str = "2026-10-20T01:00:01Z";

struct Fixture {
    temp: tempfile::TempDir,
    repo: PathBuf,
    other: PathBuf,
    oid: String,
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_AUTHOR_DATE", TIME)
        .env("GIT_COMMITTER_DATE", TIME)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let other = temp.path().join("other");
        for p in [&repo, &other] {
            fs::create_dir(p).unwrap();
            git(p, &["init", "--initial-branch=main"]);
            for message in ["first", "second"] {
                git(
                    p,
                    &[
                        "-c",
                        "user.name=Synthetic",
                        "-c",
                        "user.email=synthetic@example.invalid",
                        "commit",
                        "--allow-empty",
                        "-m",
                        message,
                    ],
                );
            }
        }
        let oid = git(&repo, &["rev-parse", "HEAD"]);
        Self {
            temp,
            repo,
            other,
            oid,
        }
    }

    fn home(&self) -> &Path {
        self.temp.path()
    }
    fn sha(&self) -> &str {
        &self.oid[..7]
    }

    fn write(&self, id: &str, records: &[Value]) {
        let dir = self.home().join(".claude/projects/synthetic");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{id}.jsonl")),
            records
                .iter()
                .map(|v| v.to_string() + "\n")
                .collect::<String>(),
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn call(
        &self,
        id: &str,
        command: &str,
        result: Option<&str>,
        error: bool,
        structured: bool,
        cwd: &Path,
        time: &str,
    ) {
        let mut records = vec![json!({"type":"assistant","timestamp":time,"cwd":cwd,
            "message":{"content":[{"type":"tool_use","name":"Bash","id":"call-1","input":{"command":command}}]}})];
        if let Some(output) = result {
            let mut record = json!({"type":"user","timestamp":LAST,"cwd":cwd,
                "message":{"content":[{"type":"tool_result","tool_use_id":"call-1","content":output,"is_error":error}]}});
            if structured {
                record["toolUseResult"] = json!({"gitOperation":{"commit":{"sha":self.sha(),"kind":"committed","branch":"main"}}});
            }
            records.push(record);
        }
        self.write(id, &records);
    }

    fn run(&self, rev: &str, extra: &[&str]) -> (Value, i32, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
            .args(["commit", rev, "--json", "--repo"])
            .arg(&self.repo)
            .args(extra)
            .env_clear()
            .env("HOME", self.home())
            .env("USERPROFILE", self.home())
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("AGENT_TRACES_NOW", NOW)
            .env("AGENT_TRACES_CACHE", self.home().join("cache"))
            .env("AGENT_TRACES_OUT", self.home().join("out"))
            .env("AGENT_TRACES_HOST", "fixture-host")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home().join("no-git-config"))
            .current_dir(self.home())
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        (
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}\n{stderr}")),
            output.status.code().unwrap(),
            stderr,
        )
    }
}

fn candidate<'a>(doc: &'a Value, id: &str) -> &'a Value {
    doc["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["session"]["id"] == id)
        .unwrap_or_else(|| panic!("missing {id}: {doc}"))
}

fn claim<'a>(doc: &'a Value, id: &str) -> &'a Value {
    &candidate(doc, id)["claims"][0]
}

#[test]
fn adversarial_cases_a_through_j_and_m() {
    let f = Fixture::new();
    let banner = format!("[main {}] synthetic", f.sha());
    // a: heredoc message, paired harness record, and a real repository resolution.
    f.call(
        "case-a",
        "git add -A && git commit -m \"$(cat <<'EOF'\nsynthetic message\nEOF\n)\"",
        Some(&banner),
        false,
        true,
        &f.repo,
        TIME,
    );
    // b: code-mode Codex exec, stored in a synthetic indexed rollout.
    let dir = f.home().join(".codex/sessions/2026/10/20");
    fs::create_dir_all(&dir).unwrap();
    let rollout = dir.join("rollout-case-b.jsonl");
    let records = [
        json!({"type":"session_meta","payload":{"id":"case-b","cwd":f.repo,"timestamp":TIME}}),
        json!({"type":"response_item","timestamp":TIME,"payload":{"type":"custom_tool_call","name":"exec","call_id":"exec-1","input":"const result = await tools.exec_command({cmd: \"git commit -m 'synthetic'\"});"}}),
        json!({"type":"response_item","timestamp":LAST,"payload":{"type":"custom_tool_call_output","call_id":"exec-1","output":format!("Process exited with code 0\n{banner}")}}),
    ];
    fs::write(
        &rollout,
        records
            .iter()
            .map(|v| v.to_string() + "\n")
            .collect::<String>(),
    )
    .unwrap();
    let db = rusqlite::Connection::open(f.home().join(".codex/state_5.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT, cwd TEXT, created_at INTEGER, updated_at INTEGER, rollout_path TEXT, source TEXT);").unwrap();
    let ts = DateTime::parse_from_rfc3339(TIME).unwrap().timestamp();
    db.execute(
        "INSERT INTO threads VALUES (?1,?2,?3,?4,?5,'cli')",
        rusqlite::params![
            "case-b",
            f.repo.to_str().unwrap(),
            ts,
            ts + 1,
            rollout.to_str().unwrap()
        ],
    )
    .unwrap();
    drop(db);
    // c,d: user and assistant forgeries, including structured-looking text.
    f.write("case-c",&[
        json!({"type":"user","cwd":f.repo,"timestamp":TIME,"message":{"content":banner}}),
        json!({"type":"assistant","cwd":f.repo,"timestamp":LAST,"message":{"content":[{"type":"text","text":banner}]}}),
    ]);
    f.write("case-d",&[json!({"type":"user","cwd":f.repo,"timestamp":TIME,"message":{"content":json!({"gitOperation":{"commit":{"sha":f.sha()}}}).to_string()}})]);
    // e: compound command with two banners cannot pretend to be a single result.
    f.call(
        "case-e",
        &format!("echo '{banner}' && git commit -m synthetic"),
        Some(&format!("{banner}\n{banner}")),
        false,
        false,
        &f.repo,
        TIME,
    );
    // f,g,h: failed calls, non-commit output and words inside an argument.
    f.call(
        "case-f",
        "git commit -m synthetic",
        Some(&banner),
        true,
        false,
        &f.repo,
        TIME,
    );
    f.call(
        "case-g-log",
        "git log --oneline",
        Some(&format!("{} synthetic", f.sha())),
        false,
        false,
        &f.repo,
        TIME,
    );
    f.call(
        "case-g-cat",
        "cat notes.txt",
        Some(&banner),
        false,
        false,
        &f.repo,
        TIME,
    );
    f.call(
        "case-h",
        "grep 'git commit' file",
        Some(&banner),
        false,
        false,
        &f.repo,
        TIME,
    );
    // i: no SHA in the source. Decision 10's time/repo supplement must find it.
    f.call(
        "case-i",
        "git commit -m synthetic",
        None,
        false,
        false,
        &f.repo,
        TIME,
    );
    f.call(
        "case-i-outside",
        "git commit -m synthetic",
        None,
        false,
        false,
        &f.repo,
        "2026-10-19T23:00:00Z",
    );
    // j: repository identity, not a matching token, determines repo_match.
    f.call(
        "case-j",
        "git commit -m synthetic",
        Some(&banner),
        false,
        false,
        &f.other,
        TIME,
    );
    // m: an ended session remains a factual address with its final event time.
    f.write("case-m",&[
        json!({"type":"user","cwd":f.repo,"timestamp":TIME,"message":{"content":f.sha()}}),
        json!({"type":"assistant","cwd":f.repo,"timestamp":LAST,"message":{"content":[{"type":"text","text":"finished"}]}}),
    ]);

    let (doc, code, stderr) = f.run(f.sha(), &[]);
    assert_eq!(code, 0, "{stderr}\n{doc}");
    assert_eq!(doc["schema"], "agent-traces.commit-evidence");
    assert_eq!(doc["version"], "1.0");
    assert_eq!(doc["commit"]["status"], "resolved");
    assert_eq!(doc["commit"]["oid"], f.oid);
    assert_eq!(doc["commit"]["candidates"], json!([f.oid]));
    assert_eq!(doc["complete"], true);
    assert_eq!(doc["authorship"], "not_inferred");
    assert_eq!(candidate(&doc, "case-a")["repo_match"], "same");
    let a = claim(&doc, "case-a");
    assert_eq!(a["claim"], "ran_commit");
    assert_eq!(a["strength"], "harness_record");
    assert_eq!(a["shell_form"], "single");
    assert_eq!(a["abbrev_unique"], true);
    assert_eq!(a["operation"], "committed");
    assert_eq!(a["branch"], "main");
    assert_eq!(a["sha_match"], "prefix");
    assert_eq!(a["source"]["selector"], "tool_use_id=call-1");
    assert!(Path::new(a["source"]["path"].as_str().unwrap()).is_absolute());
    let b = claim(&doc, "case-b");
    assert_eq!(b["claim"], "ran_commit");
    assert_eq!(b["strength"], "tool_output");
    assert_eq!(b["shell_form"], "single");
    assert_eq!(b["banner_count"], 1);
    for id in ["case-c", "case-d"] {
        assert_eq!(candidate(&doc, id)["best_claim"], "mentioned");
        assert!(
            candidate(&doc, id)["claims"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["strength"] == "text")
        );
    }
    let e = claim(&doc, "case-e");
    assert_eq!(e["claim"], "ran_commit");
    assert_eq!(e["strength"], "tool_output");
    assert_eq!(e["shell_form"], "compound");
    assert_eq!(e["banner_count"], 2);
    for id in ["case-f", "case-g-log", "case-g-cat", "case-h"] {
        assert_eq!(candidate(&doc, id)["best_claim"], "observed", "{id}");
    }
    let i = claim(&doc, "case-i");
    assert_eq!(i["claim"], "commit_without_result");
    assert!(i["recorded_sha"].is_null());
    assert!(i["banner_count"].is_null());
    assert!(
        !doc["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["session"]["id"] == "case-i-outside")
    );
    assert_eq!(candidate(&doc, "case-j")["repo_match"], "different");
    let m = &candidate(&doc, "case-m")["session"];
    assert_eq!(m["address"], "claude:case-m@fixture-host");
    assert_eq!(
        DateTime::parse_from_rfc3339(m["updated"].as_str().unwrap())
            .unwrap()
            .with_timezone(&Utc),
        DateTime::parse_from_rfc3339(LAST).unwrap()
    );
    assert!(m.get("ended").is_none());
    assert!(m.get("status").is_none());

    // Common filters apply, and a complete scan of leads alone exits 1.
    let (filtered, code, _) = f.run(f.sha(), &["--since", "2026-10-21"]);
    assert_eq!(code, 1);
    assert_eq!(filtered["candidates"], json!([]));
    // l: corrupt coverage takes precedence over an existing ran_commit claim.
    fs::write(f.home().join(".codex/state_5.sqlite"), b"not a database").unwrap();
    let (partial, code, _) = f.run(f.sha(), &[]);
    assert_eq!(code, 3);
    assert_eq!(partial["complete"], false);
    assert_eq!(candidate(&partial, "case-a")["best_claim"], "ran_commit");
}

#[test]
fn structured_record_requires_one_non_error_result_and_a_paired_call() {
    let f = Fixture::new();
    let banner = format!("[main {}] synthetic", f.sha());
    f.call(
        "error-record",
        "cat notes",
        Some(&banner),
        true,
        true,
        &f.repo,
        TIME,
    );
    f.write("unpaired-record", &[
        json!({"type":"user","timestamp":LAST,"cwd":f.repo,
            "toolUseResult":{"gitOperation":{"commit":{"sha":f.sha(),"kind":"committed"}}},
            "message":{"content":[{"type":"tool_result","tool_use_id":"missing","content":f.sha()}]}}),
    ]);
    let r = json!({"type":"user","timestamp":LAST,"cwd":f.repo,"toolUseResult":{"gitOperation":{"commit":{"sha":f.sha(),"kind":"committed"}}},
        "message":{"content":[{"type":"tool_result","tool_use_id":"call-1","content":f.sha()},{"type":"tool_result","tool_use_id":"call-2","content":f.sha()}]}});
    f.write("two-results",&[
        json!({"type":"assistant","timestamp":TIME,"cwd":f.repo,"message":{"content":[{"type":"tool_use","id":"call-1","name":"Bash","input":{"command":"git commit -m synthetic"}}]}}),r.clone()]);
    let (doc, code, _) = f.run(f.sha(), &[]);
    assert_eq!(code, 1);
    assert_eq!(candidate(&doc, "error-record")["best_claim"], "observed");
    assert_eq!(candidate(&doc, "unpaired-record")["best_claim"], "observed");
    assert!(
        candidate(&doc, "two-results")["claims"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["strength"] != "harness_record")
    );
}

#[test]
fn harness_record_accepts_a_powershell_call_with_unknown_shell_form() {
    let f = Fixture::new();
    f.write("powershell", &[
        json!({"type":"assistant","timestamp":TIME,"cwd":f.repo,
            "message":{"content":[{"type":"tool_use","id":"ps","name":"PowerShell",
                "input":{"command":"git -C $r commit -am $msg 2>&1 | Select-Object -First 1"}}]}}),
        json!({"type":"user","timestamp":LAST,"cwd":f.repo,
            "toolUseResult":{"gitOperation":{"commit":{"sha":f.sha(),"kind":"committed"}}},
            "message":{"content":[{"type":"tool_result","tool_use_id":"ps","content":"committed","is_error":false}]}}),
    ]);
    let (doc, code, stderr) = f.run(f.sha(), &[]);
    assert_eq!(code, 0, "{stderr}\n{doc}");
    let c = claim(&doc, "powershell");
    assert_eq!(c["claim"], "ran_commit");
    assert_eq!(c["strength"], "harness_record");
    assert_eq!(c["shell_form"], "unknown");
}

#[test]
fn missing_indexed_codex_transcripts_are_absent_coverage() {
    let f = Fixture::new();
    f.call(
        "valid-claude",
        "git commit -m x",
        Some(&format!("[main {}] synthetic", f.sha())),
        false,
        true,
        &f.repo,
        TIME,
    );
    let home = f.home().join(".codex");
    fs::create_dir_all(&home).unwrap();
    let rollout = home.join("present.jsonl");
    let records = [
        json!({"type":"session_meta","payload":{"id":"present","cwd":f.repo,"timestamp":TIME}}),
        json!({"type":"response_item","timestamp":TIME,"payload":{"type":"function_call","name":"exec_command","call_id":"c","arguments":"{\"cmd\":\"git commit -m synthetic\"}"}}),
        json!({"type":"response_item","timestamp":LAST,"payload":{"type":"function_call_output","call_id":"c","output":format!("Process exited with code 0\n[main {}] synthetic", f.sha())}}),
    ];
    fs::write(
        &rollout,
        records
            .iter()
            .map(|r| r.to_string() + "\n")
            .collect::<String>(),
    )
    .unwrap();
    let db = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
    // Schema from the Codex adapter's synthetic database tests.
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT,cwd TEXT,created_at INTEGER,updated_at INTEGER,rollout_path TEXT,source TEXT,agent_nickname TEXT,agent_role TEXT);CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT);").unwrap();
    let ts = DateTime::parse_from_rfc3339(TIME).unwrap().timestamp();
    for (id, path) in [
        ("present", rollout),
        ("missing", home.join("missing.jsonl")),
    ] {
        db.execute(
            "INSERT INTO threads VALUES(?1,?2,?3,?4,?5,'cli',NULL,NULL)",
            rusqlite::params![
                id,
                f.repo.to_str().unwrap(),
                ts,
                ts + 1,
                path.to_str().unwrap()
            ],
        )
        .unwrap();
    }
    drop(db);
    let (doc, code, stderr) = f.run(f.sha(), &[]);
    assert_eq!(code, 0, "{stderr}\n{doc}");
    assert_eq!(doc["complete"], true);
    let c = claim(&doc, "valid-claude");
    assert_eq!(c["claim"], "ran_commit");
    assert_eq!(c["strength"], "harness_record");
    assert_eq!(c["shell_form"], "single");
    assert_eq!(candidate(&doc, "valid-claude")["repo_match"], "same");
    assert_eq!(c["abbrev_unique"], true);
    assert_eq!(candidate(&doc, "present")["best_claim"], "ran_commit");
    // This absence makes the consumer rule in docs/commit-evidence.md ineligible for automatic selection, so keep it visible.
    let absent: Vec<_> = doc["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["harness"] == "codex" && c["status"] == "absent" && c["store"].is_null())
        .collect();
    assert_eq!(absent.len(), 1, "{doc}");
    assert_eq!(
        absent[0]["reason"],
        "1 indexed sessions have no transcript on disk"
    );
    assert!(stderr.is_empty(), "{stderr}");
    let (doc, code, stderr) = f.run("0000000", &[]);
    assert_eq!(code, 1, "{stderr}\n{doc}");
    assert_eq!(doc["complete"], true);
}

#[test]
fn harness_scope_omits_absent_unselected_stores() {
    let f = Fixture::new();
    f.call(
        "claude",
        "git commit -m x",
        Some(&format!("[main {}] synthetic", f.sha())),
        false,
        true,
        &f.repo,
        TIME,
    );
    let home = f.home().join(".codex");
    fs::create_dir_all(&home).unwrap();
    let db = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT,cwd TEXT,created_at INTEGER,updated_at INTEGER,rollout_path TEXT,source TEXT,agent_nickname TEXT,agent_role TEXT);CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT);").unwrap();
    drop(db);
    let (all, code, stderr) = f.run(f.sha(), &[]);
    assert_eq!(code, 0, "{stderr}\n{all}");
    let (scoped, code, stderr) = f.run(f.sha(), &["--harness", "claude,codex"]);
    assert_eq!(code, 0, "{stderr}\n{scoped}");
    // An absent store blocks the consumer rule unless scoped out: a missing store and an uninstalled harness are indistinguishable (docs/commit-evidence.md).
    for harness in ["grok", "opencode", "t3"] {
        assert!(
            all["coverage"].as_array().unwrap().iter().any(|c| {
                c["harness"] == harness
                    && c["status"] == "absent"
                    && c["reason"] == "store not found"
            }),
            "{all}"
        );
        assert!(
            scoped["coverage"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["harness"] != harness),
            "{scoped}"
        );
    }
}

#[test]
fn unchecked_missing_revision_and_invalid_arguments() {
    let f = Fixture::new();
    f.write(
        "mention",
        &[json!({"type":"user","cwd":f.repo,"timestamp":TIME,"message":{"content":f.sha()}})],
    );
    let (doc, code, _) = f.run("0000000", &[]);
    assert_eq!(code, 1);
    assert_eq!(doc["commit"]["status"], "not_found");
    assert!(doc["commit"]["oid"].is_null());
    for extra in [
        vec!["commit", "not-a-sha", "--json"],
        vec!["commit", f.sha(), "--json", "--schema-version", "1"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
            .args(extra)
            .env_clear()
            .env("HOME", f.home())
            .env("USERPROFILE", f.home())
            .current_dir(f.home())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .args(["commit", f.sha(), "--json"])
        .env_clear()
        .env("HOME", f.home())
        .env("USERPROFILE", f.home())
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("AGENT_TRACES_CACHE", f.home().join("cache"))
        .env("AGENT_TRACES_HOST", "fixture-host")
        .current_dir(f.home())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["commit"]["status"], "unchecked");
    assert!(doc["query"]["repo"].is_null());
    assert_eq!(candidate(&doc, "mention")["repo_match"], "unknown");
    assert!(claim(&doc, "mention")["abbrev_unique"].is_null());
}

#[test]
fn provider_and_t3_sources_merge_under_the_thread_address() {
    let f = Fixture::new();
    f.call(
        "native",
        "git commit -m synthetic",
        Some(&format!("[main {}] synthetic", f.sha())),
        false,
        true,
        &f.repo,
        TIME,
    );
    let dir = f.home().join(".t3/userdata");
    fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("state.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL;
CREATE TABLE projection_threads(thread_id TEXT,title TEXT,branch TEXT,worktree_path TEXT,created_at TEXT,updated_at TEXT,latest_user_message_at TEXT,model_selection_json TEXT,archived_at TEXT,deleted_at TEXT,project_id TEXT);
CREATE TABLE projection_projects(project_id TEXT,workspace_root TEXT,title TEXT);
CREATE TABLE provider_session_runtime(thread_id TEXT,provider_name TEXT,provider_instance_id TEXT,resume_cursor_json TEXT,status TEXT);
CREATE TABLE projection_thread_messages(thread_id TEXT,role TEXT,text TEXT,created_at TEXT);
CREATE TABLE projection_thread_activities(thread_id TEXT,kind TEXT,summary TEXT,payload_json TEXT,created_at TEXT);
INSERT INTO provider_session_runtime VALUES('thread','claude',NULL,'{\"resume\":\"native\"}','ended');").unwrap();
    db.execute("INSERT INTO projection_threads VALUES('thread','Synthetic','main',?1,?2,?2,?2,'{}',NULL,NULL,'p')",rusqlite::params![f.repo.to_str().unwrap(),TIME]).unwrap();
    db.execute(
        "INSERT INTO projection_thread_messages VALUES('thread','user',?1,?2)",
        rusqlite::params![f.sha(), TIME],
    )
    .unwrap();
    drop(db);
    let (doc, code, stderr) = f.run(f.sha(), &[]);
    assert_eq!(code, 0, "{stderr}");
    let candidates = doc["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1);
    let c = &candidates[0];
    assert_eq!(c["session"]["address"], "t3:thread@fixture-host");
    assert_eq!(
        c["session"]["provider_address"],
        "claude:native@fixture-host"
    );
    assert_eq!(c["best_claim"], "ran_commit");
    assert_eq!(c["claims"].as_array().unwrap().len(), 2);
    assert!(c["claims"].as_array().unwrap().iter().any(|claim| {
        claim["source"]["path"]
            .as_str()
            .unwrap()
            .ends_with("state.sqlite")
    }));
}

#[test]
fn a_forged_harness_record_from_a_compound_command_stays_compound() {
    // Claude Code derives gitOperation from output, so an echoed banner beside a
    // git commit call yields a harness record. The fact is kept, labelled
    // compound, which the consumer rule in docs/commit-evidence.md rejects.
    let f = Fixture::new();
    let banner = format!("[main {}] forged", f.sha());
    f.call(
        "forged-record",
        &format!("echo \"{banner}\" && git commit --dry-run -m synthetic"),
        Some(&banner),
        false,
        true,
        &f.repo,
        TIME,
    );
    let (doc, code, stderr) = f.run(f.sha(), &[]);
    assert_eq!(code, 0, "{stderr}\n{doc}");
    let c = claim(&doc, "forged-record");
    assert_eq!(c["claim"], "ran_commit");
    assert_eq!(c["strength"], "harness_record");
    assert_eq!(c["shell_form"], "compound");
    assert_eq!(c["banner_count"], 1);
}
