//! Read-only commit evidence. Transcript strings never become harness records.
use crate::{
    commands::CommandOutput,
    context::Context,
    core::{self, Filters},
    model::{Event, Session},
    search,
    util::{parse_ts, s},
};
use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

fn stamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn valid_sha(t: &str) -> bool {
    (7..=40).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_hexdigit())
}

fn sha_tokens(text: &str) -> Vec<String> {
    // Split at all non-alphanumeric characters. A hex substring of an identifier
    // is not a SHA token, and neither is a truncated >40-character hex run.
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| valid_sha(t))
        .map(str::to_ascii_lowercase)
        .collect()
}

fn sha_match(recorded: &str, target: &str) -> Option<&'static str> {
    let recorded = recorded.to_ascii_lowercase();
    if !valid_sha(&recorded) || !(recorded.starts_with(target) || target.starts_with(&recorded)) {
        None
    } else if recorded.len() == 40 && target.len() == 40 {
        Some("full")
    } else {
        Some("prefix")
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

fn disambiguated(text: &str) -> Vec<String> {
    let mut ids: Vec<_> = text
        .lines()
        .filter(|l| l.len() == 40 && valid_sha(l))
        .map(str::to_ascii_lowercase)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn resolution(ids: &[String]) -> &'static str {
    match ids.len() {
        0 => "not_found",
        1 => "resolved",
        _ => "ambiguous",
    }
}

struct Repository {
    path: Option<PathBuf>,
    common: Option<PathBuf>,
    commit: Value,
    matches: HashMap<String, &'static str>,
    unique: HashMap<String, bool>,
    warning: Option<String>,
}

impl Repository {
    fn new(repo: Option<&Path>, rev: &str) -> Result<Self> {
        let mut this = Self {
            path: None,
            common: None,
            commit: json!({"status":"unchecked","oid":null,"candidates":[],"committed_at":null}),
            matches: HashMap::new(),
            unique: HashMap::new(),
            warning: None,
        };
        let cwd = std::env::current_dir()?;
        let path = repo.unwrap_or(&cwd);
        match git(path, &["rev-parse", "--is-inside-work-tree"]) {
            Ok(inside) if inside == "true" => {}
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                this.warning = Some("git is unavailable; commit resolution is unchecked".into());
                return Ok(this);
            }
            Err(_) | Ok(_) if repo.is_none() => return Ok(this),
            Err(e) => return Err(e),
            Ok(_) => bail!("--repo must be a git work tree"),
        }
        this.path = Some(std::path::absolute(path)?);
        this.common = Some(PathBuf::from(git(
            path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?));
        let ids = disambiguated(&git(
            path,
            &["rev-parse", &format!("--disambiguate={rev}")],
        )?);
        let mut commits = Vec::new();
        for id in ids {
            if git(path, &["cat-file", "-t", &id])? == "commit" {
                commits.push(id);
            }
        }
        this.commit["status"] = json!(resolution(&commits));
        this.commit["candidates"] = json!(commits);
        if commits.len() == 1 {
            this.commit["oid"] = json!(commits[0]);
            let time = git(path, &["show", "-s", "--format=%cI", &commits[0]])?;
            this.commit["committed_at"] = parse_ts(&json!(time)).map(stamp).into();
        }
        Ok(this)
    }

    fn repo_match(&mut self, cwd: &str) -> &'static str {
        self.matches.entry(cwd.to_owned()).or_insert_with(|| {
            let Some(common) = &self.common else {
                return "unknown";
            };
            if cwd.is_empty() {
                return "unknown";
            }
            let Ok(other) = git(
                Path::new(cwd),
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            ) else {
                return "unknown";
            };
            let a = std::fs::canonicalize(common).unwrap_or_else(|_| common.clone());
            let p = PathBuf::from(other);
            let b = std::fs::canonicalize(&p).unwrap_or(p);
            if a == b { "same" } else { "different" }
        })
    }

    fn abbrev_unique(&mut self, sha: &str) -> Option<bool> {
        let path = self.path.as_ref()?;
        Some(*self.unique.entry(sha.to_owned()).or_insert_with(|| {
            git(path, &["rev-parse", &format!("--disambiguate={sha}")])
                .is_ok_and(|s| disambiguated(&s).len() == 1)
        }))
    }
}

