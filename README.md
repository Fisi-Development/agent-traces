> Published mirror of Fisi-Development/harbor `agent-traces/` (Harbor decision D043). Changes land in Fisi-Development/harbor; do not open pull requests here.

# agent-traces

Read coding-agent sessions across Claude Code, Codex, T3 Code, OpenCode, and
Grok Build with one native executable. SQLite is bundled. Running the executable
does not require Python, ripgrep, Cargo, or a separately installed SQLite.

All eleven commands are implemented. Windows unit, schema, and golden parity
checks pass; the Linux musl executable cross-builds. Follow the native Windows,
macOS ARM64/x64, and Linux [CI results](https://github.com/royalaid/agent-traces/actions/workflows/ci.yml).
Stored golden outputs define the regression expectations. See
[verification and measurements](docs/verification-2026-09-26.md).
Download native binaries from the [latest release](https://github.com/royalaid/agent-traces/releases/latest).
This package is not published to crates.io.

Development happens in the `agent-traces/` folder of the Harbor monorepo
(`royalaid/harbor`). This repository, `royalaid/agent-traces`, is its one-way
published mirror: releases and CI run here, and changes land in Harbor.

## Install a release

Authenticate GitHub CLI with access to `royalaid/agent-traces`. Download the archive
and its `.sha256` file for your host with:

```sh
gh release download --repo royalaid/agent-traces --pattern 'ARCHIVE_NAME*' --dir downloads
```

| Host | Archive name |
| --- | --- |
| Windows x64 | `agent-traces-x86_64-pc-windows-msvc.zip` |
| macOS Apple Silicon | `agent-traces-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `agent-traces-x86_64-apple-darwin.tar.gz` |
| Linux x64, including WSL | `agent-traces-x86_64-unknown-linux-musl.tar.gz` |

Replace `ARCHIVE_NAME` with the exact table entry. Verify the archive against its
SHA-256 file before extracting: `Get-FileHash -Algorithm SHA256` on Windows,
`shasum -a 256 -c ARCHIVE_NAME.sha256` on macOS, or
`sha256sum -c ARCHIVE_NAME.sha256` on Linux, from the download directory.
Extract with `Expand-Archive` on Windows or `tar -xzf` on Unix. Copy the executable
inside the extracted directory into a directory on PATH, then verify
`agent-traces --version` and `agent-traces where`. Run it on the host that owns
the stores; Windows and WSL are separate hosts.

## Build from source

Clone this repository and enter its directory (in a Harbor checkout, enter
`agent-traces/`), then install a Rust toolchain and the platform C compiler. Run:

```sh
cargo build --locked --release
cargo install --locked --path .
agent-traces --help
```

The release executable is `target/release/agent-traces` on Unix and
`target/release/agent-traces.exe` on Windows. Copy that file to a directory on
PATH. macOS binaries use the operating system libraries; "self-contained" does
not mean a fully static macOS executable. Windows release archives select the
static MSVC runtime. Linux release archives target musl.

## Commands

From 0.2.26, `agent-traces prompts [--since S] [--until U] [--harness H] [--cwd DIR]`
exports user turns in chronological order as JSON Lines. The default window is
seven days, with the same session filters as `ls`; only turns inside the time
window are emitted. `--max-chars N` clips redacted text to N Unicode characters
(default 2000), appending `…` when clipped. `chars` is the original, unclipped
character count. Each row includes `address`, `harness`, `host`, UTC `time`,
`cwd`, nullable `t3_thread`, and a zero-based `turn` index before filtering.

T3-linked sessions use T3 messages: `origin: "t3"` requires `createdBy: "user"`
and `creationSource` of `web`, `desktop`, or `mobile`. Other sessions use
`origin: "heuristic"`, excluding subagents, injected notices, skill expansions,
and machine prefixes. Non-T3 Claude sessions with an `sdk*` user entrypoint and
Codex sessions with `session_meta.payload.source: "exec"` classify every user
turn as machine, as do sessions whose cwd is agent-guard's classifier
directory (`.agent-guard/llm-cwd`), which carry no exec marker on Windows;
interactive or unmarked sessions keep the heuristic rules.
`--include-machine` includes excluded user turns with
`origin: "machine"`; tool results are never user turns. Missing provenance is
not proof of a human author. Turns without a timestamp are skipped with a
diagnostic. T3 messages are emitted once per thread, without duplicating their
provider transcript. Legacy T3 messages without provenance appear only in the
machine audit.

From 0.2.12, `agent-traces commit REV --repo DIR --json` reports read-only local
session evidence for a commit, with paired tool results, repository comparison,
and scan coverage. See [Commit evidence](docs/commit-evidence.md) for the JSON
document, claim rules, and exit codes.

From 0.2.10, `find -q "ticket-one" -q "ticket-two" --json` runs independent
literal queries in one store discovery. Each JSON line is a group with `query`,
`total` (before `--limit`), and `matches` (legacy session rows). Missing queries
still get an empty group. Text output labels each group with `Query:`. Limits,
scope, sorting, hit budgets and `--width` apply to every query. Positional terms
remain an AND search; they cannot be combined with `-q`. Grouped queries cannot
be combined with `--ids`, `--count`, or `--schema-version`. Exit 1 means all
queries missed; exit 0 means at least one matched.

`agent-traces t3 state THREAD [THREAD ...] --json` reads metadata from the local
T3 v2 store without transcript output: thread id, title, branch, worktree path,
updated_at (when available), latest run id/status and latest checkpoint id/status/time/ref. Pass a full thread id, with
or without its `mcp:`, `thread:` or `import:` prefix, or a local `t3:ID@host`
address. It emits one JSON object for one id or an array in argument order for several;
without `--json`, it pretty-prints the result. All ids share one SQLite snapshot,
and any missing or ambiguous id fails without partial output. Select dotted
fields with `--fields thread_id,latest_run.status,updated_at`: one tab-separated
line per thread, or one compact JSON object per line with `--json`. Missing
run/checkpoint metadata is null; it never guesses between threads.

From 0.2.24, `t3 threads` lists local thread metadata, newest update first,
excluding deleted threads unless `--include-deleted` is set. Filter by project,
creator, source, settled state, archive state, or `--since`, then apply `--limit`.
For example, `agent-traces t3 threads --count-by creation_source --json` groups
threads by creation source, including a null bucket.

`t3 events` reads orchestration events in sequence order, with a default limit
of 200. Repeat `--type` for exact types or a trailing `*` prefix. Actor, thread,
kind, time bounds, and `--after-seq` narrow the results. `--count-by` counts all
matching events regardless of the limit. For example,
`agent-traces t3 events --type thread.settled --actor client --count-by creation_source --json`.
T3 indexes events by sequence and by thread, so `--thread` and `--after-seq` are
fast; a type, actor or time filter without them reads the whole events table,
which on a multi-gigabyte store takes seconds to minutes.
Payloads are omitted unless `--payload` is set, then redacted and capped at
2000 characters. Both commands use a read-only snapshot. Omit `--json` for
pretty JSON.

`t3 snapshot` returns one object with `threads` and `requests`. It includes
archived and child threads across all projects, uses `idle` for threads with
no runs, and maps waiting secret requests on live runs to `user_input`.
It exports no message or request payloads. `t3 summary THREAD` returns
`{"thread_id":"…","text":"…"}`, collapses whitespace, and redacts before
truncating. An existing thread without messages has empty text; unknown or
deleted threads fail. Both commands read only the local T3 v2 store and fail
on missing required schema. Omit `--json` for pretty JSON.

| Command | Purpose |
| --- | --- |
| `where` | Locate stores and describe coverage |
| `ls` | List, filter, project, or count sessions by date, harness, cwd, and keyword |
| `find QUERY` | Rank matching sessions with BM25 |
| `index` | Bring the `find` index up to date for every Claude, Codex and Grok session, with no time budget |
| `t3 snapshot --json` | Read all undeleted local threads, latest run state and unique pending request kinds |
| `t3 summary THREAD --json` | Read the latest user/assistant text for an exact full thread id, redacted and capped at 500 Unicode characters |
| `t3 threads [filters]` | List thread metadata or group counts with `--count-by` |
| `t3 events [filters]` | Read orchestration events or group counts with `--count-by` |
| `t3 state THREAD [THREAD ...] [--fields a,b.c]` | Read T3 v2 branch, worktree, updated_at, latest run and checkpoint metadata |
| `touched PATH` | Locate recorded file edits |
| `commit REV` | Report local session evidence for a git commit |
| `failures` | Rank errors and user corrections |
| `prompts` | Export user-typed turns as redacted JSON Lines |
| `show ID` | Render a transcript or selected turns |
| `usage ID... [--json]` | Per-session token usage, API calls and wall time for Claude, Codex and OpenCode, including T3 provider transcripts |
| `resolve ID` | Resolve full IDs and prefixes |
| `tree ID` | List recorded child sessions |
| `handoff ID` | Extract a handoff draft |
| `live` | Query available liveness evidence |
| `me` | Resolve the caller's environment session IDs |

An `ID` is a full id, a prefix of at least six characters, a session file
path, or an address: `harness:id` (for example `claude:f91f78af` or
`t3:mcp:301cb290`) limits the lookup to one harness, and `harness:id@host`, the
form Telepathica and Harbor breadcrumbs record, also checks that the session is
on this host and names the host to run on when it is not. A T3 thread id
matches with or without its `mcp:`, `thread:` or `import:` prefix. When two
stores match, `show`, `tree` and `handoff` take the session whose own id is the
full query, and set aside T3 threads that matched only through a provider they
ran earlier. `show` output over 20,000 characters goes to a file; its path is
printed in the summary and again as the last line.

`--since` and `--until` select sessions by recorded activity: `--since` keeps a
session whose last timestamped event is at or after the cutoff, and `--until`
one that started at or before it. File modification time is only a prefilter,
because harnesses append undated metadata to old sessions. `failures` also
scores only the events inside that window, so a session resumed this week does
not carry last month's errors into this week's ranking. Pass `--all-events` to
score whole sessions. A score ranks candidates for reading; it is not a failure
rate. Skipped records are reported on stderr with their store path, and an
unterminated final record is named as such, since it usually means the file was
being written while it was read. Malformed JSONL records, including invalid
Unicode escapes, are skipped without discarding valid neighboring records or
modifying the source. Search coverage remains incomplete when a record cannot
be decoded: text output warns, and `--schema-version 1` reports `complete=false`.
Repeated reads in one command report a bad record once, by source byte offset.
The find index retains these warnings across later searches, including searches
with no matches. Completing a partial final record or rewriting a source without
corruption clears the corresponding cached warning on refresh.

`ls` answers listing questions without a script. `--cwd DIR` matches any cwd
containing DIR, case-insensitive. `--grep TEXT` keeps sessions whose id, title,
first prompt, cwd, branch or T3 thread contains TEXT; repeat it to keep any of
several. `--t3` keeps sessions that belong to a T3 thread. Filters apply before
`--limit`. `--fields address,updated,cwd,title` prints one tab-separated line
per session (one JSON object each with `--json`), where `address` is the
`harness:id@host` form, or `t3:<thread>@host` for a session inside a T3 thread.
`--count-by FIELD` prints how many matching sessions share each value of FIELD
and ignores `--limit`. An unknown field name is an error. `--all` drops the
default seven-day window; from 0.2.8 an explicit `--since` still applies with it.

Session rows include `launch_surface`, `launch_subject`, `launch_launcher`, and
`launch_parent`, parsed from a launch stamp line `[harbor-launch v1 key=value ...]`
at the start of a line in the first prompt: the first such stamp within the first 4,000 characters of the
full first user message (Claude and Codex; other harnesses use the 600-character
`first_prompt` excerpt), so a stamp after a wrapper's header still counts.
Missing values are null; percent-encoded values are decoded.
`ls` and `find` accept repeatable `--surface VALUE` and `--launcher VALUE` filters
with exact matching before `--limit`; `--surface none` selects unstamped sessions.
The launch fields also work with `--fields` and `ls --count-by`.
`usage --json` includes a `launch` object with `surface`, `subject`, `launcher`,
and `parent` keys, each null when absent. Session rows include `duplicates_dropped`:
repeated calls across the requested ids are counted only in the first session,
including when a T3 thread and its provider id name the same transcript.
`--keep-duplicates` (from 0.2.39) skips that cross-id step and leaves `duplicates_dropped`
at 0, for callers that batch ids and drop repeated `(harness, dedupe_key)` pairs themselves.

`usage ID... --calls` emits one row per call. After `tools`, call rows include
`dedupe_key`, `speed` (`standard`, `fast`, or `ultrafast`) and `reported_cost_usd`
(null when unreported). Claude keeps the first usage for each message/request
pair and emits advisor calls separately. Codex validates request usage against
cumulative counters and suppresses copied fork history. Session totals sum the
kept calls. `--since` and `--until` filter calls before cross-session deduplication.
OpenCode calls have a null key and cost, and standard speed.

`find` prints each session's header and up to `--hits` matching lines
(default 3) of `--width` characters (default 220). `--ids` prints only one
address per matching session, best first, in the same form as `ls --fields
address`, which `show` and `resolve` take back; `--count` prints only the number of matching sessions,
including those past `--limit`. The summary line goes to stderr.

From 0.3.0, `find` over the default `--in prompts` scope and `--in assistant`
answers from a SQLite full-text index instead of reading every Claude, Codex and
Grok transcript. The index holds one row per prompt, command and assistant event
(lowercased, FTS5 trigram tokenizer, `find-index-v4.sqlite` in the cache
directory, `~/.cache/agent-traces` unless `AGENT_TRACES_CACHE` says otherwise)
and is this tool's own file, never a session store. Each `find` first checks the
size and modification time of every transcript it would search, reads only the
new complete lines of a transcript that grew with its indexed tail intact, and
reindexes one that shrank, was rewritten in place or is compressed. It also drops
the rows of transcripts deleted from disk. A
query spends at most 8 seconds refreshing files; any transcript still out of date
after that is scanned raw in the same query, so the answer is the same and only
slower. `--in tools`, `--in results`, `--in all` and `AGENT_TRACES_NO_INDEX=1`
scan raw transcripts as before. T3 and OpenCode keep their own SQL prefilter.
If the index cannot be opened or written (a locked, corrupt or read-only cache),
the query scans raw and reports an info diagnostic `index_unavailable`.
The `searched:` footer ends with `index: N indexed, M refreshed now, K scanned
raw` (or `off`, or `unavailable (reason)`), and versioned `find --json` carries the
same text as `result.index`. The first `find` after install indexes within its
budget and scans the rest raw; run `agent-traces index` once, or on a schedule,
to index everything ahead of time. It prints the files already indexed, those
refreshed, the event count, the seconds taken and the index size on disk.
`index` takes the session filters of `ls` (`--harness`, `--subagents`, `--cwd`,
`--since`, `--until`). Deleting `find-index-v4.sqlite` is always safe: it is
rebuilt on demand. Older binaries use separate versioned index files, which are
left intact so both versions can share a cache safely. The v4 index starts empty
and validates sources itself; it never imports older rows as validated coverage.
Raw scans also validate records that do not contain the search terms, which
costs extra CPU compared with a byte-only prefilter. A current v4 index reuses
stored diagnostics without reparsing those transcripts.

From 0.4.0 the index also stores each event as the adapter produced it, so a
`find` scores the transcripts it holds current from the index and reads none of
them. BM25 corpus statistics then cover every in-scope event of those sessions,
where the raw scan counts only lines that hold a term plus a count of the rest, so
`_score` and the order of close results can differ between the two paths. The
matching sessions and the set of hits per session do not differ, apart from the
exceptions below: repeated uuids, terms holding characters JSON escapes, and text that
only lowercases to an ASCII term. Hits are ordered by BM25, so their order, and where `--hits N`
truncates, can differ: the same session may show a different N hits.

Both paths fold case with Unicode lowercasing over the decoded text, so `ärger` finds
`ÄRGER` and a JSON-escaped `\u00c4RGER` with or without the index. The raw scan (no index,
over the refresh budget, `AGENT_TRACES_NO_INDEX=1`) keeps its byte prefilter for ASCII terms;
for a term with a non-ASCII character it keeps every file holding the term in any simple case,
its lowercase, its uppercase, a `\u` escape or `İ`, and every line holding a `\u` escape or whose Unicode-lowercased text contains the term,
for the decoded check, so it runs about twice as long as an ASCII term. A term whose lowercase
is ASCII stays on the byte prefilter, so text that only lowercases to it (the Kelvin sign `K` for `k`,
a `\u`-escaped ASCII letter) matches only through the index. Claude records that repeat a uuid are deduplicated in both paths, keeping the first, but not identically: the raw scan drops lines without a term before it checks uuids, so which record claims a uuid depends on the query, and the index keeps the first stored record whatever the query. The two differ only when records sharing a uuid differ in content or kind (text, or `isMeta` and other records the index does not store); copies from a resumed session are identical.  Likewise the raw scan tests the JSON line, so a
term holding a double quote, a backslash or a control character such as a newline
(escaped there), or one that matches only through the replacement character an invalid
UTF-8 byte decodes to, also matches only through the index.

Legacy `ls --json`, `find --json`, `resolve --json`, and `usage --json` emit JSON Lines. A
`find --json` line is the session row plus `_score` and `hits` (each `ts`,
`role`, `snippet`): five hits, or as many as an explicit `--hits` asks for.
Mailbox consumers should use `--json --schema-version 1` and set
`AGENT_TRACES_HOST_ID` to a stable host namespace. The versioned envelope applies
to ls, find, live, resolve, me, and handoff. See [the contract](docs/cli-contract.md)
and [schema](schemas/cli-v1.schema.json). Check exit status and coverage before
treating an empty result as a complete scan.

Stores are opened read-only using their existing WAL configuration. The program
does not switch journal mode, checkpoint, migrate, or modify store content.
Read-only SQLite access can take transient read locks and use WAL shared-memory
coordination. Generated caches and handoff output belong outside session stores.
T3 Code reads orchestrator v2 threads from `~/.t3/userdata/statev2.sqlite` and
adds v1 threads from `state.sqlite` whose IDs are absent from v2. A v2 row takes
precedence, including a deleted row. Machines with only v1 keep using the v1
store. Provider session IDs resolve to their T3 thread, including IDs from
previous provider threads in v2. `where` reports each store separately.

Hermes has no adapter in this release.

## Verify

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked
python tests/parity.py
```

Python is a development-only fixture and golden test dependency. Optional schema checks
use `python -m pip install jsonschema==4.26.0`, then
`python tests/parity.py --schema`. See [parity scope](docs/parity.md).

CI is configured for native Windows, macOS ARM64/x64, and Linux verification. The manual release
workflow verifies native release executables and builds archives with SHA-256
checksums. Publication attaches those verified archives to a versioned GitHub
release; the workflow itself has read-only repository permissions.
Its targets are Windows x64, macOS ARM64 and x64, and Linux x64 musl.
Linux ARM64 is a later matrix addition requiring a native runner or a tested
cross-toolchain. Rust target details come from the
[official platform list](https://doc.rust-lang.org/rustc/platform-support.html).

## License

MIT. See [LICENSE](LICENSE).
