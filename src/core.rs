use crate::{
    adapters::StoreAdapter,
    context::Context,
    model::Session,
    util::{parse_ts, s},
};
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct Filters {
    pub harness: Option<String>,
    pub cwd: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub subagents: bool,
}
#[derive(Debug)]
pub enum SelectionError {
    NotFound(String),
    Ambiguous(Vec<Session>),
    Invalid(String),
}
impl std::fmt::Display for SelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(s) => write!(f, "no session matches {s:?}"),
            Self::Ambiguous(rows) => write!(
                f,
                "AMBIGUOUS: {} sessions match; resolve the full id",
                rows.len()
            ),
            Self::Invalid(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for SelectionError {}
pub fn adapters() -> Vec<Box<dyn StoreAdapter>> {
    vec![
        Box::new(crate::adapters::claude::Claude),
        Box::new(crate::adapters::codex::Codex),
        Box::new(crate::adapters::t3::T3),
        Box::new(crate::adapters::opencode::OpenCode),
        Box::new(crate::adapters::grok::Grok),
    ]
}
pub fn adapter(name: &str) -> Result<Box<dyn StoreAdapter>> {
    adapters()
        .into_iter()
        .find(|a| a.name() == name)
        .ok_or_else(|| anyhow::anyhow!("no adapter for {name}"))
}
pub fn merge_t3(rows: Vec<Session>) -> Vec<Session> {
    let (mut others, t3): (Vec<_>, Vec<_>) =
        rows.into_iter().partition(|r| s(r, "harness") != "t3");
    for r in t3 {
        if let Some(target) = others
            .iter_mut()
            .rev()
            .find(|p| s(p, "harness") == s(&r, "provider") && s(p, "id") == s(&r, "provider_id"))
        {
            target["t3_thread"] = r["id"].clone();
            target["t3_title"] = r["title"].clone();
            target["provider_instance"] = r["provider_instance"].clone();
        } else {
            others.push(r);
        }
    }
    others
}
pub fn cwd_match(ctx: &Context, cwd: &str, want: &str) -> bool {
    if cwd.is_empty() {
        return false;
    }
    if want == "." || want == "here" {
        let here = std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        cwd == here || cwd.starts_with(&(here + std::path::MAIN_SEPARATOR_STR))
    } else {
        cwd.to_lowercase()
            .contains(&ctx.expand(want).to_string_lossy().to_lowercase())
    }
}
pub fn gather(ctx: &Context, filters: &Filters) -> Result<Vec<Session>> {
    let names = filters
        .harness
        .as_ref()
        .map(|s| s.split(',').collect::<Vec<_>>());
    let mut rows = Vec::new();
    for a in adapters() {
        if names.as_ref().is_some_and(|n| !n.contains(&a.name())) {
            continue;
        }
        if a.homes(ctx).is_empty() {
            ctx.cover(
                a.name(),
                None,
                "sessions",
                "absent",
                Some("store not found".into()),
            );
        }
        match a.sessions(ctx, filters.since, filters.subagents) {
            Ok(rs) => {
                ctx.remember(&rs);
                rows.extend(rs.into_iter().filter(|r| {
                    filters
                        .cwd
                        .as_ref()
                        .is_none_or(|w| cwd_match(ctx, s(r, "cwd"), w))
                }));
            }
            Err(e) => ctx.diagnostic("store_unreadable", Some(a.name()), None, e.to_string()),
        }
    }
    if names.as_ref().is_none_or(|n| n.contains(&"t3")) {
        rows = merge_t3(rows);
    }
    // Adapters prefilter on file mtime, which moves when a harness appends an
    // undated metadata line to an old session. `--since` means last recorded
    // activity, so recheck it against `updated`.
    if let Some(since) = filters.since {
        rows.retain(|r| parse_ts(&r["updated"]).is_none_or(|d| d >= since));
    }
    if let Some(until) = filters.until {
        rows.retain(|r| parse_ts(&r["started"]).unwrap_or(ctx.now) <= until);
    }
    Ok(rows)
}
/// This host's short name, lowercased, to check the `@host` of an address.
fn local_host() -> &'static str {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        std::env::var("AGENT_TRACES_HOST")
            .ok()
            .filter(|h| !h.trim().is_empty())
            .or_else(|| std::env::var("COMPUTERNAME").ok())
            .or_else(|| {
                std::process::Command::new("hostname")
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            })
            .unwrap_or_default()
            .trim()
            .split('.')
            .next()
            .unwrap_or("")
            .to_lowercase()
    })
}
/// Splits the address form Telepathica and Harbor breadcrumbs use,
/// `harness:id` or `harness:id@host`, into an adapter name and the bare id.
/// Anything else, including T3's own `mcp:...` ids, passes through whole.
pub fn split_address(ident: &str) -> Result<(Option<&'static str>, String)> {
    split_address_on(ident, local_host())
}
fn split_address_on(ident: &str, here: &str) -> Result<(Option<&'static str>, String)> {
    let mut id = ident.trim();
    let mut only = None;
    if let Some((name, rest)) = id.split_once(':')
        && let Some(a) = adapters().into_iter().find(|a| a.name() == name)
    {
        only = Some(a.name());
        id = rest;
    }
    if let Some((bare, host)) = id.rsplit_once('@') {
        let host = host.split('.').next().unwrap_or("").to_lowercase();
        if !here.is_empty() && host != here {
            let addr = only.map(|n| format!("{n}:{bare}")).unwrap_or(bare.into());
            return Err(SelectionError::Invalid(format!(
                "{} is on {host}, not this host ({here}); run `agent-traces show {addr}` on {host}",
                ident.trim()
            ))
            .into());
        }
        id = bare;
    }
    Ok((only, id.to_owned()))
}
/// A T3 thread id without its `mcp:`, `thread:` or `import:` prefix.
pub fn bare_thread_id(id: &str) -> &str {
    match id.split_once(':') {
        Some((p, rest)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_alphabetic()) => rest,
        _ => id,
    }
}
pub fn resolve_any(ctx: &Context, ident: &str) -> Result<Vec<Session>> {
    let mut id = ident.trim().to_owned();
    let mut only = None;
    let path = ctx.expand(&id);
    if !path.exists() {
        (only, id) = split_address(&id)?;
    } else {
        let base = path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(rest) = base.strip_prefix("agent-") {
            id = rest.split('.').next().unwrap_or("").into();
        } else {
            let re =
                regex::Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")?;
            if let Some(m) = re.find(&base) {
                id = m.as_str().into();
            }
        }
    }
    if id.chars().count() < 6 {
        return Err(SelectionError::Invalid(
            "id prefix too short; give at least 6 characters".into(),
        )
        .into());
    }
    let mut rows = Vec::new();
    for a in adapters() {
        if only.is_some_and(|n| n != a.name()) {
            continue;
        }
        if a.homes(ctx).is_empty() {
            ctx.cover(
                a.name(),
                None,
                "resolve",
                "absent",
                Some("store not found".into()),
            );
        }
        match a.resolve(ctx, &id) {
            Ok(rs) => {
                ctx.remember(&rs);
                rows.extend(rs)
            }
            Err(e) => ctx.diagnostic("store_unreadable", Some(a.name()), None, e.to_string()),
        }
    }
    Ok(rows)
}
pub fn pick_one(ctx: &Context, id: &str, prefer_provider: bool) -> Result<Session> {
    let mut rows = resolve_any(ctx, id)?;
    if rows.is_empty() {
        return Err(SelectionError::NotFound(id.into()).into());
    }
    if prefer_provider {
        rows = merge_t3(rows);
    }
    if rows.len() > 1
        && let Some(i) = exact_pick(&rows, id, prefer_provider)
    {
        let mut row = rows.remove(i);
        // The rest are T3 threads that once ran this session under an
        // earlier provider; keep the link when there is just one.
        if let [t3] = rows.as_slice()
            && s(t3, "harness") == "t3"
            && s(&row, "harness") != "t3"
        {
            row["t3_thread"] = t3["id"].clone();
            row["t3_title"] = t3["title"].clone();
        }
        return Ok(row);
    }
    if rows.len() > 1 {
        return Err(SelectionError::Ambiguous(rows).into());
    }
    let row = rows.remove(0);
    if prefer_provider && s(&row, "harness") == "t3" && !s(&row, "provider_id").is_empty() {
        let mut p: Vec<_> = resolve_any(ctx, s(&row, "provider_id"))?
            .into_iter()
            .filter(|r| s(r, "harness") == s(&row, "provider"))
            .collect();
        if p.len() == 1 {
            let mut r = p.remove(0);
            r["t3_thread"] = row["id"].clone();
            r["t3_title"] = row["title"].clone();
            r["provider_instance"] = row["provider_instance"].clone();
            return Ok(r);
        }
    }
    Ok(row)
}
/// Chooses among several matches when the query names one of them: the T3
/// thread under `--t3`, otherwise the session whose own id is the full query,
/// or the one session left once T3 threads that matched only through a
/// provider they ran earlier are set aside (they are wrappers of it).
fn exact_pick(rows: &[Session], ident: &str, prefer_provider: bool) -> Option<usize> {
    let want = split_address(ident).ok()?.1.to_lowercase();
    let only = |keep: &dyn Fn(&Session) -> bool| {
        let hits: Vec<_> = (0..rows.len()).filter(|&i| keep(&rows[i])).collect();
        (hits.len() == 1).then(|| hits[0])
    };
    if !prefer_provider && let Some(i) = only(&|r| s(r, "harness") == "t3") {
        return Some(i);
    }
    let exact = only(&|r| {
        let id = s(r, "id").to_lowercase();
        id == want || bare_thread_id(&id) == want
    });
    if exact.is_some() || !prefer_provider {
        return exact;
    }
    let names_it = |r: &Session| {
        let id = s(r, "id").to_lowercase();
        s(r, "harness") != "t3"
            || id.starts_with(&want)
            || bare_thread_id(&id).starts_with(&want)
            || s(r, "provider_id").to_lowercase().starts_with(&want)
    };
    let named: Vec<_> = (0..rows.len()).filter(|&i| names_it(&rows[i])).collect();
    match named.as_slice() {
        [i] if s(&rows[*i], "harness") != "t3" => Some(*i),
        _ => None,
    }
}
pub fn scope_summary(ctx: &Context) -> String {
    adapters()
        .iter()
        .map(|a| {
            let hs = a
                .homes(ctx)
                .iter()
                .map(|p| ctx.tilde(p))
                .collect::<Vec<_>>();
            format!(
                "{}({})",
                a.name(),
                if hs.is_empty() {
                    "absent".into()
                } else {
                    hs.join(", ")
                }
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}
pub fn legacy_row(row: &Session) -> Session {
    let mut r = row.clone();
    for key in ["started", "updated"] {
        r[key] = if let Some(d) = parse_ts(&r[key]) {
            Value::String(crate::util::iso(d))
        } else {
            Value::Null
        };
    }
    r
}
pub fn sorted_recent(rows: &mut [Session]) {
    rows.sort_by_key(|r| {
        std::cmp::Reverse(parse_ts(&r["updated"]).or_else(|| parse_ts(&r["started"])))
    });
}
pub fn exclude_self(ctx: &Context, rows: &mut Vec<Session>) {
    rows.retain(|r| {
        ![s(r, "id"), s(r, "parent"), s(r, "provider_id")]
            .iter()
            .any(|id| ctx.self_ids.contains(*id))
    });
}
pub fn absolute_path(ctx: &Context, path: &std::path::Path) -> Result<std::path::PathBuf> {
    Ok(clean(&std::path::absolute(
        ctx.expand(&path.to_string_lossy()),
    )?))
}
pub fn ensure_output_path(ctx: &Context, path: &std::path::Path) -> Result<std::path::PathBuf> {
    let path = ctx.expand(&path.to_string_lossy());
    let normalized = clean(&std::path::absolute(path)?);
    let physical = physical_path(&normalized);
    let mut protected = Vec::new();
    for a in adapters() {
        protected.extend(a.homes(ctx));
    }
    for parts in [
        vec![".hermes"],
        vec![".cursor"],
        vec![".gemini"],
        vec![".grok"],
        vec![".t3"],
        vec![".codex"],
        vec![".claude"],
        vec![".local", "share", "opencode"],
        vec!["Library", "Application Support", "Cursor"],
        vec!["Library", "Application Support", "Claude"],
    ] {
        let mut root = ctx.home.clone();
        for part in parts {
            root.push(part);
        }
        protected.push(root);
    }
    if let Ok(entries) = std::fs::read_dir(&ctx.home) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".claude") || name.starts_with(".codex") {
                protected.push(entry.path());
            }
        }
    }
    for key in ["CLAUDE_CONFIG_DIR", "CODEX_HOME"] {
        if let Some(p) = std::env::var_os(key).filter(|p| !p.is_empty()) {
            protected.push(ctx.expand(&p.to_string_lossy()));
        }
    }
    for source in ctx.protected_sources.borrow().iter() {
        protected.push(source.clone());
        if let Some(parent) = source.parent() {
            protected.push(parent.to_path_buf());
        }
    }
    // T3 keeps its git worktrees beside its store; those are ordinary
    // checkouts, not session data.
    let t3_root = physical_path(&clean(&ctx.home.join(".t3")));
    let in_worktree = physical.starts_with(t3_root.join("worktrees"));
    if protected.iter().any(|p| {
        let root = physical_path(&clean(p));
        physical.starts_with(&root) && !(in_worktree && root == t3_root)
    }) {
        bail!(
            "refusing to write inside a session store: {}",
            normalized.display()
        );
    }
    Ok(normalized)
}
pub fn write_artifacts(ctx: &Context, artifacts: &[(std::path::PathBuf, String)]) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    for (path, content) in artifacts {
        let destination = ensure_output_path(ctx, path)?;
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow::anyhow!("output has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let temp = parent.join(format!(
            ".agent-traces-{}-{stamp}-{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            let text = if cfg!(windows) {
                content.replace("\n", "\r\n")
            } else {
                content.clone()
            };
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, &destination)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result?;
    }
    Ok(())
}
fn physical_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut ancestor = path;
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        if let Some(name) = ancestor.file_name() {
            suffix.push(name.to_os_string());
        }
        match ancestor.parent() {
            Some(p) => ancestor = p,
            None => break,
        }
    }
    let mut out = std::fs::canonicalize(ancestor).unwrap_or_else(|_| ancestor.to_path_buf());
    for s in suffix.into_iter().rev() {
        out.push(s);
    }
    out
}
fn clean(path: &std::path::Path) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                p.pop();
            }
            _ => p.push(c),
        }
    }
    p
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    #[test]
    fn protects_unsupported_and_external_sources() {
        let temp = tempfile::tempdir().unwrap();
        let mut ctx = Context::from_env();
        ctx.home = temp.path().join("home");
        std::fs::create_dir_all(&ctx.home).unwrap();
        let hermes = ctx.home.join(".hermes").join("state.db");
        assert!(ensure_output_path(&ctx, &hermes).is_err());
        let external = temp.path().join("external").join("rollout.jsonl");
        std::fs::create_dir_all(external.parent().unwrap()).unwrap();
        std::fs::write(&external, "unchanged").unwrap();
        ctx.protect_source(&external);
        assert!(write_artifacts(&ctx, &[(external.clone(), "destructive".into())]).is_err());
        assert_eq!(std::fs::read_to_string(external).unwrap(), "unchanged");
    }
    #[test]
    fn t3_worktrees_are_writable_but_its_store_is_not() {
        let temp = tempfile::tempdir().unwrap();
        let mut ctx = Context::from_env();
        ctx.home = temp.path().join("home");
        let tree = ctx.home.join(".t3").join("worktrees").join("repo");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::create_dir_all(ctx.home.join(".t3").join("userdata")).unwrap();
        assert!(ensure_output_path(&ctx, &tree.join("notes.md")).is_ok());
        let store = ctx.home.join(".t3").join("userdata").join("notes.md");
        assert!(ensure_output_path(&ctx, &store).is_err());
        assert!(ensure_output_path(&ctx, &ctx.home.join(".t3").join("notes.md")).is_err());
    }
    #[test]
    fn addresses_name_a_harness_and_check_the_host() {
        let (only, id) =
            split_address_on("claude:f91f78af@Bouncehouse.local", "bouncehouse").unwrap();
        assert_eq!((only, id.as_str()), (Some("claude"), "f91f78af"));
        let (only, id) = split_address_on("t3:mcp:301cb290", "bouncehouse").unwrap();
        assert_eq!((only, id.as_str()), (Some("t3"), "mcp:301cb290"));
        let (only, id) = split_address_on("mcp:301cb290", "bouncehouse").unwrap();
        assert_eq!((only, id.as_str()), (None, "mcp:301cb290"));
        let err = split_address_on("t3:7b0f5ce8@desktop-o91444g", "bouncehouse").unwrap_err();
        assert!(
            err.to_string()
                .contains("run `agent-traces show t3:7b0f5ce8` on desktop-o91444g")
        );
        assert_eq!(bare_thread_id("thread:abc"), "abc");
        assert_eq!(bare_thread_id("f91f78af-f067"), "f91f78af-f067");
    }
    #[test]
    fn a_full_id_picks_its_own_session_over_a_t3_thread_that_once_ran_it() {
        let id = "f91f78af-f067-475f-b5a9-5ce9db542cea";
        let rows = vec![
            serde_json::json!({"harness":"claude","id":id}),
            serde_json::json!({"harness":"t3","id":"mcp:301cb290","provider":"codex","provider_id":"01a145f9"}),
        ];
        assert_eq!(exact_pick(&rows, id, true), Some(0));
        assert_eq!(exact_pick(&rows, &format!("claude:{id}"), true), Some(0));
        assert_eq!(exact_pick(&rows, id, false), Some(1));
        assert_eq!(exact_pick(&rows, "f91f78af", true), Some(0));
        assert_eq!(exact_pick(&rows, "f91f78af", false), Some(1));
        let own = vec![
            serde_json::json!({"harness":"claude","id":"abc123-1"}),
            serde_json::json!({"harness":"t3","id":"abc123-2"}),
        ];
        assert_eq!(exact_pick(&own, "abc123", true), None);
        let twins = vec![
            serde_json::json!({"harness":"claude","id":id}),
            serde_json::json!({"harness":"codex","id":id}),
        ];
        assert_eq!(exact_pick(&twins, id, true), None);
    }
    #[test]
    fn artifact_replace_preserves_prior_target_on_rename_failure() {
        let temp = tempfile::tempdir().unwrap();
        let mut ctx = Context::from_env();
        ctx.home = temp.path().join("home");
        std::fs::create_dir_all(&ctx.home).unwrap();
        let target = temp.path().join("output.json");
        std::fs::write(&target, "old").unwrap();
        write_artifacts(&ctx, &[(target.clone(), "new".into())]).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        let dir = temp.path().join("existing-directory");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("keep"), "original").unwrap();
        assert!(write_artifacts(&ctx, &[(dir.clone(), "replace".into())]).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join("keep")).unwrap(),
            "original"
        );
        assert!(!std::fs::read_dir(temp.path()).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".agent-traces-")
        }));
    }
}
