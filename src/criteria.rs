//! Deciding which backends are worth reaping. Pure logic — no database
//! connection anywhere in this file — so every rule (and its edge cases)
//! is unit tested without needing a live Postgres.

/// One row of `pg_stat_activity`, already flattened to plain fields.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Backend {
    pub pid: i32,
    pub user: String,
    pub database: String,
    pub application_name: String,
    /// Raw `pg_stat_activity.state`: `active`, `idle`,
    /// `idle in transaction`, `idle in transaction (aborted)`, or empty
    /// for background workers that don't report one.
    pub state: String,
    /// Seconds since `state_change` — how long it has sat in `state`.
    pub state_secs: f64,
    /// Seconds since `query_start` (0 when there is no current query).
    pub query_secs: f64,
    pub query: String,
}

impl Backend {
    pub fn is_idle_in_transaction(&self) -> bool {
        self.state == "idle in transaction" || self.state == "idle in transaction (aborted)"
    }

    pub fn is_active(&self) -> bool {
        self.state == "active"
    }
}

/// What tripped the reap criteria for one backend, and for how long.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reason {
    IdleInTransaction { for_secs: f64 },
    LongRunningQuery { for_secs: f64 },
}

impl Reason {
    pub fn for_secs(self) -> f64 {
        match self {
            Reason::IdleInTransaction { for_secs } => for_secs,
            Reason::LongRunningQuery { for_secs } => for_secs,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Reason::IdleInTransaction { .. } => "idle in transaction",
            Reason::LongRunningQuery { .. } => "long-running query",
        }
    }
}

/// The thresholds and filters a run was invoked with.
#[derive(Debug, Clone)]
pub struct Criteria {
    /// Flag idle-in-transaction backends that have sat that way at least
    /// this long. `None` disables idle-in-transaction detection entirely.
    pub idle_tx_seconds: Option<f64>,
    /// Flag `active` backends whose current query has run at least this
    /// long. `None` (the default) disables this — a long-running query is
    /// often a legitimate batch job, unlike a forgotten open transaction.
    pub query_seconds: Option<f64>,
    pub database: Option<String>,
    pub user: Option<String>,
    pub exclude_pids: Vec<i32>,
}

