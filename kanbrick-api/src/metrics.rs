//! Prometheus text exposition for the mesh pressure metrics (#63, Track A).
//!
//! Hand-rolled rather than pulling in the `prometheus` crate: the surface is a
//! handful of counters and gauges, so a small renderer avoids an extra dependency
//! and a global registry singleton. The output follows the Prometheus text format
//! (`text/plain; version=0.0.4`).
//!
//! **Exposure note:** the per-guest `guest="…"` labels reveal the guest catalogue
//! (e.g. `valuation`, `compliance`). `/metrics` is unauthenticated for in-cluster
//! scraping and MUST NOT be exposed through the public ingress — see
//! `docs/SECURITY.md`.

use std::collections::BTreeMap;
use std::fmt::Write;

use kanbrick_mesh::GuestMetric;

use crate::admission::AdmissionMetric;

/// The Prometheus content type for the text exposition format.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Escape a Prometheus label value (`\`, `"`, and newlines). Guest names are
/// simple identifiers today, but correctness should not depend on that.
fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// One guest's joined view across the mesh-core and admission counters.
#[derive(Default)]
struct Row {
    active: i64,
    completed: u64,
    failed: u64,
    timed_out: u64,
    queued: i64,
    rejected: u64,
}

/// Render the full `/metrics` body by joining the mesh-core guest counters with
/// the API admission counters and computing the overall pressure ratio.
pub fn render_prometheus(guests: &[GuestMetric], admission: &[AdmissionMetric]) -> String {
    let mut rows: BTreeMap<&str, Row> = BTreeMap::new();
    for g in guests {
        let row = rows.entry(g.name.as_str()).or_default();
        row.active = g.active;
        row.completed = g.completed;
        row.failed = g.failed;
        row.timed_out = g.timed_out;
    }
    for a in admission {
        let row = rows.entry(a.name.as_str()).or_default();
        row.queued = a.queued;
        row.rejected = a.rejected;
    }

    let mut total_in_use: u64 = 0;
    let mut total_capacity: u64 = 0;
    for a in admission {
        total_in_use += a.in_use;
        total_capacity += a.capacity as u64;
    }
    let pressure = if total_capacity == 0 {
        0.0
    } else {
        total_in_use as f64 / total_capacity as f64
    };

    let mut out = String::new();

    out.push_str(
        "# HELP kanbrick_guest_invocations_active Guest invocations currently executing.\n",
    );
    out.push_str("# TYPE kanbrick_guest_invocations_active gauge\n");
    for (name, row) in &rows {
        let _ = writeln!(
            out,
            "kanbrick_guest_invocations_active{{guest=\"{}\"}} {}",
            escape_label(name),
            row.active
        );
    }

    out.push_str(
        "# HELP kanbrick_guest_invocations_queued Guest invocations admitted but awaiting a slot.\n",
    );
    out.push_str("# TYPE kanbrick_guest_invocations_queued gauge\n");
    for (name, row) in &rows {
        let _ = writeln!(
            out,
            "kanbrick_guest_invocations_queued{{guest=\"{}\"}} {}",
            escape_label(name),
            row.queued
        );
    }

    out.push_str("# HELP kanbrick_guest_invocations_total Guest invocations by terminal result.\n");
    out.push_str("# TYPE kanbrick_guest_invocations_total counter\n");
    for (name, row) in &rows {
        let guest = escape_label(name);
        let _ = writeln!(
            out,
            "kanbrick_guest_invocations_total{{guest=\"{guest}\",result=\"completed\"}} {}",
            row.completed
        );
        let _ = writeln!(
            out,
            "kanbrick_guest_invocations_total{{guest=\"{guest}\",result=\"failed\"}} {}",
            row.failed
        );
        let _ = writeln!(
            out,
            "kanbrick_guest_invocations_total{{guest=\"{guest}\",result=\"timed_out\"}} {}",
            row.timed_out
        );
    }

    out.push_str(
        "# HELP kanbrick_guest_invocations_rejected_total Guest invocations rejected for overload.\n",
    );
    out.push_str("# TYPE kanbrick_guest_invocations_rejected_total counter\n");
    for (name, row) in &rows {
        let _ = writeln!(
            out,
            "kanbrick_guest_invocations_rejected_total{{guest=\"{}\"}} {}",
            escape_label(name),
            row.rejected
        );
    }

    out.push_str(
        "# HELP kanbrick_mesh_pressure_ratio In-flight guest concurrency permits over total capacity.\n",
    );
    out.push_str("# TYPE kanbrick_mesh_pressure_ratio gauge\n");
    let _ = writeln!(out, "kanbrick_mesh_pressure_ratio {pressure}");

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_all_series_and_pressure_ratio() {
        let guests = vec![GuestMetric {
            name: "valuation".to_string(),
            active: 1,
            completed: 2,
            failed: 1,
            timed_out: 0,
        }];
        let admission = vec![AdmissionMetric {
            name: "valuation".to_string(),
            queued: 3,
            rejected: 4,
            in_use: 1,
            capacity: 4,
        }];
        let out = render_prometheus(&guests, &admission);

        assert!(out.contains("# TYPE kanbrick_guest_invocations_active gauge"));
        assert!(out.contains("kanbrick_guest_invocations_active{guest=\"valuation\"} 1"));
        assert!(out.contains("kanbrick_guest_invocations_queued{guest=\"valuation\"} 3"));
        assert!(out.contains(
            "kanbrick_guest_invocations_total{guest=\"valuation\",result=\"completed\"} 2"
        ));
        assert!(out
            .contains("kanbrick_guest_invocations_total{guest=\"valuation\",result=\"failed\"} 1"));
        assert!(out.contains(
            "kanbrick_guest_invocations_total{guest=\"valuation\",result=\"timed_out\"} 0"
        ));
        assert!(out.contains("kanbrick_guest_invocations_rejected_total{guest=\"valuation\"} 4"));
        // pressure = in_use 1 / capacity 4.
        assert!(out.contains("kanbrick_mesh_pressure_ratio 0.25"));
    }

    #[test]
    fn empty_runtime_reports_zero_pressure() {
        let out = render_prometheus(&[], &[]);
        assert!(out.contains("kanbrick_mesh_pressure_ratio 0"));
    }

    #[test]
    fn label_values_are_escaped() {
        let guests = vec![GuestMetric {
            name: "we\"ird".to_string(),
            active: 0,
            completed: 0,
            failed: 0,
            timed_out: 0,
        }];
        let out = render_prometheus(&guests, &[]);
        assert!(out.contains("guest=\"we\\\"ird\""));
    }
}
