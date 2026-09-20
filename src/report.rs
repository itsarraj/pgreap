//! Rendering a poll's results, in both the default dry-run form and the
//! `--execute` form that records what was actually done to each match.

use crate::criteria::{Backend, Reason};

/// `61.0` -> `61.0s`, `125.4` -> `2m05s`, `3725.0` -> `1h02m`. Mirrors the
/// units a human actually reads off a duration at a glance rather than a
/// raw seconds count.
pub fn format_secs(secs: f64) -> String {
    let secs = secs.max(0.0);
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else if secs < 3600.0 {
        let m = (secs / 60.0) as u64;
        let s = (secs - (m as f64 * 60.0)) as u64;
        format!("{m}m{s:02}s")
    } else {
        let h = (secs / 3600.0) as u64;
        let m = ((secs - (h as f64 * 3600.0)) / 60.0) as u64;
        format!("{h}h{m:02}m")
    }
}

fn one_line(backend: &Backend, reason: Reason) -> String {
    let query = backend.query.replace('\n', " ");
    let query = if query.len() > 80 {
        format!("{}...", &query[..77])
    } else {
        query
    };
    format!(
        "  pid {:<8} {:<20} {}/{:<12} {:<10} for {:<8} {}",
        backend.pid,
        backend.application_name,
        backend.user,
        backend.database,
        reason.label(),
        format_secs(reason.for_secs()),
        query
    )
}

/// The default, safe mode: list what *would* be terminated, touch nothing.
pub fn render_dry_run(matches: &[(Backend, Reason)]) -> String {
    if matches.is_empty() {
        return "no backends match the reap criteria\n".to_string();
    }
    let mut out = format!(
        "{} backend(s) match the reap criteria (dry run — nothing terminated; pass --execute to act):\n",
        matches.len()
    );
    for (backend, reason) in matches {
        out.push_str(&one_line(backend, *reason));
        out.push('\n');
    }
    out
}

/// `--execute` mode: the same list, plus whether each terminate actually
/// landed (it can race and find the backend already gone, which isn't an
/// error).
pub fn render_execute(results: &[(Backend, Reason, bool)]) -> String {
    if results.is_empty() {
        return "no backends matched the reap criteria — nothing to terminate\n".to_string();
    }
    let mut out = format!("{} backend(s) matched; terminating:\n", results.len());
    let mut terminated = 0;
    for (backend, reason, ok) in results {
        out.push_str(&one_line(backend, *reason));
        if *ok {
            out.push_str("  -> terminated\n");
            terminated += 1;
        } else {
            out.push_str("  -> already gone (no signal sent)\n");
        }
    }
    out.push_str(&format!(
        "\n{terminated}/{} backend(s) actually terminated\n",
        results.len()
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(pid: i32) -> Backend {
        Backend {
            pid,
            user: "app".to_string(),
            database: "appdb".to_string(),
            application_name: "worker".to_string(),
            state: "idle in transaction".to_string(),
            state_secs: 125.4,
            query_secs: 0.0,
            query: "UPDATE accounts SET balance = balance - 1 WHERE id = 1".to_string(),
        }
    }

    #[test]
    fn format_secs_sub_minute() {
        assert_eq!(format_secs(0.0), "0.0s");
        assert_eq!(format_secs(59.9), "59.9s");
    }

    #[test]
    fn format_secs_minutes() {
        assert_eq!(format_secs(60.0), "1m00s");
        assert_eq!(format_secs(125.0), "2m05s");
    }

    #[test]
    fn format_secs_hours() {
        assert_eq!(format_secs(3725.0), "1h02m");
    }

    #[test]
    fn format_secs_clamps_negative() {
        assert_eq!(format_secs(-5.0), "0.0s");
    }

    #[test]
    fn dry_run_empty_is_a_clean_message() {
        assert_eq!(render_dry_run(&[]), "no backends match the reap criteria\n");
    }

    #[test]
    fn dry_run_lists_every_match() {
        let m = vec![
            (backend(101), Reason::IdleInTransaction { for_secs: 125.4 }),
            (backend(102), Reason::LongRunningQuery { for_secs: 61.0 }),
        ];
        let rendered = render_dry_run(&m);
        assert!(rendered.contains("2 backend(s)"));
        assert!(rendered.contains("pid 101"));
        assert!(rendered.contains("pid 102"));
        assert!(rendered.contains("dry run"));
    }

    #[test]
    fn dry_run_truncates_long_query_text() {
        let mut b = backend(101);
        b.query = "x".repeat(200);
        let rendered = render_dry_run(&[(b, Reason::IdleInTransaction { for_secs: 1.0 })]);
        assert!(rendered.contains("..."));
        // Truncated to 77 chars + "...", never the full 200-char query.
        assert!(!rendered.contains(&"x".repeat(200)));
    }

    #[test]
    fn dry_run_flattens_multiline_query() {
        let mut b = backend(101);
        b.query = "BEGIN;\nUPDATE t SET x = 1;".to_string();
        let rendered = render_dry_run(&[(b, Reason::IdleInTransaction { for_secs: 1.0 })]);
        // The query itself must be one physical line even though the
        // rendered report as a whole still has multiple lines.
        let query_line = rendered.lines().nth(1).unwrap();
        assert!(query_line.contains("BEGIN; UPDATE t SET x = 1;"));
    }

    #[test]
    fn execute_empty_says_nothing_to_terminate() {
        assert_eq!(
            render_execute(&[]),
            "no backends matched the reap criteria — nothing to terminate\n"
        );
    }

    #[test]
    fn execute_reports_terminated_and_already_gone() {
        let results = vec![
            (
                backend(101),
                Reason::IdleInTransaction { for_secs: 125.4 },
                true,
            ),
            (
                backend(102),
                Reason::IdleInTransaction { for_secs: 500.0 },
                false,
            ),
        ];
        let rendered = render_execute(&results);
        assert!(rendered.contains("-> terminated"));
        assert!(rendered.contains("-> already gone"));
        assert!(rendered.contains("1/2 backend(s) actually terminated"));
    }

    #[test]
    fn execute_all_terminated_counts_correctly() {
        let results = vec![(
            backend(101),
            Reason::IdleInTransaction { for_secs: 10.0 },
            true,
        )];
        let rendered = render_execute(&results);
        assert!(rendered.contains("1/1 backend(s) actually terminated"));
    }
}