/// Does `backend` meet the reap criteria, and if so, why?
///
/// Idle-in-transaction is checked before long-running-query so a backend
/// that somehow matches both (state flapped between polls, or a caller
/// passed unusual thresholds) is reported once, for the more actionable
/// reason.
pub fn evaluate(backend: &Backend, criteria: &Criteria) -> Option<Reason> {
    if criteria.exclude_pids.contains(&backend.pid) {
        return None;
    }
    if let Some(db) = &criteria.database {
        if &backend.database != db {
            return None;
        }
    }
    if let Some(user) = &criteria.user {
        if &backend.user != user {
            return None;
        }
    }

    if let Some(threshold) = criteria.idle_tx_seconds {
        if backend.is_idle_in_transaction() && backend.state_secs >= threshold {
            return Some(Reason::IdleInTransaction {
                for_secs: backend.state_secs,
            });
        }
    }

    if let Some(threshold) = criteria.query_seconds {
        if backend.is_active() && backend.query_secs >= threshold {
            return Some(Reason::LongRunningQuery {
                for_secs: backend.query_secs,
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(state: &str, state_secs: f64, query_secs: f64) -> Backend {
        Backend {
            pid: 100,
            user: "app".to_string(),
            database: "appdb".to_string(),
            application_name: "worker".to_string(),
            state: state.to_string(),
            state_secs,
            query_secs,
            query: "UPDATE accounts SET balance = 1".to_string(),
        }
    }

    fn default_criteria() -> Criteria {
        Criteria {
            idle_tx_seconds: Some(60.0),
            query_seconds: None,
            database: None,
            user: None,
            exclude_pids: Vec::new(),
        }
    }

    #[test]
    fn flags_idle_in_transaction_past_threshold() {
        let b = backend("idle in transaction", 61.0, 0.0);
        let reason = evaluate(&b, &default_criteria());
        assert_eq!(reason, Some(Reason::IdleInTransaction { for_secs: 61.0 }));
    }

    #[test]
    fn exactly_at_threshold_is_flagged_inclusive() {
        let b = backend("idle in transaction", 60.0, 0.0);
        assert!(evaluate(&b, &default_criteria()).is_some());
    }

    #[test]
    fn just_under_threshold_is_not_flagged() {
        let b = backend("idle in transaction", 59.9, 0.0);
        assert!(evaluate(&b, &default_criteria()).is_none());
    }

    #[test]
    fn aborted_idle_in_transaction_counts_too() {
        let b = backend("idle in transaction (aborted)", 100.0, 0.0);
        assert!(evaluate(&b, &default_criteria()).is_some());
    }

    #[test]
    fn plain_idle_is_never_flagged_by_idle_tx_rule() {
        // Plain `idle` (no open transaction) holds no locks and pins
        // nothing — reaping it would just be closing an idle pool
        // connection for no reason.
        let b = backend("idle", 10_000.0, 0.0);
        assert!(evaluate(&b, &default_criteria()).is_none());
    }

    #[test]
    fn active_query_not_flagged_unless_query_threshold_set() {
        let b = backend("active", 0.0, 10_000.0);
        assert!(evaluate(&b, &default_criteria()).is_none());
    }

    #[test]
    fn active_query_flagged_once_threshold_set_and_exceeded() {
        let mut c = default_criteria();
        c.query_seconds = Some(300.0);
        let b = backend("active", 0.0, 301.0);
        assert_eq!(
            evaluate(&b, &c),
            Some(Reason::LongRunningQuery { for_secs: 301.0 })
        );
    }

    #[test]
    fn active_query_under_threshold_not_flagged() {
        let mut c = default_criteria();
        c.query_seconds = Some(300.0);
        let b = backend("active", 0.0, 299.0);
        assert!(evaluate(&b, &c).is_none());
    }

    #[test]
    fn excluded_pid_is_never_flagged() {
        let mut c = default_criteria();
        c.exclude_pids.push(100);
        let b = backend("idle in transaction", 1000.0, 0.0);
        assert!(evaluate(&b, &c).is_none());
    }

    #[test]
    fn database_filter_excludes_other_databases() {
        let mut c = default_criteria();
        c.database = Some("otherdb".to_string());
        let b = backend("idle in transaction", 1000.0, 0.0);
        assert!(evaluate(&b, &c).is_none());
    }

    #[test]
    fn database_filter_matches_correct_database() {
        let mut c = default_criteria();
        c.database = Some("appdb".to_string());
        let b = backend("idle in transaction", 1000.0, 0.0);
        assert!(evaluate(&b, &c).is_some());
    }

    #[test]
    fn user_filter_excludes_other_users() {
        let mut c = default_criteria();
        c.user = Some("someone_else".to_string());
        let b = backend("idle in transaction", 1000.0, 0.0);
        assert!(evaluate(&b, &c).is_none());
    }

    #[test]
    fn idle_tx_disabled_when_threshold_is_none() {
        let mut c = default_criteria();
        c.idle_tx_seconds = None;
        let b = backend("idle in transaction", 999_999.0, 0.0);
        assert!(evaluate(&b, &c).is_none());
    }

    #[test]
    fn idle_in_transaction_takes_priority_over_long_running_query() {
        // A backend can't really be both `active` and `idle in
        // transaction` at once, but the priority rule is still worth
        // pinning down: idle-in-transaction is checked first.
        let mut c = default_criteria();
        c.query_seconds = Some(1.0);
        let mut b = backend("idle in transaction", 1000.0, 1000.0);
        b.state = "idle in transaction".to_string();
        assert_eq!(
            evaluate(&b, &c),
            Some(Reason::IdleInTransaction { for_secs: 1000.0 })
        );
    }

    #[test]
    fn reason_label_and_for_secs() {
        let r = Reason::IdleInTransaction { for_secs: 42.0 };
        assert_eq!(r.label(), "idle in transaction");
        assert_eq!(r.for_secs(), 42.0);
        let r = Reason::LongRunningQuery { for_secs: 7.0 };
        assert_eq!(r.label(), "long-running query");
        assert_eq!(r.for_secs(), 7.0);
    }
}
