use crate::{
    context::Context,
    model::{Event, Session},
    util::*,
};
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::{collections::BTreeSet, path::Path, sync::LazyLock};
fn truth(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.),
    }
}
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Null => "None".into(),
        _ => python_json(v),
    }
}
pub fn tool_summary(name: &str, input: &Value) -> String {
    tool_summary_limit(name, input, 160)
}
pub fn tool_summary_unclipped(name: &str, input: &Value) -> String {
    tool_summary_limit(name, input, usize::MAX)
}
/// The input fields a tool summary shows, in order of preference.
const SUMMARY_KEYS: [&str; 12] = [
    "command",
    "cmd",
    "file_path",
    "path",
    "filePath",
    "pattern",
    "query",
    "url",
    "description",
    "prompt",
    "code",
    "input",
];
/// The tool input `show --full` prints, losslessly. A text input (Codex code mode,
/// patches) is printed as is. An object prints the field the summary picks first,
/// strings byte for byte, then every other field on its own indented line; arrays
/// and other values print as JSON so argument boundaries survive.
pub fn tool_input_verbatim(_name: &str, input: &Value) -> String {
    let parsed;
    let input = match input.as_str() {
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(v) if v.is_object() => {
                parsed = v;
                &parsed
            }
            _ => return text.replace('\0', ""),
        },
        None => input,
    };
    let Some(obj) = input.as_object() else {
        return if input.is_null() {
            String::new()
        } else {
            input.to_string()
        };
    };
    let field = |v: &Value| match v {
        Value::String(s) => s.replace('\0', ""),
        v => v.to_string(),
    };
    let Some(primary) = SUMMARY_KEYS
        .iter()
        .copied()
        .find(|k| obj.get(*k).is_some_and(truth))
    else {
        return input.to_string();
    };
    let mut out = if ["command", "cmd", "code", "input"].contains(&primary) {
        field(&obj[primary])
    } else {
        format!("{primary}={}", field(&obj[primary]))
    };
    for (k, v) in obj {
        if k != primary && !v.is_null() {
            out.push_str(&format!("\n    {k}: {}", field(v)));
        }
    }
    out
}
fn tool_summary_limit(_name: &str, input: &Value, width: usize) -> String {
    if let Some(src) = input.as_str().filter(|s| s.contains("tools.")) {
        static N: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"tools\.([A-Za-z_$][\w$]*)\(").unwrap());
        static C: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"\b(?:cmd|command)\s*:\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`[^`]*`)"#)
                .unwrap()
        });
        let mut names = vec![];
        for m in N.captures_iter(src) {
            let name = m[1].to_string();
            if !names.contains(&name) {
                names.push(name);
            }
        }
        let cmd = C.captures(src).map(|m| {
            let q = &m[1];
            serde_json::from_str::<String>(q).unwrap_or_else(|_| {
                q[1..q.len() - 1]
                    .replace("\\n", "\n")
                    .replace("\\t", "\t")
                    .replace("\\r", "\r")
                    .replace("\\'", "'")
                    .replace("\\\\", "\\")
            })
        });
        return one_line(
            &format!(
                "{}{}",
                if names.is_empty() {
                    String::new()
                } else {
                    format!(
                        "[{}] ",
                        names.iter().take(4).cloned().collect::<Vec<_>>().join(",")
                    )
                },
                cmd.as_deref().filter(|s| !s.is_empty()).unwrap_or(src)
            ),
            width,
        );
    }
    let parsed;
    let input = if let Some(text) = input.as_str() {
        match serde_json::from_str::<Value>(text) {
            Ok(p) => {
                parsed = p;
                &parsed
            }
            Err(_) => return one_line(text, width),
        }
    } else {
        input
    };
    if !input.is_object() {
        return if input.is_null() {
            String::new()
        } else {
            one_line(&python_json(input), width)
        };
    }
    for key in SUMMARY_KEYS {
        if let Some(v) = input.get(key).filter(|v| truth(v)) {
            let val = if let Some(a) = v.as_array() {
                a.iter().map(py_str).collect::<Vec<_>>().join(" ")
            } else {
                py_str(v)
            };
            let prefix = if ["command", "cmd", "code", "input"].contains(&key) {
                String::new()
            } else {
                format!("{key}=")
            };
            let extra = if key == "description" {
                input
                    .get("subagent_type")
                    .filter(|v| truth(v))
                    .map(|v| format!(" [{}]", py_str(v)))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            return format!("{}{extra}", one_line(&format!("{prefix}{val}"), width));
        }
    }
    one_line(&python_json(input), width)
}
pub fn touched_paths(name: &str, input: &Value) -> Vec<String> {
    let parsed;
    let input = if let Some(text) = input.as_str() {
        parsed = serde_json::from_str(text).unwrap_or_else(|_| serde_json::json!({"input":text}));
        &parsed
    } else {
        input
    };
    let mut out = vec![];
    if !input.is_object() {
        return out;
    }
    if [
        "write",
        "edit",
        "multiedit",
        "notebookedit",
        "str_replace_based_edit_tool",
        "create_file",
        "edit_file",
        "write_file",
        "apply_patch",
        "patch",
    ]
    .contains(&name.to_lowercase().as_str())
    {
        for key in ["file_path", "path", "filePath", "notebook_path"] {
            if let Some(v) = input.get(key).filter(|v| truth(v)) {
                out.push(py_str(v));
            }
        }
    }
    if let Some(patch) = ["input", "patch", "code"]
        .iter()
        .filter_map(|k| input.get(k))
        .find(|v| truth(v))
        .and_then(Value::as_str)
    {
        static P: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"\*\*\* (?:Add|Update|Delete) File: ([^\n\\"'`]+)"#).unwrap()
        });
        for m in P.captures_iter(patch) {
            let p = m[1].trim().to_owned();
            if !out.contains(&p) {
                out.push(p)
            }
        }
    }
    out
}
pub fn fmt_row(ctx: &Context, r: &Session) -> String {
    fmt_row_width(ctx, r, 90)
}
pub fn fmt_row_width(ctx: &Context, r: &Session, width: usize) -> String {
    let upd = r
        .get("updated")
        .and_then(parse_ts)
        .or_else(|| r.get("started").and_then(parse_ts));
    let mut tags = vec![];
    if !s(r, "kind").is_empty() && s(r, "kind") != "main" {
        tags.push(s(r, "kind").to_owned())
    }
    for (key, prefix) in [("parent", "parent="), ("t3_thread", "t3=")] {
        if !s(r, key).is_empty() {
            tags.push(format!("{prefix}{}", s(r, key)))
        }
    }
    if s(r, "harness") == "t3" && !s(r, "provider").is_empty() {
        tags.push(format!(
            "provider={}:{}",
            if s(r, "provider_instance").is_empty() {
                s(r, "provider")
            } else {
                s(r, "provider_instance")
            },
            if s(r, "provider_id").is_empty() {
                "?"
            } else {
                s(r, "provider_id")
            }
        ))
    }
    if r.get("archived").is_some_and(truth) {
        tags.push("archived".into())
    }
    if let Some(a) = r
        .get("copies")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    {
        tags.push(format!(
            "also in {}",
            a.iter().map(py_str).collect::<Vec<_>>().join(", ")
        ))
    }
    if ctx.self_ids.contains(s(r, "id")) {
        tags.push("THIS SESSION".into())
    }
    format!(
        "{}  {:<8} {:<18} {}\n    {}  {}{}",
        fmt_ts(upd),
        s(r, "harness"),
        s(r, "home"),
        s(r, "id"),
        ctx.tilde(Path::new(if s(r, "cwd").is_empty() {
            "?"
        } else {
            s(r, "cwd")
        })),
        one_line(&label(r), width),
        if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join(", "))
        }
    )
}
pub fn header(ctx: &Context, row: &Session) -> String {
    let mut lines = vec![
        format!("# {} session {}", s(row, "harness"), s(row, "id")),
        format!("- title: {}", one_line(&label(row), 160)),
    ];
    if !s(row, "t3_thread").is_empty() {
        lines.push(format!(
            "- T3 thread: {} ({})",
            s(row, "t3_thread"),
            s(row, "t3_title")
        ))
    }
    for key in [
        "home",
        "path",
        "cwd",
        "branch",
        "model",
        "parent",
        "provider_instance",
        "provider",
        "provider_id",
        "originator",
    ] {
        if let Some(v) = row.get(key).filter(|v| truth(v)) {
            lines.push(format!("- {key}: {}", ctx.tilde(Path::new(&py_str(v)))));
        }
    }
    lines.push(format!(
        "- time: {} -> {} (local {})",
        fmt_ts(row.get("started").and_then(parse_ts)),
        fmt_ts(row.get("updated").and_then(parse_ts)),
        local_timezone_name(ctx.now)
    ));
    lines.join("\n")
}
pub fn turns_of(events: &[Event]) -> Vec<Vec<Event>> {
    let mut turns = vec![];
    let mut cur = vec![];
    for e in events {
        if ["prompt", "command"].contains(&e.role.as_str()) && !cur.is_empty() {
            turns.push(std::mem::take(&mut cur));
        }
        cur.push(e.clone());
    }
    if !cur.is_empty() {
        turns.push(cur)
    }
    turns
}
/// Under `--full`, tool calls and their results carry the call id so a result can be
/// matched to its call: ` [toolu_…]` after the tool name, ` [toolu_…]` after the arrow.
fn call_id(full: bool, id: Option<&str>) -> String {
    match id.filter(|s| full && !s.is_empty()) {
        Some(id) => format!(" [{id}]"),
        None => String::new(),
    }
}
pub fn render_event(e: &Event, full: bool, results: bool) -> String {
    let ts = if e.ts.is_some() {
        fmt_ts(e.ts)[11..].to_string()
    } else {
        "     ".into()
    };
    match e.role.as_str() {
        "prompt" | "command" => format!(
            "\n## {ts} USER{}\n{}",
            if e.role == "command" {
                " (command)"
            } else {
                ""
            },
            redact(&if full {
                e.text.clone()
            } else {
                clip(&e.text, 4000)
            })
        ),
        "assistant" => format!(
            "\n{ts} ASSISTANT{}:\n{}",
            e.phase
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|p| format!(" ({p})"))
                .unwrap_or_default(),
            redact(&if full {
                e.text.clone()
            } else {
                clip(&e.text, 2500)
            })
        ),
        "tool" => {
            let name = e.name.as_deref().unwrap_or("");
            let input = e.input.as_ref().unwrap_or(&Value::Null);
            format!(
                "{ts}   tool {}{}: {}",
                e.name.as_deref().unwrap_or("None"),
                call_id(full, e.id.as_deref()),
                redact(&if full {
                    tool_input_verbatim(name, input)
                } else {
                    tool_summary(name, input)
                })
            )
        }
        "result" if e.error || results => format!(
            "{ts}   {}{}{}",
            if e.error { "! tool error" } else { "->" },
            match call_id(full, e.tool_use_id.as_deref()) {
                id if id.is_empty() && e.error => ": ".to_string(),
                id if id.is_empty() => " ".to_string(),
                id if e.error => format!("{id}: "),
                id => format!("{id} "),
            },
            redact(&if full {
                e.text.clone()
            } else {
                one_line(&e.text, 300)
            })
        ),
        "notice" => format!(
            "{ts}   [notice] {}",
            redact(&if full {
                e.text.clone()
            } else {
                one_line(&e.text, 300)
            })
        ),
        "system" => format!("{ts}   [system] {}", redact(&one_line(&e.text, 300))),
        _ => String::new(),
    }
}
pub fn parse_range(spec: Option<&str>, n: usize) -> Result<Option<BTreeSet<usize>>> {
    let Some(spec) = spec.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let mut out = BTreeSet::new();
    static RANGE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(-?\d+)\s*-\s*(-?\d+)$").unwrap());
    for part in spec.split(',').map(str::trim) {
        let (a, b) = if let Some(c) = RANGE.captures(part) {
            (c[1].parse::<i64>()?, c[2].parse::<i64>()?)
        } else {
            let i = part.parse::<i64>()?;
            (i, i)
        };
        let a = if a < 0 { a + n as i64 + 1 } else { a };
        let b = if b < 0 { b + n as i64 + 1 } else { b };
        for i in a.max(1)..=b.min(n as i64) {
            out.insert(i as usize);
        }
    }
    Ok(Some(out))
}
pub fn compress(nums: &BTreeSet<usize>) -> String {
    let mut ranges = vec![];
    let mut it = nums.iter().copied();
    let Some(mut start) = it.next() else {
        return "none".into();
    };
    let mut prev = start;
    for n in it {
        if n == prev + 1 {
            prev = n;
            continue;
        }
        ranges.push(if start == prev {
            start.to_string()
        } else {
            format!("{start}-{prev}")
        });
        start = n;
        prev = n;
    }
    ranges.push(if start == prev {
        start.to_string()
    } else {
        format!("{start}-{prev}")
    });
    ranges.join(",")
}

