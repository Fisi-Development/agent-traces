//! `find` ranks a hyphenated name by whole-name mentions. An orchestrator
//! session that names `impl-k7-latch-r1` in many short notifications must not
//! outrank the session whose task is `contracts/k7-latch.md`. Runs the binary
//! against a synthetic store with a cleared environment, so no real store is read.
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

const NOW: &str = "2026-10-17T12:00:00Z";
const EXECUTOR: &str = "bbbbbbbb-0000-4000-8000-000000000001";
const ORCHESTRATOR: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const EMBEDDED_ONLY: &str = "bbbbbbbb-0000-4000-8000-000000000003";

fn write(home: &Path, id: &str, prompts: &[String]) {
    let dir = home.join(".claude/projects/w");
    fs::create_dir_all(&dir).unwrap();
    let body = prompts
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let ts = format!("2026-10-15T10:{i:02}:00Z");
            json!({"type": "user", "cwd": "/w", "timestamp": ts, "message": {"content": text}})
                .to_string()
                + "\n"
        })
        .collect::<String>();
    fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
}

fn store() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    write(
        home,
        EXECUTOR,
        &[
            "Act as the executor on a closed contract. Read contracts/k7-latch.md and do exactly \
             what it says. Work only in the worktree it names, commit when green, and report as \
             its done section asks."
                .into(),
            "Carry on with the remaining tests.".into(),
        ],
    );
    let mut orchestrator = vec![
        "Plan the broker work and dispatch each contract to an executor.".to_owned(),
        format!(
            "Summary so far. Contracts: k7-latch, k8-gate, k9-vault, r1-fixes. {}",
            "The reviewer read each diff against its contract and reran the checks. ".repeat(8)
        ),
    ];
    // A long session: idf comes from the messages of matching sessions only, so
    // filler keeps the name as rare as it is in a real orchestrator transcript.
    for step in 0..150 {
        orchestrator.push(format!(
            "Continue with step {step} of the plan and report back."
        ));
    }
    for round in 1..=5 {
        orchestrator.push(format!(
            "Task delegate-task:impl-k7-latch-r{round} and delegate-task:impl-k7-latch-r{} \
             reached a terminal state.",
            round + 1
        ));
    }
    write(home, ORCHESTRATOR, &orchestrator);
    write(
        home,
        EMBEDDED_ONLY,
        &["Summarize what impl-k7-latch-r1 changed.".into()],
    );
    t
}

fn find(home: &Path, terms: &[&str]) -> Vec<(String, f64)> {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
        .arg("find")
        .args(terms)
        .args(["--json", "--hits", "0"])
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_TRACES_NOW", NOW)
        .env("AGENT_TRACES_CACHE", home.join("cache"))
        .env("AGENT_TRACES_OUT", home.join("out"))
        .env("TZ", "UTC")
        .output()
        .unwrap();
    assert!(out.status.success(), "{terms:?}: {out:?}");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| {
            let row: Value = serde_json::from_str(l).unwrap();
            (
                row["id"].as_str().unwrap().to_owned(),
                row["_score"].as_f64().unwrap(),
            )
        })
        .collect()
}

fn ids(rows: &[(String, f64)]) -> Vec<&str> {
    rows.iter().map(|(id, _)| id.as_str()).collect()
}

#[test]
fn a_whole_hyphenated_name_outranks_it_embedded_in_longer_names() {
    let t = store();
    let rows = find(t.path(), &["k7-latch"]);
    let ids = ids(&rows);
    assert_eq!(ids.first(), Some(&EXECUTOR), "{rows:?}");
    // Embedded mentions still match; they only rank lower.
    assert!(ids.contains(&EMBEDDED_ONLY), "{rows:?}");
    assert!(ids.contains(&ORCHESTRATOR), "{rows:?}");
}

#[test]
fn plain_terms_count_the_same_inside_a_longer_name() {
    let t = tempfile::tempdir().unwrap();
    let standalone = "dddddddd-0000-4000-8000-000000000001";
    let embedded = "dddddddd-0000-4000-8000-000000000002";
    write(t.path(), standalone, &["Check the latch spring.".into()]);
    write(t.path(), embedded, &["Check the k7-latch spring.".into()]);
    for terms in [&["latch"][..], &["spring", "latch"]] {
        let rows = find(t.path(), terms);
        assert_eq!(rows.len(), 2, "{terms:?}: {rows:?}");
        assert_eq!(rows[0].1, rows[1].1, "{terms:?}: {rows:?}");
    }
    let rows = find(t.path(), &["k7-latch"]);
    assert_eq!(ids(&rows), [embedded], "{rows:?}");
}
