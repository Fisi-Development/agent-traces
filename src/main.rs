use agent_traces::{
    commands::{self, CommandOutput, ShowOptions},
    context::Context,
    contract,
    core::{self, Filters, SelectionError},
    model::{Event, Session},
    platform, render,
    search::{self, SearchOptions},
    util::*,
};
use anyhow::{Result, bail};
use clap::{Args, CommandFactory, Parser, Subcommand};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    path::PathBuf,
};

#[derive(Parser)]
#[command(
    name = "agent-traces",
    version,
    about = "Find, read, and hand off coding-agent sessions stored on this machine."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Args, Clone, Default)]
struct Format {
    #[arg(long)]
    json: bool,
    #[arg(long, requires = "json")]
    schema_version: Option<String>,
}
impl Format {
    fn v1(&self) -> bool {
        self.schema_version.as_deref() == Some("1")
    }
}
#[derive(Args, Clone, Default)]
struct Common {
    /// Comma-separated: claude, codex, t3, opencode, grok
    #[arg(long)]
    harness: Option<String>,
    /// Keep sessions whose cwd contains DIR, case-insensitive; `.` means here and below
    #[arg(long, value_name = "DIR")]
    cwd: Option<String>,
    /// Last activity at or after: 30m, 12h, 3d, 2w, a date, or an RFC 3339 time
    #[arg(long)]
    since: Option<String>,
    /// Started at or before, in the same forms as --since
    #[arg(long)]
    until: Option<String>,
    /// Include subagent sessions
    #[arg(long)]
    subagents: bool,
    #[command(flatten)]
    format: Format,
}
impl Common {
    fn filters(&self, ctx: &Context, default_since: Option<&str>) -> Result<Filters> {
        Ok(Filters {
            harness: self.harness.clone(),
            cwd: self.cwd.clone(),
            since: self
                .since
                .as_deref()
                .or(default_since)
                .map(|s| parse_since(s, ctx.now))
                .transpose()?,
            until: self
                .until
                .as_deref()
                .map(|s| parse_since(s, ctx.now))
                .transpose()?,
            subagents: self.subagents,
        })
    }
}
#[derive(Subcommand)]
enum Command {
    Where,
    /// Inspect T3 orchestration metadata
    T3 {
        #[command(subcommand)]
        command: T3Command,
    },
    Ls {
        #[command(flatten)]
        common: Common,
        /// Drop the default 7-day window; an explicit --since still applies
        #[arg(long)]
        all: bool,
        /// Show the N most recent sessions; -N shows all but the N oldest
        #[arg(long, default_value_t = 30, allow_hyphen_values = true)]
        limit: i64,
        /// Keep sessions whose id, title, first prompt, cwd, branch or T3 thread
        /// contains TEXT, case-insensitive; repeat to keep any of several
        #[arg(long, value_name = "TEXT")]
        grep: Vec<String>,
        /// Keep sessions that belong to a T3 thread
        #[arg(long)]
        t3: bool,
        /// Print only these comma-separated fields: one tab-separated line per
        /// session, or one JSON object with --json. For example
        /// address,updated,cwd,title; `address` is harness:id@host
        #[arg(long, value_delimiter = ',', conflicts_with_all = ["count_by", "schema_version"])]
        fields: Vec<String>,
        /// Count matching sessions by FIELD instead of listing them; ignores --limit
        #[arg(long, value_name = "FIELD", conflicts_with = "schema_version")]
        count_by: Option<String>,
    },
    Find {
        #[arg(required_unless_present="query",num_args=1.., conflicts_with="query")]
        terms: Vec<String>,
        /// Run independent literal queries, preserving a group for each query
        #[arg(short = 'q', long = "query", conflicts_with_all = ["schema_version", "ids", "count"])]
        query: Vec<String>,
        #[command(flatten)]
        common: Common,
        #[arg(long="in",default_value="prompts",value_parser=["prompts","assistant","tools","results","all"])]
        scope: String,
        #[arg(long,default_value="relevance",value_parser=["relevance","recent","oldest"])]
        sort: String,
        #[arg(long, default_value_t = 15, allow_hyphen_values = true)]
        limit: i64,
        /// Hits shown per session [default: 3; --json: 5]
        #[arg(long, allow_hyphen_values = true)]
        hits: Option<i64>,
        /// Characters of text shown around each hit
        #[arg(long, default_value_t = 220)]
        width: usize,
        /// Print one address (harness:id@host) per matching session, best first
        #[arg(long, conflicts_with_all = ["json", "count"])]
        ids: bool,
        /// Print only the number of matching sessions
        #[arg(long, conflicts_with = "json")]
        count: bool,
        #[arg(long)]
        include_self: bool,
    },
    Touched {
        path: String,
        #[command(flatten)]
        common: Common,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        exact: bool,
        #[arg(long, default_value_t = 25, allow_hyphen_values = true)]
        limit: i64,
        #[arg(long)]
        include_self: bool,
    },
    /// Read local session evidence for a git commit
    Commit {
        rev: String,
        #[arg(long)]
        repo: Option<PathBuf>,
        #[command(flatten)]
        common: Common,
    },
    Failures {
        #[command(flatten)]
        common: Common,
        #[arg(long, default_value_t = 10, allow_hyphen_values = true)]
        limit: i64,
        #[arg(long, default_value_t = 3, allow_hyphen_values = true)]
        min: i64,
        #[arg(long)]
        include_self: bool,
        /// Score every event of each selected session, not only those in the
        /// --since/--until window.
        #[arg(long)]
        all_events: bool,
    },
    Show {
        id: String,
        #[arg(long, allow_hyphen_values = true)]
        turns: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        tail: Option<i64>,
        #[arg(long,num_args=1..)]
        grep: Vec<String>,
        #[arg(long)]
        no_tools: bool,
        #[arg(long)]
        results: bool,
        #[arg(long)]
        notices: bool,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        full_stdout: bool,
        #[arg(long)]
        t3: bool,
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
    Resolve {
        id: String,
        #[command(flatten)]
        format: Format,
    },
    Tree {
        id: String,
    },
    Handoff {
        id: String,
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        format: Format,
    },
    Live {
        #[command(flatten)]
        format: Format,
    },
    Me {
        #[command(flatten)]
        format: Format,
    },
}
#[derive(Subcommand)]
enum T3Command {
    /// Read a thread's branch, worktree, latest run and latest checkpoint
    State {
        thread_id: String,
        #[arg(long)]
        json: bool,
    },
}
impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::Where => "where",
            Self::T3 { .. } => "t3",
            Self::Ls { .. } => "ls",
            Self::Find { .. } => "find",
            Self::Touched { .. } => "touched",
            Self::Commit { .. } => "commit",
            Self::Failures { .. } => "failures",
            Self::Show { .. } => "show",
            Self::Resolve { .. } => "resolve",
            Self::Tree { .. } => "tree",
            Self::Handoff { .. } => "handoff",
            Self::Live { .. } => "live",
            Self::Me { .. } => "me",
        }
    }
    fn format(&self) -> Option<&Format> {
        match self {
            Self::Ls { common, .. }
            | Self::Find { common, .. }
            | Self::Touched { common, .. }
            | Self::Commit { common, .. }
            | Self::Failures { common, .. } => Some(&common.format),
            Self::Resolve { format, .. }
            | Self::Handoff { format, .. }
            | Self::Live { format }
            | Self::Me { format } => Some(format),
            _ => None,
        }
    }
}
fn rows_text(ctx: &Context, rows: &[Session], json: bool) -> String {
    rows.iter()
        .map(|r| {
            if json {
                core::legacy_row(r).to_string()
            } else {
                render::fmt_row(ctx, r)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if rows.is_empty() { "" } else { "\n" }
}
fn since_note(filters: &Filters) -> String {
    filters
        .since
        .map(|d| format!(" updated since {}", fmt_ts(Some(d))))
        .unwrap_or_default()
}
/// A field as one tab-separated cell: no tabs or line breaks inside.
fn cell(v: &Value) -> String {
    let text = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(cell).collect::<Vec<_>>().join(","),
        v => v.to_string(),
    };
    text.split(['\t', '\n', '\r'])
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
fn rows_fields(rows: &[Session], fields: &[String], json: bool) -> String {
    rows.iter()
        .map(|r| {
            let r = core::legacy_row(r);
            if json {
                Value::Object(
                    fields
                        .iter()
                        .map(|f| (f.clone(), core::ls_field(&r, f)))
                        .collect(),
                )
                .to_string()
            } else {
                fields
                    .iter()
                    .map(|f| cell(&core::ls_field(&r, f)))
                    .collect::<Vec<_>>()
                    .join("\t")
            }
        })
        .map(|line| line + "\n")
        .collect()
}
fn ls_counts(
    ctx: &Context,
    rows: &[Session],
    field: &str,
    filters: &Filters,
    json: bool,
) -> CommandOutput {
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for r in rows {
        *counts
            .entry(cell(&core::ls_field(&core::legacy_row(r), field)))
            .or_default() += 1;
    }
    let mut counts = counts.into_iter().collect::<Vec<_>>();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let stdout = counts
        .iter()
        .map(|(value, n)| {
            if json {
                json!({ field: value, "count": n }).to_string()
            } else {
                format!("{n:>6}  {}", if value.is_empty() { "-" } else { value })
            }
        })
        .map(|line| line + "\n")
        .collect();
    CommandOutput {
        stdout,
        stderr: if json {
            String::new()
        } else {
            format!(
                "\n{} sessions in {} groups by {field}{}. searched: {}\n",
                rows.len(),
                counts.len(),
                since_note(filters),
                core::scope_summary(ctx)
            )
        },
        ..Default::default()
    }
}
fn limit_len(total: usize, limit: i64) -> usize {
    if limit < 0 {
        total.saturating_sub(limit.unsigned_abs() as usize)
    } else {
        total.min(limit as usize)
    }
}
fn v1_output(ctx: &Context, command: &str, status: &str, result: Value) -> CommandOutput {
    let envelope = contract::envelope(ctx, command, status, result);
    let exit_code = match s(&envelope, "status") {
        "not_found" => 1,
        "error" => 2,
        "partial" => 3,
        "ambiguous" => 4,
        _ => 0,
    };
    CommandOutput {
        stdout: envelope.to_string() + "\n",
        exit_code,
        ..Default::default()
    }
}
fn load_events<'a>(
    ctx: &'a Context,
    rows: Vec<Session>,
) -> impl Iterator<Item = (Session, Vec<Event>)> + 'a {
    rows.into_iter().filter_map(move |r| {
        match core::adapter(s(&r, "harness"))
            .and_then(|ad| ad.events(ctx, &r, None).map(Iterator::collect))
        {
            Ok(es) => Some((r, es)),
            Err(e) => {
                ctx.diagnostic(
                    "store_unreadable",
                    Some(s(&r, "harness")),
                    None,
                    e.to_string(),
                );
                None
            }
        }
    })
}

fn execute(ctx: &Context, cmd: &Command) -> Result<CommandOutput> {
    execute_with_search(ctx, cmd, None)
}
fn execute_with_search(
    ctx: &Context,
    cmd: &Command,
    searched: Option<search::SearchResult>,
) -> Result<CommandOutput> {
    match cmd {
        Command::T3 {
            command: T3Command::State { thread_id, json },
        } => {
            let state = agent_traces::adapters::t3::T3::state(ctx, thread_id)?;
            let text = if *json {
                state.to_string()
            } else {
                serde_json::to_string_pretty(&state)?
            };
            Ok(CommandOutput {
                stdout: redact(&text) + "\n",
                ..Default::default()
            })
        }
        Command::Ls {
            common,
            all,
            limit,
            grep,
            t3,
            fields,
            count_by,
        } => {
            if common.format.v1() && *limit < 0 {
                bail!("negative limits are invalid in v1");
            }
            for f in fields.iter().chain(count_by) {
                if !core::LS_FIELDS.contains(&f.as_str()) {
                    bail!(
                        "unknown field {f:?}; fields: {}",
                        core::LS_FIELDS.join(", ")
                    );
                }
            }
            let filters = common.filters(ctx, if *all { None } else { Some("7d") })?;
            let mut rows = core::gather(ctx, &filters)?;
            rows.retain(|r| core::ls_keep(r, grep, *t3));
            if let Some(field) = count_by {
                return Ok(ls_counts(ctx, &rows, field, &filters, common.format.json));
            }
            if common.format.v1() {
                rows.sort_by_key(|r| contract::identity_key(&contract::identity(ctx, r)));
            }
            core::sorted_recent(&mut rows);
            let total = rows.len();
            rows.truncate(limit_len(total, *limit));
            if common.format.v1() {
                return Ok(v1_output(
                    ctx,
                    "ls",
                    "ok",
                    contract::ls_result(ctx, &rows, total, *limit as usize),
                ));
            }
            Ok(CommandOutput {
                stdout: if fields.is_empty() {
                    rows_text(ctx, &rows, common.format.json)
                } else {
                    rows_fields(&rows, fields, common.format.json)
                },
                stderr: if common.format.json {
                    String::new()
                } else {
                    format!(
                        "\n{} of {total} sessions{}. searched: {}\n",
                        rows.len(),
                        since_note(&filters),
                        core::scope_summary(ctx)
                    )
                },
                ..Default::default()
            })
        }
        Command::Find {
            terms,
            query,
            common,
            scope,
            sort,
            limit,
            hits,
            width,
            ids,
            count,
            include_self,
        } => {
            if common.format.v1() && *limit < 0 {
                bail!("negative limits are invalid in v1");
            }
            if !query.is_empty() {
                let rows = core::gather(ctx, &common.filters(ctx, None)?)?;
                let mut output = CommandOutput::default();
                let mut found = false;
                for q in query {
                    let command = Command::Find {
                        terms: vec![q.clone()],
                        query: vec![],
                        common: common.clone(),
                        scope: scope.clone(),
                        sort: sort.clone(),
                        limit: *limit,
                        hits: *hits,
                        width: *width,
                        ids: false,
                        count: false,
                        include_self: *include_self,
                    };
                    let result = search::search(
                        ctx,
                        rows.clone(),
                        &SearchOptions {
                            terms: vec![q.clone()],
                            scope: scope.clone(),
                            sort: sort.clone(),
                            include_self: *include_self,
                            limit: if *limit < 0 {
                                usize::MAX
                            } else {
                                *limit as usize
                            },
                        },
                    )?;
                    let total = result.total;
                    let part = execute_with_search(ctx, &command, Some(result))?;
                    found |= part.exit_code == 0;
                    if common.format.json {
                        let matches = part
                            .stdout
                            .lines()
                            .map(serde_json::from_str::<Value>)
                            .collect::<std::result::Result<Vec<_>, _>>()?;
                        output.stdout.push_str(&format!(
                            "{}\n",
                            json!({"query":q,"total":total,"matches":matches})
                        ));
                    } else {
                        output
                            .stdout
                            .push_str(&format!("Query: {q}\n{}", part.stdout));
                    }
                    output
                        .stderr
                        .push_str(&format!("Query: {q}\n{}", part.stderr));
                }
                output.exit_code = if found { 0 } else { 1 };
                return Ok(output);
            }
            let options = SearchOptions {
                terms: terms.clone(),
                scope: scope.clone(),
                sort: sort.clone(),
                include_self: *include_self,
                limit: if *limit < 0 {
                    usize::MAX
                } else {
                    *limit as usize
                },
            };
            let mut result = match searched {
                Some(result) => result,
                None => {
                    let mut rows = core::gather(ctx, &common.filters(ctx, None)?)?;
                    if common.format.v1() {
                        rows.sort_by_key(|r| contract::identity_key(&contract::identity(ctx, r)));
                    }
                    search::search(ctx, rows, &options)?
                }
            };
            if *limit < 0 {
                result.rows.truncate(limit_len(result.total, *limit));
            }
            if common.format.v1() {
                let matches=result.rows.iter().map(|m|json!({"session":contract::session(ctx,&m.row),"score":search::rounded_score(m.score),"match_kind":match m.match_kind.as_str(){"title"=>"title","first prompt"|"first_prompt"=>"first_prompt",_=>"body"},"hits":m.hits.iter().take(5).map(|e|json!({"ts":e.ts.map(|d|d.to_rfc3339_opts(chrono::SecondsFormat::Micros,true)),"role":e.role,"snippet":redact(&search::snippet(&event_text(e),terms,220))})).collect::<Vec<_>>()})).collect::<Vec<_>>();
                return Ok(v1_output(
                    ctx,
                    "find",
                    if result.total == 0 { "not_found" } else { "ok" },
                    json!({"query_terms":terms,"scope":scope,"sort":sort,"score_algorithm":"agent-traces-python-bm25-v1","total":result.total,"limit":limit,"truncated":result.total>matches.len(),"matches":matches}),
                ));
            }
            let mut out = if *count {
                format!("{}\n", result.total)
            } else {
                String::new()
            };
            let shown = if *count {
                &result.rows[..0]
            } else {
                &result.rows[..]
            };
            for m in shown {
                if *ids {
                    out.push_str(&(core::address(&m.row) + "\n"));
                } else if common.format.json {
                    let mut row = core::legacy_row(&m.row);
                    if let Some(object) = row.as_object_mut() {
                        object.remove("_match");
                    }
                    row["_score"] = json!(search::rounded_score(m.score));
                    // Legacy JSON carries five hits unless --hits asks otherwise.
                    let shown = hits.map_or(m.hits.len().min(5), |h| limit_len(m.hits.len(), h));
                    row["hits"]=json!(m.hits.iter().take(shown).map(|e|json!({"ts":e.ts.map(iso),"role":e.role,"snippet":redact(&search::snippet(&event_text(e),terms,*width))})).collect::<Vec<_>>());
                    out.push_str(&(row.to_string() + "\n"));
                } else {
                    let hits = hits.unwrap_or(3);
                    out.push_str(&format!(
                        "{}  <score {:.1}{}>\n",
                        render::fmt_row(ctx, &m.row),
                        search::rounded_score(m.score),
                        if m.match_kind.is_empty() {
                            String::new()
                        } else {
                            format!(", match in {}", m.match_kind)
                        }
                    ));
                    for e in m.hits.iter().take(limit_len(m.hits.len(), hits)) {
                        let ts =
                            e.ts.map(|d| fmt_ts(Some(d))[5..].to_string())
                                .unwrap_or_else(|| " ".repeat(11));
                        out.push_str(&format!(
                            "      {} {:<9} {}\n",
                            ts,
                            e.role,
                            redact(&search::snippet(&event_text(e), terms, *width))
                        ));
                    }
                    if (m.hits.len() as i128) > i128::from(hits) {
                        out.push_str(&format!(
                            "      (+{} more hits)\n",
                            m.hits.len() as i128 - i128::from(hits)
                        ));
                    }
                }
            }
            let mut stderr = format!(
                "\n{} matching sessions ({} shown); scope={scope}; sort={sort}; current session {}. searched: {}\n",
                result.total,
                result.rows.len(),
                if *include_self {
                    "included"
                } else {
                    "excluded"
                },
                core::scope_summary(ctx)
            );
            if result.total == 0 {
                stderr.push_str("no hits. Try --in all, fewer or shorter terms, --subagents, or check `where` for a store this tool does not adapt.\n");
            }
            Ok(CommandOutput {
                stdout: out,
                stderr,
                exit_code: if result.total == 0 { 1 } else { 0 },
                ..Default::default()
            })
        }
        Command::Resolve { id, format } => {
            let rows = core::merge_t3(core::resolve_any(ctx, id)?);
            if format.v1() {
                return Ok(v1_output(
                    ctx,
                    "resolve",
                    match rows.len() {
                        0 => "not_found",
                        1 => "ok",
                        _ => "ambiguous",
                    },
                    contract::resolve_result(ctx, id, &rows),
                ));
            }
            if rows.is_empty() {
                return Ok(CommandOutput {
                    stderr: format!(
                        "no match for {id:?}. searched: {}\n",
                        core::scope_summary(ctx)
                    ),
                    exit_code: 1,
                    ..Default::default()
                });
            }
            if format.json {
                return Ok(CommandOutput {
                    stdout: rows_text(ctx, &rows, true),
                    ..Default::default()
                });
            }
            let mut out = if rows.len() > 1 {
                format!(
                    "AMBIGUOUS: {} different sessions match {id:?}. Ask the user which one they mean. If you cannot ask, open your answer with every candidate below and the one you assumed.\n\n",
                    rows.len()
                )
            } else {
                String::new()
            };
            for r in &rows {
                out.push_str(&format!(
                    "{}\n    path: {}\n",
                    render::fmt_row(ctx, r),
                    ctx.tilde(&ctx.expand(s(r, "path")))
                ));
                if s(r, "harness") == "t3" && !s(r, "provider_id").is_empty() {
                    let p: Vec<_> = core::resolve_any(ctx, s(r, "provider_id"))?
                        .into_iter()
                        .filter(|p| s(p, "harness") == s(r, "provider"))
                        .collect();
                    for p in &p {
                        out.push_str(&format!(
                            "    provider transcript: {}\n",
                            ctx.tilde(&ctx.expand(s(p, "path")))
                        ));
                    }
                    if p.is_empty() {
                        out.push_str(&format!(
                            "    provider transcript: not found for {} id {}\n",
                            s(r, "provider"),
                            s(r, "provider_id")
                        ));
                    }
                }
            }
            Ok(CommandOutput {
                stdout: out,
                ..Default::default()
            })
        }
        Command::Show {
            id,
            turns,
            tail,
            grep,
            no_tools,
            results,
            notices,
            full,
            full_stdout,
            t3,
            output,
        } => {
            let row = core::pick_one(ctx, id, !t3)?;
            let ad = core::adapter(s(&row, "harness"))?;
            let es = ad.events(ctx, &row, None)?.collect::<Vec<_>>();
            commands::show(
                ctx,
                &row,
                &es,
                &ShowOptions {
                    turns: turns.clone(),
                    tail: tail.unwrap_or(0),
                    grep: grep.clone(),
                    notices: *notices,
                    tools: !no_tools,
                    full: *full,
                    results: *results,
                    full_stdout: *full_stdout,
                    output: output.clone(),
                },
            )
        }
        Command::Tree { id } => {
            let row = core::pick_one(ctx, id, true)?;
            let kids = core::adapter(s(&row, "harness"))?.children(ctx, &row)?;
            Ok(commands::tree(ctx, &row, &kids))
        }
        Command::Handoff { id, output, format } => {
            let row = core::pick_one(ctx, id, true)?;
            let ad = core::adapter(s(&row, "harness"))?;
            let es = ad.events(ctx, &row, None)?.collect::<Vec<_>>();
            let mut kids = ad.children(ctx, &row)?;
            if format.v1() {
                kids.sort_by_key(|r| contract::identity_key(&contract::identity(ctx, r)));
                let mut data = commands::handoff_data(ctx, &row, &es, &kids);
                data["session"] = contract::session(ctx, &row);
                data["children"] = json!(
                    kids.iter()
                        .take(30)
                        .map(|r| contract::identity(ctx, r))
                        .collect::<Vec<_>>()
                );
                let mut out = v1_output(ctx, "handoff", "ok", data);
                if let Some(path) = output {
                    out.artifacts
                        .push((path.clone(), std::mem::take(&mut out.stdout)));
                }
                return Ok(out);
            }
            if format.json {
                bail!("handoff JSON requires --schema-version 1");
            }
            Ok(commands::handoff(ctx, &row, &es, &kids, output.as_deref()))
        }
        Command::Failures {
            common,
            limit,
            min,
            include_self,
            all_events,
        } => {
            if common.format.v1() {
                bail!("versioned JSON is supported for ls, find, live, resolve, me, handoff");
            }
            let filters = common.filters(ctx, Some("3d"))?;
            let mut rows = core::gather(ctx, &filters)?;
            if !include_self {
                core::exclude_self(ctx, &mut rows);
            }
            Ok(commands::failures(
                ctx,
                load_events(ctx, rows),
                *min,
                *limit,
                common.format.json,
                filters.since,
                if *all_events {
                    commands::EventWindow::default()
                } else {
                    commands::EventWindow {
                        since: filters.since,
                        until: filters.until,
                    }
                },
            ))
        }
        Command::Commit { rev, repo, common } => {
            if common.format.schema_version.is_some() {
                bail!("commit does not support --schema-version");
            }
            agent_traces::evidence::run(
                ctx,
                rev,
                repo.as_deref(),
                common.filters(ctx, None)?,
                common.format.json,
            )
        }
        Command::Touched {
            path,
            common,
            all,
            exact,
            limit,
            include_self,
        } => {
            if common.format.v1() {
                bail!("versioned JSON is supported for ls, find, live, resolve, me, handoff");
            }
            let mut filters = common.filters(ctx, None)?;
            filters.subagents = true;
            let mut rows = core::gather(ctx, &filters)?;
            if !include_self {
                core::exclude_self(ctx, &mut rows);
            }
            let target = core::absolute_path(ctx, std::path::Path::new(path))?;
            let needle = if *exact {
                target.to_string_lossy().into_owned()
            } else {
                target
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            };
            rows.sort_by_key(|r| {
                ["claude", "codex", "grok", "t3", "opencode"]
                    .iter()
                    .position(|h| *h == s(r, "harness"))
                    .unwrap_or(5)
            });
            let rows = search::prefilter_candidates(ctx, rows, &[needle], search::roles("all")?)?;
            commands::touched(
                ctx,
                load_events(ctx, rows),
                std::path::Path::new(path),
                *exact,
                *all,
                *limit,
            )
        }
        Command::Live { format } => {
            if format.json && !format.v1() {
                bail!("live JSON requires --schema-version 1");
            }
            let mut result = platform::live(ctx)?;
            if format.v1() {
                result.observations.sort_by_key(|r| {
                    (
                        contract::identity_key(&r["identity"]),
                        s(r, "evidence").to_owned(),
                        r["pid"].as_u64().unwrap_or(u64::MAX),
                    )
                });
                Ok(v1_output(
                    ctx,
                    "live",
                    if result.found { "ok" } else { "not_found" },
                    json!({"observations":result.observations}),
                ))
            } else {
                Ok(CommandOutput {
                    stdout: result.text,
                    stderr: result.stderr,
                    exit_code: if result.found { 0 } else { 1 },
                    ..Default::default()
                })
            }
        }
        Command::Me { format } => {
            if format.json && !format.v1() {
                bail!("me JSON requires --schema-version 1");
            }
            let mut ids = ctx.self_ids.iter().cloned().collect::<Vec<_>>();
            ids.sort();
            let mut rows = vec![];
            for id in &ids {
                rows.extend(core::resolve_any(ctx, id)?);
            }
            if format.v1() {
                let mut vars = [
                    "CLAUDE_CODE_SESSION_ID",
                    "CODEX_THREAD_ID",
                    "CODEX_SESSION_ID",
                ]
                .into_iter()
                .filter_map(|k| {
                    std::env::var(k)
                        .ok()
                        .filter(|v| !v.is_empty())
                        .map(|v| (k.into(), v))
                })
                .collect::<Vec<_>>();
                vars.sort();
                return Ok(v1_output(
                    ctx,
                    "me",
                    if rows.is_empty() { "not_found" } else { "ok" },
                    contract::me_result(ctx, &vars, &rows),
                ));
            }
            if ids.is_empty() {
                return Ok(CommandOutput{stderr:"no session id in the environment (CLAUDE_CODE_SESSION_ID / CODEX_THREAD_ID unset)\n".into(),exit_code:1,..Default::default()});
            }
            Ok(CommandOutput {
                stdout: rows
                    .iter()
                    .map(|r| {
                        format!(
                            "{}\n    path: {}\n",
                            render::fmt_row(ctx, r),
                            ctx.tilde(&ctx.expand(s(r, "path")))
                        )
                    })
                    .collect(),
                ..Default::default()
            })
        }
        Command::Where => {
            let mut out =
                "Stores on this machine (newest = latest activity, local time):\n\n".to_owned();
            for ad in core::adapters() {
                for w in ad.where_info(ctx)? {
                    let newest = parse_ts(&w["newest"]);
                    let status = if newest
                        .is_some_and(|d| ctx.now.signed_duration_since(d).num_days() <= 14)
                    {
                        "LIVE"
                    } else if newest.is_some() {
                        "STALE"
                    } else {
                        "?"
                    };
                    let count = w["sessions"]
                        .as_u64()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "None".into());
                    out.push_str(&format!(
                        "{:<8} {:<5} {:<44} sessions={:<5} newest={}\n         index: {}\n",
                        s(&w, "harness"),
                        status,
                        s(&w, "path"),
                        count,
                        newest
                            .map(|d| fmt_ts(Some(d)))
                            .unwrap_or_else(|| "?".into()),
                        s(&w, "index")
                    ));
                    if !s(&w, "notes").is_empty() {
                        out.push_str(&format!("         notes: {}\n", s(&w, "notes")));
                    }
                }
            }
            out.push_str("\nNot adapted (read by hand if needed):\n");
            for (name, path, what) in [
                (
                    "cursor",
                    "~/Library/Application Support/Cursor/User/globalStorage/state.vscdb",
                    "cursorDiskKV composerData:*/bubbleId:*; no adapter",
                ),
                (
                    "cursor-agent",
                    "~/.cursor/projects/*/agent-transcripts",
                    "jsonl; no adapter",
                ),
                (
                    "gemini",
                    "~/.gemini/tmp/*/chats",
                    "session-*.json; no adapter",
                ),
                (
                    "hermes",
                    "~/.hermes/state.db",
                    "sessions + messages tables; few local rows; ignore ~/.hermes/sessions and hermes-agent/",
                ),
                (
                    "context-mode",
                    "~/.claude*/context-mode/sessions, ~/.codex/context-mode/sessions",
                    "sidecar index of prompts/decisions, not transcripts; session ids match the harness",
                ),
            ] {
                out.push_str(&format!("  {name:<12} {path}  ({what})\n"));
            }
            if !ctx.self_ids.is_empty() {
                let mut ids = ctx.self_ids.iter().cloned().collect::<Vec<_>>();
                ids.sort();
                out.push_str(&format!("\nThis process runs inside: {}\n", ids.join(", ")));
            }
            Ok(CommandOutput {
                stdout: out,
                ..Default::default()
            })
        }
    }
}
fn main() {
    let cli = Cli::parse();
    let Some(cmd) = cli.command else {
        let _ = Cli::command().print_help();
        std::process::exit(2)
    };
    let ctx = Context::from_env();
    let v1 = cmd.format().is_some_and(Format::v1);
    if let Some(version) = cmd.format().and_then(|f| f.schema_version.as_deref())
        && version != "1"
    {
        eprintln!("unsupported schema version: {version}");
        std::process::exit(2);
    }
    if v1 && !["ls", "find", "live", "resolve", "me", "handoff"].contains(&cmd.name()) {
        eprintln!("schema version 1 is not supported for {}", cmd.name());
        std::process::exit(2);
    }
    if v1 && ctx.host_id.is_none() {
        eprintln!("AGENT_TRACES_HOST_ID is required for schema version 1");
        std::process::exit(2);
    }
    let mut out = match execute(&ctx, &cmd) {
        Ok(out) => out,
        Err(e) => {
            let (status, code) = match e.downcast_ref::<SelectionError>() {
                Some(SelectionError::NotFound(_)) => ("not_found", 1),
                Some(SelectionError::Ambiguous(_)) => ("error", 4),
                Some(SelectionError::Invalid(_)) => ("error", 1),
                None => ("error", 2),
            };
            if v1 {
                ctx.diagnostic(
                    if code == 4 {
                        "ambiguous_identity"
                    } else if status == "not_found" {
                        "not_found"
                    } else {
                        "invalid_argument"
                    },
                    None,
                    None,
                    e.to_string(),
                );
                let status = if cmd.name() == "handoff"
                    && ctx.diagnostics.borrow().iter().any(|d| {
                        matches!(
                            d.code.as_str(),
                            "store_busy" | "store_unreadable" | "unsupported_storage_mode"
                        )
                    }) {
                    "partial"
                } else {
                    status
                };
                let mut out = v1_output(&ctx, cmd.name(), status, Value::Null);
                if code == 4
                    && s(
                        &serde_json::from_str::<Value>(&out.stdout).unwrap_or(Value::Null),
                        "status",
                    ) == "error"
                {
                    out.exit_code = 4;
                }
                out
            } else {
                let mut out = CommandOutput {
                    stderr: format!("{e}\n"),
                    exit_code: if code == 4 { 2 } else { code },
                    ..Default::default()
                };
                if let Some(SelectionError::Ambiguous(rows)) = e.downcast_ref::<SelectionError>() {
                    out.stdout = rows_text(&ctx, rows, false);
                }
                out
            }
        }
    };
    if let Err(e) = core::write_artifacts(&ctx, &out.artifacts) {
        out = if v1 {
            ctx.diagnostic("internal_error", None, None, e.to_string());
            v1_output(&ctx, cmd.name(), "error", Value::Null)
        } else {
            CommandOutput {
                stderr: format!("{e}\n"),
                exit_code: 1,
                ..Default::default()
            }
        };
    }
    if !v1 {
        for d in ctx.diagnostics.borrow().iter() {
            // Name the store, or a skipped record cannot be traced back to it.
            let store = d
                .store_id
                .as_deref()
                .map(|p| format!("{}: ", ctx.tilde(std::path::Path::new(p))))
                .unwrap_or_default();
            out.stderr.push_str(&format!(
                "{}: {store}{}\n",
                d.harness.as_deref().unwrap_or("agent-traces"),
                redact(&d.message)
            ));
        }
    }
    if cfg!(windows) && !v1 {
        out.stdout = out.stdout.replace("\n", "\r\n");
        out.stderr = out.stderr.replace("\n", "\r\n");
    }
    let _ = io::stderr().write_all(out.stderr.as_bytes());
    if let Err(e) = io::stdout().write_all(out.stdout.as_bytes()) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        eprintln!("{e}");
        std::process::exit(1);
    }
    std::process::exit(out.exit_code);
}