#[derive(Debug, PartialEq)]
struct Shell {
    commit: bool,
    form: &'static str,
}

fn git_subcommand(tokens: &[String]) -> Option<&str> {
    let mut i = 0;
    while tokens.get(i).is_some_and(|t| assignment(t)) {
        i += 1;
    }
    if tokens.get(i).map(String::as_str) != Some("git") {
        return None;
    }
    i += 1;
    loop {
        match tokens.get(i)?.as_str() {
            "-C" | "-c" => {
                tokens.get(i + 1)?;
                i += 2;
            }
            "--no-pager" => i += 1,
            t if t.starts_with("--git-dir=") || t.starts_with("--work-tree=") => i += 1,
            t => return Some(t),
        }
    }
}

fn assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(k, _)| {
        k.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn family(sub: &str) -> bool {
    ["commit", "merge", "cherry-pick", "revert", "rebase", "am"].contains(&sub)
}

fn shell(command: &str) -> Shell {
    let unknown = || Shell {
        commit: false,
        form: "unknown",
    };
    // This one message substitution is allowed. Keep its body opaque, including
    // quotes and shell separators, just as a shell message argument would be.
    static MESSAGE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)(-m\s+)"\$\(cat\s+<<'([A-Za-z_][A-Za-z_0-9]*)'\r?\n(.*?)\r?\n([A-Za-z_][A-Za-z_0-9]*)\r?\n?\)""#).unwrap()
    });
    let mut invalid_heredoc = false;
    let command = MESSAGE.replace_all(command, |c: &regex::Captures<'_>| {
        if c[2] != c[4] {
            invalid_heredoc = true;
        }
        format!("{}'message'", &c[1])
    });
    if invalid_heredoc {
        return unknown();
    }
    let mut segments = Vec::new();
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut active = false;
    let mut heredocs = Vec::new();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' || (c == '$' && chars.peek() == Some(&'(')) {
            return unknown();
        } else if c == '\\' && quote != Some('\'') {
            let Some(next) = chars.next() else {
                return unknown();
            };
            if next != '\n' {
                token.push(next);
                active = true;
            }
        } else if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                token.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            active = true;
        } else if c == '<' {
            if chars.next() != Some('<') {
                return unknown();
            }
            let strip_tabs = chars.peek() == Some(&'-');
            if strip_tabs {
                chars.next();
            }
            let Some(q @ ('\'' | '"')) = chars.next() else {
                return unknown();
            };
            let mut delimiter = String::new();
            loop {
                match chars.next() {
                    Some(c) if c == q => break,
                    Some(c) if c.is_ascii_alphanumeric() || c == '_' => delimiter.push(c),
                    _ => return unknown(),
                }
            }
            if !delimiter.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                return unknown();
            }
            heredocs.push((delimiter, strip_tabs));
        } else if c == '>' {
            // Only a standalone optional single-digit fd followed by >&digit.
            if !(token.is_empty() || (token.len() == 1 && token.as_bytes()[0].is_ascii_digit()))
                || chars.next() != Some('&')
                || !chars.next().is_some_and(|c| c.is_ascii_digit())
                || chars
                    .peek()
                    .is_some_and(|c| !c.is_whitespace() && !['&', '|', ';'].contains(c))
            {
                return unknown();
            }
            token.clear();
            active = false;
        } else if ['&', '|', ';', '\n'].contains(&c) {
            if c == '\n' {
                for (delimiter, strip_tabs) in heredocs.drain(..) {
                    loop {
                        let mut line = String::new();
                        let mut newline = false;
                        for c in chars.by_ref() {
                            if c == '\n' {
                                newline = true;
                                break;
                            }
                            line.push(c);
                        }
                        let line = line.strip_suffix('\r').unwrap_or(&line);
                        let line = if strip_tabs {
                            line.trim_start_matches('\t')
                        } else {
                            line
                        };
                        if line == delimiter {
                            break;
                        }
                        if !newline {
                            return unknown();
                        }
                    }
                }
            }
            if c == '&' && chars.peek() != Some(&'&') {
                return unknown();
            }
            if (c == '&' || c == '|') && chars.peek() == Some(&c) {
                chars.next();
            }
            if active {
                tokens.push(std::mem::take(&mut token));
                active = false;
            }
            if !tokens.is_empty() {
                segments.push(std::mem::take(&mut tokens));
            }
        } else if c.is_whitespace() {
            if active {
                tokens.push(std::mem::take(&mut token));
                active = false;
            }
        } else if ['<', '>', '(', ')', '#'].contains(&c) {
            return unknown();
        } else {
            token.push(c);
            active = true;
        }
    }
    if quote.is_some() || !heredocs.is_empty() {
        return unknown();
    }
    if active {
        tokens.push(token);
    }
    if !tokens.is_empty() {
        segments.push(tokens);
    }
    if segments.is_empty() {
        return unknown();
    }
    if segments.iter().any(|t| {
        t.iter()
            .find(|t| !assignment(t))
            .is_some_and(|t| ["eval", "bash", "sh", "pwsh"].contains(&t.as_str()))
    }) {
        return unknown();
    }
    let commits = segments
        .iter()
        .filter(|t| git_subcommand(t).is_some_and(family))
        .count();
    let safe = segments.iter().all(|t| {
        git_subcommand(t).is_some_and(|sub| {
            family(sub) || ["add", "rm", "mv", "status", "config"].contains(&sub)
        }) || t
            .iter()
            .find(|t| !assignment(t))
            .is_some_and(|t| ["cd", "pushd"].contains(&t.as_str()))
    });
    Shell {
        commit: commits > 0,
        form: if commits == 1 && safe {
            "single"
        } else if commits > 0 {
            "compound"
        } else {
            "unknown"
        },
    }
}

