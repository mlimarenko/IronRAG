//! Error records and error tracking.
//!
//! The structured log sink is the OTLP log exporter. This module gives every
//! failure one record shape an operator can group and alert on, instead of a
//! per-domain `error = %error` line whose text changes on every occurrence:
//!
//! * `event = "error"` (or `"panic"`),
//! * `error.type` — the stable fault class,
//! * `error.message` — redacted and bounded,
//! * `error.fingerprint` — 16 hex characters grouping repeats,
//! * `error.stack` — captured only when `RUST_BACKTRACE` enables it,
//! * `error.occurrences` — how many identical faults this record stands for.
//!
//! Repeats of one fingerprint inside a 60-second window are suppressed and
//! counted; the next emission carries the count. A panic is never suppressed.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use regex::Regex;
use sha2::{Digest as _, Sha256};
use tracing::error;

use super::redaction;

/// A redacted error message longer than this is a diagnostic dump, not a
/// message. The fingerprint is computed from the bounded value, so grouping
/// does not depend on where the cut landed.
const MAX_MESSAGE_CHARS: usize = 512;
/// Repeated identical fingerprints inside this window are folded into the
/// occurrence counter instead of emitting a record.
const DEDUP_WINDOW: Duration = Duration::from_secs(60);
/// Upper bound on tracked fingerprints, so a fault storm cannot grow the map
/// without limit. The oldest entry is evicted when the map is full.
const MAX_TRACKED_FINGERPRINTS: usize = 1024;
/// A panic stack is captured unconditionally (the process is going down); the
/// scrubber bounds it.
const MAX_STACK_CHARS: usize = 8192;

/// Stable 16-character lowercase hex id for one fault class.
///
/// `kind` separates unrelated faults that happen to share wording, and the
/// normalised message folds the volatile parts (ids, addresses, counts, paths)
/// so the second occurrence of the same fault produces the same id.
#[must_use]
pub fn fingerprint(kind: &str, message: &str) -> String {
    let normalised = normalise_message(message);
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0x1f]);
    hasher.update(normalised.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

/// Emits one error record for a typed error.
pub fn report_error(kind: &str, error: &(dyn std::error::Error + 'static)) {
    emit(kind, &error.to_string(), true);
}

/// Emits one error record for a message that has no typed error behind it.
pub fn report_message(kind: &str, message: &str) {
    emit(kind, message, true);
}

/// Installs the process panic hook exactly once, chaining the previous hook.
///
/// The previous hook still runs, so a custom panic strategy or a test harness
/// keeps its behaviour; this only adds the structured record in front of it.
pub fn install_panic_hook() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report_panic(info);
        previous(info);
    }));
}

fn report_panic(info: &std::panic::PanicHookInfo<'_>) {
    let message = redaction::truncate_chars(
        &redaction::scrub(&panic_payload_message(info)),
        MAX_MESSAGE_CHARS,
    );
    let fingerprint = fingerprint("panic", &message);
    let stack = redaction::truncate_chars(
        &redaction::scrub(&std::backtrace::Backtrace::force_capture().to_string()),
        MAX_STACK_CHARS,
    );
    let location = info.location().map_or_else(
        || "unknown".to_string(),
        |location| format!("{}:{}:{}", location.file(), location.line(), location.column()),
    );
    // A panic is never deduplicated: the process is terminating and every
    // panic record is evidence, even an identical second one.
    error!(
        event = "panic",
        "error.type" = "panic",
        "error.message" = %message,
        "error.fingerprint" = %fingerprint,
        "error.stack" = %stack,
        "error.location" = %location,
        "error.release" = env!("CARGO_PKG_VERSION"),
        "panic record",
    );
}

fn panic_payload_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_string()
}

/// Emits one record and returns the occurrence count, or `None` when the
/// fingerprint was suppressed by the dedup window.
fn emit(kind: &str, message: &str, suppressible: bool) -> Option<u64> {
    let message = redaction::truncate_chars(&redaction::scrub(message), MAX_MESSAGE_CHARS);
    let fingerprint = fingerprint(kind, &message);
    let occurrences = if suppressible { admit(&fingerprint)? } else { 1 };
    let stack = redaction::truncate_chars(&redaction::scrub(&capture_stack()), MAX_STACK_CHARS);
    error!(
        event = "error",
        "error.type" = %kind,
        "error.message" = %message,
        "error.fingerprint" = %fingerprint,
        "error.stack" = %stack,
        "error.occurrences" = occurrences,
        "error record",
    );
    Some(occurrences)
}

/// `disabled` unless `RUST_BACKTRACE` made the capture possible. Capturing a
/// disabled backtrace still allocates nothing; the literal keeps the field
/// present so every error record has the same shape.
fn capture_stack() -> String {
    let backtrace = std::backtrace::Backtrace::capture();
    match backtrace.status() {
        std::backtrace::BacktraceStatus::Captured => backtrace.to_string(),
        _ => "disabled".to_string(),
    }
}

fn admit(fingerprint: &str) -> Option<u64> {
    let mut window = dedup_window().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    window.admit(fingerprint, Instant::now())
}

fn dedup_window() -> &'static Mutex<DedupWindow> {
    static WINDOW: OnceLock<Mutex<DedupWindow>> = OnceLock::new();
    WINDOW.get_or_init(|| Mutex::new(DedupWindow::default()))
}

struct DedupEntry {
    last_emitted: Instant,
    suppressed: u64,
}

