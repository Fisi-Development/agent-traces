use crate::{
    adapters::StoreAdapter,
    context::Context,
    model::{Event, Session},
    storage::{jsonl, sqlite as db},
    util::{one_line, opt_s, parse_ts, python_json, s},
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
pub struct Codex;
fn dirs(path: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}
/// Two Codex homes can index the same thread and transcript (a copied or
/// migrated state_5.sqlite). Keeps one row per id and transcript, preferring
/// the home that holds the file; different transcripts under one id stay.
fn one_row_per_transcript(ctx: &Context, rows: Vec<Session>) -> Vec<Session> {
    // One spelling per file: expanded, canonical, and case-folded on Windows,
    // used for both the transcript key and the home that owns it.
    let norm = |raw: &str| {
        let p = ctx.expand(raw);
        let p = std::fs::canonicalize(&p)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned();
        if cfg!(windows) { p.to_lowercase() } else { p }
    };
    let key = |r: &Session| {
        let path = s(r, "path");
        (
            s(r, "id").to_owned(),
            if path.is_empty() {
                String::new()
            } else {
                norm(path)
            },
        )
    };
    let owns = |r: &Session, path: &str| Path::new(path).starts_with(norm(s(r, "home")));
    let mut out: Vec<Session> = Vec::new();
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    for row in rows {
        let k = key(&row);
        if k.1.is_empty() {
            out.push(row);
            continue;
        }
        match seen.get(&k) {
            Some(&i) => {
                if !owns(&out[i], &k.1) && owns(&row, &k.1) {
                    out[i] = row;
                }
            }
            None => {
                seen.insert(k, out.len());
                out.push(row);
            }
        }
    }
    out
}
impl Codex {
    fn rows(
        &self,
        ctx: &Context,
        home: &Path,
        filter: &str,
        args: &[Value],
    ) -> Result<Vec<Session>> {
        let path = home.join("state_5.sqlite");
        if !path.exists() {
            return Ok(vec![]);
        }
        let con = match db::open(ctx, "codex", &path) {
            Ok(c) => c,
            Err(_) => return Ok(vec![]),
        };
        let result = (|| -> Result<Vec<Session>> {
            let rows = db::query(&con, &format!("select * from threads {filter}"), args)?;
            let mut parents = HashMap::new();
            if db::table_exists(&con, "thread_spawn_edges")? {
                for r in db::query(
                    &con,
                    "select parent_thread_id,child_thread_id from thread_spawn_edges",
                    &[],
                )? {
                    parents.insert(
                        s(&r, "child_thread_id").to_owned(),
                        r["parent_thread_id"].clone(),
                    );
                }
            }
            Ok(rows.iter().map(|r| {
                let parent=parents.get(s(r,"id")).cloned().unwrap_or(Value::Null);let src=s(r,"source");let sub=db::truth(&parent)||(!r["thread_source"].is_null()&&!matches!(s(r,"thread_source"),""|"user"))||src.contains("subagent");
                let mut name=r["name"].clone();if !db::truth(&name)&&sub&&db::truth(&r["agent_nickname"]) {name=json!(format!("{} ({})",s(r,"agent_nickname"),if s(r,"agent_role").is_empty(){"subagent"}else{s(r,"agent_role")}));}
                json!({"harness":"codex","home":ctx.tilde(home),"id":r["id"],"parent":parent,"kind":if sub{"subagent"}else if src=="exec"{"worker"}else{"main"},"cwd":r["cwd"],"branch":r["git_branch"],"title":if db::truth(&name){name}else{Value::Null},"first_prompt":if r.get("first_user_message").is_some(){json!(s(r,"first_user_message").chars().take(600).collect::<String>())}else{Value::Null},"started":db::stamp(&r["created_at"]),"updated":db::stamp(&r["updated_at"]),"model":r["model"],"path":r["rollout_path"],"archived":db::truth(&r["archived"]),"originator":r["originator"],"nickname":r["agent_nickname"]})
            }).collect())
        })();
        match result {
            Ok(r) => {
                ctx.cover("codex", Some(&path), "sessions", "read", None);
                Ok(r)
            }
            Err(e) => {
                db::report(ctx, "codex", &path, &e);
                Ok(vec![])
            }
        }
    }
    fn from_rollout(ctx: &Context, home: &Path, path: &Path) -> Session {
        let rec = jsonl::records(ctx, path, 0, None)
            .next()
            .map(|(_, v)| v)
            .unwrap_or(Value::Null);
        let meta = if s(&rec, "type") == "session_meta" {
            rec["payload"].clone()
        } else {
            Value::Null
        };
        let filename = path.file_name().unwrap_or_default().to_string_lossy();
        let stem = filename.strip_suffix(".zst").unwrap_or(&filename);
        let fallback = stem
            .chars()
            .rev()
            .skip(6)
            .take(35)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        json!({"harness":"codex","home":ctx.tilde(home),"id":if db::truth(&meta["id"]){meta["id"].clone()}else{json!(fallback)},"parent":meta["parent_thread_id"],"kind":"unindexed","cwd":meta["cwd"],"title":null,"first_prompt":null,"started":meta["timestamp"],"updated":null,"model":null,"path":path.to_string_lossy(),"originator":meta["originator"]})
    }
}
impl StoreAdapter for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn homes(&self, ctx: &Context) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(h) = std::env::var_os("CODEX_HOME").filter(|s| !s.is_empty()) {
            candidates.push(PathBuf::from(h));
        }
        candidates.push(ctx.home.join(".codex"));
        let roots = dirs(&ctx.home)
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .starts_with(".codex")
            })
            .collect::<Vec<_>>();
        candidates.extend(roots.clone());
        for root in roots {
            candidates.extend(dirs(&root));
        }
        let settings = ctx.t3_settings();
        if let Some(instances) = settings["providerInstances"].as_object() {
            for v in instances.values() {
                if s(v, "driver") == "codex" {
                    let home = s(&v["config"], "homePath").trim();
                    if !home.is_empty() {
                        candidates.push(ctx.expand(home));
                    }
                }
            }
        }
        let mut seen = HashSet::new();
        candidates
            .into_iter()
            .filter(|p| {
                let index = p.join("state_5.sqlite");
                let key = if index.exists() {
                    index
                } else {
                    let sessions = p.join("sessions");
                    if !dirs(&sessions).iter().any(|p| {
                        let n = p.file_name().unwrap_or_default().to_string_lossy();
                        n.len() == 4 && n.starts_with("20") && n.chars().all(|c| c.is_ascii_digit())
                    }) {
                        return false;
                    }
                    sessions
                };
                let resolved = std::fs::canonicalize(&key)
                    .unwrap_or(key)
                    .to_string_lossy()
                    .into_owned();
                seen.insert(if cfg!(windows) {
                    resolved.to_lowercase()
                } else {
                    resolved
                })
            })
            .collect()
    }
    fn sessions(
        &self,
        ctx: &Context,
        since: Option<DateTime<Utc>>,
        subagents: bool,
    ) -> Result<Vec<Session>> {
        let mut out = Vec::new();
        for home in self.homes(ctx) {
            out.extend(if let Some(t) = since {
                self.rows(ctx, &home, "where updated_at >= ?", &[json!(t.timestamp())])?
            } else {
                self.rows(ctx, &home, "", &[])?
            });
        }
        if !subagents {
            out.retain(|r| s(r, "kind") != "subagent");
        }
        Ok(one_row_per_transcript(ctx, out))
    }
    fn resolve(&self, ctx: &Context, id: &str) -> Result<Vec<Session>> {
        let mut out = Vec::new();
        for home in self.homes(ctx) {
            let mut rows = self.rows(ctx, &home, "where id like ?", &[json!(format!("{id}%"))])?;
            if rows.is_empty() {
                for (root, depth) in [
                    (home.join("sessions"), 4),
                    (home.join("archived_sessions"), 1),
                ] {
                    for entry in walkdir::WalkDir::new(root)
                        .min_depth(depth)
                        .max_depth(depth)
                        .follow_links(true)
                        .into_iter()
                        .filter_map(std::result::Result::ok)
                    {
                        let name = entry.file_name().to_string_lossy();
                        if entry.file_type().is_file()
                            && name.starts_with("rollout-")
                            && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
                            && name[8..].contains(id)
                        {
                            rows.push(Self::from_rollout(ctx, &home, entry.path()));
                        }
                    }
                }
            }
            out.extend(rows);
        }
        Ok(one_row_per_transcript(ctx, out))
    }
    fn events<'a>(
        &self,
        ctx: &'a Context,
        row: &'a Session,
        needles: Option<&[String]>,
    ) -> Result<Box<dyn Iterator<Item = Event> + 'a>> {
        let path = Path::new(s(row, "path"));
        Ok(Box::new(
            jsonl::records(ctx, path, 0, needles).filter_map(|(_, rec)| codex_event(&rec)),
        ))
    }
    fn children(&self, ctx: &Context, row: &Session) -> Result<Vec<Session>> {
        let home = ctx.expand(s(row, "home"));
        let all = self.rows(ctx, &home, "", &[])?;
        let mut kids: HashMap<String, Session> = all
            .into_iter()
            .filter(|r| r["parent"] == row["id"])
            .map(|r| (s(&r, "id").into(), r))
            .collect();
        let start = parse_ts(&row["started"])
            .map(|t| t.timestamp())
            .unwrap_or(0);
        for mut r in self.rows(ctx, &home, "where created_at >= ?", &[json!(start)])? {
            if kids.contains_key(s(&r, "id"))
                || r["id"] == row["id"]
                || !jsonl::exists(Path::new(s(&r, "path")))
            {
                continue;
            }
            if let Some((_, rec)) = jsonl::records(ctx, Path::new(s(&r, "path")), 0, None).next()
                && s(&rec, "type") == "session_meta"
            {
                let p = &rec["payload"];
                if p["parent_thread_id"] == row["id"]
                    || p["source"]["subagent"]["thread_spawn"]["parent_thread_id"] == row["id"]
                {
                    r["kind"] = json!("subagent");
                    r["parent"] = row["id"].clone();
                    kids.insert(s(&r, "id").into(), r.clone());
                }
            }
        }
        let mut out = kids.into_values().collect::<Vec<_>>();
        out.sort_by(|a, b| s(a, "started").cmp(s(b, "started")));
        Ok(out)
    }
    fn where_info(&self, ctx: &Context) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for home in self.homes(ctx) {
            let path = home.join("state_5.sqlite");
            let counts = db::read(
                ctx,
                "codex",
                &path,
                "select count(*) as n,max(updated_at) as newest from threads",
                &[],
            )
            .unwrap_or_default();
            let r = counts.first().cloned().unwrap_or(Value::Null);
            let newest = parse_ts(&r["newest"]);
            let mut notes = Vec::new();
            let sessions = home.join("sessions");
            if std::fs::symlink_metadata(&sessions)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                notes.push(format!(
                    "sessions/ is a symlink to {}",
                    ctx.tilde(&std::fs::canonicalize(sessions).unwrap_or_default())
                ));
            }
            for index in ["session_index.jsonl", "history.jsonl"] {
                if let (Some(newest), Some(mtime)) = (
                    newest,
                    std::fs::metadata(home.join(index))
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(DateTime::<Utc>::from),
                ) {
                    let lag = newest - mtime;
                    if lag > chrono::Duration::days(1) {
                        notes.push(format!(
                            "{index} is stale by {} days; do not search it",
                            lag.num_days()
                        ));
                    }
                }
            }
            out.push(json!({"harness":"codex","path":ctx.tilde(&home),"sessions":r["n"],"newest":newest,"index":"state_5.sqlite threads (sessions/ + archived_sessions/ rollouts)","notes":notes.join("; ")}));
        }
        Ok(out)
    }
}
fn codex_event(rec: &Value) -> Option<Event> {
    let ts = parse_ts(&rec["timestamp"]);
    let p = &rec["payload"];
    match s(rec, "type") {
        "response_item" => match s(p, "type") {
            "message" => {
                let text = p["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b["text"].as_str())
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.trim().is_empty() {
                    return None;
                }
                match s(p, "role") {
                    "user" => Some(Event::new(
                        if [
                            "<",
                            "# AGENTS.md instructions",
                            "# Context from my IDE setup",
                        ]
                        .iter()
                        .any(|prefix| text.trim_start().starts_with(prefix))
                        {
                            "notice"
                        } else {
                            "prompt"
                        },
                        text,
                        ts,
                    )),
                    "assistant" => {
                        let mut e = Event::new("assistant", text, ts);
                        e.phase = opt_s(p, "phase");
                        Some(e)
                    }
                    _ => None,
                }
            }
            kind @ ("function_call" | "custom_tool_call" | "local_shell_call"
            | "web_search_call" | "tool_search_call") => {
                let mut e = Event::new("tool", "", ts);
                e.name = Some(
                    if s(p, "name").is_empty() {
                        kind
                    } else {
                        s(p, "name")
                    }
                    .into(),
                );
                e.input = Some(if kind == "function_call" {
                    p["arguments"].clone()
                } else {
                    p.get("input").unwrap_or(&p["action"]).clone()
                });
                e.id = opt_s(p, "call_id");
                Some(e)
            }
            "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
                let mut output = p["output"].clone();
                if output.is_object() {
                    output = db::or(
                        &output["output"],
                        &db::or(&output["content"], &json!(python_json(&output))),
                    );
                } else if let Some(a) = output.as_array() {
                    output = json!(
                        a.iter()
                            .filter(|v| v.is_object())
                            .map(|v| s(v, "text"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                }
                let text = if !db::truth(&output) {
                    String::new()
                } else {
                    output
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| output.to_string())
                };
                let first = text.chars().take(400).collect::<String>();
                let mut e = Event::new("result", text, ts);
                static ERROR_RE: std::sync::LazyLock<regex::Regex> =
                    std::sync::LazyLock::new(|| {
                        regex::Regex::new(
                            r#""exit_code":\s*[1-9]|Process exited with code [1-9]|^error"#,
                        )
                        .unwrap()
                    });
                e.error = ERROR_RE.is_match(&first);
                e.tool_use_id = opt_s(p, "call_id");
                Some(e)
            }
            _ => None,
        },
        "event_msg" => match s(p, "type") {
            "turn_aborted" => {
                let mut e = Event::new(
                    "system",
                    format!("[turn aborted: {}]", p["reason"].as_str().unwrap_or("None")),
                    ts,
                );
                e.error = true;
                Some(e)
            }
            "error" | "stream_error" => {
                let text = if db::truth(&p["message"]) {
                    s(p, "message").to_owned()
                } else {
                    python_json(p)
                };
                let mut e = Event::new("system", format!("[error] {}", one_line(&text, 300)), ts);
                e.error = true;
                Some(e)
            }
            _ => None,
        },
        "compacted" => Some(Event::new("system", "[compacted]", ts)),
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codex_event_roles_and_errors() {
        let e=codex_event(&json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"# AGENTS.md instructions for test"}]}})).unwrap();
        assert_eq!(e.role, "notice");
        let e=codex_event(&json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":{"output":"Process exited with code 2"}}})).unwrap();
        assert!(e.error);
        assert_eq!(e.tool_use_id.as_deref(), Some("c"));
    }
}

