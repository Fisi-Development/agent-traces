# Commit evidence (`agent-traces commit`), v1.0

Frozen 2026-10-20 with Telepathica, the consumer. agent-traces emits facts only, with no eligibility or
routing field; the consumer owns routing and liveness. Changing a field, a claim rule, the shell-form
rules or the consumer rule needs a new version agreed with the consumer.

Command (local, read-only, one host): `agent-traces commit REV [--repo DIR] [--since T] [--json]`
REV: 7-40 hex chars. `--repo` (default: the current directory when it is inside a git work tree) lets
agent-traces resolve REV with local git and compare each session's cwd repository. Exit: 0 at least one
`ran_commit` claim; 1 complete scan with no `ran_commit` (leads may still be listed); 2 error;
3 partial coverage (whatever was found). `complete=true` is operational scan completeness: every session
transcript present on disk in the selected stores was read. It is not exhaustive historical coverage. An indexed session whose transcript no longer exists on disk (Codex index rows
outliving deleted rollouts) cannot be read by anything on that host; such sessions are counted in one
coverage entry per harness with `status=absent`, `store=null` and reason `<n> indexed sessions have no
transcript on disk`, and do not make the scan partial. That entry, with its count, is always kept in
`coverage`; a missing transcript may have held the evidence.

One JSON object on stdout:

```json
{
  "schema": "agent-traces.commit-evidence", "version": "1.0",
  "host": "host-a",
  "observed_at": "2026-10-20T01:02:03.000000Z",
  "query": {"rev": "abc1234", "repo": "<abs path>|null"},
  "commit": {"status": "resolved|ambiguous|not_found|unchecked",
             "oid": "<40 hex>|null", "candidates": ["<40 hex>"], "committed_at": "...Z|null"},
  "authorship": "not_inferred",
  "complete": true,
  "coverage": [{"harness": "claude", "store": "<abs path>|null", "status": "<v1 coverage status>", "reason": null}],
  "candidates": [{
    "session": {"address": "t3:<thread>@host | claude:<id>@host",
                "provider_address": "claude:<id>@host",
                "harness": "claude", "id": "<id>", "host": "host-a",
                "cwd": "<native path>|null", "updated": "...Z|null"},
    "best_claim": "ran_commit|commit_without_result|observed|mentioned",
    "repo_match": "same|different|unknown",
    "claims": [{
      "claim": "ran_commit",
      "strength": "harness_record|tool_output|text",
      "shell_form": "single|compound|unknown|null",
      "banner_count": 1,
      "recorded_sha": "abc1234",
      "sha_match": "full|prefix",
      "abbrev_unique": true,
      "operation": "committed|amended|null",
      "branch": "main|null",
      "ts": "...Z|null",
      "source": {"path": "<abs transcript or db path>", "selector": "tool_use_id=<id>|null"}
    }]
  }]
}
```

## Claims

- `ran_commit`: a git commit-family tool call (commit, merge, cherry-pick, revert, rebase, am) whose
  paired, non-error result reports the sha. `harness_record` only from a harness-owned structured field
  (Claude Code `toolUseResult.gitOperation.commit` on the record that carries the paired `tool_result`).
  `tool_output` from a `[branch sha]` banner in the paired output. A paired, non-error result that carries
  a harness record is `ran_commit` whatever its call's `shell_form`, including `unknown` (for example a
  PowerShell call): the harness recorded the commit, and `shell_form` still reports what the classifier
  could confirm. A banner alone needs a classified commit-family call. Evidence, not proof or authority.
  The harness record is written by the harness but derived from the command's output: probed on Claude
  Code 2026-10-20, a command containing `git commit --dry-run` beside `echo "[main <sha>] x"` produced a
  `gitOperation.commit` for the echoed sha, while a plain echo produced none and a `git commit -q`
  (no banner) produced none. A compound command can therefore forge one.
  `ran_commit` is a producer evidence classification. It is not independently verified execution,
  authorship or authority; the forged record above is a `ran_commit` claim for a commit that never ran.
- `commit_without_result`: a commit-family call with no paired result, `repo_match=same`, within
  15 minutes of `commit.committed_at`. Never meets the consumer rule.
- `observed`: the sha in output of a non-commit call, a failed commit call, or any output the rules above
  do not accept. Never meets the consumer rule.
- `mentioned`: the sha in user, assistant, injected text or tool input, including banners and
  structured-looking JSON pasted as text. Never meets the consumer rule.

## Shell form (command classification, not substring matching)

The tool input is split into segments on unquoted `&&`, `||`, `;`, `|` and newlines.
- `single`: exactly one commit-family git segment; any other segments are only `cd`/`pushd` or
  `git add|rm|mv|status|config`. A `-m "$(cat <<'EOF' ... EOF)"` message argument is allowed, and so is a
  quoted here-document feeding the commit segment's stdin (`git commit -F- <<'EOF'` ... a line `EOF`;
  `<<"EOF"` and `<<-'EOF'` too), whose body stays opaque. File-descriptor duplication (`2>&1`, `1>&2`)
  is ignored; any other redirection is `unknown`.
- `compound`: a commit-family segment plus any other command (echo, printf, cat, scripts, ...).
- `unknown`: unbalanced quotes, other command substitution, backticks, `eval`, `bash -c`/`sh -c`/`pwsh -c`,
  a Codex code-mode script with zero or more than one extractable command, or anything unparsed.
Only a classified commit-family segment makes a call commit-family; the words "git commit" inside an
argument, a message, or a string do not.

## Consumer rule (frozen invariant; Telepathica applies it, agent-traces does not emit it)

Automatic recipient selection from commit evidence is allowed only when all hold:
- every expected host returned a document with `complete=true` (a missing or unreachable host is incomplete);
- no expected host's `coverage` has an `unreadable`, `busy`, `unsupported_storage_mode` or `absent`
  item for a harness inside the expected scan scope. agent-traces cannot tell a store that is missing
  from a harness that is not installed, so an `absent` store blocks like unknown coverage. A harness is
  outside the scope only when the caller excluded it (`--harness`) or knows it is not installed on that
  host. Absent indexed-transcript items always block;
- `commit.status=resolved` on the host whose repo is used;
- exactly one candidate across all hosts has a `ran_commit` claim meeting: `repo_match=same`,
  `sha_match` against `commit.oid`, `abbrev_unique=true` (or `sha_match=full`), `shell_form=single` and
  `banner_count=1`, with either strength. A `ran_commit` claim with `shell_form` compound or unknown is
  factual evidence but never meets this rule, whatever its strength (revision of 2026-10-20 after the
  forged-record probe above; the earlier text allowed `harness_record` with `compound`).
Otherwise every candidate is a lead and the caller selects an explicit address. Runtime auto-routing is
deferred (2026-10-20); the explicit-address path is unchanged. Liveness,
ended and unavailable are Telepathica's, resolved from the address. A read-only lookup of the exact id (Telepathica
`find <full id> --host H --harness X --json`) that returns no exact match shows only that the session is
not currently discoverable in that query, not that it ended; an ended state needs an explicit record. No transcript text is included;
`agent-traces show <address>` reads it on its own host. Cross-host = the same command per host over
existing host transport; no shared archive.

`banner_count` counts `[branch sha]` banners in the paired output (null when no paired output).
