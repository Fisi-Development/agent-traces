use crate::{
    adapters::t3::T3,
    context::Context,
    core::{self, Filters},
    storage::sqlite::truth,
    util::{parse_ts, redact, s},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

fn machine_prefix(text: &str) -> bool {
    [
        "[telepathica]",
        "[agent-mailbox]",
        "[lookout-nudge]",
        "<task-notification>",
        "Delegated task ",
        "<command-",
        "Base directory for this skill",
    ]
    .iter()
    .any(|prefix| text.trim_start().starts_with(prefix))
}

// agent-guard runs its LLM classifier from this directory. On Windows it goes through the Codex
// app server, so the session carries no exec or SDK marker and only the cwd identifies it.
fn guard_classifier_cwd(cwd: &str) -> bool {
    cwd.replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
        .ends_with("/.agent-guard/llm-cwd")
}

pub fn collect(
    ctx: &Context,
    filters: &Filters,
    max_chars: usize,
    include_machine: bool,
) -> Result<Vec<Value>> {
    // Gather before filtering so a harness filter cannot hide its T3 provenance.
    core::gather(
        ctx,
        &Filters {
            since: filters.since,
            until: filters.until,
            subagents: include_machine,
            ..Default::default()
        },
    )?;
    let selected: Vec<_> = ctx
        .known_sessions
        .borrow()
        .iter()
        .filter(|row| {
            filters
                .harness
                .as_ref()
                .is_none_or(|hs| hs.split(',').any(|h| h == s(row, "harness")))
        })
        .filter(|row| {
            filters
                .since
                .is_none_or(|since| parse_ts(&row["updated"]).is_none_or(|t| t >= since))
        })
        .filter(|row| {
            filters
                .until
                .is_none_or(|until| parse_ts(&row["started"]).unwrap_or(ctx.now) <= until)
        })
        .cloned()
        .collect();
    let rows = if filters
        .harness
        .as_ref()
        .is_none_or(|hs| hs.split(',').any(|h| h == "t3"))
    {
        core::merge_t3(selected)
    } else {
        selected
    };
    let threads: HashMap<_, _> = ctx
        .known_sessions
        .borrow()
        .iter()
        .filter(|r| s(r, "harness") == "t3")
        .map(|r| (s(r, "id").to_owned(), r.clone()))
        .collect();
    let mut links = HashMap::new();
    for t in threads.values() {
        links.insert(
            (s(t, "provider").to_owned(), s(t, "provider_id").to_owned()),
            s(t, "id").to_owned(),
        );
    }
    for link in T3::prompt_links(ctx)? {
        let driver = match s(&link, "driver") {
            "claudeAgent" => "claude",
            d => d,
        };
        links.insert(
            (driver.to_owned(), s(&link, "native_id").to_owned()),
            s(&link, "thread_id").to_owned(),
        );
    }
    let mut seen_threads = HashSet::new();
    let mut output = vec![];
    for mut row in rows {
        let harness = s(&row, "harness").to_owned();
        if filters
            .harness
            .as_ref()
            .is_some_and(|hs| !hs.split(',').any(|h| h == harness))
            || filters
                .cwd
                .as_ref()
                .is_some_and(|cwd| !core::cwd_match(ctx, s(&row, "cwd"), cwd))
        {
            continue;
        }
        let thread_id = if harness == "t3" {
            Some(s(&row, "id").to_owned())
        } else {
            links
                .get(&(harness.clone(), s(&row, "id").to_owned()))
                .cloned()
        };
        let events = if let Some(id) = &thread_id {
            if !seen_threads.insert(id.clone()) {
                continue;
            }
            let Some(thread) = threads.get(id) else {
                continue;
            };
            row["t3_thread"] = json!(id);
            T3::user_turns(ctx, thread)?
        } else {
            core::adapter(&harness)?.prompt_events(ctx, &row)?
        };
        let subagent = s(&row, "kind") == "subagent" || truth(&row["parent"]);
        let launched = s(&row, "kind") == "worker"
            || matches!(s(&row, "entrypoint"), "agent" | "delegate" | "telepathica")
            || matches!(s(&row, "originator"), "agent" | "delegate" | "telepathica");
        let classifier = guard_classifier_cwd(s(&row, "cwd"));
        let mut turn = 0;
        for event in events {
            let Some((text, excluded)) = event.user_turn else {
                continue;
            };
            let index = turn;
            turn += 1;
            let (classified, _) =
                crate::adapters::claude::classify_claude_text(&text, &Value::Null);
            let machine = excluded
                || (thread_id.is_none()
                    && (subagent
                        || classifier
                        || (index == 0 && launched)
                        || machine_prefix(&text)
                        || classified != "prompt"));
            if machine && !include_machine {
                continue;
            }
            let Some(time) = event.ts else {
                ctx.diagnostic(
                    "missing_prompt_time",
                    Some(&harness),
                    None,
                    format!("{} user turn {index} has no timestamp", core::address(&row)),
                );
                continue;
            };
            if filters.since.is_some_and(|since| time < since)
                || filters.until.is_some_and(|until| time > until)
            {
                continue;
            }
            let chars = text.chars().count();
            let redacted = redact(&text);
            let clipped = if redacted.chars().count() > max_chars && max_chars > 0 {
                redacted.chars().take(max_chars - 1).collect::<String>() + "…"
            } else {
                redacted.chars().take(max_chars).collect()
            };
            output.push(json!({"address":core::address(&row), "harness":harness, "host":core::local_host(),
                "time":time.to_rfc3339_opts(chrono::SecondsFormat::Micros, true), "cwd":row["cwd"],
                "t3_thread":thread_id, "turn":index, "origin":if machine { "machine" } else if thread_id.is_some() { "t3" } else { "heuristic" },
                "text":clipped,"chars":chars}));
        }
    }
    output.sort_by(|a, b| {
        s(a, "time")
            .cmp(s(b, "time"))
            .then_with(|| s(a, "address").cmp(s(b, "address")))
            .then_with(|| a["turn"].as_u64().cmp(&b["turn"].as_u64()))
    });
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::guard_classifier_cwd;

    #[test]
    fn guard_classifier_cwd_matches_windows_and_unix_forms() {
        assert!(guard_classifier_cwd(r"\\?\C:\Users\u\.agent-guard\llm-cwd"));
        assert!(guard_classifier_cwd("/home/u/.agent-guard/llm-cwd/"));
        assert!(!guard_classifier_cwd("/home/u/git/agent-guard/llm-cwd"));
        assert!(!guard_classifier_cwd("/home/u/.agent-guard/llm-cwd/sub"));
        assert!(!guard_classifier_cwd(""));
    }
}
