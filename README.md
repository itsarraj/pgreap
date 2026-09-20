# pgreap

Finds `idle in transaction` and long-running connections on a live
Postgres server and, only with an explicit `--execute` flag, terminates
them. The manual version of this is a `pg_stat_activity` query pasted from
a wiki, eyeballed, then a hand-typed `pg_terminate_backend(1234)` typed
under pressure during an incident — this is that same workflow made safe
to run without thinking twice, and safe enough to put in a cron job.

## Usage

```bash
pgreap "postgres://postgres@localhost:5432/mydb"                      # dry run, default 60s idle-tx threshold
pgreap "postgres://postgres@localhost:5432/mydb" --idle-tx-seconds 300 # only flag ones idle 5+ minutes
pgreap "$DATABASE_URL" --query-seconds 600                             # also flag active queries running 10+ min
pgreap "$DATABASE_URL" --database billing --user batch_job             # narrow to one db/role
pgreap "$DATABASE_URL" --idle-tx-seconds 300 --execute                 # actually terminate the matches
```

```
$ pgreap "postgres://postgres@localhost:5432/pgreap_test" --idle-tx-seconds 1 --database pgreap_test
1 backend(s) match the reap criteria (dry run — nothing terminated; pass --execute to act):
  pid 128624   psql                 postgres/pgreap_test  idle in transaction for 10.1s    SELECT 1;

$ pgreap "postgres://postgres@localhost:5432/pgreap_test" --idle-tx-seconds 1 --database pgreap_test --execute
1 backend(s) matched; terminating:
  pid 128624   psql                 postgres/pgreap_test  idle in transaction for 17.8s    SELECT 1;  -> terminated

1/1 backend(s) actually terminated
```

**The default is always a dry run.** Nothing is ever terminated unless
`--execute` is passed explicitly — there is no config file setting, env
var, or "yes to all" flag that skips this. `pgreap` also can never flag or
kill its own connection: the query it runs excludes its own backend pid
server-side (`pid <> pg_backend_pid()`), regardless of thresholds.

## What it flags, and why the two categories are separate

**`idle in transaction`** (and `idle in transaction (aborted)`) is on by
default (`--idle-tx-seconds`, default 60s) because it is almost never
intentional: a backend that ran a statement, never called `COMMIT` or
`ROLLBACK`, and is now just sitting there — holding whatever row/table
locks it acquired, and pinning `xmin` so autovacuum can't clean up dead
tuples anywhere in the database, not just in the tables it touched. A
connection pool that "returns" a connection to the pool without an
implicit rollback is the most common real cause.

**Long-running active queries** (`--query-seconds`, off by default) are a
different, murkier case — a nightly ETL job or a big analytical query can
legitimately run for an hour. Flagging those by default would make
`pgreap --execute` in a cron job a good way to kill a real report
mid-run, so this category is opt-in: pass `--query-seconds N` explicitly
to have `pgreap` also look at `active` backends by `query_start` age.

Plain `idle` (no open transaction, no locks held) is never flagged by
either rule — closing an idle pooled connection isn't a safety problem
this tool exists to fix.

`--database`/`--user` narrow the scan; `--exclude-pid` (repeatable) is an
escape hatch for a specific backend you know is fine and don't want asked
about every run.

## Status: built and verified live against a real Postgres, both reap criteria, both dry-run and --execute

- **27 unit tests** (`cargo test --lib`), all on pure logic: the
  idle-in-transaction rule (including the exact-threshold boundary
  inclusive, `(aborted)` counted the same as the plain state, plain
  `idle` never matched no matter how old); the long-running-query rule
  off by default and only active once `--query-seconds` is set, at its
  own boundary; `--database`/`--user`/`--exclude-pid` filtering; priority
  when a backend could theoretically match both rules; and the report
  renderer (empty-match messaging, long query text truncated to 80
  characters without splitting a UTF-8 boundary in ASCII-only content,
  multi-line query text flattened to one line, the terminated/already-gone
  count in `--execute` output).
- **The idle-in-transaction path, live, exactly as specified**: opened a
  real `psql` session against a throwaway local Postgres 18.6 scratch
  database, ran `BEGIN; SELECT 1;` and left it connected and idle.
  `pg_stat_activity` confirmed it directly: `state = idle in transaction`.
  Running `pgreap --idle-tx-seconds 1 --database pgreap_test` (dry run,
  the default) correctly listed pid 128624 as a match — and a follow-up
  `pg_stat_activity` query immediately after confirmed **it was still
  connected and still idle in transaction**, proving the dry run touched
  nothing. Running the identical command again with `--execute` reported
  `pid 128624 ... -> terminated`, and an immediate `pg_stat_activity`
  query came back **zero rows for that database** — the backend was
  genuinely gone, not just reported as gone.
- **The long-running-query path, live, same rigor**: started a real
  `SELECT pg_sleep(30)` in a background `psql` session, waited 3 seconds,
  and ran `pgreap --query-seconds 1` (with idle-tx effectively disabled
  via an unreachable threshold, isolating this rule). Dry run correctly
  reported it as `long-running query for 3.1s` and a follow-up query
  confirmed the session was still `active`; `--execute` on the same
  criteria then reported `-> terminated`, and `pg_stat_activity`
  confirmed zero matching rows afterward.
- **Self-protection confirmed as a side effect of every run above**: each
  of those runs itself opened a `pg_stat_activity`-querying connection to
  the same database it was scanning, and never once appeared in its own
  results — the server-side `pid <> pg_backend_pid()` filter held.

**Not done / deliberately deferred**:
- **No `pg_cancel_backend()` (soft cancel) option.** This only ever does
  a hard `pg_terminate_backend()`, which drops the whole connection. A
  gentler "cancel the current statement, leave the connection" mode isn't
  implemented.
- **No continuous/daemon mode.** This takes one snapshot and exits — a
  `--watch`/interval mode like `pgtop`'s doesn't exist here; running this
  on a schedule is left to cron/systemd-timer.
- **No confirmation prompt before `--execute`.** Unlike `pganonymize`'s
  `--yes` gate on a destructive write, `--execute` here is the only gate;
  there's no dry-run-then-confirm two-step, which is a deliberate
  tradeoff for scriptability (a cron job can't answer an interactive
  prompt) but does mean a typo'd `--idle-tx-seconds 0 --execute` on the
  wrong connection string is a real footgun.
- **No output beyond plain text.** No `--json`, unlike some of this
  workspace's other Postgres tools — scripting against `pgreap`'s output
  today means parsing the plain-text table.
- **Superuser/`pg_signal_backend` requirement is Postgres's own rule, not
  checked in advance.** A role without permission to terminate a given
  backend gets whatever error `pg_terminate_backend()` itself returns;
  `pgreap` doesn't pre-check role membership and give a friendlier
  message.
