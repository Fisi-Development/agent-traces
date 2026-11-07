//! Full-transcript usage extraction, kept separate from session listing.
use crate::{context::Context, core, model::Session, storage::jsonl, util::*};
use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub fn session(ctx: &Context, id: &str) -> Result<(Value, Vec<Value>)> {
    let row = core::pick_one(ctx, id, true)?;
    // pick_one may attach a T3 alias even for a provider query. Only expose it
    // when the query actually names a T3 thread.
    let t3_thread = if s(&row, "harness") == "t3" {
        row["id"].clone()
    } else {
        let (harness, query) = core::split_address(id)?;
        let query = query.to_lowercase();
        if harness.is_none_or(|h| h == "t3")
            && !query.is_empty()
            && s(&row, "t3_thread").to_lowercase().starts_with(&query)
        {
            row["t3_thread"].clone()
        } else {
            Value::Null
        }
    };
    if !t3_thread.is_null()
        && s(&row, "harness") != "t3"
        && !jsonl::exists(&ctx.expand(s(&row, "path")))
    {
        let thread = core::pick_one(ctx, id, false)?;
        return Ok(extract(ctx, &thread, t3_thread));
    }
    Ok(extract(ctx, &row, t3_thread))
}

fn extract(ctx: &Context, row: &Session, t3_thread: Value) -> (Value, Vec<Value>) {
    let mut call_rows = Vec::new();
    let wall = parse_ts(&row["started"])
        .zip(parse_ts(&row["updated"]))
        .map(|(start, end)| (end - start).num_seconds());
    let mut out = json!({"harness":row["harness"],"id":row["id"],"t3_thread":t3_thread,
        "model":row["model"],"path":row["path"],"started":row["started"],"updated":row["updated"],
        "wall_s":wall,"calls":null,"input_tokens":null,"cache_read_tokens":null,"cache_write_tokens":null,
        "output_tokens":null,"reasoning_tokens":null,"total_tokens":null});
    let harness = s(row, "harness");
    if !["claude", "codex"].contains(&harness) {
        out["note"] = json!(if harness == "t3" {
            "provider transcript not found"
        } else {
            "usage is read for claude and codex sessions only"
        });
        return (out, call_rows);
    }
    // A missing transcript is not a session with no usage: keep the fields null.
    if !jsonl::exists(&ctx.expand(s(row, "path"))) {
        out["note"] = json!("transcript not found");
        return (out, call_rows);
    }
    for key in [
        "calls",
        "input_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "output_tokens",
        "total_tokens",
    ] {
        out[key] = json!(0);
    }
    let records = jsonl::records(ctx, &ctx.expand(s(row, "path")), 0, None);
    let n = |v: &Value, key: &str| v[key].as_u64().unwrap_or(0);
    if harness == "claude" {
        let mut positions = HashMap::new();
        let mut messages: Vec<(Value, Value, Vec<Value>, HashSet<String>)> = Vec::new();
        for (_, record) in records {
            if s(&record, "type") == "assistant"
                && let Some(id) = record["message"]["id"].as_str()
            {
                let index = *positions.entry(id.to_owned()).or_insert_with(|| {
                    messages.push((Value::Null, timestamp(&record), Vec::new(), HashSet::new()));
                    messages.len() - 1
                });
                let (message, _, tools, seen) = &mut messages[index];
                *message = record["message"].clone();
                if let Some(content) = message["content"].as_array() {
                    for block in content {
                        if s(block, "type") == "tool_use"
                            && let Some(name) = block["name"].as_str()
                            && seen.insert(s(block, "id").to_owned())
                        {
                            tools.push(json!(name));
                        }
                    }
                }
            }
        }
        let mut sums = [0u64; 4];
        let mut calls = 0u64;
        for (message, ts, tools, _) in &messages {
            if s(message, "model") == "<synthetic>" || !message["usage"].is_object() {
                continue;
            }
            calls += 1;
            let counters = [
                n(&message["usage"], "input_tokens"),
                n(&message["usage"], "cache_read_input_tokens"),
                n(&message["usage"], "cache_creation_input_tokens"),
                n(&message["usage"], "output_tokens"),
            ];
            call_rows.push(json!({"id":row["id"],"harness":harness,"ts":ts,"model":message["model"],"input_tokens":counters[0],"cache_read_tokens":counters[1],"cache_write_tokens":counters[2],"output_tokens":counters[3],"reasoning_tokens":null,"total_tokens":counters.iter().sum::<u64>(),"tools":tools}));
            for (sum, count) in sums.iter_mut().zip(counters) {
                *sum += count;
            }
        }
        out["calls"] = json!(calls);
        for (key, sum) in [
            "input_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "output_tokens",
        ]
        .iter()
        .zip(sums)
        {
            out[*key] = json!(sum);
        }
        out["total_tokens"] = json!(sums.iter().sum::<u64>());
    } else {
        let mut previous = Value::Null;
        let mut tools = Vec::new();
        let mut calls = 0;
        out["reasoning_tokens"] = json!(0);
        for (_, record) in records {
            let payload = &record["payload"];
            let usage = &payload["info"]["total_token_usage"];
            let kind = s(payload, "type");
            if s(&record, "type") == "response_item"
                && ["function_call", "custom_tool_call", "local_shell_call"].contains(&kind)
            {
                tools.push(json!(if s(payload, "name").is_empty() {
                    kind
                } else {
                    s(payload, "name")
                }));
            }
            if s(&record, "type") != "event_msg"
                || s(payload, "type") != "token_count"
                || !usage.is_object()
            {
                continue;
            }
            let total = n(usage, "total_tokens");
            if total > n(&previous, "total_tokens") {
                calls += 1;
                let delta = |key| n(usage, key).saturating_sub(n(&previous, key));
                call_rows.push(json!({"id":row["id"],"harness":harness,"ts":timestamp(&record),"model":row["model"],"input_tokens":delta("input_tokens").saturating_sub(delta("cached_input_tokens")),"cache_read_tokens":delta("cached_input_tokens"),"cache_write_tokens":delta("cache_write_input_tokens"),"output_tokens":delta("output_tokens"),"reasoning_tokens":delta("reasoning_output_tokens"),"total_tokens":delta("total_tokens"),"tools":std::mem::take(&mut tools)}));
            }
            previous = usage.clone();
            out["input_tokens"] =
                json!(n(usage, "input_tokens").saturating_sub(n(usage, "cached_input_tokens")));
            for (key, source) in [
                ("cache_read_tokens", "cached_input_tokens"),
                ("cache_write_tokens", "cache_write_input_tokens"),
                ("output_tokens", "output_tokens"),
                ("reasoning_tokens", "reasoning_output_tokens"),
                ("total_tokens", "total_tokens"),
            ] {
                out[key] = json!(n(usage, source));
            }
        }
        out["calls"] = json!(calls);
    }
    (out, call_rows)
}

