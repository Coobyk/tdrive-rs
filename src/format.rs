//! Small formatting helpers shared by the file browser.
//!
//! These mirror `frontend/src/utils.ts` so the Rust rows read the same as the
//! Svelte ones: `formatBytes` reproduces `parseFloat(x.toFixed(1))` and
//! `formatDate` reproduces `toLocaleDateString('en-US', …)`.

use chrono::{DateTime, Local, Utc};

/// `formatBytes`: `0 B`, `812 KB`, `2.3 MB`.
pub fn human_size(bytes: i64) -> String {
    if bytes <= 0 {
        return "0 B".to_string();
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let steps = (bytes as f64).log(1024.0).floor();
    let unit = (steps as usize).min(UNITS.len() - 1);
    let value = bytes as f64 / 1024f64.powi(unit as i32);
    format!("{:.1} {}", value, UNITS[unit])
}

/// `formatDate`, the value the Date column shows: `Oct 14, 2026`.
pub fn short_date(date: Option<DateTime<Utc>>) -> String {
    let Some(date) = date else {
        return "—".to_string();
    };
    let local: DateTime<Local> = date.into();
    local.format("%b %-d, %Y").to_string()
}

/// `purgeCountdown` from `ui/trash/trash-view.ts`: how long is left before the
/// backend purges a deleted item, in the words a person would use. The second
/// value is the original's `urgent` flag, which colours the line.
pub fn purge_countdown(
    purge_after: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (String, bool) {
    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const URGENT_WINDOW: i64 = 2 * DAY;

    let Some(purge_after) = purge_after else {
        return ("Purging soon".to_string(), true);
    };
    // A missing or already-elapsed deadline reads as imminent rather than as a
    // negative number: the item is gone at the next sweep either way.
    let remaining = (purge_after - now).num_milliseconds();

    if remaining <= 0 {
        return ("Purging soon".to_string(), true);
    }
    if remaining < HOUR {
        return ("Less than an hour left".to_string(), true);
    }
    if remaining < DAY {
        let hours = (remaining as f64 / HOUR as f64).round() as i64;
        let label = if hours == 1 {
            "1 hour left".to_string()
        } else {
            format!("{hours} hours left")
        };
        return (label, true);
    }
    // Between one and two days out, "tomorrow" is only true if the purge really
    // does land on tomorrow's date.
    if remaining < URGENT_WINDOW {
        let label = if falls_tomorrow(purge_after, now) {
            "Purges tomorrow"
        } else {
            "1 day left"
        };
        return (label.to_string(), true);
    }
    let days = remaining / DAY;
    let label = format!("{days} days left");
    (label, remaining < URGENT_WINDOW)
}

/// Whether the deadline lands on the calendar day after the current one, in
/// the same local calendar the countdown is read in.
fn falls_tomorrow(deadline: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    let deadline: DateTime<Local> = deadline.into();
    let now: DateTime<Local> = now.into();
    let today = now.date_naive();
    let target = today.succ_opt().unwrap_or(today);
    deadline.date_naive() == target
}

/// `entryOrigin` from `ui/trash/trash-view.ts`: where a deleted item came from
/// and how big it was.
pub fn entry_origin(origin: &str, size: i64, is_file: bool) -> String {
    let origin = origin.trim();
    let origin = if origin.is_empty() { "Drive root" } else { origin };
    if is_file && size > 0 {
        format!("{origin} · {}", human_size(size))
    } else {
        origin.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn sizes_match_the_frontend() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(2_412_544), "2.3 MB");
        assert_eq!(human_size(1_073_741_824), "1.0 GB");
    }

    #[test]
    fn countdown_wording_matches_the_frontend() {
        let now = Utc::now();
        let after = |d: Duration| Some(now + d);
        assert_eq!(purge_countdown(None, now).0, "Purging soon");
        assert_eq!(
            purge_countdown(after(Duration::minutes(30)), now).0,
            "Less than an hour left"
        );
        assert_eq!(purge_countdown(after(Duration::hours(1)), now).0, "1 hour left");
        assert_eq!(
            purge_countdown(after(Duration::hours(5)), now).0,
            "5 hours left"
        );
        assert_eq!(purge_countdown(after(Duration::days(30)), now).0, "30 days left");
    }
}
