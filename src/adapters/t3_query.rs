//! Read-only orchestration metadata queries. Each command holds one SQLite snapshot.
use super::t3::T3;
use crate::{
    context::Context,
    storage::sqlite as db,
    util::{parse_since, redact, s},
};
use anyhow::Result;
use clap::Args;
use serde_json::{Value, json};

#[derive(Args)]
pub struct ThreadsArgs {
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub project: Option<String>,
    #[arg(long)]
    pub created_by: Option<String>,
    #[arg(long)]
    pub source: Option<String>,
    #[arg(long, conflicts_with = "unsettled")]
    pub settled: bool,
    #[arg(long)]
    pub unsettled: bool,
    #[arg(long, conflicts_with = "active")]
    pub archived: bool,
    #[arg(long)]
    pub active: bool,
    #[arg(long)]
    pub include_deleted: bool,
    #[arg(long)]
    pub limit: Option<usize>,
    #[arg(long, value_parser = ["created_by", "creation_source", "project_id", "status", "settled", "archived", "provider_instance_id"])]
    pub count_by: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct EventsArgs {
    #[arg(long = "type")]
    pub types: Vec<String>,
    #[arg(long, value_parser = ["server", "client"])]
    pub actor: Option<String>,
    #[arg(long)]
    pub thread: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long)]
    pub after_seq: Option<i64>,
    #[arg(long, default_value_t = 200)]
    pub limit: usize,
    #[arg(long)]
    pub payload: bool,
    #[arg(long, value_parser = ["type", "actor", "thread", "kind", "day", "created_by", "creation_source"])]
    pub count_by: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct TasksArgs {
    pub ids: Vec<String>,
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    #[arg(long)]
    pub json: bool,
}

pub fn tasks(ctx: &Context, opts: &TasksArgs) -> Result<Value> {
    let con = T3::open_v2(ctx)?.ok_or_else(|| anyhow::anyhow!("T3 v2 store not found"))?;
    con.execute_batch("BEGIN")?;
    anyhow::ensure!(
        db::table_exists(&con, "orchestration_v2_projection_subagents")?,
        "T3 v2 subagent table not found"
    );
    let mut sql = "select subagent_id,thread_id,run_id,child_thread_id,provider,status,
        completion_delivery_state,started_at,completed_at,updated_at
        from orchestration_v2_projection_subagents"
        .to_owned();
    let mut args = vec![];
    if let Some(status) = &opts.status {
        sql.push_str(" where status=?");
        args.push(json!(status));
    }
    if opts.ids.is_empty() {
        sql.push_str(" order by julianday(updated_at) desc,subagent_id limit ?");
        args.push(json!(opts.limit));
        return Ok(json!({"tasks":db::query(&con, &sql, &args)?}));
    }
    let rows = db::query(&con, &sql, &args)?;
    let mut selected = Vec::with_capacity(opts.ids.len());
    for term in &opts.ids {
        let matches: Vec<&Value> = rows
            .iter()
            .filter(|row| {
                ["subagent_id", "child_thread_id"].iter().any(|field| {
                    let id = s(row, field);
                    id.contains(term)
                        || percent_encoding::percent_decode_str(id)
                            .decode_utf8_lossy()
                            .contains(term)
                })
            })
            .collect();
        let exact: Vec<&Value> = matches
            .iter()
            .copied()
            .filter(|row| {
                ["subagent_id", "child_thread_id"].iter().any(|field| {
                    let id = s(row, field);
                    let decoded = percent_encoding::percent_decode_str(id).decode_utf8_lossy();
                    // The delegate id T3 tools print: decoded, without the
                    // `node:`/`thread:` and `delegated-task:` prefixes.
                    let bare = ["node:", "thread:"]
                        .iter()
                        .find_map(|p| decoded.strip_prefix(p))
                        .map(|rest| rest.strip_prefix("delegated-task:").unwrap_or(rest));
                    id == term || decoded == term.as_str() || bare == Some(term.as_str())
                })
            })
            .collect();
        let row = if exact.len() == 1 {
            exact[0]
        } else {
            anyhow::ensure!(!matches.is_empty(), "no T3 task matches {term:?}");
            anyhow::ensure!(
                matches.len() == 1,
                "ambiguous T3 task {term:?}: {} matches; give more of its id",
                matches.len()
            );
            matches[0]
        };
        selected.push(row.clone());
    }
    if opts.ids.len() == 1 {
        Ok(selected.pop().unwrap())
    } else {
        Ok(json!({"tasks":selected}))
    }
}

fn count_object(key: &str, mut counts: Vec<Value>) -> Value {
    counts.sort_by(|a, b| {
        b["count"]
            .as_u64()
            .cmp(&a["count"].as_u64())
            .then_with(|| a["value"].as_str().cmp(&b["value"].as_str()))
    });
    let total: u64 = counts.iter().filter_map(|v| v["count"].as_u64()).sum();
    json!({"count_by":key,"total":total,"counts":counts})
}

