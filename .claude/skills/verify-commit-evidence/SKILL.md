---
name: verify-commit-evidence
description: >
  Check commit-evidence SHA token boundaries and ambiguous-prefix resolution with
  generated inputs through the real CLI. Use when changing src/evidence.rs
  tokenization, SHA matching, disambiguation or resolution, or investigating a
  session incorrectly listed for a commit.
---

# Verify commit evidence

This runs proptest through the real `agent-traces commit` command, disposable
Git repositories and synthetic Claude transcripts. It samples bounded inputs
and shrinks failures. It is not an exhaustive proof over arbitrary text.

## The rules

| Rule | Plain sentence | Harm when broken |
|---|---|---|
| `generated_token_lengths_and_prefixes` | Text yields a mention only for a matching 7–40 digit hex token, normalized to lowercase. | Unrelated text is listed as evidence for a commit, or valid evidence disappears. |
| `generated_identifier_boundaries` | A hex run within an alphanumeric identifier yields no mention; underscores delimit tokens. | An identifier links an unrelated session to a commit. |
| `generated_ambiguous_prefix_never_resolves` | Two commits sharing the queried prefix keep status ambiguous and oid null; a longer unique query resolves. | A caller selects the wrong commit or session. |

## Run it

From the Harbor checkout:

```sh
agent-traces/.claude/skills/verify-commit-evidence/scripts/check.sh
```

Requires Python 3, Git, Rust/Cargo and the existing build-slot wrapper at
`~/git/skills/delegation-skills/shared/scripts/preflight.py`. Every Cargo call
uses that wrapper with `--crate agent-traces`. Cargo.lock pins proptest 1.12.0;
Cargo.toml declares the crate-local dev-dependency `proptest = "1"`. First use
may need network access for locked dependencies.

The script runs only `commit_evidence_properties` with `--locked`,
`--include-ignored`, and one test thread. The three tests are ignored by the
normal suite. It keeps cargo.log and verdicts.json in the printed directory
under `~/.cache/harbor-verify-commit-evidence/` or `$XDG_CACHE_HOME`.
The process group has a 150-second budget including compilation and slot waits.
Timeout, compile errors and missing test results are infrastructure errors.

`model/expectations.json` is authoritative:

| Config (test name) | Expected verdict |
|---|---|
| `generated_token_lengths_and_prefixes` | holds |
| `generated_identifier_boundaries` | holds |
| `generated_ambiguous_prefix_never_resolves` | holds |

Exit 0 means every expectation matched; 1 means a property verdict mismatched;
2 means infrastructure failure or SOURCE DRIFT. All tests run even after a
property fails. No baseline findings are expected.

## What the model covers

Sync commit: `4a2c1c018f0e2a1ee70b45405206969760e9b2e5`.
Move it forward only after confirming that the generators and expectations
still describe the covered code. The code-level harness is
`tests/commit_evidence_properties.rs`; there is no separate state-machine model.

| Harness action | Code |
|---|---|
| Write assistant text into a synthetic store, query a full commit SHA | `sha_tokens`, `valid_sha`, `sha_match`, `ClaimBuilder::claims` text path |
| Check recorded_sha, sha_match and mentioned claim | `ClaimBuilder::claim`, `run` candidate assembly |
| Import two real colliding commit objects, query their common prefix | `Repository::new`, `disambiguated`, `resolution` |
| Check ambiguous status, null oid, two candidates and abbrev_unique=false | `Repository::new`, `Repository::abbrev_unique`, `ClaimBuilder::claim` |
| Query 8–40 characters to resolve the first collision member | `Repository::new`, full-SHA resolution and metadata lookup |

`model/source.json` binds `src/evidence.rs`, `docs/commit-evidence.md`, the
harness, Cargo.toml and Cargo.lock. Unknown edits stop with SOURCE DRIFT.
It also recognizes three exact real-source mutations so they run the same
properties and fail behaviorally. These fingerprints detect drift, not security
custody. The CLI also executes adapters, search and core code; their behavior
is exercised but their source is outside this target's binding.

Bounds and assumptions:

- Three proptest runners each use 32 initial cases, ChaCha seed `[61; 32]`,
  at most 128 shrink iterations and no on-disk regression persistence.