fn call_shell(event: &Event) -> Shell {
    let unknown = || Shell {
        commit: false,
        form: "unknown",
    };
    let Some(input) = &event.input else {
        return unknown();
    };
    if event.name.as_deref() == Some("exec") {
        return exec_command(input.as_str().unwrap_or("")).map_or_else(unknown, |c| shell(&c));
    }
    let parsed;
    let input = if let Some(raw) = input.as_str() {
        parsed = serde_json::from_str::<Value>(raw).unwrap_or(Value::Null);
        &parsed
    } else {
        input
    };
    let name = event.name.as_deref().unwrap_or("");
    if ![
        "Bash",
        "bash",
        "exec_command",
        "shell_command",
        "shell",
        "local_shell_call",
    ]
    .contains(&name)
    {
        return unknown();
    }
    input
        .get("command")
        .or_else(|| input.get("cmd"))
        .and_then(Value::as_str)
        .map_or_else(unknown, shell)
}

/// Tokenize the limited JavaScript surface we can inspect without executing it.
/// Literal strings and comments stay opaque, so quoted examples cannot be calls.
fn exec_command(script: &str) -> Option<String> {
    let mut tokens: Vec<(String, bool)> = Vec::new();
    let mut chars = script.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '`' {
            return None;
        }
        if c == '/' && chars.peek().is_some_and(|(_, c)| *c == '/') {
            for (_, c) in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '/' && chars.peek().is_some_and(|(_, c)| *c == '*') {
            chars.next();
            let mut ended = false;
            while let Some((_, c)) = chars.next() {
                if c == '*' && chars.peek().is_some_and(|(_, c)| *c == '/') {
                    chars.next();
                    ended = true;
                    break;
                }
            }
            if !ended {
                return None;
            }
            continue;
        }
        if c == '\'' || c == '"' {
            let mut end = None;
            while let Some((index, next)) = chars.next() {
                if next == '\\' {
                    chars.next()?;
                } else if next == c {
                    end = Some(index + 1);
                    break;
                }
            }
            let raw = &script[start..end?];
            let value = if c == '"' {
                serde_json::from_str(raw).ok()?
            } else {
                decode_single_literal(raw)?
            };
            tokens.push((value, true));
        } else if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
            let mut word = c.to_string();
            while chars
                .peek()
                .is_some_and(|(_, c)| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
            {
                word.push(chars.next()?.1);
            }
            tokens.push((word, false));
        } else {
            tokens.push((c.to_string(), false));
        }
    }
    let punct = |i: usize, text: &str| {
        tokens
            .get(i)
            .is_some_and(|(t, literal)| !literal && t == text)
    };
    let mut commands = Vec::new();
    let mut calls = 0;
    for i in 0..tokens.len() {
        if !["tools", ".", "exec_command", "("]
            .iter()
            .enumerate()
            .all(|(n, t)| punct(i + n, t))
        {
            continue;
        }
        calls += 1;
        if !punct(i + 4, "{") {
            return None;
        }
        let mut depth = 1;
        let mut j = i + 5;
        let mut command = None;
        while j < tokens.len() && depth > 0 {
            if depth == 1
                && tokens[j].0 == "cmd"
                && (punct(j - 1, "{") || punct(j - 1, ","))
                && punct(j + 1, ":")
            {
                let (value, literal) = tokens.get(j + 2)?;
                if !literal || !(punct(j + 3, ",") || punct(j + 3, "}")) || command.is_some() {
                    return None;
                }
                command = Some(value.clone());
            }
            if !tokens[j].1 {
                match tokens[j].0.as_str() {
                    "{" | "[" | "(" => depth += 1,
                    "}" | "]" | ")" => depth -= 1,
                    _ => {}
                }
            }
            j += 1;
        }
        if depth != 0 || !punct(j, ")") {
            return None;
        }
        commands.push(command?);
    }
    if calls == 1 && commands.len() == 1 {
        commands.pop()
    } else {
        None
    }
}