fn timestamp(record: &Value) -> Value {
    json!(parse_ts(&record["timestamp"]).map(|ts| ts.to_rfc3339_opts(SecondsFormat::AutoSi, true)))
}

pub fn window(
    session: &mut Value,
    calls: &mut Vec<Value>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) {
    if since.is_none() && until.is_none() {
        return;
    }
    calls.retain(|call| {
        parse_ts(&call["ts"]).is_some_and(|ts| {
            since.is_none_or(|start| ts >= start) && until.is_none_or(|end| ts <= end)
        })
    });
    session["window"] = json!({"since":since.map(|ts| ts.to_rfc3339_opts(SecondsFormat::AutoSi, true)),"until":until.map(|ts| ts.to_rfc3339_opts(SecondsFormat::AutoSi, true))});
    if !session["note"].is_null() {
        return;
    }
    session["calls"] = json!(calls.len());
    for key in [
        "input_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "output_tokens",
        "reasoning_tokens",
        "total_tokens",
    ] {
        if key == "reasoning_tokens" && s(session, "harness") == "claude" {
            continue;
        }
        session[key] = json!(
            calls
                .iter()
                .map(|call| call[key].as_u64().unwrap_or(0))
                .sum::<u64>()
        );
    }
}

pub fn calls_text(rows: &[Value]) -> String {
    let mut out = String::from(
        "id\tharness\tts\tmodel\tinput\tcache_read\tcache_write\toutput\treasoning\ttotal\ttools\n",
    );
    for row in rows {
        let mut fields = [
            "id",
            "harness",
            "ts",
            "model",
            "input_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "output_tokens",
            "reasoning_tokens",
            "total_tokens",
        ]
        .map(|key| match &row[key] {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            value => value.to_string(),
        })
        .to_vec();
        fields.push(
            row["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str(&fields.join("\t"));
        out.push('\n');
    }
    out
}

pub fn text(rows: &[Value]) -> String {
    let mut out = String::from(
        "id\tharness\tcalls\tinput\tcache_read\tcache_write\toutput\treasoning\ttotal\twall_s\n",
    );
    for row in rows {
        out.push_str(
            &[
                "id",
                "harness",
                "calls",
                "input_tokens",
                "cache_read_tokens",
                "cache_write_tokens",
                "output_tokens",
                "reasoning_tokens",
                "total_tokens",
                "wall_s",
            ]
            .map(|key| match &row[key] {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                value => value.to_string(),
            })
            .join("\t"),
        );
        out.push('\n');
    }
    out
}