- Each token-length case checks every length 0–45 plus one full SHA with a
  changed digit at a generated position. Generated dimensions are uppercase
  versus lowercase, that position (0–39), and one of six delimiters: space,
  underscore, newline, crab emoji, slash or colon. Each transcript also holds
  `g36a3723g`, an invalid identifier that admits it through the seven-character
  search prefilter without adding a valid SHA token. This lets the property
  observe accidental acceptance of shorter tokens.
- Boundary cases use prefix lengths 7–40, 1–12 nonhex ASCII letters, either
  case, leading/trailing letter identifiers, fixed Unicode letters é and 中,
  digits on both sides and an underscore-delimited positive control.
- Ambiguity uses two valid SHA-1 commit objects with common prefix `36a3723`:
  `36a3723a82d823d5f6b3816f8d5e169e2a263def` and
  `36a3723b4273a295d58f417fce01d3662c4660aa`. Git computes and checks their
  IDs when importing their bodies. They reference the real empty tree.
  Cases generate query case, 1–4 repeated mentions, and unique lengths 8–40.
- Git is installed and supports SHA-1 object storage; subprocesses operate only
  on disposable repositories. Synthetic homes isolate store discovery and
  caches; generated store records use the real adapter's text-block format.

Not covered:

- Authorship or automatic recipient selection. The frozen documentation says
  `authorship=not_inferred`; producer claims are evidence, not proof. The
  dispatch's blanket “credited with a commit” wording is narrowed accordingly.
- Suppressing every claim for ambiguous prefixes is not a producer promise:
  `claims` retains leads, while `Repository::new` leaves oid null and the
  documented consumer rule requires resolved status. The harness checks this
  real resolution promise and does not impose a new rejection policy.
- `commit_without_result` intentionally needs no SHA token. Structured harness
  records and tool-output banners have separate claim paths, outside this
  text-token property. Shell classification, pairing, errors and consumer
  routing require their own checks.
- Every possible SHA value/collision, arbitrary Unicode strings, arbitrary
  delimiters, multiple stores/harnesses, scan completeness, corrupted stores,
  Git failure handling, SHA-256 repositories and performance.

## When the check disagrees with its expectation

1. Read the printed cargo.log. Treat SOURCE DRIFT, compilation failure, timeout
   and missing results as infrastructure/coverage errors, not product findings.
2. Read proptest's shrunk tuple and assertion, then follow the corresponding
   text through the real adapter, tokenization, matching and claim assembly.
   Check fixture format and prefiltering before reporting missing evidence.
3. For ambiguity, compare the two Git IDs, status, oid and abbrev_unique. Keep
   feasible violations as findings; retain the property and name an expected
   failing verdict until an authorized product fix lands.

## When a change is intended

Review the functions and documentation in the coverage table against the
harness, then update the generators from the intended promise. Renew the five
file hashes in `model/source.json` only after that review. Recompute mutation
hashes by applying each exact replacement below to the new evidence.rs bytes;
review each replacement still targets the intended guard. Rerun all verdicts
and all mutation proofs, then move the sync commit forward. Use
`/maintain-formal-verification-skill` when a change skipped this check.

## Prove it before you hand off

In an isolated checkout, make each real-source mutation separately:

| Mutation | Exact replacement in src/evidence.rs | Must violate |
|---|---|---|
| HexSubstring | `!c.is_alphanumeric()` → `!c.is_ascii_hexdigit()` | identifier boundaries |
| ShortToken | `(7..=40).contains(&t.len())` → `(6..=40).contains(&t.len())` | token lengths |
| ResolveAmbiguous | `_ => "ambiguous",` → `_ => "resolved",` | ambiguity |

For each, save `git diff -- agent-traces/src/evidence.rs` outside the checkout,
run check.sh and require exit 1 with the named behavioral verdict mismatch.
Restore using `git apply -R` on that saved diff, and confirm the product diff
is empty. Finally rerun check.sh: all three baseline verdicts must hold.
Unknown edits must stop with SOURCE DRIFT instead. All three mutations were
run against the real code for this unit; scratch diffs and logs remain outside
the repository. Product changes do not belong in the final diff.
