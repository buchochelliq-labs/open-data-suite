# ADR-0004: CLI framework, module registration and exit codes

- **Status:** Proposed
- **Date:** 2026-09-24
- **Issues:** #6 (builds on ADR-0003 / #108; config is #7)
- **Deciders:** @n1ckyb

## Context
`ods` is one binary that fronts many independently developed modules (State, ERD, Usage,
CI, LSP, Agent, and later Mesh and Synthetic). Issue #6 asks for:

- a way to register module commands;
- consistent config, logging and exit codes;
- a machine-readable JSON output mode;
- shell completion that is at least planned and documented.

ADR-0001 constrains where command code lives: module crates cannot depend on `ods-cli`,
because it is the composition root. ADR-0003 already defines output modes, streams and
the JSON envelope. What is missing is how commands are assembled, how failures are
reported, and what scripts can rely on.

## Options considered

### Option A: one static clap `enum` in `ods-cli` (the status quo)
- ✅ Simple, fully typed.
- ❌ Every new command edits one central enum and `match`. Modules can't be added,
  tested or feature-gated independently, and there's no seam for plugins.

### Option B: a `Module` trait and registry in `ods-cli` (chosen)
Each command group implements a small trait that returns its `clap::Command` and runs
against a shared `Context`. A registry assembles the root command.
- ✅ Command groups are self-contained and testable, and can be feature-gated.
- ✅ Keeps ADR-0001's direction: adapters live in `ods-cli` and call module-crate APIs.
- ❌ Uses clap's builder API at the root, so the root is not a single derive.

### Option C: out-of-process plugins only (git-style `ods-<name>` executables)
- ✅ Any language, no recompilation.
- ❌ Executes arbitrary binaries found on `PATH`, which needs a policy (#9) and
  signature story. Doesn't solve structure for first-party modules.

## Decision
Adopt **Option B** now. **Option C** is planned as an extension of the same registry
(see §6) once the policy framework (#9) exists.

### 1. Module registration
```rust
pub trait Module {
    /// The subcommand definition; its name is the command word (`state`, `erd`, …).
    fn command(&self) -> clap::Command;
    /// Runs the subcommand. Results go through `ctx.emit` (ADR-0003); failures are
    /// returned as `CliError`, never printed or `exit`ed directly.
    fn run(&self, matches: &clap::ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError>;
}
```
- The `Registry` holds modules in display order and builds the root command:
  `ods` global flags plus one subcommand per module.
- Registration rejects duplicate names and reserved words (`help`), so a clash fails
  in tests, not at runtime.
- Adapters for module crates live in `ods-cli/src/commands/`. They translate
  arguments, call the module crate's API, and hand the result model to `ctx.emit`.
  Module crates never see clap or the terminal.
- Modules on the roadmap but not implemented yet are registered as **planned**
  commands. They show in `--help` with their milestone, accept any arguments, and
  exit with status 3. So `ods state plan` reports "not implemented", not a usage error.
- **Global flags work anywhere on the line.** A module may declare a passthrough
  argument that captures an arbitrary tail, as planned commands do. The framework then
  moves any global flags it finds in that tail (`--json`, `-o plain`, `-vv`, `--help`, …)
  to just after the subcommand and parses the line once more. Clap still validates
  them, so `-o yaml` remains a usage error. Flag spellings come from the clap
  definitions, so the two cannot drift, and tokens after `--` stay literal.

### 2. Context
`Context` is the single thing a command receives besides its arguments:
- the resolved output settings (ADR-0003 §2);
- the stdout writer, used through `emit`;
- the root `clap::Command`, used by `completions`.

Configuration (#7) and policy (#9) will be added to `Context`, so every command gets
them the same way.

### 3. Exit codes (public contract)
| Code | Name | Meaning |
|---|---|---|
| 0 | success | The command did what was asked. Also used when stdout closes early (broken pipe). |
| 1 | failure | The command could not complete: I/O, provider or internal error. |
| 2 | usage | Invalid arguments or flags (reported by clap). |
| 3 | not implemented | The command exists on the roadmap but is not available yet. |
| 4 | config | Configuration or profile is invalid or missing (#7). |
| 5 | check failed | The command ran correctly and its verdict is negative, e.g. a CI gate or validation found blocking issues. |

- Codes are stable. New meanings take new numbers below 64. 64 and above are not used,
  to avoid clashing with `sysexits.h` and shell-reserved codes.
- The output mode never changes the exit code.

### 4. Errors
Commands return `CliError { status, code, message, hint }`:
- `code` is a stable identifier (`ODS-E0001`, …) documented in `docs/cli.md`;
- `hint` is an optional next step.

Rendering:
- **human/plain:** `error[ODS-E0003]: message`, then `  hint: …`, on **stderr**.
- **json:** the ADR-0003 envelope on **stdout**, with `"result": null` and the error as
  a diagnostic (`hint` is its own optional field). Machines always get exactly one JSON
  document, even on failure, with these exceptions:
  - usage errors come from clap before output settings are known, so they are always
    printed as human text on stderr, with exit 2;
  - if writing to stdout itself failed, or writing the envelope fails, the error goes
    to stderr, so there is never a second or partial document on stdout;
  - `ods completions` prints its script as-is in every mode.

### 5. Logging
- `tracing` events go to **stderr** only, so they never corrupt stdout or JSON.
- The level defaults to `warn`: `-v` gives info, `-vv` debug and `-vvv` trace, and
  `-q` gives errors only. `ODS_LOG` (`off|error|warn|info|debug|trace`) overrides the
  flags for debugging in CI.
- *Amended (#220):* `--log-level <off|error|warn|info|debug|trace>` names a level on
  the command line. It conflicts with `-v`/`-q` and overrides `ODS_LOG`: a level
  named for this one command is the most specific wish.
- Log colour follows the stderr terminal and the same `--color` / `NO_COLOR` /
  `TERM=dumb` rules as results.

### 6. Shell completion and plugins
- `ods completions <bash|zsh|fish|powershell|elvish>` prints a completion script
  generated from the registered commands, so completions always match the binary.
  Installation is documented in `docs/cli.md`.
- **Planned (not implemented):** external plugins as `ods-<name>` executables,
  registered through the same registry as an "external" module kind. They run only when
  allowed by policy (#9), receive the resolved global flags, and must honour this
  ADR's streams and exit codes.

## Consequences
- **Positive:**
  - Scripts and CI can rely on documented exit codes and on a JSON document for every
    outcome.
  - Adding a command is one adapter file and one `register` call.
  - Completions and `--help` stay in sync with what is registered.
- **Negative / trade-offs:**
  - The root command uses clap's builder API.
  - Planned stubs accept any arguments, so a typo under a planned command is reported
    as "not implemented", not as a usage error.
  - Two new dependencies: `clap_complete` and `tracing-subscriber`. Both are
    MIT/Apache-2.0.
- **Follow-up work:**
  - #7: load config into `Context`, using exit code 4 for config errors.
  - #9: policy for external plugins.
  - #101: record the exit codes and error codes in the changelog as a public contract.

## References
- #6; ADR-0001 (module boundaries); ADR-0003 (output modes and JSON envelope)
- `docs/cli.md`: user-facing flags, exit codes, error codes and completions