#[cfg(test)]
mod database_tests {
    use super::*;
    #[test]
    fn codex_rows_keep_missing_fields_and_discover_metadata_children() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".codex");
        std::fs::create_dir_all(&home).unwrap();
        let con = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
        con.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT,cwd TEXT,created_at INTEGER,updated_at INTEGER,rollout_path TEXT,source TEXT,agent_nickname TEXT,agent_role TEXT);CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT);").unwrap();
        let child = home.join("child.jsonl");
        std::fs::write(&child,r#"{"type":"session_meta","payload":{"source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}}}"#).unwrap();
        con.execute(
            "INSERT INTO threads VALUES('parent','/work',1700000000,1700000001,'','cli',NULL,NULL)",
            [],
        )
        .unwrap();
        con.execute(
            "INSERT INTO threads VALUES('child','/work',1700000002,1700000003,?,'exec',NULL,NULL)",
            [child.to_string_lossy().as_ref()],
        )
        .unwrap();
        let mut ctx = Context::from_env();
        ctx.home = dir.path().into();
        let rows = Codex.rows(&ctx, &home, "", &[]).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0]["first_prompt"].is_null());
        assert!(rows[0]["model"].is_null());
        assert_eq!(rows[1]["kind"], "worker");
        let kids = Codex.children(&ctx, &rows[0]).unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0]["kind"], "subagent");
        assert_eq!(kids[0]["parent"], "parent");
    }
    #[test]
    fn a_compressed_unindexed_rollout_resolves_and_reads() {
        use ruzstd::encoding::{CompressionLevel, compress_to_vec};
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".codex");
        let day = home.join("sessions/2026/10/01");
        std::fs::create_dir_all(&day).unwrap();
        let id = "01a00000-0000-7000-8000-00000000c0de";
        let body = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/repo\",\"timestamp\":\"2026-10-01T17:41:37Z\"}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"compressed hello\"}}]}}}}\n"
        );
        std::fs::write(
            day.join(format!("rollout-2026-10-01T10-41-37-{id}.jsonl.zst")),
            compress_to_vec(body.as_bytes(), CompressionLevel::Fastest),
        )
        .unwrap();
        let mut ctx = Context::from_env();
        ctx.home = dir.path().into();
        std::fs::write(home.join("state_5.sqlite"), b"").unwrap();
        let rows = Codex.resolve(&ctx, &id[..13]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], id);
        assert_eq!(rows[0]["cwd"], "/repo");
        let texts = Codex
            .events(&ctx, &rows[0], None)
            .unwrap()
            .map(|e| serde_json::to_string(&e).unwrap())
            .collect::<Vec<_>>();
        assert!(
            texts.iter().any(|t| t.contains("compressed hello")),
            "{texts:?}"
        );
    }
    fn index_thread(home: &Path, id: &str, rollout: &Path) {
        std::fs::create_dir_all(home).unwrap();
        let con = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
        con.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT,cwd TEXT,created_at INTEGER,updated_at INTEGER,rollout_path TEXT,source TEXT,archived INTEGER,name TEXT);").unwrap();
        let name = home.file_name().unwrap().to_string_lossy().into_owned();
        con.execute(
            "INSERT INTO threads VALUES(?,'/repo',1700000000,1700000001,?,'cli',1,?)",
            [id, rollout.to_string_lossy().as_ref(), &name],
        )
        .unwrap();
    }
    #[test]
    fn two_indexes_of_one_archived_rollout_resolve_to_one_session() {
        let dir = tempfile::tempdir().unwrap();
        let owner = dir.path().join(".codex");
        let other = dir.path().join(".codex-work");
        let archived = owner.join("archived_sessions");
        std::fs::create_dir_all(&archived).unwrap();
        let id = "019eb9ff-338d-79e2-8be3-8444bb8fb03a";
        let rollout = archived.join(format!("rollout-2026-10-20T10-00-00-{id}.jsonl"));
        std::fs::write(
            &rollout,
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\"}}}}\n"),
        )
        .unwrap();
        index_thread(&owner, id, &rollout);
        // The second index names the same file through a different spelling.
        index_thread(
            &other,
            id,
            &owner
                .join("archived_sessions/../archived_sessions")
                .join(rollout.file_name().unwrap()),
        );
        let mut ctx = Context::from_env();
        ctx.home = dir.path().into();
        let rows = Codex.resolve(&ctx, id).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["home"], ctx.tilde(&owner));
        let listed = Codex.sessions(&ctx, None, true).unwrap();
        assert_eq!(
            listed.iter().filter(|r| r["id"] == id).count(),
            1,
            "{listed:?}"
        );
        // Two different transcripts under one id stay two candidates.
        let third = dir.path().join(".codex-old");
        let elsewhere = third.join("archived_sessions");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let second = elsewhere.join(rollout.file_name().unwrap());
        std::fs::write(&second, "{}\n").unwrap();
        index_thread(&third, id, &second);
        assert_eq!(Codex.resolve(&ctx, id).unwrap().len(), 2);
    }
    #[test]
    fn the_home_holding_the_transcript_wins_when_it_comes_second() {
        let dir = tempfile::tempdir().unwrap();
        // ~/.codex is discovered first but only indexes ~/.codex-work's file.
        let first = dir.path().join(".codex");
        let owner = dir.path().join(".codex-work");
        let archived = owner.join("archived_sessions");
        std::fs::create_dir_all(&archived).unwrap();
        let id = "019eb9ff-338d-79e2-8be3-8444bb8fb03b";
        let rollout = archived.join(format!("rollout-2026-10-20T10-00-00-{id}.jsonl"));
        std::fs::write(&rollout, "{}\n").unwrap();
        index_thread(&first, id, &rollout);
        index_thread(&owner, id, &rollout);
        let mut ctx = Context::from_env();
        ctx.home = dir.path().into();
        let rows = Codex.resolve(&ctx, id).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["home"], ctx.tilde(&owner));
        assert_eq!(rows[0]["title"], ".codex-work");
    }
    #[test]
    fn unindexed_rollout_has_legacy_absent_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-test.jsonl");
        std::fs::write(&path,r#"{"type":"session_meta","payload":{"id":"thread","cwd":"/repo","timestamp":"2026-01-01T00:00:00Z"}}"#).unwrap();
        let ctx = Context::from_env();
        let row = Codex::from_rollout(&ctx, dir.path(), &path);
        assert_eq!(row["id"], "thread");
        assert_eq!(row["started"], "2026-01-01T00:00:00Z");
        assert!(row.get("archived").is_none());
        assert!(row.get("branch").is_none());
    }
}
