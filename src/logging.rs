// Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.

//! Antenna's logging subscriber.
//!
//! Canonical output format (one line per record):
//!
//! ```text
//! <ISO-8601-ms> LEVEL [TAG] key=value ... message
//! ```
//!
//! Example:
//!
//! ```text
//! 2026-04-21T17:42:03.412Z DEBUG [JAMI] account=abc123 registered ok
//! ```
//!
//! The `TAG` is the event's `target` — Rust emitters use `tracing::event!(target:
//! "DISPATCH", ...)` etc., and the carrier→tracing bridge forwards carrier's
//! CLOG tag as-is. Grep by tag works reliably: `grep '\[JAMI\]' antenna.log`.
//!
//! Filter precedence: `RUST_LOG` > `--log LEVEL` > `--debug` > default `warn`.
//! `--log-tags` is an additional post-filter that drops records whose tag isn't
//! in the allowed set (empty list = no restriction).

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fmt;
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Known log-tag taxonomy. Used by `--log-tags` validation and mirrors the
/// static targets explicitly handled by `carrier::log_callback`. Tags emitted
/// by the carrier shim ("JAMI", "SHIM"), antenna's own subsystems, and the
/// catch-all ("CARRIER") all live here.
pub const KNOWN_TAGS: &[&str] = &[
    "JAMI",
    "SHIM",
    "CARRIER",
    "DISPATCH",
    "SPARQL",
    "PIPELINE",
    "SCRIPT",
    "CHANNEL",
    "LLM",
    "WS",
    "STATION",
];

/// ISO-8601 millisecond UTC timestamp: `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn iso8601_millis(system_time: std::time::SystemTime) -> String {
    let duration = system_time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs() as i64;
    let millis = duration.subsec_millis();

    let days_since_epoch = secs / 86_400;
    let time_of_day = secs.rem_euclid(86_400);
    let hour = time_of_day / 3_600;
    let minute = (time_of_day % 3_600) / 60;
    let second = time_of_day % 60;

    // Gregorian Y-M-D from days since 1970-01-01.
    let mut year = 1970i64;
    let mut remaining = days_since_epoch;
    loop {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days_in_year = if leap { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_months: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month_idx = 0usize;
    for (i, &d) in days_in_months.iter().enumerate() {
        if remaining < d {
            month_idx = i;
            break;
        }
        remaining -= d;
    }
    let day = remaining + 1;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year,
        month_idx + 1,
        day,
        hour,
        minute,
        second,
        millis,
    )
}

/// Custom `tracing_subscriber::fmt` event formatter.
///
/// Renders: `<ISO-8601> LEVEL [TAG] key=value ... message`
///
/// TAG comes from `event.metadata().target()`. When `allowed_tags` is
/// non-empty, events whose tag isn't in the set are silently skipped
/// (this is the `--log-tags` post-filter).
struct CanonicalFormat {
    allowed_tags: Option<HashSet<String>>,
}

impl<S, N> FormatEvent<S, N> for CanonicalFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let target = event.metadata().target();

        // --log-tags post-filter: drop records whose target isn't in the
        // allow-list. Unknown/module targets (e.g. "tungstenite::...") get
        // through only when allowed_tags is None (no restriction).
        if let Some(allowed) = &self.allowed_tags {
            if !allowed.contains(target) {
                return Ok(());
            }
        }

        let ts = iso8601_millis(std::time::SystemTime::now());
        let level = event.metadata().level();

        // <timestamp> <LEVEL> [TAG]
        write!(writer, "{} {:5} [{}] ", ts, level, target)?;

        // key=value fields + message (tracing_subscriber formats them)
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Build the allow-list from the `--log-tags` CLI arg. Empty string → None.
/// Unknown tags are silently accepted (in case a future tag isn't baked in
/// yet); they just never match any event.
fn parse_allowed_tags(raw: &str) -> Option<HashSet<String>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        trimmed
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// Initialise the global tracing subscriber with antenna's canonical format.
///
/// `default_level` is the `antenna`-crate fallback level (e.g. "warn",
/// "debug"). `tags` is the comma-separated `--log-tags` arg ("" = no filter).
/// `RUST_LOG` always wins when set.
pub fn init(default_level: &str, tags: &str) -> Result<()> {
    // The level directive applies to the antenna crate by default. Carrier
    // records come through with `target: "TOX"` etc. (not a module path),
    // so they're filtered by the root-level directive. We set the root to
    // the same level so carrier's CLOG lines flow at the requested level.
    //
    // EnvFilter::try_from_default_env() reads RUST_LOG if present; else
    // we build a default with two directives: root = default_level, and
    // `antenna` = default_level (explicit for clarity — any dependency
    // noise still stays at WARN via the dep crates' own defaults).
    let env_filter = match EnvFilter::try_from_default_env() {
        Ok(f) => f,
        Err(_) => {
            let root = default_level.to_string();
            let antenna = format!("antenna={}", default_level);
            EnvFilter::new(root)
                .add_directive(
                    antenna
                        .parse()
                        .with_context(|| format!("invalid --log level '{}'", default_level))?,
                )
        }
    };

    let formatter = CanonicalFormat {
        allowed_tags: parse_allowed_tags(tags),
    };

    let layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        // ANSI off: log lines are single-line, grep-friendly plain text.
        // Terminals that want color can pipe through `ccze` or similar.
        .with_ansi(false)
        .event_format(formatter);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(layer)
        .try_init()
        .ok(); // Ignore double-init in tests.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_epoch() {
        assert_eq!(
            iso8601_millis(std::time::UNIX_EPOCH),
            "1970-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn iso8601_known_instant() {
        // 86_400 s = 1 day after epoch, + 100 ms
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_millis(86_400_100);
        assert_eq!(iso8601_millis(t), "1970-01-02T00:00:00.100Z");
    }

    #[test]
    fn parse_tags_empty_returns_none() {
        assert!(parse_allowed_tags("").is_none());
        assert!(parse_allowed_tags("   ").is_none());
    }

    #[test]
    fn parse_tags_one() {
        let set = parse_allowed_tags("JAMI").unwrap();
        assert!(set.contains("JAMI"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn parse_tags_many_with_whitespace() {
        let set = parse_allowed_tags(" JAMI, SHIM,  WS ").unwrap();
        assert!(set.contains("JAMI"));
        assert!(set.contains("SHIM"));
        assert!(set.contains("WS"));
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn parse_tags_skips_empty_tokens() {
        let set = parse_allowed_tags("JAMI,,SHIM,").unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains("JAMI"));
        assert!(set.contains("SHIM"));
    }

    #[test]
    fn known_tags_count() {
        assert_eq!(KNOWN_TAGS.len(), 11);
    }
}