pub fn threads(ctx: &Context, opts: &ThreadsArgs) -> Result<Value> {
    let con = T3::open_v2(ctx)?.ok_or_else(|| anyhow::anyhow!("T3 v2 store not found"))?;
    con.execute_batch("BEGIN")?;
    let mut conditions = vec!["1=1".to_owned()];
    let mut args = vec![];
    if !opts.include_deleted {
        conditions.push("t.deleted_at is null".into());
    }
    if let Some(since) = &opts.since {
        conditions.push("julianday(t.updated_at)>=julianday(?)".into());
        args.push(json!(parse_since(since, ctx.now)?.to_rfc3339()));
    }
    for (value, column) in [
        (&opts.project, "t.project_id"),
        (
            &opts.created_by,
            "json_extract(t.payload_json,'$.createdBy')",
        ),
        (
            &opts.source,
            "json_extract(t.payload_json,'$.creationSource')",
        ),
    ] {
        if let Some(value) = value {
            conditions.push(format!("{column}=?"));
            args.push(json!(value));
        }
    }
    for (yes, no, column) in [
        (
            opts.settled,
            opts.unsettled,
            "json_extract(t.payload_json,'$.settledAt')",
        ),
        (opts.archived, opts.active, "t.archived_at"),
    ] {
        if yes {
            conditions.push(format!("{column} is not null"));
        }
        if no {
            conditions.push(format!("{column} is null"));
        }
    }
    if let Some(key) = &opts.count_by {
        let column = match key.as_str() {
            "created_by" => "json_extract(t.payload_json,'$.createdBy')",
            "creation_source" => "json_extract(t.payload_json,'$.creationSource')",
            "project_id" => "t.project_id",
            "provider_instance_id" => "t.provider_instance_id",
            "status" => "coalesce(r.status,'idle')",
            "settled" => {
                "case when json_extract(t.payload_json,'$.settledAt') is null then 'unsettled' else 'settled' end"
            }
            "archived" => "case when t.archived_at is null then 'active' else 'archived' end",
            _ => unreachable!(),
        };
        let join = if key == "status" {
            "left join orchestration_v2_projection_runs r on r.run_id=(select run_id from orchestration_v2_projection_runs where thread_id=t.thread_id order by ordinal desc,run_id desc limit 1)"
        } else {
            ""
        };
        let mut selected = format!(
            "select {column} as value from orchestration_v2_projection_threads t {join} where {} order by julianday(t.updated_at) desc,t.thread_id",
            conditions.join(" and ")
        );
        if let Some(limit) = opts.limit {
            selected.push_str(" limit ?");
            args.push(json!(limit));
        }
        let sql = format!("select value,count(*) as count from ({selected}) group by value");
        return Ok(count_object(key, db::query(&con, &sql, &args)?));
    }
    let mut sql = format!("select t.thread_id,t.title,t.project_id,t.created_at,t.updated_at,t.archived_at,t.deleted_at,
        t.provider_instance_id,t.runtime_mode,t.interaction_mode,t.payload_json,
        r.run_id,r.status,r.ordinal from orchestration_v2_projection_threads t
        left join orchestration_v2_projection_runs r on r.run_id=(select run_id from orchestration_v2_projection_runs
        where thread_id=t.thread_id order by ordinal desc,run_id desc limit 1)
        where {} order by julianday(t.updated_at) desc,t.thread_id", conditions.join(" and "));
    if let Some(limit) = opts.limit {
        sql.push_str(" limit ?");
        args.push(json!(limit));
    }
    let mut rows = db::query(&con, &sql, &args)?;
    for row in &mut rows {
        let payload: Value = serde_json::from_str(s(row, "payload_json"))?;
        for (field, key) in [
            ("settled_at", "settledAt"),
            ("unsettled_at", "unsettledAt"),
            ("snoozed_until", "snoozedUntil"),
            ("pinned_at", "pinnedAt"),
            ("created_by", "createdBy"),
            ("creation_source", "creationSource"),
            ("forked_from", "forkedFrom"),
            ("branch", "branch"),
            ("worktree_path", "worktreePath"),
            ("pull_requests", "pullRequests"),
        ] {
            row[field] = payload[key].clone();
        }
        row["link"] = json!(format!("t3-thread://v1/{}", s(row, "thread_id")));
        row["latest_run"] = if row["run_id"].is_null() {
            Value::Null
        } else {
            json!({"run_id":row["run_id"],"status":row["status"],"ordinal":row["ordinal"]})
        };
        let object = row.as_object_mut().unwrap();
        for field in ["payload_json", "run_id", "status", "ordinal"] {
            object.remove(field);
        }
        if !opts.include_deleted {
            object.remove("deleted_at");
        }
    }
    Ok(json!({"threads":rows}))
}

fn redact_payload(value: &mut Value) {
    match value {
        Value::String(text) => *text = redact(text),
        Value::Array(values) => values.iter_mut().for_each(redact_payload),
        Value::Object(values) => values.values_mut().for_each(redact_payload),
        _ => {}
    }
}

pub fn events(ctx: &Context, opts: &EventsArgs) -> Result<Value> {
    let con = T3::open_v2(ctx)?.ok_or_else(|| anyhow::anyhow!("T3 v2 store not found"))?;
    con.execute_batch("BEGIN")?;
    let mut conditions = vec!["1=1".to_owned()];
    let mut args = vec![];
    if !opts.types.is_empty() {
        let mut types = vec![];
        for kind in &opts.types {
            if let Some(prefix) = kind.strip_suffix('*') {
                types.push("substr(e.event_type,1,length(?))=?");
                args.extend([json!(prefix), json!(prefix)]);
            } else {
                types.push("e.event_type=?");
                args.push(json!(kind));
            }
        }
        conditions.push(format!("({})", types.join(" or ")));
    }
    for (value, column) in [
        (&opts.actor, "e.actor_kind"),
        (&opts.kind, "e.aggregate_kind"),
    ] {
        if let Some(value) = value {
            conditions.push(format!("{column}=?"));
            args.push(json!(value));
        }
    }
    if let Some(thread) = &opts.thread {
        let (harness, id) = crate::core::split_address(thread)?;
        anyhow::ensure!(harness.is_none_or(|h| h == "t3"), "expected a T3 thread id");
        // Resolve the unprefixed form through the projection first, so the event
        // filter is an exact stream match that T3's stream index serves.
        let found = db::query(
            &con,
            "select thread_id from orchestration_v2_projection_threads
             where thread_id=?1 or substr(thread_id,instr(thread_id,':')+1)=?1",
            &[json!(id)],
        )?;
        anyhow::ensure!(
            found.len() <= 1,
            "ambiguous T3 thread {thread:?}; give its prefixed id"
        );
        let stream = found
            .first()
            .map_or(id, |row| s(row, "thread_id").to_owned());
        conditions.push("e.aggregate_kind='thread' and e.stream_id=?".into());
        args.push(json!(stream));
    }
    for (value, op) in [(&opts.since, ">="), (&opts.until, "<=")] {
        if let Some(value) = value {
            conditions.push(format!("julianday(e.occurred_at){op}julianday(?)"));
            args.push(json!(parse_since(value, ctx.now)?.to_rfc3339()));
        }
    }
    if let Some(seq) = opts.after_seq {
        conditions.push("e.sequence>?".into());
        args.push(json!(seq));
    }
    let join = "left join orchestration_v2_projection_threads t on e.aggregate_kind='thread' and t.thread_id=e.stream_id";
    let filter = conditions.join(" and ");
    if let Some(key) = &opts.count_by {
        let column = match key.as_str() {
            "type" => "e.event_type",
            "actor" => "e.actor_kind",
            "thread" => "e.stream_id",
            "kind" => "e.aggregate_kind",
            "day" => "date(e.occurred_at)",
            "created_by" => "json_extract(t.payload_json,'$.createdBy')",
            "creation_source" => "json_extract(t.payload_json,'$.creationSource')",
            _ => unreachable!(),
        };
        let join = if matches!(key.as_str(), "created_by" | "creation_source") {
            join
        } else {
            ""
        };
        let sql = format!(
            "select {column} as value,count(*) as count from orchestration_events e {join} where {filter} group by {column}"
        );
        return Ok(count_object(key, db::query(&con, &sql, &args)?));
    }
    let payload = if opts.payload { ",e.payload_json" } else { "" };
    let mut sql = format!("select e.sequence,e.occurred_at,e.event_type,e.aggregate_kind,e.stream_id,e.actor_kind,
        t.thread_id as joined_thread_id,t.title,json_extract(t.payload_json,'$.createdBy') as created_by,
        json_extract(t.payload_json,'$.creationSource') as creation_source,t.project_id{payload}
        from orchestration_events e {join} where {filter} order by e.sequence");
    if opts.limit != 0 {
        sql.push_str(" limit ?");
        args.push(json!(opts.limit));
    }
    let mut rows = db::query(&con, &sql, &args)?;
    for row in &mut rows {
        if !row["joined_thread_id"].is_null() {
            row["thread"] = json!({"title":row["title"],"created_by":row["created_by"],"creation_source":row["creation_source"],"project_id":row["project_id"]});
        }
        if opts.payload {
            let mut payload: Value = serde_json::from_str(s(row, "payload_json"))?;
            redact_payload(&mut payload);
            let text = payload.to_string();
            row["payload"] = if text.chars().count() > 2000 {
                json!({"truncated":true,"bytes":text.len()})
            } else {
                payload
            };
        }
        let object = row.as_object_mut().unwrap();
        for key in [
            "joined_thread_id",
            "title",
            "created_by",
            "creation_source",
            "project_id",
            "payload_json",
        ] {
            object.remove(key);
        }
    }
    Ok(json!({"events":rows}))
}
