use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};

struct Fixture {
    home: tempfile::TempDir,
    _writer: Connection,
    db: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".t3/userdata");
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("statev2.sqlite");
        let con = Connection::open(&db).unwrap();
        con.execute_batch(r#"PRAGMA journal_mode=WAL;
            CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT PRIMARY KEY,title TEXT,project_id TEXT,created_at TEXT,updated_at TEXT,archived_at TEXT,deleted_at TEXT,payload_json TEXT,provider_instance_id TEXT,runtime_mode TEXT,interaction_mode TEXT);
            CREATE TABLE orchestration_v2_projection_runs(run_id TEXT PRIMARY KEY,thread_id TEXT,ordinal INTEGER,status TEXT);
            CREATE TABLE orchestration_events(sequence INTEGER PRIMARY KEY,occurred_at TEXT,event_type TEXT,aggregate_kind TEXT,stream_id TEXT,actor_kind TEXT,payload_json TEXT,metadata_json TEXT);
            INSERT INTO orchestration_v2_projection_runs VALUES ('z-old','thread:a',1,'running'),('a-new','thread:a',3,'completed'),('middle','thread:a',2,'failed');
        "#).unwrap();
        for (id, updated, archived, deleted, payload) in [
            (
                "thread:a",
                "2026-10-02T00:00:00Z",
                None,
                None,
                json!({"createdBy":"client","creationSource":"manual","settledAt":"2026-10-02T00:00:00Z","pullRequests":[{"number":42}],"branch":"feature","worktreePath":"/work","pinnedAt":"pin","unsettledAt":"previous","snoozedUntil":"later","forkedFrom":"parent"}),
            ),
            (
                "thread:b",
                "2026-10-03T00:00:00Z",
                Some("archive"),
                None,
                json!({"creationSource":"automation"}),
            ),
            (
                "thread:c",
                "2026-10-04T00:00:00Z",
                None,
                Some("deleted"),
                json!({"createdBy":"client"}),
            ),
        ] {
            con.execute("INSERT INTO orchestration_v2_projection_threads VALUES (?, 'Title', 'project', '2026-10-01T00:00:00Z', ?, ?, ?, ?, 'provider', 'worktree', 'default')",params![id,updated,archived,deleted,payload.to_string()]).unwrap();
        }
        for seq in 1..=205 {
            let (kind, actor, stream, aggregate, time) = match seq {
                1 => (
                    "thread.settled",
                    "client",
                    "thread:a",
                    "thread",
                    "2026-10-01T23:00:00Z",
                ),
                2 => (
                    "thread.created",
                    "server",
                    "thread:a",
                    "thread",
                    "2026-10-02T00:00:00Z",
                ),
                3 => (
                    "run.created",
                    "client",
                    "run:a",
                    "run",
                    "2026-10-02T01:00:00Z",
                ),
                4 => (
                    "thread.settled",
                    "client",
                    "thread:b",
                    "thread",
                    "2026-10-02T01:00:00Z",
                ),
                5 => (
                    "threadXcreated",
                    "server",
                    "missing",
                    "thread",
                    "2026-10-02T01:00:00Z",
                ),
                _ => (
                    "message.updated",
                    "server",
                    "thread:a",
                    "thread",
                    "2026-10-02T02:00:00Z",
                ),
            };
            let payload = if seq == 1 {
                json!({"nested":[{"secret":format!("sk-{}","a".repeat(30))}],"text":"private payload marker"})
            } else if seq == 2 {
                json!({"large":"界".repeat(2100)})
            } else {
                json!({"text":"private payload marker"})
            };
            con.execute(
                "INSERT INTO orchestration_events VALUES (?,?,?,?,?,?,?,'private metadata marker')",
                params![
                    seq,
                    time,
                    kind,
                    aggregate,
                    stream,
                    actor,
                    payload.to_string()
                ],
            )
            .unwrap();
        }
        Self {
            home,
            _writer: con,
            db,
        }
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        let wal = self.db.with_file_name("statev2.sqlite-wal");
        let before = fs::read(&self.db).unwrap();
        let modified = fs::metadata(&self.db).unwrap().modified().unwrap();
        let wal_before = fs::read(&wal).unwrap();
        let wal_modified = fs::metadata(&wal).unwrap().modified().unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
            .arg("t3")
            .args(args)
            .env_clear()
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("AGENT_TRACES_CACHE", self.home.path().join("cache"))
            .output()
            .unwrap();
        assert_eq!(
            fs::read(&self.db).unwrap(),
            before,
            "DB bytes changed: {args:?}"
        );
        assert_eq!(
            fs::metadata(&self.db).unwrap().modified().unwrap(),
            modified,
            "DB mtime changed: {args:?}"
        );
        assert_eq!(
            fs::read(&wal).unwrap(),
            wal_before,
            "WAL bytes changed: {args:?}"
        );
        assert_eq!(
            fs::metadata(&wal).unwrap().modified().unwrap(),
            wal_modified,
            "WAL mtime changed: {args:?}"
        );
        out
    }
    fn value(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}