/// Bounded, time-aware suppression of identical fingerprints.
#[derive(Default)]
struct DedupWindow {
    entries: HashMap<String, DedupEntry>,
}

impl DedupWindow {
    /// Returns the occurrence count to report, or `None` when the record must
    /// be suppressed. The count is `1` on first sight and `suppressed + 1`
    /// when a new window opens after a suppressed run.
    fn admit(&mut self, fingerprint: &str, now: Instant) -> Option<u64> {
        if let Some(entry) = self.entries.get_mut(fingerprint) {
            if now.saturating_duration_since(entry.last_emitted) < DEDUP_WINDOW {
                entry.suppressed = entry.suppressed.saturating_add(1);
                return None;
            }
            let occurrences = entry.suppressed.saturating_add(1);
            entry.suppressed = 0;
            entry.last_emitted = now;
            return Some(occurrences);
        }
        if self.entries.len() >= MAX_TRACKED_FINGERPRINTS {
            self.evict_oldest();
        }
        self.entries
            .insert(fingerprint.to_string(), DedupEntry { last_emitted: now, suppressed: 0 });
        Some(1)
    }

    fn evict_oldest(&mut self) {
        let oldest = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_emitted)
            .map(|(fingerprint, _)| fingerprint.clone());
        if let Some(oldest) = oldest {
            self.entries.remove(&oldest);
        }
    }
}

/// Folds the volatile parts of a message into stable tokens so repeats of the
/// same fault share a fingerprint. Order matters: URLs and emails contain
/// digits and hex characters that must be replaced before the numeric pass.
fn normalise_message(message: &str) -> String {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        vec![
            (
                Regex::new(r#"(?i)\b[a-z][a-z0-9+.\-]*://[^\s"']+"#)
                    .expect("static URL pattern is valid"),
                "<url>",
            ),
            (
                Regex::new(r"(?i)\b[A-Z0-9._%+\-]+@[A-Z0-9.\-]+\.[A-Z]{2,}\b")
                    .expect("static email pattern is valid"),
                "<email>",
            ),
            (
                Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
                    .expect("static UUID pattern is valid"),
                "<uuid>",
            ),
            (
                Regex::new(r#""[^"]*[\\/][^"]*"|'[^']*[\\/][^']*'"#)
                    .expect("static path pattern is valid"),
                "<path>",
            ),
            (Regex::new(r"\b[0-9a-fA-F]{16,}\b").expect("static hex pattern is valid"), "<hex>"),
            (Regex::new(r"\b\d+\b").expect("static number pattern is valid"), "<n>"),
        ]
    });

    static WHITESPACE: OnceLock<Regex> = OnceLock::new();
    let whitespace =
        WHITESPACE.get_or_init(|| Regex::new(r"\s+").expect("static whitespace pattern is valid"));

    let mut normalised = message.to_lowercase();
    for (pattern, replacement) in rules {
        normalised = pattern.replace_all(&normalised, *replacement).into_owned();
    }
    whitespace.replace_all(normalised.trim(), " ").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_across_volatile_parts_of_one_fault() {
        let first = fingerprint(
            "ingest.attempt_failed",
            "job 550e8400-e29b-41d4-a716-446655440000 failed at \"/var/lib/ironrag/a\" after 3 \
             attempts for alice@example.com",
        );
        let second = fingerprint(
            "ingest.attempt_failed",
            "job 123e4567-e89b-12d3-a456-426614174000 failed at '/opt/ironrag/b' after 7 \
             attempts for bob@example.com",
        );

        assert_eq!(first, second);
        assert_eq!(first.len(), 16);
        assert!(first.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()));
    }

    #[test]
    fn fingerprint_separates_fault_classes_and_real_wording_changes() {
        let message = "connection refused after 3 attempts";

        assert_ne!(
            fingerprint("ingest.attempt_failed", message),
            fingerprint("query.execution_failed", message),
        );
        assert_ne!(
            fingerprint("ingest.attempt_failed", message),
            fingerprint("ingest.attempt_failed", "connection reset after 3 attempts"),
        );
    }

    #[test]
    fn dedup_window_suppresses_then_reports_the_occurrence_count() {
        let mut window = DedupWindow::default();
        let start = Instant::now();

        assert_eq!(window.admit("fingerprint", start), Some(1));
        assert_eq!(window.admit("fingerprint", start + Duration::from_secs(1)), None);
        assert_eq!(window.admit("fingerprint", start + Duration::from_secs(2)), None);
        assert_eq!(window.admit("fingerprint", start + Duration::from_secs(61)), Some(3));
    }

    #[test]
    fn dedup_window_stays_bounded() {
        let mut window = DedupWindow::default();
        let start = Instant::now();
        for index in 0..(MAX_TRACKED_FINGERPRINTS + 10) {
            assert_eq!(window.admit(&format!("fingerprint-{index}"), start), Some(1));
        }

        assert_eq!(window.entries.len(), MAX_TRACKED_FINGERPRINTS);
    }

    #[test]
    fn unsuppressed_records_are_never_folded() {
        assert_eq!(emit("panic-test", "boom", false), Some(1));
        assert_eq!(emit("panic-test", "boom", false), Some(1));
    }

    #[test]
    fn suppressed_records_report_no_emission() {
        let kind = format!("suppression-test-{}", std::process::id());
        assert_eq!(emit(&kind, "boom", true), Some(1));
        assert_eq!(emit(&kind, "boom", true), None);
    }
}