fn decode_single_literal(literal: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = literal[1..literal.len() - 1].chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(match chars.next()? {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                c @ ('\\' | '\'' | '"') => c,
                _ => return None,
            });
        } else {
            out.push(c);
        }
    }
    Some(out)
}

static BANNER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[([^\]\s]+)(?: \(root-commit\))? ([0-9a-fA-F]{7,40})\]").unwrap()
});

fn rank(claim: &str) -> usize {
    [
        "ran_commit",
        "commit_without_result",
        "observed",
        "mentioned",
    ]
    .iter()
    .position(|c| *c == claim)
    .unwrap_or(4)
}

struct ClaimBuilder<'a> {
    repo: &'a mut Repository,
    target: &'a str,
    path: String,
}

impl ClaimBuilder<'_> {
    // Each argument supplies one of the frozen evidence fields.
    #[allow(clippy::too_many_arguments)]
    fn claim(
        &mut self,
        claim: &str,
        strength: &str,
        sha: Option<&str>,
        event: &Event,
        form: Option<&str>,
        count: Option<usize>,
        record: Option<&Value>,
    ) -> Value {
        let sha = sha.map(str::to_ascii_lowercase);
        json!({"claim":claim,"strength":strength,"shell_form":form,"banner_count":count,
            "recorded_sha":sha,"sha_match":sha.as_deref().and_then(|s| sha_match(s, self.target)),
            "abbrev_unique":sha.as_deref().and_then(|s| self.repo.abbrev_unique(s)),
            "operation":record.and_then(|r| r.get("kind")).filter(|v| v.as_str().is_some_and(|s| ["committed","amended"].contains(&s))),
            "branch":record.and_then(|r| r.get("branch")).and_then(Value::as_str),
            "ts":event.ts.map(stamp),
            "source":{"path":self.path,"selector":event.tool_use_id.as_ref().or(event.id.as_ref()).map(|id| format!("tool_use_id={id}"))}})
    }

    fn claims(&mut self, events: &[Event], same_repo: bool) -> Vec<Value> {
        let mut claims = Vec::new();
        let calls: HashMap<_, _> = events
            .iter()
            .filter(|e| e.role == "tool")
            .filter_map(|e| e.id.as_ref().map(|id| (id, e)))
            .collect();
        for event in events {
            if event.role == "result" {
                let call = event.tool_use_id.as_ref().and_then(|id| calls.get(id));
                let classified = call.map(|e| call_shell(e));
                let form = classified.as_ref().map(|s| s.form);
                let committed = !event.error && classified.as_ref().is_some_and(|s| s.commit);
                let banners: Vec<_> = BANNER.captures_iter(&event.text).collect();
                let record = event.git.as_ref().and_then(|g| g.get("commit"));
                let mut accepted = HashSet::new();
                if let Some(record) = record {
                    let sha = s(record, "sha").to_ascii_lowercase();
                    if sha_match(&sha, self.target).is_some() {
                        claims.push(self.claim(
                            if !event.error && call.is_some() {
                                "ran_commit"
                            } else {
                                "observed"
                            },
                            "harness_record",
                            Some(&sha),
                            event,
                            form,
                            Some(banners.len()),
                            Some(record),
                        ));
                        accepted.insert(sha);
                    }
                }
                for banner in &banners {
                    let sha = banner[2].to_ascii_lowercase();
                    if sha_match(&sha, self.target).is_some() && !accepted.contains(&sha) {
                        let record = json!({"branch":&banner[1]});
                        claims.push(self.claim(
                            if committed { "ran_commit" } else { "observed" },
                            "tool_output",
                            Some(&sha),
                            event,
                            form,
                            Some(banners.len()),
                            Some(&record),
                        ));
                        accepted.insert(sha);
                    }
                }
                for sha in sha_tokens(&event.text) {
                    if sha_match(&sha, self.target).is_some() && accepted.insert(sha.clone()) {
                        claims.push(self.claim(
                            "observed",
                            "text",
                            Some(&sha),
                            event,
                            form,
                            Some(banners.len()),
                            None,
                        ));
                    }
                }
            } else {
                let input = event
                    .input
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_default();
                for sha in sha_tokens(&format!("{} {input}", event.text)) {
                    if sha_match(&sha, self.target).is_some() {
                        claims.push(self.claim(
                            "mentioned",
                            "text",
                            Some(&sha),
                            event,
                            None,
                            None,
                            None,
                        ));
                    }
                }
                if event.role == "tool"
                    && same_repo
                    && call_shell(event).commit
                    && !events.iter().any(|e| {
                        e.role == "result" && event.id.is_some() && e.tool_use_id == event.id
                    })
                    && event
                        .ts
                        .zip(parse_ts(&self.repo.commit["committed_at"]))
                        .is_some_and(|(a, b)| (a - b).abs() <= Duration::minutes(15))
                {
                    claims.push(self.claim(
                        "commit_without_result",
                        "text",
                        None,
                        event,
                        Some(call_shell(event).form),
                        None,
                        None,
                    ));
                }
            }
        }
        claims
    }
}

