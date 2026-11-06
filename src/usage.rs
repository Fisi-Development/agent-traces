//! Full-transcript usage extraction, kept separate from session listing.
use crate::{context::Context, core, model::Session, storage::jsonl, util::*};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;

pub fn session(ctx: &Context, id: &str) -> Result<Value> {
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

fn extract(ctx: &Context, row: &Session, t3_thread: Value) -> Value {
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
        return out;
    }
    // A missing transcript is not a session with no usage: keep the fields null.
    if !jsonl::exists(&ctx.expand(s(row, "path"))) {
        out["note"] = json!("transcript not found");
        return out;
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
        let mut messages = HashMap::new();
        for (_, record) in records {
            if s(&record, "type") == "assistant"
                && let Some(id) = record["message"]["id"].as_str()
            {
                messages.insert(id.to_owned(), record["message"].clone());
            }
        }
        let mut sums = [0u64; 4];
        let mut calls = 0u64;
        for message in messages.values() {
            if s(message, "model") == "<synthetic>" || !message["usage"].is_object() {
                continue;
            }
            calls += 1;
            for (i, key) in [
                "input_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
                "output_tokens",
            ]
            .iter()
            .enumerate()
            {
                sums[i] += n(&message["usage"], key);
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
        let mut previous = 0;
        let mut calls = 0;
        out["reasoning_tokens"] = json!(0);
        for (_, record) in records {
            let payload = &record["payload"];
            let usage = &payload["info"]["total_token_usage"];
            if s(&record, "type") != "event_msg"
                || s(payload, "type") != "token_count"
                || !usage.is_object()
            {
                continue;
            }
            let total = n(usage, "total_tokens");
            if total > previous {
                calls += 1;
            }
            previous = total;
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
