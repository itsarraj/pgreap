use anyhow::Result;
use clap::Parser;

use pgreap::criteria::{self, Criteria};
use pgreap::{db, report};

#[derive(Parser)]
#[command(
    name = "pgreap",
    version,
    about = "Finds idle-in-transaction and long-running Postgres connections; only terminates them with --execute"
)]
struct Cli {
    /// Connection string — `postgres://user@host:5432/dbname` or libpq
    /// keyword form. Required on purpose: this reaches into a running
    /// server and (optionally) kills sessions on it.
    conninfo: String,

    /// Flag `idle in transaction` (and `idle in transaction (aborted)`)
    /// backends that have sat in that state at least this many seconds.
    /// Pass 0 to flag every idle-in-transaction backend regardless of
    /// age; there is no way to disable this check other than picking an
    /// unreachably high number, since it is this tool's primary purpose.
    #[arg(long, default_value_t = 60.0)]
    idle_tx_seconds: f64,

    /// Also flag `active` backends whose current query has been running
    /// at least this many seconds. Off by default — a long-running query
    /// is often a legitimate report or batch job, unlike a forgotten open
    /// transaction, so this is opt-in rather than on-by-default.
    #[arg(long)]
    query_seconds: Option<f64>,

    /// Only consider backends connected to this database.
    #[arg(long)]
    database: Option<String>,

    /// Only consider backends connected as this role.
    #[arg(long)]
    user: Option<String>,

    /// Never flag this pid, even if it otherwise matches. Repeatable.
    #[arg(long = "exclude-pid")]
    exclude_pids: Vec<i32>,

    /// Actually call `pg_terminate_backend()` on every match. Without
    /// this flag, pgreap only lists what it would do — the default is
    /// always a dry run.
    #[arg(long)]
    execute: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let criteria = Criteria {
        idle_tx_seconds: Some(cli.idle_tx_seconds),
        query_seconds: cli.query_seconds,
        database: cli.database.clone(),
        user: cli.user.clone(),
        exclude_pids: cli.exclude_pids.clone(),
    };

    let mut client = db::connect(&cli.conninfo)?;
    let backends = db::snapshot(&mut client)?;

    let matches: Vec<(pgreap::criteria::Backend, criteria::Reason)> = backends
        .into_iter()
        .filter_map(|b| criteria::evaluate(&b, &criteria).map(|r| (b, r)))
        .collect();

    if !cli.execute {
        print!("{}", report::render_dry_run(&matches));
        return Ok(());
    }

    let mut results = Vec::with_capacity(matches.len());
    for (backend, reason) in matches {
        let ok = db::terminate(&mut client, backend.pid)?;
        results.push((backend, reason, ok));
    }
    print!("{}", report::render_execute(&results));

    Ok(())
}