pub fn run(
    ctx: &Context,
    rev: &str,
    repo: Option<&Path>,
    mut filters: Filters,
    json_output: bool,
) -> Result<CommandOutput> {
    if !valid_sha(rev) {
        bail!("REV must contain 7-40 hexadecimal characters");
    }
    let rev_lower = rev.to_ascii_lowercase();
    let mut repo = Repository::new(repo, &rev_lower)?;
    filters.subagents = true;
    let mut rows = core::gather(ctx, &filters)?;
    core::exclude_self(ctx, &mut rows);
    // gather attaches the T3 address to provider rows and hides the T3 row.
    // Restore those native sources for evidence so both transcripts contribute.
    let threads: HashSet<_> = rows
        .iter()
        .map(|r| s(r, "t3_thread").to_owned())
        .filter(|t| !t.is_empty())
        .collect();
    rows.extend(
        ctx.known_sessions
            .borrow()
            .iter()
            .filter(|r| {
                s(r, "harness") == "t3"
                    && threads.contains(s(r, "id"))
                    && filters
                        .cwd
                        .as_ref()
                        .is_none_or(|cwd| core::cwd_match(ctx, s(r, "cwd"), cwd))
                    && filters
                        .since
                        .is_none_or(|since| parse_ts(&r["updated"]).is_none_or(|t| t >= since))
                    && filters
                        .until
                        .is_none_or(|until| parse_ts(&r["started"]).unwrap_or(ctx.now) <= until)
            })
            .cloned(),
    );
    core::exclude_self(ctx, &mut rows);
    let scope = search::roles("all")?;
    let before = rows.len();
    rows.retain(|row| {
        s(row, "harness") != "codex" || crate::storage::jsonl::exists(&ctx.expand(s(row, "path")))
    });
    let missing = before - rows.len();
    if missing > 0 {
        ctx.cover(
            "codex",
            None,
            "search",
            "absent",
            Some(format!(
                "{missing} indexed sessions have no transcript on disk"
            )),
        );
    }
    let mut selected =
        search::prefilter_candidates(ctx, rows.clone(), &[rev_lower[..7].to_owned()], scope)?;
    if let Some(committed_at) = parse_ts(&repo.commit["committed_at"]) {
        let start = committed_at - Duration::minutes(15);
        let end = committed_at + Duration::minutes(15);
        let extra = rows
            .into_iter()
            .filter(|r| {
                parse_ts(&r["started"])
                    .or_else(|| parse_ts(&r["updated"]))
                    .is_some_and(|t| t <= end)
                    && parse_ts(&r["updated"]).is_some_and(|t| t >= start)
                    && repo.repo_match(s(r, "cwd")) == "same"
            })
            .collect();
        selected.extend(search::prefilter_candidates(
            ctx,
            extra,
            &["commit".into()],
            scope,
        )?);
    }
    let mut seen = HashSet::new();
    selected.retain(|r| seen.insert((s(r, "harness").to_owned(), s(r, "id").to_owned())));
    let target = repo.commit["oid"].as_str().unwrap_or(&rev_lower).to_owned();
    let host = core::local_host();
    let mut groups: BTreeMap<String, Value> = BTreeMap::new();
    for row in selected {
        let events: Vec<_> = match core::adapter(s(&row, "harness"))
            .and_then(|a| a.events(ctx, &row, None).map(Iterator::collect))
        {
            Ok(events) => events,
            Err(e) => {
                ctx.diagnostic(
                    "store_unreadable",
                    Some(s(&row, "harness")),
                    None,
                    e.to_string(),
                );
                continue;
            }
        };
        let repo_match = repo.repo_match(s(&row, "cwd"));
        let mut builder = ClaimBuilder {
            repo: &mut repo,
            target: &target,
            path: source_path(ctx, &row)?,
        };
        let claims = builder.claims(&events, repo_match == "same");
        if claims.is_empty() {
            continue;
        }
        let address = core::address(&row);
        let candidate = groups.entry(address.clone()).or_insert_with(|| json!({
            "session":{"address":address,"provider_address":format!("{}:{}@{host}",s(&row,"harness"),s(&row,"id")),
                "harness":row["harness"],"id":row["id"],"host":host,"cwd":row.get("cwd"),"updated":row.get("updated").and_then(parse_ts).map(stamp)},
            "best_claim":null,"repo_match":repo_match,"claims":[]
        }));
        if parse_ts(&row["updated"]) > parse_ts(&candidate["session"]["updated"]) {
            candidate["session"]["updated"] = parse_ts(&row["updated"]).map(stamp).into();
        }
        candidate["claims"].as_array_mut().unwrap().extend(claims);
    }
    let mut candidates: Vec<_> = groups.into_values().collect();
    for candidate in &mut candidates {
        let claims = candidate["claims"].as_array_mut().unwrap();
        claims.sort_by(|a, b| {
            rank(s(a, "claim"))
                .cmp(&rank(s(b, "claim")))
                .then_with(|| s(a, "ts").cmp(s(b, "ts")))
        });
        let mut seen = HashSet::new();
        claims.retain(|c| seen.insert((c["source"].to_string(), s(c, "claim").to_owned())));
        candidate["best_claim"] = claims[0]["claim"].clone();
    }
    candidates.sort_by(|a, b| {
        rank(s(a, "best_claim"))
            .cmp(&rank(s(b, "best_claim")))
            .then_with(|| latest(b).cmp(latest(a)))
            .then_with(|| s(&a["session"], "address").cmp(s(&b["session"], "address")))
    });
    let envelope = crate::contract::envelope(ctx, "find", "ok", Value::Null);
    let complete = s(&envelope, "status") != "partial";
    let coverage: Vec<_> = envelope["coverage"].as_array().unwrap().iter().map(|c| json!({"harness":c["harness"],"store":c["store_id"],"status":c["status"],"reason":c["reason"]})).collect();
    let ran = candidates.iter().any(|c| c["best_claim"] == "ran_commit");
    let doc = json!({"schema":"agent-traces.commit-evidence","version":"1.0","host":host,"observed_at":stamp(ctx.now),
        "query":{"rev":rev,"repo":repo.path},"commit":repo.commit,"authorship":"not_inferred","complete":complete,"coverage":coverage,"candidates":candidates});
    let stdout = if json_output {
        doc.to_string() + "\n"
    } else {
        text_output(&doc)
    };
    let mut stderr = repo.warning.map(|s| s + "\n").unwrap_or_default();
    if !complete {
        stderr.push_str("warning: incomplete session-store coverage\n");
    }
    Ok(CommandOutput {
        stdout,
        stderr,
        exit_code: if !complete {
            3
        } else if ran {
            0
        } else {
            1
        },
        ..Default::default()
    })
}

