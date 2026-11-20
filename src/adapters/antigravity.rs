use crate::{
    adapters::{
        StoreAdapter,
        claude::{entries, mtime},
    },
    context::Context,
    model::{Event, Session},
    storage::{jsonl::records, sqlite as db},
    util::{iso, opt_s, parse_ts, s},
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::LazyLock,
};

pub struct Antigravity;

pub fn unwrap_user_text(raw: &str) -> String {
    static REQ: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?s)<USER_REQUEST>\s*(.*?)\s*</USER_REQUEST>").unwrap()
    });
    static T3_INSTR: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?s)<t3_code_instructions>.*?</t3_code_instructions>\s*").unwrap()
    });
    let text = REQ
        .captures(raw)
        .map(|c| c[1].to_owned())
        .unwrap_or_else(|| raw.to_owned());
    T3_INSTR.replace_all(&text, "").trim().to_owned()
}

fn uri_to_path(uri: &str) -> String {
    let Some(rest) = uri.strip_prefix("file://") else {
        return uri.to_owned();
    };
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_encoding::percent_decode_str(rest)
        .decode_utf8_lossy()
        .into_owned();
    let bytes = decoded.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        decoded[1..].to_owned()
    } else {
        decoded
    }
}

fn first_workspace(raw: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let arr = parsed.as_array()?;
    arr.iter()
        .filter_map(Value::as_str)
        .find(|u| !u.is_empty())
        .map(uri_to_path)
}

fn summary_map(ctx: &Context, home: &Path) -> HashMap<String, Value> {
    let db_path = home.join("conversation_summaries.db");
    if !db_path.is_file() {
        return HashMap::new();
    }
    let Ok(con) = db::open_without_idle_sidecars(ctx, "antigravity", &db_path) else {
        return HashMap::new();
    };
    if !db::table_exists(&con, "conversation_summaries").unwrap_or(false) {
        return HashMap::new();
    }
    let rows = match db::query(
        &con,
        "select conversation_id, title, preview, workspace_uris, last_modified_time, parent_conversation_id, status from conversation_summaries",
        &[],
    ) {
        Ok(r) => r,
        Err(e) => {
            db::report(ctx, "antigravity", &db_path, &e);
            return HashMap::new();
        }
    };
    rows.into_iter()
        .filter_map(|r| {
            let id = s(&r, "conversation_id").to_owned();
            (!id.is_empty()).then_some((id, r))
        })
        .collect()
}

fn discover_homes(ctx: &Context) -> Vec<PathBuf> {
    let mut candidates = vec![
        ctx.home.join(".gemini").join("antigravity-cli"),
        ctx.home.join(".gemini").join("antigravity"),
    ];
    let t3_root = ctx
        .home
        .join(".t3")
        .join("userdata")
        .join("providers")
        .join("antigravity");
    for child in entries(ctx, &t3_root) {
        if child.is_dir() {
            candidates.push(child.join("antigravity-acp"));
        }
    }
    let mut seen = HashSet::new();
    let mut homes = Vec::new();
    for dir in candidates {
        if !dir.join("brain").is_dir()
            && !dir.join("conversation_summaries.db").is_file()
            && !dir.join("conversations").is_dir()
        {
            continue;
        }
        let key = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if seen.insert(key) {
            homes.push(dir);
        }
    }
    homes.sort();
    homes
}

fn transcript_path(home: &Path, conv_id: &str) -> PathBuf {
    home.join("brain")
        .join(conv_id)
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl")
}

fn is_t3_home(home: &Path) -> bool {
    home.components().any(|c| c.as_os_str() == ".t3")
}

