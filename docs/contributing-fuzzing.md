# Fuzzing

ODS reads files and text it doesn't control: dbt's artifacts and messages, SQL, and
lineage exports from Databricks. Fuzzing throws millions of generated inputs at those
readers and watches for a crash: a panic, a hang, or runaway memory. A fuzzer is
**coverage-guided**: it mutates the inputs that reach new code, so it finds inputs no
one would think to write. A crash there means a malformed or hostile file stops `ods`
or the dashboard, so every reader must turn bad input into an error instead (#192).

This complements the property tests (`tests/properties.rs` in `ods-core`,
`ods-lineage`, `ods-state` and the SQL provider), which check that results are
*right* over generated input; fuzzing checks that nothing *breaks* on any input.

## Targets

| Target | What it reads |
|---|---|
| `dbt_manifest` | `manifest.json` |
| `dbt_run_results` | `run_results.json` |
| `dbt_messages` | dbt's error output: `error_summary`, `project_failure` |
| `sql_analyzer` | any SQL, in every dialect; an opaque result must claim nothing |
| `redact` | redaction of engine messages; the result must be one short line with no control characters |
| `uc_lineage` | Unity Catalog lineage exports, CSV and JSON |

## Running

The fuzz crate lives in `fuzz/`, as its own workspace: `cargo-fuzz` uses
`libfuzzer-sys`, whose licence (LLVM's NCSA) isn't on the list in `deny.toml`, and it
never needs to be: nothing in ODS's build links it. You need a nightly toolchain and
`cargo-fuzz`:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz --locked
fuzz/seed.sh                                   # start from the fixtures
cargo +nightly fuzz run sql_analyzer -- -max_total_time=300
```

The `fuzz` workflow runs every target for ten minutes each night, and for one minute on
a pull request that changes `fuzz/`.

## When it finds something

A crash stops the run and saves the input under `fuzz/artifacts/<target>/` (in CI, as
the `fuzz-crash-<target>` artifact). To reproduce it:

```bash
cargo +nightly fuzz run <target> fuzz/artifacts/<target>/crash-…
```

Fix the reader so the input is an error, not a crash, and add the input as a unit test
next to it, so it stays fixed without the fuzzer.

## Adding a target

Add a file under `fuzz/fuzz_targets/`, a `[[bin]]` for it in `fuzz/Cargo.toml`, seeds
in `fuzz/seed.sh`, and the target to the matrix in `.github/workflows/fuzz.yml`. A
target calls the reader as ODS does (from a file, if that's how ODS reads it) and may
assert what must hold for any input.
