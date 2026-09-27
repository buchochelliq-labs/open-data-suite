# ADR-0018: State store migrations, integrity and recovery

- **Status:** Proposed
- **Date:** 2026-09-27
- **Issues:** #188
- **Deciders:** @n1ckyb

## Context
The state store (ADR-0013) holds the only record of what was built and when. Once
users have real history, it has to survive four things:
- a new ODS changing the store's own storage format;
- an older ODS opening a store a newer one wrote;
- runs that fail or are interrupted part-way;
- damage from disk faults or other programs.

A damaged store must never lead to a reused build that didn't happen (AGENTS.md
rule 3). A failed write must never replace the last good state (rule 5). SQLite is the
only store today, but PostgreSQL and file stores are planned, so the rules must hold
for any store, not just one.

Before this ADR, the SQLite store already migrated forward in one transaction and
refused a database written by a newer ODS. It had no copy before migrating, no check,
and nothing to point people at when a store failed: they saw a raw SQLite error.

## Options considered
### Option A: rely on SQLite alone
SQLite's transactions already make commits and migrations all-or-nothing. But a
migration bug, or a damaged file, would still leave people without their state and
without a way forward. Not enough.

### Option B: a store-neutral contract, with store-specific recovery tools (chosen)
The contract says what every store guarantees and adds a read-only check. Each store
decides how it keeps copies, because a SQLite file, a PostgreSQL schema and a
directory of files are copied in different ways.

### Option C: automatic repair
Have ODS rebuild damaged state from the snapshots it can still read. Rejected for now:
repair would have to guess which history is right, and a wrong guess reuses builds
that never happened. People decide; ODS tells them what it found.

## Decision
**Contract (`state_store` 0.2).**
- `check()` reads the whole store without changing it. It reports:
  - the store's storage version, and the latest this build knows;
  - every scope, with its head and number of snapshots;
  - every problem, as one of: `damaged`, `newer_schema`, `unreadable_snapshot`,
    `dangling_head`, `broken_chain`.
  It fails only if it can't run at all. Damage it finds is a result, not an error.
- A new error, `ProviderError::Corrupt`. A store returns it when its storage can't be
  read, or when a record can't be decoded. It never guesses: callers stop, and never
  reuse builds on the strength of damaged state.
- A failed or interrupted commit leaves no trace. The conformance suite checks that
  refused commits leave the store sound.

**Storage versions and compatibility.**
- A store's storage format has a version of its own. It is separate from the snapshot
  schema version (ADR-0013), which is checked for every snapshot as before.
- A newer ODS migrates an older store forward when it opens it.
- An older ODS refuses a newer store and says to upgrade. It never downgrades.

**Migrations (SQLite).**
- A migration runs in one transaction. If any statement fails, nothing changes, and
  the error says so.
- Before migrating a database that already holds data, the store writes a copy next
  to it with `VACUUM INTO`: `<db>.v<version>-<unix time>.bak`. If the copy can't be
  written, the store doesn't migrate.
- The migration count is re-read under the write lock, so two ODS processes opening
  the same database at once migrate it only once.

**Damage (SQLite).**
- The store maps SQLite's `SQLITE_CORRUPT` and `SQLITE_NOTADB` errors, and snapshot
  documents that can't be decoded, to `Corrupt`.
- `check()` runs:
  - SQLite's `PRAGMA quick_check`;
  - a check that the store's tables exist;
  - a decode of every snapshot;
  - a check that every head and every parent points at a snapshot in the same scope.
- If the storage version is newer, `check()` reports that and reads no further,
  because its tables may mean something else now.

**Recovery (CLI).**
- `ods state doctor` opens the database read-only, runs `check()`, lists any copies
  beside it, and says what to do. It exits 1 with `ODS-E0405` if it finds a problem.
- Commands that hit a `Corrupt` error exit with `ODS-E0405` and point at
  `ods state doctor`.
- `ods state backup` writes a consistent copy with `VACUUM INTO`. This works while
  other runs are using the database.
- `ods state reset --yes` moves the database and its `-wal`/`-shm` files aside. It
  deletes nothing, and the files keep their suffixes so the moved database still opens.
  The next run then builds everything: the conservative outcome.
- Restoring means copying a backup over the database; `docs/cli.md` gives the steps.

## Consequences
- **Positive:**
  - A migration bug or a disk fault costs, at worst, one full rebuild. It never
    produces a wrong reuse, and a copy of the old state always exists.
  - People get a diagnosis and next steps instead of a raw SQLite error.
  - Future stores have a contract to meet, with conformance cases for `check()`.
- **Negative / trade-offs:**
  - Every migration of an existing database writes a full copy. That costs disk space
    once per storage version. Old copies aren't pruned yet.
  - `check()` decodes every snapshot, so it is slower on large histories. It only runs
    when someone asks for it.
  - There is no automatic repair and no reset of a single scope. Recovery replaces the
    whole database.
- **Follow-up:**
  - An `ods doctor` for the whole tool should include `ods state doctor`.
  - PostgreSQL and file stores (M2) must implement `check()`, run migrations as a
    single unit, and document how to back them up.
  - Pruning old copies, and resetting a single scope.

## References
- #188; ADR-0013 (state store); AGENTS.md rules 3 and 5
- SQLite: `VACUUM INTO`, `PRAGMA quick_check`, and the "How To Corrupt An SQLite
  Database File" page
- `docs/cli.md#recovering-state`