fn build_row(
    ctx: &Context,
    home: &Path,
    conv_id: &str,
    path: &Path,
    summary: Option<&Value>,
) -> Session {
    let mt = mtime(path).map(iso);
    let mut first_prompt = Value::Null;
    let mut started: Option<String> = None;
    let mut model = Value::Null;
    let mut scanned = 0usize;

    for (_, r) in records(ctx, path, 0, None) {
        scanned += 1;
        if started.is_none()
            && let Some(ts) = parse_ts(&r["created_at"])
        {
            started = Some(iso(ts));
        }
        if first_prompt.is_null()
            && s(&r, "type") == "USER_INPUT"
            && s(&r, "source") == "USER_EXPLICIT"
        {
            let prompt = unwrap_user_text(s(&r, "content"));
            if !prompt.is_empty() {
                first_prompt = json!(prompt.chars().take(600).collect::<String>());
            }
        }
        if model.is_null()
            && s(&r, "type") == "PLANNER_RESPONSE"
            && let Some(m) = opt_s(&r, "model")
        {
            model = json!(m);
        }
        if started.is_some() && !first_prompt.is_null() && !model.is_null() {
            break;
        }
        if scanned >= 40 {
            break;
        }
    }

    let summary_title = summary.and_then(|v| {
        let t = s(v, "title").trim();
        if !t.is_empty() {
            Some(t.to_owned())
        } else {
            let p = s(v, "preview").trim();
            (!p.is_empty()).then(|| p.to_owned())
        }
    });
    let cwd = summary
        .and_then(|v| first_workspace(s(v, "workspace_uris")))
        .map(Value::String)
        .unwrap_or(Value::Null);
    let parent = summary
        .and_then(|v| {
            let p = s(v, "parent_conversation_id").trim();
            (!p.is_empty()).then(|| Value::String(p.to_owned()))
        })
        .unwrap_or(Value::Null);
    let kind = if parent.is_null() { "main" } else { "subagent" };
    let updated = summary
        .and_then(|v| parse_ts(&v["last_modified_time"]).map(iso))
        .or_else(|| mt.clone())
        .or_else(|| started.clone());
    let started_val = started.or_else(|| updated.clone()).or(mt);
    let status = summary
        .and_then(|v| {
            let st = s(v, "status").trim();
            (!st.is_empty()).then(|| Value::String(st.to_owned()))
        })
        .unwrap_or(Value::Null);

    json!({
        "harness": "antigravity",
        "home": ctx.tilde(home),
        "id": conv_id,
        "parent": parent,
        "kind": kind,
        "cwd": cwd,
        "branch": null,
        "title": summary_title,
        "first_prompt": first_prompt,
        "started": started_val,
        "updated": updated,
        "model": model,
        "path": path.to_string_lossy(),
        "originator": if is_t3_home(home) { Some("t3") } else { None },
        "status": status,
    })
}

pub(crate) fn event_records(r: &Value) -> Vec<Event> {
    let ts = parse_ts(&r["created_at"]);
    let mut out = Vec::new();
    match s(r, "type") {
        "USER_INPUT" => {
            let raw = s(r, "content");
            if s(r, "source") == "USER_EXPLICIT" {
                let text = unwrap_user_text(raw);
                if !text.is_empty() {
                    let mut event = Event::new("prompt", &text, ts);
                    event.user_turn = Some((text, false));
                    out.push(event);
                } else if !raw.trim().is_empty() {
                    let mut event = Event::new("notice", raw.trim(), ts);
                    event.user_turn = Some((event.text.clone(), true));
                    out.push(event);
                }
            } else if !raw.trim().is_empty() {
                let mut event = Event::new("notice", raw.trim(), ts);
                event.user_turn = Some((event.text.clone(), true));
                out.push(event);
            }
        }
        "SYSTEM_MESSAGE" => {
            let text = s(r, "content").trim();
            if !text.is_empty() {
                out.push(Event::new("notice", text, ts));
            }
        }
        "PLANNER_RESPONSE" => {
            let text = s(r, "content").trim();
            if !text.is_empty() {
                let mut e = Event::new("assistant", text, ts);
                e.model = opt_s(r, "model");
                out.push(e);
            }
            if let Some(calls) = r["tool_calls"].as_array() {
                for tc in calls {
                    let mut e = Event::new("tool", "", ts);
                    e.name = opt_s(tc, "name");
                    e.input = tc
                        .get("args")
                        .or_else(|| tc.get("arguments"))
                        .or_else(|| tc.get("input"))
                        .cloned();
                    e.id = opt_s(tc, "id");
                    out.push(e);
                }
            }
        }
        "GENERIC" => {
            let text = s(r, "content").trim();
            if !text.is_empty() {
                let mut e = Event::new("result", text, ts);
                e.error = s(r, "status").eq_ignore_ascii_case("ERROR");
                out.push(e);
            }
        }
        _ => {}
    }
    out
}

