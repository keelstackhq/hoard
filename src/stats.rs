//! Request counters and the markdown job summary.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

/// Live counters, shared between request handlers.
#[derive(Debug, Default)]
pub struct Stats {
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub puts: AtomicU64,
    pub put_skipped_existing: AtomicU64,
    pub put_errors: AtomicU64,
    pub rate_limited: AtomicU64,
    pub corrupt: AtomicU64,
    pub bytes_down: AtomicU64,
    pub bytes_up: AtomicU64,
    pub time_saved_ms: AtomicU64,
}

/// Point-in-time copy of [`Stats`].
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct StatsSnapshot {
    pub hits: u64,
    pub misses: u64,
    pub puts: u64,
    pub put_skipped_existing: u64,
    pub put_errors: u64,
    pub rate_limited: u64,
    pub corrupt: u64,
    pub bytes_down: u64,
    pub bytes_up: u64,
    pub time_saved_ms: u64,
}

impl Stats {
    pub fn snapshot(&self) -> StatsSnapshot {
        let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
        StatsSnapshot {
            hits: load(&self.hits),
            misses: load(&self.misses),
            puts: load(&self.puts),
            put_skipped_existing: load(&self.put_skipped_existing),
            put_errors: load(&self.put_errors),
            rate_limited: load(&self.rate_limited),
            corrupt: load(&self.corrupt),
            bytes_down: load(&self.bytes_down),
            bytes_up: load(&self.bytes_up),
            time_saved_ms: load(&self.time_saved_ms),
        }
    }
}

/// Renders the job summary: a table of the headline numbers, then one line with
/// `storage_line` and `extra_lines` joined by ` · `.
pub fn render_summary(s: &StatsSnapshot, storage_line: &str, extra_lines: &[String]) -> String {
    let mut out = String::from(
        "### hoard\n\
         | Hits | Misses | Uploaded | Downloaded | Time saved |\n\
         |---|---|---|---|---|\n",
    );
    out.push_str(&format!(
        "| {} | {} | {} | {} | {} |\n",
        s.hits,
        s.misses,
        human_bytes(s.bytes_up),
        human_bytes(s.bytes_down),
        human_duration_ms(s.time_saved_ms),
    ));

    let mut parts: Vec<&str> = Vec::new();
    if !storage_line.is_empty() {
        parts.push(storage_line);
    }
    parts.extend(extra_lines.iter().map(String::as_str));
    if !parts.is_empty() {
        out.push_str(&parts.join(" · "));
        out.push('\n');
    }
    out
}

/// Formats a byte count with powers of 1000: `999 B`, `12.4 MB`, `1.2 GB`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    loop {
        value /= 1000.0;
        unit += 1;
        // Step up a unit when one decimal would round to "1000.0".
        if value < 999.95 || unit == UNITS.len() {
            break;
        }
    }
    format!("{value:.1} {}", UNITS[unit - 1])
}

/// Formats a duration: `59s`, `6m 12s`, `1h 2m`. Sub-second parts are dropped.
pub fn human_duration_ms(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_table() {
        let s = StatsSnapshot {
            hits: 42,
            misses: 3,
            bytes_up: 12_400_000,
            bytes_down: 210_000_000,
            time_saved_ms: 372_000,
            ..Default::default()
        };
        let md = render_summary(
            &s,
            "Storage: r2 (b/cache/)",
            &["Pruned: 18 objects, 1.2 GB".into()],
        );
        assert!(md.starts_with("### hoard\n"));
        assert!(md.contains("| 42 | 3 | 12.4 MB | 210.0 MB | 6m 12s |"));
        assert!(md.contains("Storage: r2 (b/cache/) · Pruned: 18 objects, 1.2 GB"));
    }

    #[test]
    fn summary_exact_layout() {
        let s = StatsSnapshot {
            hits: 1,
            misses: 2,
            bytes_up: 500,
            bytes_down: 1_200_000_000,
            time_saved_ms: 3_720_000,
            ..Default::default()
        };
        assert_eq!(
            render_summary(&s, "Storage: fs (/tmp/x)", &[]),
            "### hoard\n\
             | Hits | Misses | Uploaded | Downloaded | Time saved |\n\
             |---|---|---|---|---|\n\
             | 1 | 2 | 500 B | 1.2 GB | 1h 2m |\n\
             Storage: fs (/tmp/x)\n"
        );
    }

    #[test]
    fn summary_omits_empty_storage_line_and_extras() {
        let md = render_summary(&StatsSnapshot::default(), "", &[]);
        assert!(md.ends_with("| 0 | 0 | 0 B | 0 B | 0s |\n"));
        assert_eq!(md.lines().count(), 4);
    }

    #[test]
    fn summary_extras_without_storage_line() {
        let md = render_summary(&StatsSnapshot::default(), "", &["A".into(), "B".into()]);
        assert!(md.ends_with("| 0 | 0 | 0 B | 0 B | 0s |\nA · B\n"));
    }

    #[test]
    fn snapshot_reads_every_counter() {
        let st = Stats::default();
        st.hits.fetch_add(2, Ordering::Relaxed);
        st.put_errors.fetch_add(1, Ordering::Relaxed);
        st.time_saved_ms.fetch_add(950, Ordering::Relaxed);
        st.bytes_up.fetch_add(7, Ordering::Relaxed);
        let snap = st.snapshot();
        assert_eq!(
            snap,
            StatsSnapshot {
                hits: 2,
                put_errors: 1,
                time_saved_ms: 950,
                bytes_up: 7,
                ..Default::default()
            }
        );
    }

    #[test]
    fn snapshot_serializes_field_names() {
        let json = serde_json::to_value(StatsSnapshot {
            hits: 5,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(json["hits"], 5);
        assert_eq!(json["put_skipped_existing"], 0);
        assert_eq!(json.as_object().unwrap().len(), 10);
    }

    #[test]
    fn humanize() {
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_duration_ms(59_000), "59s");
    }

    #[test]
    fn bytes_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1_000), "1.0 KB");
        assert_eq!(human_bytes(12_400_000), "12.4 MB");
        assert_eq!(human_bytes(210_000_000), "210.0 MB");
        assert_eq!(human_bytes(1_200_000_000), "1.2 GB");
        assert_eq!(human_bytes(3_000_000_000_000), "3.0 TB");
    }

    #[test]
    fn bytes_rounding_promotes_unit() {
        // 999_960 bytes is 999.96 KB, which would print as "1000.0 KB".
        assert_eq!(human_bytes(999_960), "1.0 MB");
    }

    #[test]
    fn duration_units() {
        assert_eq!(human_duration_ms(0), "0s");
        assert_eq!(human_duration_ms(999), "0s");
        assert_eq!(human_duration_ms(59_999), "59s");
        assert_eq!(human_duration_ms(60_000), "1m 0s");
        assert_eq!(human_duration_ms(372_000), "6m 12s");
        assert_eq!(human_duration_ms(3_599_999), "59m 59s");
        assert_eq!(human_duration_ms(3_600_000), "1h 0m");
        assert_eq!(human_duration_ms(3_720_000), "1h 2m");
    }
}