#[test]
fn threads_metadata_filters_counts_and_order() {
    let f = Fixture::new();
    let rows = f.value(&["threads", "--json"]);
    assert_eq!(rows["threads"].as_array().unwrap().len(), 2);
    assert_eq!(rows["threads"][0]["thread_id"], "thread:b");
    let a = &rows["threads"][1];
    assert_eq!(
        a["latest_run"],
        json!({"run_id":"a-new","status":"completed","ordinal":3})
    );
    assert!(rows["threads"][0]["latest_run"].is_null());
    assert!(a.get("deleted_at").is_none());
    assert_eq!(a["link"], "t3-thread://v1/thread:a");
    assert_eq!(a["pull_requests"], json!([{"number":42}]));
    assert_eq!(a["worktree_path"], "/work");
    assert_eq!(a["forked_from"], "parent");
    assert_eq!(a["runtime_mode"], "worktree");
    assert_eq!(f.value(&["threads"]), rows);
    let all = f.value(&["threads", "--include-deleted", "--json"]);
    assert_eq!(all["threads"].as_array().unwrap().len(), 3);
    assert_eq!(all["threads"][0]["deleted_at"], "deleted");
    for (flag, value, id) in [
        ("--source", Some("manual"), "thread:a"),
        ("--created-by", Some("client"), "thread:a"),
        ("--settled", None, "thread:a"),
        ("--unsettled", None, "thread:b"),
        ("--archived", None, "thread:b"),
        ("--active", None, "thread:a"),
        ("--since", Some("2026-10-03T00:00:00Z"), "thread:b"),
    ] {
        let mut args = vec!["threads", flag];
        if let Some(value) = value {
            args.push(value);
        }
        let v = f.value(&args);
        assert_eq!(v["threads"].as_array().unwrap().len(), 1, "{flag}");
        assert_eq!(v["threads"][0]["thread_id"], id);
    }
    assert_eq!(
        f.value(&["threads", "--project", "other"])["threads"],
        json!([])
    );
    assert_eq!(
        f.value(&["threads", "--limit", "1"])["threads"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(f.value(&["threads", "--limit", "0"])["threads"], json!([]));
    assert_eq!(
        f.value(&["threads", "--count-by", "created_by", "--json"]),
        json!({"count_by":"created_by","total":2,"counts":[{"value":null,"count":1},{"value":"client","count":1}]})
    );
    for (key, values) in [
        ("status", vec!["completed", "idle"]),
        ("settled", vec!["settled", "unsettled"]),
        ("archived", vec!["active", "archived"]),
    ] {
        let v = f.value(&["threads", "--count-by", key]);
        assert_eq!(
            v["counts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["value"].as_str().unwrap())
                .collect::<Vec<_>>(),
            values
        );
    }
    assert_eq!(
        f.value(&["threads", "--limit", "1", "--count-by", "project_id"])["total"],
        1
    );
}
#[test]
fn events_filters_paging_and_sql_counts() {
    let f = Fixture::new();
    let events = |args: &[&str]| -> Value {
        let mut cmd = vec!["events"];
        cmd.extend(args);
        f.value(&cmd)["events"].clone()
    };
    assert_eq!(events(&[]).as_array().unwrap().len(), 200);
    assert_eq!(events(&["--limit", "0"]).as_array().unwrap().len(), 205);
    assert_eq!(events(&["--type", "thread_*"]), json!([]));
    assert_eq!(events(&["--type", "thread%*"]), json!([]));
    let exact = events(&["--type", "thread.settled"]);
    assert_eq!(exact.as_array().unwrap().len(), 2);
    assert_eq!(
        exact[0]["thread"],
        json!({"title":"Title","created_by":"client","creation_source":"manual","project_id":"project"})
    );
    assert_eq!(events(&["--type", "thread.*"]).as_array().unwrap().len(), 3);
    assert_eq!(
        events(&["--type", "thread.settled", "--type", "run.created"])
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(events(&["--actor", "client"]).as_array().unwrap().len(), 3);
    assert_eq!(
        events(&["--thread", "a", "--actor", "client"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        events(&["--thread", "t3:thread:a", "--actor", "client"])[0]["sequence"],
        1
    );
    assert_eq!(events(&["--kind", "run"])[0]["sequence"], 3);
    assert!(events(&["--kind", "run"])[0].get("thread").is_none());
    let bounded = events(&[
        "--since",
        "2026-10-02T00:00:00Z",
        "--until",
        "2026-10-02T01:00:00Z",
    ]);
    assert_eq!(bounded.as_array().unwrap().len(), 4);
    assert_eq!(bounded[0]["sequence"], 2);
    assert_eq!(bounded[3]["sequence"], 5);
    assert_eq!(
        events(&[
            "--since",
            "2026-10-01T17:00:00-07:00",
            "--until",
            "2026-10-01T17:00:00-07:00"
        ])[0]["sequence"],
        2
    );

    assert_eq!(events(&["--after-seq", "200"])[0]["sequence"], 201);
    assert_eq!(events(&["--after-seq", "200"]).as_array().unwrap().len(), 5);
    let counts = f.value(&[
        "events",
        "--type",
        "thread.*",
        "--count-by",
        "type",
        "--limit",
        "1",
        "--json",
    ]);
    assert_eq!(
        counts,
        json!({"count_by":"type","total":3,"counts":[{"value":"thread.settled","count":2},{"value":"thread.created","count":1}]})
    );
    assert_eq!(
        f.value(&["events", "--count-by", "day"]),
        json!({"count_by":"day","total":205,"counts":[{"value":"2026-10-02","count":204},{"value":"2026-10-01","count":1}]})
    );
    assert_eq!(
        f.value(&[
            "events",
            "--actor",
            "client",
            "--count-by",
            "creation_source"
        ]),
        json!({"count_by":"creation_source","total":3,"counts":[{"value":null,"count":1},{"value":"automation","count":1},{"value":"manual","count":1}]})
    );
    assert_eq!(
        f.value(&["events", "--count-by", "type", "--after-seq", "203"])["total"],
        2
    );
}
#[test]
fn payload_is_opt_in_redacted_and_bounded() {
    let f = Fixture::new();
    let out = f.run(&["events", "--json"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains("payload"));
    assert!(!text.contains("private"));
    let v = f.value(&["events", "--payload", "--limit", "2", "--json"]);
    assert_eq!(
        v["events"][0]["payload"]["nested"][0]["secret"],
        "<redacted>"
    );
    assert_eq!(
        v["events"][1]["payload"],
        json!({"truncated":true,"bytes":json!({"large":"界".repeat(2100)}).to_string().len()})
    );
    assert!(!v.to_string().contains("metadata"));
}
#[test]
fn clap_rejects_unknown_keys_and_conflicting_filters() {
    let f = Fixture::new();
    for args in [
        vec!["threads", "--count-by", "unknown"],
        vec!["events", "--count-by", "unknown"],
        vec!["threads", "--settled", "--unsettled"],
        vec!["threads", "--active", "--archived"],
        vec!["events", "--actor", "unknown"],
    ] {
        let out = f.run(&args);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
    }
}
#[test]
fn queries_require_a_v2_store() {
    let home = tempfile::tempdir().unwrap();
    for command in ["threads", "events"] {
        let out = Command::new(env!("CARGO_BIN_EXE_agent-traces"))
            .args(["t3", command, "--json"])
            .env_clear()
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("T3 v2 store not found"));
    }
}
fn stdout(f: &Fixture, args: &[&str]) -> String {
    let out = f.run(args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
#[test]
fn events_fields_print_event_thread_and_payload_values() {
    let f = Fixture::new();
    f._writer
        .execute_batch(
            "UPDATE orchestration_v2_projection_threads SET title='SPRAWL probe A' WHERE thread_id='thread:a';
             UPDATE orchestration_v2_projection_threads SET title='Other' WHERE thread_id='thread:b';",
        )
        .unwrap();
    // One tab-separated line per event; payload paths read the payload without
    // --payload, index arrays by number, stay redacted, and print empty when absent.
    assert_eq!(
        stdout(
            &f,
            &[
                "events",
                "--type",
                "thread.settled",
                "--fields",
                "sequence,stream_id,thread.title,payload.nested.0.secret,payload.text",
            ],
        ),
        "1\tthread:a\tSPRAWL probe A\t<redacted>\tprivate payload marker\n\
         4\tthread:b\tOther\t\tprivate payload marker\n"
    );
    // A selected payload field is not cut by the 2000-character payload bound.
    assert_eq!(
        stdout(
            &f,
            &[
                "events",
                "--type",
                "thread.created",
                "--fields",
                "sequence,payload.large"
            ],
        ),
        format!("2\t{}\n", "界".repeat(2100))
    );
    // --json prints one object per event, keyed by the field names given.
    let lines: Vec<Value> = stdout(
        &f,
        &[
            "events",
            "--limit",
            "2",
            "--fields",
            "sequence,payload.text",
            "--json",
        ],
    )
    .lines()
    .map(|l| serde_json::from_str(l).unwrap())
    .collect();
    assert_eq!(
        lines,
        [
            json!({"sequence":1,"payload.text":"private payload marker"}),
            json!({"sequence":2,"payload.text":null}),
        ]
    );
    let out = f.run(&["events", "--fields", "bogus"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown field \"bogus\""));
    assert!(
        !f.run(&["events", "--fields", "sequence", "--count-by", "type"])
            .status
            .success()
    );
}
#[test]
fn threads_title_filter_and_fields() {
    let f = Fixture::new();
    f._writer
        .execute_batch(
            "UPDATE orchestration_v2_projection_threads SET title='SPRAWL probe A' WHERE thread_id='thread:a';
             UPDATE orchestration_v2_projection_threads SET title='Other' WHERE thread_id='thread:b';
             UPDATE orchestration_v2_projection_threads SET title='sprawl deleted' WHERE thread_id='thread:c';",
        )
        .unwrap();
    // --title is a case-insensitive substring; deleted threads stay out by default.
    assert_eq!(
        stdout(
            &f,
            &[
                "threads",
                "--title",
                "sprawl",
                "--fields",
                "thread_id,title,latest_run.status,created_by,pull_requests.0.number",
            ],
        ),
        "thread:a\tSPRAWL probe A\tcompleted\tclient\t42\n"
    );
    // Repeating --title keeps a thread matching any of them, newest first.
    assert_eq!(
        stdout(
            &f,
            &[
                "threads",
                "--title",
                "sprawl",
                "--title",
                "oth",
                "--fields",
                "thread_id"
            ],
        ),
        "thread:b\nthread:a\n"
    );
    // The filter applies to counts too.
    assert_eq!(
        f.value(&[
            "threads",
            "--title",
            "sprawl",
            "--count-by",
            "created_by",
            "--json"
        ])["total"],
        1
    );
    let line: Value = serde_json::from_str(
        stdout(
            &f,
            &[
                "threads",
                "--title",
                "probe",
                "--fields",
                "thread_id,settled_at",
                "--json",
            ],
        )
        .trim(),
    )
    .unwrap();
    assert_eq!(
        line,
        json!({"thread_id":"thread:a","settled_at":"2026-10-02T00:00:00Z"})
    );
    let out = f.run(&["threads", "--fields", "bogus"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown field \"bogus\""));
    assert!(
        !f.run(&["threads", "--fields", "thread_id", "--count-by", "status"])
            .status
            .success()
    );
}
#[test]
fn threads_title_folds_unicode_case() {
    let f = Fixture::new();
    f._writer
        .execute(
            "UPDATE orchestration_v2_projection_threads SET title='ÉCOLE probe' WHERE thread_id='thread:a'",
            [],
        )
        .unwrap();
    for needle in ["école", "ÉCOLE", "Probe"] {
        assert_eq!(
            stdout(&f, &["threads", "--title", needle, "--fields", "thread_id"]),
            "thread:a\n",
            "{needle}"
        );
    }
}