pub(crate) fn full_transcript(compact: &Path) -> Option<PathBuf> {
    let full = compact.with_file_name("transcript_full.jsonl");
    full.is_file().then_some(full)
}

fn load_full_steps(ctx: &Context, transcript: &Path) -> HashMap<i64, Value> {
    let Some(full) = full_transcript(transcript) else {
        return HashMap::new();
    };
    records(ctx, &full, 0, None)
        .filter_map(|(_, r)| r["step_index"].as_i64().map(|idx| (idx, r)))
        .collect()
}

impl StoreAdapter for Antigravity {
    fn name(&self) -> &'static str {
        "antigravity"
    }

    fn homes(&self, ctx: &Context) -> Vec<PathBuf> {
        discover_homes(ctx)
    }

    fn sessions(
        &self,
        ctx: &Context,
        since: Option<DateTime<Utc>>,
        subagents: bool,
    ) -> Result<Vec<Session>> {
        let mut rows = Vec::new();
        for home in self.homes(ctx) {
            let summaries = summary_map(ctx, &home);
            let brain = home.join("brain");
            for dir in entries(ctx, &brain) {
                if !dir.is_dir() {
                    continue;
                }
                let Some(conv_id) = dir.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let path = transcript_path(&home, conv_id);
                if !path.is_file() {
                    continue;
                }
                let summary = summaries.get(conv_id);
                if let Some(cut) = since {
                    let mod_ts = summary
                        .and_then(|v| parse_ts(&v["last_modified_time"]))
                        .or_else(|| mtime(&path));
                    if mod_ts.is_some_and(|t| t < cut) {
                        continue;
                    }
                }
                let row = build_row(ctx, &home, conv_id, &path, summary);
                if !subagents && s(&row, "kind") == "subagent" {
                    continue;
                }
                rows.push(row);
            }
            ctx.cover("antigravity", Some(&home), "sessions", "read", None);
        }
        Ok(rows)
    }

    fn resolve(&self, ctx: &Context, ident: &str) -> Result<Vec<Session>> {
        let mut rows = Vec::new();
        for home in self.homes(ctx) {
            let summaries = summary_map(ctx, &home);
            let brain = home.join("brain");
            for dir in entries(ctx, &brain) {
                if !dir.is_dir() {
                    continue;
                }
                let Some(conv_id) = dir.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !conv_id.starts_with(ident) {
                    continue;
                }
                let path = transcript_path(&home, conv_id);
                if !path.is_file() {
                    continue;
                }
                rows.push(build_row(
                    ctx,
                    &home,
                    conv_id,
                    &path,
                    summaries.get(conv_id),
                ));
            }
        }
        Ok(rows)
    }

    fn events<'a>(
        &self,
        ctx: &'a Context,
        row: &'a Session,
        needles: Option<&[String]>,
    ) -> Result<Box<dyn Iterator<Item = Event> + 'a>> {
        let path = PathBuf::from(s(row, "path"));
        if needles.is_some()
            && let Some(full) = full_transcript(&path)
        {
            let events =
                records(ctx, &full, 0, needles).flat_map(|(_, record)| event_records(&record));
            return Ok(Box::new(events));
        }
        let mut full_cache: Option<HashMap<i64, Value>> = None;
        let mut out = Vec::new();
        for (_, r) in records(ctx, &path, 0, needles) {
            let has_trunc = r["truncated_fields"]
                .as_array()
                .is_some_and(|a| !a.is_empty());
            let effective = if has_trunc {
                let map = full_cache.get_or_insert_with(|| load_full_steps(ctx, &path));
                r["step_index"]
                    .as_i64()
                    .and_then(|idx| map.get(&idx).cloned())
                    .unwrap_or(r)
            } else {
                r
            };
            out.extend(event_records(&effective));
        }
        Ok(Box::new(out.into_iter()))
    }

    fn where_info(&self, ctx: &Context) -> Result<Vec<Value>> {
        let mut stores = Vec::new();
        for home in self.homes(ctx) {
            let brain = home.join("brain");
            let mut count = 0usize;
            let mut newest: Option<DateTime<Utc>> = None;
            for dir in entries(ctx, &brain) {
                if let Some(conv_id) = dir.file_name().and_then(|n| n.to_str()) {
                    let path = transcript_path(&home, conv_id);
                    if path.is_file() {
                        count += 1;
                        if let Some(mt) = mtime(&path) {
                            newest = Some(newest.map_or(mt, |n| n.max(mt)));
                        }
                    }
                }
            }
            let pb_only = entries(ctx, &home.join("conversations"))
                .into_iter()
                .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "pb"))
                .filter(|path| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_none_or(|id| {
                            let compact = transcript_path(&home, id);
                            !compact.is_file() && full_transcript(&compact).is_none()
                        })
                })
                .count();
            stores.push(json!({
                "harness": "antigravity",
                "path": ctx.tilde(&home),
                "sessions": count,
                "newest": newest.map(iso),
                "index": "conversation_summaries.db + brain/<id>/.system_generated/logs/transcript.jsonl",
                "notes": format!("{pb_only} .pb-only conversation(s) excluded (no JSONL transcript); subagents link via parent_conversation_id"),
            }));
        }
        Ok(stores)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn make_wal_db(path: &Path) -> Connection {
        let con = Connection::open(path).unwrap();
        con.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE conversation_summaries (
               conversation_id TEXT PRIMARY KEY,
               title TEXT,
               preview TEXT,
               workspace_uris TEXT,
               last_modified_time TEXT,
               parent_conversation_id TEXT,
               status TEXT
             );",
        )
        .unwrap();
        con
    }

    #[test]
    fn discovers_lists_resolves_and_reads_antigravity_sessions_and_subagents() {
        let temp = tempfile::tempdir().unwrap();
        let mut ctx = Context::from_env();
        ctx.home = temp.path().to_path_buf();

        let acp = ctx
            .home
            .join(".t3")
            .join("userdata")
            .join("providers")
            .join("antigravity")
            .join("profile-abc")
            .join("antigravity-acp");
        let main_logs = acp
            .join("brain")
            .join("11111111-aaaa-bbbb-cccc-000000000001")
            .join(".system_generated")
            .join("logs");
        let child_logs = acp
            .join("brain")
            .join("22222222-aaaa-bbbb-cccc-000000000002")
            .join(".system_generated")
            .join("logs");
        std::fs::create_dir_all(&main_logs).unwrap();
        std::fs::create_dir_all(&child_logs).unwrap();

        let con = make_wal_db(&acp.join("conversation_summaries.db"));
        con.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "11111111-aaaa-bbbb-cccc-000000000001",
                "Fix parser",
                "",
                r#"["file:///workspace/demo"]"#,
                "2026-11-20T19:10:00Z",
                "",
                "DONE"
            ],
        )
        .unwrap();
        con.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "22222222-aaaa-bbbb-cccc-000000000002",
                "Subagent task",
                "",
                r#"["file:///workspace/demo"]"#,
                "2026-11-20T19:11:00Z",
                "11111111-aaaa-bbbb-cccc-000000000001",
                "DONE"
            ],
        )
        .unwrap();

        std::fs::write(
            main_logs.join("transcript.jsonl"),
            concat!(
                r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","created_at":"2026-11-20T19:00:00Z","content":"<USER_REQUEST>\n<t3_code_instructions>\nrules\n</t3_code_instructions>\nInspect parser\n</USER_REQUEST>"}"#,
                "\n",
                r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","created_at":"2026-11-20T19:00:05Z","model":"gemini-test","input_tokens":100,"cache_read_tokens":20,"output_tokens":30,"thinking_tokens":10,"content":"short...","truncated_fields":["content"],"tool_calls":[{"id":"tc1","name":"read_file","args":{"Path":"src/lib.rs"}}]}"#,
                "\n",
                r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-11-20T19:00:06Z","content":"fn main() {}"}"#,
                "\n"
            ),
        )
        .unwrap();
        std::fs::write(
            main_logs.join("transcript_full.jsonl"),
            concat!(
                r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","created_at":"2026-11-20T19:00:05Z","model":"gemini-test","content":"Full expanded answer","tool_calls":[{"id":"tc1","name":"read_file","args":{"Path":"src/lib.rs"}}]}"#,
                "\n"
            ),
        )
        .unwrap();
        std::fs::write(
            child_logs.join("transcript.jsonl"),
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","created_at":"2026-11-20T19:05:00Z","content":"Child prompt"}"#,
        )
        .unwrap();

        let adapter = Antigravity;
        let main_only = adapter.sessions(&ctx, None, false).unwrap();
        assert_eq!(main_only.len(), 1);
        assert_eq!(
            s(&main_only[0], "id"),
            "11111111-aaaa-bbbb-cccc-000000000001"
        );
        assert_eq!(s(&main_only[0], "cwd"), "/workspace/demo");
        assert_eq!(s(&main_only[0], "title"), "Fix parser");
        assert_eq!(s(&main_only[0], "model"), "gemini-test");
        assert_eq!(s(&main_only[0], "originator"), "t3");

        let with_sub = adapter.sessions(&ctx, None, true).unwrap();
        assert_eq!(with_sub.len(), 2);

        let children = adapter.children(&ctx, &main_only[0]).unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(
            s(&children[0], "id"),
            "22222222-aaaa-bbbb-cccc-000000000002"
        );

        let events: Vec<Event> = adapter.events(&ctx, &main_only[0], None).unwrap().collect();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].role, "prompt");
        assert_eq!(events[0].text, "Inspect parser");
        assert_eq!(events[1].role, "assistant");
        assert_eq!(events[1].text, "Full expanded answer");
        assert_eq!(events[2].role, "tool");
        assert_eq!(events[2].name.as_deref(), Some("read_file"));
        assert_eq!(events[3].role, "result");

        let (usage_summary, calls) = crate::usage::extract(&ctx, &main_only[0], Value::Null);
        assert_eq!(usage_summary["calls"], json!(1));
        assert_eq!(usage_summary["input_tokens"], json!(100));
        assert_eq!(usage_summary["cache_read_tokens"], json!(20));
        assert_eq!(usage_summary["output_tokens"], json!(30));
        assert_eq!(usage_summary["reasoning_tokens"], json!(10));
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn reads_idle_wal_summaries_without_creating_sidecars() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("store # %");
        std::fs::create_dir(&home).unwrap();
        let path = home.join("conversation_summaries.db");
        let con = make_wal_db(&path);
        con.execute(
            "INSERT INTO conversation_summaries (conversation_id, title) VALUES ('idle-id', 'idle title')",
            [],
        )
        .unwrap();
        drop(con);

        let wal = home.join("conversation_summaries.db-wal");
        let shm = home.join("conversation_summaries.db-shm");
        assert!(!wal.exists() && !shm.exists());
        let summaries = summary_map(&Context::from_env(), &home);
        assert_eq!(summaries["idle-id"]["title"], "idle title");
        assert!(!wal.exists() && !shm.exists());
    }

    #[test]
    fn converts_windows_file_uris() {
        assert_eq!(
            uri_to_path("file:///C:/Workspaces/demo/repo"),
            "C:/Workspaces/demo/repo"
        );
        assert_eq!(
            uri_to_path("file:///workspace/my%20repo"),
            "/workspace/my repo"
        );
    }
}
