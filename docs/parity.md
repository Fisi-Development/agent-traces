# Parity gates

`tests/parity.py` compares the native executable with stored expectations in
`tests/golden/cases.json` and `tests/golden/handoff_safety.json`. It does not load
or run a Python engine. Python remains development tooling for fixtures and
checks; `--schema` additionally needs the optional `jsonschema` package.

`tests/fixtures/corpus.py` builds synthetic stores for all five adapters. The
subprocess environment isolates provider homes, cache, output, self IDs, process
inventory, clock and timezone. Both host variables are fixed to `fixture-host`.
Every subprocess starts in the scratch home's `project` directory, including
empty-store checks. Synthetic transcript mtimes are fixed as well.
No command needs access to a real session store.

The 57 scenarios comprise 36 ordinary CLI cases, 10 handoff identity cases,
four Claude child cases and seven Markdown/cap cases. Ordinary cases compare
exit codes and JSON output, text output or the written handoff artifact. The
seven Markdown/cap cases also compare stored artifacts and exit codes. Identity
and child cases independently assert marker boundaries, rejection, exit codes
and whether an artifact was written. Markdown checks preserve code-span and
fence safety, UTF-8 byte caps, protected excerpts and omission counts. A native
10,000-prompt performance guard remains independent of the goldens.

Goldens are generated on Linux or macOS; `--update-golden` refuses to run on
Windows. On Windows only, the driver turns CRLF into LF and backslashes into
`/` in paths under the scratch home, absolute or in the `~\...` form, before
comparing. Of the seven stored Markdown/cap artifacts, the four that hit the
49,152-byte cap (`10000-prompts`, `500-prompts`, `emoji-protected`,
`auxiliary-bound`) are compared on Linux and macOS only: the cap counts CRLF on
Windows, so a capped artifact keeps fewer prompts there. The other three are
compared on every platform, and every case's independent checks and exit code
run everywhere.

Each golden entry records its ID, arguments, output kind, exit code and expected
output. JSON is parsed one value per output line; object member order is
immaterial. Keys, nulls, list order and other text remain significant. Actual
output replaces the scratch home with `<HOME>`, canonicalizes equivalent
RFC3339 spellings in the listed timestamp fields, and on Windows applies the
line-ending and path-separator rules above. stderr remains
diagnostic and is not compared. Missing or extra golden entries fail the run.

All ordinary commands also run with an empty PATH to detect runtime program
dependencies. Listing must yield four merged sessions. `--schema` validates
populated and empty-store v1 envelopes, status/exit agreement, live observations,
self identity and missing-host rejection. Store-content hashes before and after
execution protect against mutation. WAL coordination files are excluded because
ordinary read-only SQLite can manage shared memory; this does not permit writes
to application data. Fixtures create WAL databases before commands open them.

## Running and regenerating

From `agent-traces/`, use an already built executable:

```sh
python3 tests/parity.py --schema --binary target/debug/agent-traces
python3 tests/parity.py --case resolve --binary target/debug/agent-traces
python3 tests/parity.py --update-golden --binary target/debug/agent-traces
```

`--binary` defaults to the debug executable. `--case` filters ordinary comparisons
and includes handoff safety when its filter contains `handoff`. It cannot be
combined with `--update-golden`. Regeneration rewrites both golden files from the
built native binary. A person reviews every regenerated diff against intended
behavior before accepting it. CI never regenerates expectations.

These synthetic checks do not establish compatibility with every real store.
Preserve sanitized counterexamples as fixtures. Native Windows, macOS and Linux
CI runs native checks and the same golden suite, including release binaries.

## Deliberate differences and boundaries

Windows process probes use query-only APIs and compare recorded FILETIME start
values to detect PID reuse. UTF-8 output is independent of the Windows code page.
Search uses an in-process prefilter. SQLite databases must already use WAL;
other modes are reported, never converted. Malformed records produce diagnostics
and v1 partial results. Legacy stderr is not byte-identical and JSON object key
order is not an interface guarantee.

The legacy parser accepted nonstandard JSON NaN/Infinity and lone surrogates
that serde_json rejects. Rust reports those records as malformed. Negative
count flags retain Python slicing behavior and have explicit golden cases.
