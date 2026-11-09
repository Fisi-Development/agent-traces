//! Full-transcript usage extraction, kept separate from session listing.
use crate::{
    adapters::opencode::OpenCode,
    context::Context,
    core,
    model::Session,
    storage::{jsonl, sqlite},
    util::*,
};
use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::collections::HashMap;

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
        && !["t3", "opencode"].contains(&s(&row, "harness"))
        && !jsonl::exists(&ctx.expand(s(&row, "path")))
    {
        let thread = core::pick_one(ctx, id, false)?;
        return Ok(extract(ctx, &thread, t3_thread));
    }
    Ok(extract(ctx, &row, t3_thread))
}

fn extract(ctx: &Context, row: &Session, t3_thread: Value) -> (Value, Vec<Value>) {
    let finalized = core::legacy_row(row);
    let mut call_rows = Vec::new();
    let wall = parse_ts(&row["started"])
        .zip(parse_ts(&row["updated"]))
        .map(|(start, end)| (end - start).num_seconds());
    let mut out = json!({"harness":row["harness"],"id":row["id"],"t3_thread":t3_thread,
        "model":row["model"],"path":row["path"],"started":row["started"],"updated":row["updated"],
        "wall_s":wall,"calls":null,"input_tokens":null,"cache_read_tokens":null,"cache_write_tokens":null,
        "output_tokens":null,"reasoning_tokens":null,"total_tokens":null});
    out["launch"] = json!({"surface":finalized["launch_surface"],"subject":finalized["launch_subject"],"launcher":finalized["launch_launcher"],"parent":finalized["launch_parent"]});
    let harness = s(row, "harness");
    if harness == "opencode" {
        let path = OpenCode::path(ctx);
        let messages = sqlite::open(ctx, "opencode", &path).and_then(|con| {
            sqlite::query(
                &con,
                "select json_valid(data) as valid, case when json_valid(data) then json_extract(data,'$.tokens') end as tokens, case when json_valid(data) then json_extract(data,'$.time.created') end as created, case when json_valid(data) then json_extract(data,'$.model.id') end as model, time_created from session_message where session_id = ?1 and type in ('assistant','compaction') order by seq",
                &[row["id"].clone()],
            ).inspect_err(|error| sqlite::report(ctx, "opencode", &path, error))
        });
        let messages = match messages {
            Ok(messages) => messages,
            Err(_) => {
                out["note"] = json!("store unreadable");
                return (out, call_rows);
            }
        };
        let n = |value: &Value, key: &str| value[key].as_u64().unwrap_or(0);
        let keys = [
            "input_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "output_tokens",
            "reasoning_tokens",
            "total_tokens",
        ];
        for key in keys {
            out[key] = json!(0);
        }
        for message in messages {
            // Only the token, time and model fields are read; message text never leaves SQLite.
            if !sqlite::truth(&message["valid"]) {
                ctx.diagnostic(
                    "malformed_record",
                    Some("opencode"),
                    Some(&path),
                    format!("session_message for {} is not valid JSON", s(row, "id")),
                );
                continue;
            }
            let Ok(tokens) = serde_json::from_str::<Value>(s(&message, "tokens")) else {
                continue;
            };
            if !tokens.is_object() {
                continue;
            }
            let tokens = &tokens;
            let input = n(tokens, "input");
            let read = n(&tokens["cache"], "read");
            let write = n(&tokens["cache"], "write");
            let reasoning = n(tokens, "reasoning");
            let output = n(tokens, "output") + reasoning;
            let ts = parse_ts(&message["created"])
                .or_else(|| parse_ts(&message["time_created"]))
                .map(|ts| ts.to_rfc3339_opts(SecondsFormat::AutoSi, true));
            let model = message["model"]
                .as_str()
                .map_or_else(|| row["model"].clone(), |id| json!(id));
            let call = json!({"id":row["id"],"harness":harness,"ts":ts,"model":model,"input_tokens":input,"cache_read_tokens":read,"cache_write_tokens":write,"output_tokens":output,"reasoning_tokens":reasoning,"total_tokens":input+read+write+output,"tools":[],"dedupe_key":null,"speed":"standard","reported_cost_usd":null});
            for key in keys {
                out[key] = json!(n(&out, key) + n(&call, key));
            }
            call_rows.push(call);
        }
        out["calls"] = json!(call_rows.len());
        return (out, call_rows);
    }
    if !["claude", "codex"].contains(&harness) {
        out["note"] = json!(if harness == "t3" {
            "provider transcript not found"
        } else {
            "usage is read for claude, codex and opencode sessions only"
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
    if harness == "claude" {
        let mut positions: HashMap<String, usize> = HashMap::new();
        for (_, record) in records {
            let message = &record["message"];
            let usage = &message["usage"];
            let model = s(message, "model");
            if s(&record, "type") != "assistant"
                || !usage.is_object()
                || usage_timestamp(&record).is_none()
                || model.is_empty()
                || model == "<synthetic>"
            {
                continue;
            }
            let key = match (message["id"].as_str(), record["requestId"].as_str()) {
                (None, None) => None,
                (id, request) => Some(format!("{}:{}", id.unwrap_or(""), request.unwrap_or(""))),
            };
            let tools = message["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|block| s(block, "type") == "tool_use")
                .filter_map(|block| block["name"].as_str())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let speed = if s(usage, "speed") == "fast" {
                "fast"
            } else {
                "standard"
            };
            let cost = record["costUSD"].as_f64().filter(|n| n.is_finite());
            let mut candidates = vec![claude_call(
                row,
                &record,
                usage,
                model,
                key.clone(),
                speed,
                cost,
            )];
            for (index, advisor) in usage["iterations"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                let model = s(advisor, "model");
                if s(advisor, "type") == "advisor_message"
                    && !model.is_empty()
                    && model != "<synthetic>"
                {
                    candidates.push(claude_call(
                        row,
                        &record,
                        advisor,
                        model,
                        key.as_ref().map(|key| format!("{key}:advisor:{index}")),
                        speed,
                        None,
                    ));
                }
            }
            for (index, call) in candidates.into_iter().enumerate() {
                let position = if let Some(key) = call["dedupe_key"].as_str() {
                    *positions.entry(key.to_owned()).or_insert_with(|| {
                        call_rows.push(call.clone());
                        call_rows.len() - 1
                    })
                } else {
                    call_rows.push(call);
                    call_rows.len() - 1
                };
                if index == 0 {
                    let target = call_rows[position]["tools"].as_array_mut().unwrap();
                    for tool in &tools {
                        let tool = json!(tool);
                        if !target.contains(&tool) {
                            target.push(tool);
                        }
                    }
                }
            }
        }
    } else {
        let mut state = CodexState::default();
        let mut tools = Vec::new();
        let mut occurrences = HashMap::new();
        for (_, record) in records {
            let payload = &record["payload"];
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
            for request in state.parse(&record) {
                let [input, cached, write, output, reasoning, _] = request.usage.0;
                let uncached = input - cached - write;
                let session_id = state.session_id.as_deref().unwrap_or_else(|| s(row, "id"));
                let prefix = format!(
                    "codex:{session_id}:{}:{}:{uncached}:{cached}:{write}:{output}",
                    request.ts.timestamp_millis(),
                    request.model
                );
                let occurrence = occurrences.entry(prefix.clone()).or_insert(0);
                *occurrence += 1;
                call_rows.push(json!({"id":row["id"],"harness":harness,
                    "ts":request.ts.to_rfc3339_opts(SecondsFormat::AutoSi, true),"model":request.model,
                    "input_tokens":uncached,"cache_read_tokens":cached,"cache_write_tokens":write,
                    "output_tokens":output,"reasoning_tokens":reasoning,"total_tokens":input+output,
                    "tools":std::mem::take(&mut tools),"dedupe_key":format!("{prefix}:{occurrence}"),
                    "speed":request.speed,"reported_cost_usd":null}));
            }
        }
    }
    sum_calls(&mut out, &call_rows);
    (out, call_rows)
}

fn claude_call(
    row: &Value,
    record: &Value,
    usage: &Value,
    model: &str,
    key: Option<String>,
    speed: &str,
    cost: Option<f64>,
) -> Value {
    let n = |key: &str| {
        usage[key]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.0)
            .map_or(0, |n| n.trunc() as u64)
    };
    let (input, cached, write, output) = (
        n("input_tokens"),
        n("cache_read_input_tokens"),
        n("cache_creation_input_tokens"),
        n("output_tokens"),
    );
    json!({"id":row["id"],"harness":"claude","ts":timestamp(record),"model":model,
        "input_tokens":input,"cache_read_tokens":cached,"cache_write_tokens":write,
        "output_tokens":output,"reasoning_tokens":null,"total_tokens":input+cached+write+output,
        "tools":[],"dedupe_key":key,"speed":speed,"reported_cost_usd":cost})
}

fn codex_integer(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        let number = value.as_f64()?;
        (number.is_finite() && number >= 0.0 && number.fract() == 0.0 && number < u64::MAX as f64)
            .then_some(number as u64)
    })
}

// Field order mirrors CODEX_USAGE_FIELDS in T3's codexUsage.ts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CodexUsage([u64; 6]);
impl CodexUsage {
    fn read(value: &Value) -> Option<Self> {
        let input = codex_integer(&value["input_tokens"])?;
        let output = codex_integer(&value["output_tokens"])?;
        let optional = |key: &str| match value.get(key) {
            None => Some(0),
            Some(value) => codex_integer(value),
        };
        let usage = Self([
            input,
            optional("cached_input_tokens")?,
            optional("cache_write_input_tokens")?,
            output,
            optional("reasoning_output_tokens")?,
            match value.get("total_tokens") {
                None => input.checked_add(output)?,
                Some(value) => codex_integer(value)?,
            },
        ]);
        usage.valid().then_some(usage)
    }
    fn valid(self) -> bool {
        let [input, cached, write, output, reasoning, total] = self.0;
        cached.checked_add(write).is_some_and(|n| n <= input)
            && reasoning <= output
            && input.checked_add(output).is_some_and(|n| total >= n)
    }
    fn add(self, other: Self) -> Option<Self> {
        let mut result = [0; 6];
        for (i, value) in result.iter_mut().enumerate() {
            *value = self.0[i].checked_add(other.0[i])?;
        }
        let result = Self(result);
        result.valid().then_some(result)
    }
    fn subtract(self, other: Self) -> Option<Self> {
        let mut result = [0; 6];
        for (i, value) in result.iter_mut().enumerate() {
            *value = self.0[i].checked_sub(other.0[i])?;
        }
        let result = Self(result);
        result.valid().then_some(result)
    }
    fn nonzero(self) -> bool {
        self.0[0] > 0 || self.0[3] > 0
    }
}

#[derive(Clone)]
struct CodexRequest {
    ts: DateTime<Utc>,
    model: String,
    speed: &'static str,
    usage: CodexUsage,
}
struct CodexState {
    model: String,
    speed: &'static str,
    session_id: Option<String>,
    last_signature: Option<CodexUsage>,
    cumulative: Option<CodexUsage>,
    sequence_start: Option<(i64, CodexUsage)>,
    pending: Option<Vec<CodexRequest>>,
    saw_meta: bool,
    fork_anchor: Option<i64>,
}
impl Default for CodexState {
    fn default() -> Self {
        Self {
            model: String::new(),
            speed: "standard",
            session_id: None,
            last_signature: None,
            cumulative: None,
            sequence_start: None,
            pending: None,
            saw_meta: false,
            fork_anchor: None,
        }
    }
}
impl CodexState {
    fn fork_copy(&mut self, ts: DateTime<Utc>) -> bool {
        let Some(anchor) = self.fork_anchor else {
            return false;
        };
        let ms = ts.timestamp_millis();
        if ms - anchor < 1000 {
            self.fork_anchor = Some(ms);
            return true;
        }
        self.fork_anchor = None;
        self.last_signature = None;
        false
    }
    fn rebase(&mut self, usage: CodexUsage) {
        self.cumulative = Some(usage);
        self.pending = None;
        if !usage.nonzero() {
            self.sequence_start = None;
        }
    }
    fn request(&self, ts: DateTime<Utc>, usage: CodexUsage) -> CodexRequest {
        CodexRequest {
            ts,
            model: self.model.clone(),
            speed: self.speed,
            usage,
        }
    }
    fn bridge(
        previous: Option<CodexUsage>,
        cumulative: CodexUsage,
        last: CodexUsage,
        pending: Option<Vec<CodexRequest>>,
    ) -> Option<Vec<CodexRequest>> {
        let previous = previous?;
        let mut pending = pending?;
        let mut sum = [0u64; 6];
        for request in &pending {
            for (i, value) in sum.iter_mut().enumerate() {
                *value = value.checked_add(request.usage.0[i])?;
            }
        }
        for (i, value) in sum.iter().enumerate() {
            if cumulative.0[i].checked_sub(previous.0[i])? != *value {
                return None;
            }
        }
        let final_request = pending.last_mut()?;
        final_request.usage = final_request.usage.subtract(last)?;
        pending.retain(|request| request.usage.nonzero());
        Some(pending)
    }
    fn parse(&mut self, record: &Value) -> Vec<CodexRequest> {
        let payload = &record["payload"];
        if !payload.is_object() {
            return vec![];
        }
        match s(record, "type") {
            "session_meta" => {
                if self.saw_meta {
                    return vec![];
                }
                self.saw_meta = true;
                if let Some(id) = payload
                    .get("id")
                    .filter(|id| !id.is_null())
                    .or_else(|| payload.get("session_id"))
                    .and_then(Value::as_str)
                {
                    self.session_id = Some(id.to_owned());
                }
                let fork = payload["forked_from_id"].is_string()
                    || payload["source"]["subagent"]["thread_spawn"]["parent_thread_id"]
                        .is_string();
                if fork {
                    self.fork_anchor = usage_timestamp(record).map(|ts| ts.timestamp_millis());
                }
                return vec![];
            }
            "turn_context" => {
                if let Some(model) = payload["model"].as_str() {
                    self.model = model.to_owned();
                }
                return vec![];
            }
            "token_usage_record" => {
                if let (Some(ts), Some(usage)) =
                    (usage_timestamp(record), CodexUsage::read(&payload["usage"]))
                    && !self.model.is_empty()
                    && !self.fork_copy(ts)
                {
                    let request = self.request(ts, usage);
                    self.pending.get_or_insert_with(Vec::new).push(request);
                }
                return vec![];
            }
            _ => {}
        }
        if s(payload, "type") == "thread_settings_applied" {
            if payload["thread_settings"].is_object() {
                self.speed = match s(&payload["thread_settings"], "service_tier") {
                    "priority" | "fast" => "fast",
                    "ultrafast" => "ultrafast",
                    _ => "standard",
                };
            }
            return vec![];
        }
        if s(payload, "type") != "token_count" || !payload["info"].is_object() {
            return vec![];
        }
        let Some(ts) = usage_timestamp(record) else {
            return vec![];
        };
        if self.model.is_empty() {
            return vec![];
        }
        let info = &payload["info"];
        let last = CodexUsage::read(&info["last_token_usage"]);
        let has_cumulative = !info["total_token_usage"].is_null();
        let cumulative = CodexUsage::read(&info["total_token_usage"]);
        if self.fork_copy(ts) {
            if let Some(cumulative) = cumulative {
                self.rebase(cumulative);
            } else if !has_cumulative
                && let Some(last) = last
                && Some(last) != self.last_signature
            {
                self.cumulative = self.cumulative.unwrap_or(CodexUsage([0; 6])).add(last);
            }
            self.last_signature = last;
            return vec![];
        }
        let previous = self.cumulative;
        let pending = self.pending.clone();
        if let Some(cumulative) = cumulative {
            self.rebase(cumulative);
        }
        let Some(last) = last else {
            return vec![];
        };
        if has_cumulative && cumulative.is_none() {
            return vec![];
        }
        if !last.nonzero() {
            self.last_signature = None;
            return vec![];
        }
        if cumulative.is_some() && cumulative == previous {
            return vec![];
        }
        if last.0[5] != last.0[0] + last.0[3] {
            return vec![];
        }
        let mut unreported = vec![];
        if let Some(cumulative) = cumulative {
            let restarted = cumulative == last;
            let sequence_start = (ts.timestamp_millis(), last);
            if restarted && self.sequence_start == Some(sequence_start) {
                self.cumulative = previous;
                return vec![];
            }
            let reconciled = match previous {
                None => cumulative.subtract(last).is_some(),
                Some(previous) => cumulative.subtract(previous) == Some(last),
            };
            if !restarted && !reconciled {
                let Some(bridged) = Self::bridge(previous, cumulative, last, pending) else {
                    return vec![];
                };
                unreported = bridged;
            }
            if restarted {
                self.sequence_start = Some(sequence_start);
            }
        } else {
            if Some(last) == self.last_signature {
                return vec![];
            }
            let Some(advanced) = previous.unwrap_or(CodexUsage([0; 6])).add(last) else {
                return vec![];
            };
            self.cumulative = Some(advanced);
        }
        self.last_signature = Some(last);
        unreported.push(self.request(ts, last));
        unreported
    }
}

fn usage_timestamp(record: &Value) -> Option<DateTime<Utc>> {
    record["timestamp"].as_str()?;
    let ts = parse_ts(&record["timestamp"])?;
    DateTime::from_timestamp_millis(ts.timestamp_millis())
}

fn timestamp(record: &Value) -> Value {
    json!(usage_timestamp(record).map(|ts| ts.to_rfc3339_opts(SecondsFormat::AutoSi, true)))
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
    sum_calls(session, calls);
}

pub fn sum_calls(session: &mut Value, calls: &[Value]) {
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
        "id\tharness\tts\tmodel\tinput\tcache_read\tcache_write\toutput\treasoning\ttotal\ttools\tdedupe_key\tspeed\treported_cost_usd\n",
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
        for key in ["dedupe_key", "speed", "reported_cost_usd"] {
            fields.push(match &row[key] {
                Value::Null => String::new(),
                Value::String(value) => value.clone(),
                value => value.to_string(),
            });
        }
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