fn latest(c: &Value) -> &str {
    c["claims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| s(c, "ts"))
        .max()
        .unwrap_or("")
}

fn source_path(ctx: &Context, row: &Session) -> Result<String> {
    let path = s(row, "path");
    let path = path
        .rsplit_once('#')
        .filter(|(_, s)| s.starts_with("thread=") || s.starts_with("session="))
        .map_or(path, |(p, _)| p);
    Ok(std::path::absolute(ctx.expand(path))
        .context("absolute evidence source")?
        .to_string_lossy()
        .into_owned())
}

fn text_output(doc: &Value) -> String {
    let mut out = format!(
        "{} {} complete {}\n",
        s(&doc["commit"], "status"),
        doc["commit"]["oid"]
            .as_str()
            .unwrap_or(s(&doc["query"], "rev")),
        if doc["complete"] == true { "yes" } else { "no" }
    );
    for c in doc["candidates"].as_array().unwrap() {
        out.push_str(&format!(
            "{}  {}  {}  {}\n",
            s(c, "best_claim"),
            s(&c["session"], "address"),
            s(c, "repo_match"),
            latest(c)
        ));
        for claim in c["claims"].as_array().unwrap() {
            out.push_str(&format!(
                "  source {} [{}]\n",
                s(&claim["source"], "path"),
                s(&claim["source"], "selector")
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_boundaries_and_prefixes() {
        assert_eq!(
            sha_tokens(
                "ABC1234 abc1234g zabc1234 _abc1234_ 01234567890123456789012345678901234567890"
            ),
            vec!["abc1234", "abc1234"]
        );
        assert_eq!(sha_match("ABC1234", "abc1234567"), Some("prefix"));
        assert_eq!(sha_match(&"a".repeat(40), &"a".repeat(40)), Some("full"));
        assert_eq!(sha_match("abc1234", "def5678"), None);
    }

    #[test]
    fn disambiguate_parser_and_status() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        assert_eq!(resolution(&disambiguated("garbage\nabc1234")), "not_found");
        assert_eq!(
            resolution(&disambiguated(&format!("{a}\n{a}\n"))),
            "resolved"
        );
        assert_eq!(
            resolution(&disambiguated(&format!("{a}\n{b}\n"))),
            "ambiguous"
        );
    }

    #[test]
    fn shell_forms() {
        for cmd in [
            "git commit -m 'hi'",
            "cd /tmp && git add -A && git commit -m x",
            "X=1 git -C /tmp -c a=b --no-pager --git-dir=.git --work-tree=. commit -m x",
            "git add -A && git commit -m \"$(cat <<'EOF'\nhello && git commit\nEOF\n)\"",
            "git add -A docs && git commit -F- <<'EOF'\nmsg && echo x\nEOF",
            "git commit -F- <<\"EOF\"\nmsg `opaque` $(opaque)\nEOF",
            "git commit -F- <<-'EOF'\n\tmsg\n\tEOF",
            "git commit -F- <<-\"EOF\"\n\tmsg\n\tEOF",
            "git commit -m x 2>&1",
            "git commit -m x >&2",
            "git commit -m x 1>&2",
        ] {
            assert_eq!(
                shell(cmd),
                Shell {
                    commit: true,
                    form: "single"
                },
                "{cmd}"
            );
        }
        for cmd in [
            "echo x && git commit -m x",
            "git commit; git commit",
            "git commit | cat",
            "git commit -m x 2>&1 | tail -3",
            "echo x <<'EOF' && git commit -m x\n[main abc1234] y\nEOF\n",
        ] {
            assert_eq!(
                shell(cmd),
                Shell {
                    commit: true,
                    form: "compound"
                },
                "{cmd}"
            );
        }
        for cmd in [
            "git commit -m 'oops",
            "git commit -m `date`",
            "bash -c 'git commit'",
            "sh -c 'git commit'",
            "pwsh -c 'git commit'",
            "eval 'git commit'",
            "git commit -m $(date)",
            "echo $(date) && git commit",
            "grep 'git commit' file",
            "echo 'git commit'",
            "git log --oneline",
            "echo x # && git commit",
            "X=1 bash -c 'echo x' && git commit",
            "git commit -F- <<EOF\nm\nEOF",
            "git commit -F- <<'EOF'\nno end",
            "git commit -m x > out.txt",
            "git commit -m x 12>&1",
            "git commit -m x 2>&12",
        ] {
            assert_eq!(
                shell(cmd),
                Shell {
                    commit: false,
                    form: "unknown"
                },
                "{cmd}"
            );
        }
    }

    #[test]
    fn code_mode_extracts_one_literal_command() {
        for code in [
            r#"await tools.exec_command({cmd: "git commit -m x"});"#,
            "await tools.exec_command({workdir: '/tmp', cmd: 'git commit -m x', timeout: 1000});",
            "// tools.exec_command({cmd: 'ignored'})\nawait tools.exec_command({'cmd': 'git commit -m x'});",
            "/* example */ await tools.exec_command({cmd: 'git commit -m x'});",
        ] {
            assert_eq!(
                exec_command(code).as_deref(),
                Some("git commit -m x"),
                "{code}"
            );
        }
        for code in [
            "const s = \"tools.exec_command({cmd: 'git commit'})\";",
            "// tools.exec_command({cmd: 'git commit'})",
            "await tools.exec_command({cmd: command});",
            "await tools.exec_command({cmd: 'git commit' + suffix});",
            "await tools.exec_command({cmd: 'git commit'}); await tools.exec_command({cmd: 'git status'});",
            "await tools.exec_command({cmd: 'git commit', cmd: 'echo x'});",
            "await tools.exec_command({cmd: `git commit`});",
            "await tools.exec_command({cmd: 'git commit'}",
            "const x = 1;",
        ] {
            assert_eq!(exec_command(code), None, "{code}");
        }
    }
}
