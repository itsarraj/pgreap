//! The only part that touches a server: one read-only poll of
//! `pg_stat_activity`, and — only when the caller has already decided to
//! act on it — `pg_terminate_backend()`.

use anyhow::{Context, Result};
use postgres::{Client, NoTls, Row};

use crate::criteria::Backend;

const ACTIVITY_SQL: &str = "
SELECT
    pid,
    COALESCE(usename::text, '')                                       AS usename,
    COALESCE(datname::text, '')                                       AS datname,
    COALESCE(application_name, '')                                    AS application_name,
    COALESCE(state, '')                                               AS state,
    COALESCE(EXTRACT(EPOCH FROM (now() - state_change)), 0)::float8   AS state_secs,
    COALESCE(EXTRACT(EPOCH FROM (now() - query_start)), 0)::float8    AS query_secs,
    COALESCE(query, '')                                               AS query
FROM pg_stat_activity
WHERE backend_type = 'client backend'
  AND pid <> pg_backend_pid()
";

pub fn connect(conninfo: &str) -> Result<Client> {
    Client::connect(conninfo, NoTls).context("could not connect to Postgres")
}

fn row_to_backend(row: &Row) -> Backend {
    Backend {
        pid: row.get("pid"),
        user: row.get("usename"),
        database: row.get("datname"),
        application_name: row.get("application_name"),
        state: row.get("state"),
        state_secs: row.get("state_secs"),
        query_secs: row.get("query_secs"),
        query: row.get("query"),
    }
}

/// Every client backend on the server except `pgreap`'s own connection —
/// excluded server-side (`pid <> pg_backend_pid()`) so this tool can never
/// flag or terminate itself no matter what thresholds it's given.
pub fn snapshot(client: &mut Client) -> Result<Vec<Backend>> {
    Ok(client
        .query(ACTIVITY_SQL, &[])
        .context("querying pg_stat_activity (needs pg_read_all_stats or superuser to see other users' backends)")?
        .iter()
        .map(row_to_backend)
        .collect())
}

/// `pg_terminate_backend(pid)` — returns `true` if a signal was actually
/// sent, `false` if the pid was already gone by the time we got here (a
/// race between the snapshot and the terminate, not an error).
pub fn terminate(client: &mut Client, pid: i32) -> Result<bool> {
    let row = client
        .query_one("SELECT pg_terminate_backend($1)", &[&pid])
        .with_context(|| format!("terminating backend {pid}"))?;
    Ok(row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_sql_excludes_self_and_filters_client_backends() {
        // Not a live-query test (that's covered by the README's live
        // verification) — just pins down the two conditions this tool's
        // safety depends on so a future edit can't silently drop them.
        assert!(ACTIVITY_SQL.contains("pid <> pg_backend_pid()"));
        assert!(ACTIVITY_SQL.contains("backend_type = 'client backend'"));
    }
}