/// Use the OS timezone label; chrono's %Z prints a numeric offset instead.
pub fn local_timezone_name(at: chrono::DateTime<chrono::Utc>) -> String {
    if crate::util::utc_requested() {
        return "UTC".into();
    }
    native_timezone_name(at)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| at.with_timezone(&chrono::Local).format("%Z").to_string())
}
#[cfg(windows)]
fn native_timezone_name(at: chrono::DateTime<chrono::Utc>) -> Option<String> {
    use chrono::Datelike;
    use windows_sys::Win32::System::Time::{GetTimeZoneInformationForYear, TIME_ZONE_INFORMATION};
    let mut info = TIME_ZONE_INFORMATION::default();
    // SAFETY: info is writable and correctly sized; a null timezone selects the current OS zone.
    if unsafe {
        GetTimeZoneInformationForYear(
            at.year().clamp(1601, 30827) as u16,
            std::ptr::null(),
            &mut info,
        )
    } == 0
    {
        return None;
    }
    let bias = -at.with_timezone(&chrono::Local).offset().local_minus_utc() / 60;
    let daylight = info.DaylightDate.wMonth != 0
        && info.StandardDate.wMonth != 0
        && info.DaylightBias != info.StandardBias
        && bias == info.Bias + info.DaylightBias;
    let name = if daylight {
        &info.DaylightName
    } else {
        &info.StandardName
    };
    Some(String::from_utf16_lossy(
        &name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())],
    ))
}
#[cfg(unix)]
fn native_timezone_name(at: chrono::DateTime<chrono::Utc>) -> Option<String> {
    // time_t is narrower on some Unix targets; keep the checked conversion.
    #[allow(clippy::useless_conversion)]
    let seconds = at.timestamp().try_into().ok()?;
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r initializes local on success; both pointers remain valid for the call.
    if unsafe { libc::localtime_r(&seconds, local.as_mut_ptr()) }.is_null() {
        return None;
    }
    let local = unsafe { local.assume_init() };
    let mut label = [0u8; 256];
    // SAFETY: output is writable for its length and the format is NUL-terminated.
    let count = unsafe {
        libc::strftime(
            label.as_mut_ptr().cast(),
            label.len(),
            c"%Z".as_ptr(),
            &local,
        )
    };
    if count == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&label[..count]).into_owned())
}
#[cfg(not(any(windows, unix)))]
fn native_timezone_name(_at: chrono::DateTime<chrono::Utc>) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn code_mode_and_patches() {
        assert_eq!(
            tool_summary(
                "exec",
                &json!("text(await tools.exec_command({cmd: \"git status\\n\"}));")
            ),
            "[exec_command] git status"
        );
        assert_eq!(
            touched_paths(
                "exec",
                &json!(
                    "*** Update File: src/main.rs\n*** Add File: src/new.rs\n*** Update File: src/main.rs"
                )
            ),
            vec!["src/main.rs", "src/new.rs"]
        );
    }
    #[test]
    fn ranges_preserve_negative_and_empty_selection() {
        assert_eq!(
            compress(&parse_range(Some("2-4,-1"), 8).unwrap().unwrap()),
            "2-4,8"
        );
        assert_eq!(
            compress(&parse_range(Some("9-11"), 8).unwrap().unwrap()),
            "none"
        );
        assert!(parse_range(Some("bad"), 8).is_err());
    }
    #[test]
    fn notices_preserve_full_delegation_context_and_redact_secrets() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyz";
        let text = format!(
            "<codex_delegation>\n{secret}\n{}\noriginal user context\n</codex_delegation>",
            "x".repeat(320)
        );
        let e = Event::new("notice", text, None);
        let full_body = format!(
            "<codex_delegation>\n<redacted>\n{}\noriginal user context\n</codex_delegation>",
            "x".repeat(320)
        );
        let compact_body = format!("<codex_delegation> <redacted> {}…", "x".repeat(250));
        for results in [false, true] {
            assert_eq!(
                render_event(&e, false, results),
                format!("        [notice] {compact_body}")
            );
            assert_eq!(
                render_event(&e, true, results),
                format!("        [notice] {full_body}")
            );
        }
    }
    #[test]
    fn full_tool_calls_keep_the_input_verbatim_and_tag_call_ids() {
        // A command past the 160-char clip, with a newline, a run of spaces and a quote.
        let command = format!(
            "X=/tmp/a; diff <(sed 's/a7973897885/b/' f)  {}\n  echo \"done\"",
            "y".repeat(400)
        );
        let mut call = Event::new("tool", "", None);
        call.name = Some("Bash".into());
        call.input = Some(json!({"command": command, "description": "diff"}));
        call.id = Some("toolu_01ABC".into());
        let mut result = Event::new("result", "ok\nsecond", None);
        result.tool_use_id = Some("toolu_01ABC".into());

        assert_eq!(
            render_event(&call, true, false),
            format!("        tool Bash [toolu_01ABC]: {command}\n    description: diff")
        );
        assert_eq!(
            render_event(&result, true, true),
            "        -> [toolu_01ABC] ok\nsecond"
        );
        result.error = true;
        assert_eq!(
            render_event(&result, true, true),
            "        ! tool error [toolu_01ABC]: ok\nsecond"
        );

        // Without --full the summary still clips to one 160-char line and carries no id.
        let compact = render_event(&call, false, false);
        assert!(!compact.contains("toolu_01ABC"));
        let summary = compact.strip_prefix("        tool Bash: ").unwrap();
        assert_eq!(summary.chars().count(), 160);
        assert!(summary.ends_with('…') && !summary.contains('\n'));
        result.error = false;
        assert_eq!(render_event(&result, false, true), "        -> ok second");
    }
    #[test]
    fn full_tool_inputs_are_lossless_for_code_mode_structured_and_array_inputs() {
        let tool = |name: &str, input: Value| {
            let mut e = Event::new("tool", "", None);
            e.name = Some(name.into());
            e.input = Some(input);
            render_event(&e, true, false)
        };
        // Codex code mode: two commands and the logic around them, printed as is.
        let src = "const a = await tools.exec_command({cmd: \"git status\"});\nif (a.ok) {\n  await tools.exec_command({cmd: \"git   diff\"});\n}";
        assert_eq!(tool("exec", json!(src)), format!("        tool exec: {src}"));
        // Claude Write: the path first, then the whole content on its own field line.
        let content = "line one\n\n  indented line two\n";
        assert_eq!(
            tool("Write", json!({"file_path": "/r/a.txt", "content": content})),
            format!("        tool Write: file_path=/r/a.txt\n    content: {content}")
        );
        // Codex function_call arguments arrive as a JSON string; an argv array keeps its boundaries.
        let args = json!({"command": ["bash", "-lc", "echo 'a b'"], "workdir": "/r"}).to_string();
        assert_eq!(
            tool("shell", json!(args)),
            "        tool shell: [\"bash\",\"-lc\",\"echo 'a b'\"]\n    workdir: /r"
        );
        // An object with none of the summary keys prints as JSON.
        assert_eq!(
            tool("custom", json!({"a": 1, "b": [true]})),
            "        tool custom: {\"a\":1,\"b\":[true]}"
        );
        // The compact summary is unchanged: first command only, one line.
        let mut e = Event::new("tool", "", None);
        e.name = Some("exec".into());
        e.input = Some(json!(src));
        assert_eq!(render_event(&e, false, false), "        tool exec: [exec_command] git status");
    }
    #[test]
    fn full_tool_calls_still_redact_secrets() {
        let mut call = Event::new("tool", "", None);
        call.name = Some("Bash".into());
        call.input = Some(json!({"command": "curl -H 'x: sk-abcdefghijklmnopqrstuvwxyz' u"}));
        let out = render_event(&call, true, false);
        assert!(!out.contains("sk-abcdefghijklmnopqrstuvwxyz") && out.contains("<redacted>"));
    }
    #[test]
    fn full_results_preserve_multiline_bodies_and_redact_secrets() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyz";
        let text = format!("{secret}\n{}\nlast line", "x".repeat(320));
        let mut e = Event::new("result", text, None);
        let expected_body = format!("<redacted>\n{}\nlast line", "x".repeat(320));
        let compact_body = format!("<redacted> {}…", "x".repeat(269));
        for error in [false, true] {
            e.error = error;
            let prefix = if error { "! tool error: " } else { "-> " };
            assert_eq!(
                render_event(&e, false, true),
                format!("        {prefix}{compact_body}")
            );
            assert_eq!(
                render_event(&e, true, true),
                format!("        {prefix}{expected_body}")
            );
            assert!(!render_event(&e, true, true).contains(secret));
        }
    }
    #[test]
    fn notices_do_not_split_turns_and_results_are_opt_in() {
        let es = vec![
            Event::new("system", "startup", None),
            Event::new("prompt", "hello", None),
            Event::new("notice", "notification", None),
            Event::new("assistant", "answer", None),
            Event::new("command", "continue", None),
        ];
        assert_eq!(
            turns_of(&es).iter().map(Vec::len).collect::<Vec<_>>(),
            vec![1, 3, 1]
        );
        assert!(render_event(&Event::new("result", "success", None), false, false).is_empty());
        let mut e = Event::new("result", "sk-abcdefghijklmnopqrstuvwxyz", None);
        e.error = true;
        assert!(render_event(&e, false, false).contains("<redacted>"));
    }
}
