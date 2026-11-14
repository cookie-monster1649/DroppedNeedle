//! Fork: what the import spends of Spotify's allowance, and how it waits
//! when Spotify says no.
//!
//! Spotify's development-mode allowance is shared by the whole developer
//! account and counted per endpoint. Every call is counted here by
//! endpoint group, in memory, and a summary line is logged once an hour
//! (and when Spotify refuses a call), so the log shows where the calls go.
//!
//! A refusal (429) is classified by the reason Spotify's body names:
//! `QUOTA_EXCEEDED` (the allowance is used up, often for many hours) or
//! none (the rolling rate limit). The wait grows: when the first call after
//! a wait ends is refused again, the next wait is the longer of what
//! Spotify asked for and twice the last wait, up to a day. A call that
//! succeeds resets it. No call is made just to probe.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use droppedneedle_providers::spotify::Refusal;

/// The longest wait the growing wait reaches.
pub const MAX_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// How often the call summary is logged.
const SUMMARY_EVERY: Duration = Duration::from_secs(60 * 60);

/// Why Spotify last refused, as the card and the log name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalKind {
    /// The app's development-mode allowance is used up.
    Quota,
    /// The rolling rate limit.
    RateLimit,
}

impl RefusalKind {
    /// The stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quota => "quota",
            Self::RateLimit => "rate_limit",
        }
    }

    /// From the stored form; anything else is the rate limit.
    pub fn from_stored(value: &str) -> Self {
        if value == "quota" {
            Self::Quota
        } else {
            Self::RateLimit
        }
    }

    /// The words the card leads with.
    pub fn label(self) -> &'static str {
        match self {
            Self::Quota => "Spotify quota used up",
            Self::RateLimit => "Spotify rate limit",
        }
    }
}

/// The last refusal, for the log, the card and the kept wait.
#[derive(Debug, Clone, PartialEq)]
pub struct RefusalNote {
    /// Quota or rate limit.
    pub kind: RefusalKind,
    /// The endpoint group refused.
    pub endpoint: String,
    /// What `Retry-After` asked for, when it said.
    pub retry_after: Option<Duration>,
    /// The wait set for it (the growing wait).
    pub wait: Duration,
}

#[derive(Debug, Default)]
struct Backoff {
    /// The wait set for the last refusal; `None` after a success.
    last_wait: Option<Duration>,
    /// When that wait ends.
    until: Option<Instant>,
}

#[derive(Debug)]
struct Counts {
    since: Instant,
    by_endpoint: BTreeMap<&'static str, u64>,
}

/// The counter and the growing wait, shared by every clone of the client.
#[derive(Debug)]
pub struct SpotifyUsage {
    counts: Mutex<Counts>,
    backoff: Mutex<Backoff>,
    last: Mutex<Option<RefusalNote>>,
}

impl Default for SpotifyUsage {
    fn default() -> Self {
        Self {
            counts: Mutex::new(Counts {
                since: Instant::now(),
                by_endpoint: BTreeMap::new(),
            }),
            backoff: Mutex::new(Backoff::default()),
            last: Mutex::new(None),
        }
    }
}

impl SpotifyUsage {
    /// Count one call to `endpoint`.
    pub fn count(&self, endpoint: &'static str) {
        let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        *counts.by_endpoint.entry(endpoint).or_default() += 1;
    }

    /// The calls counted since the last summary, by endpoint group.
    pub fn counts(&self) -> BTreeMap<&'static str, u64> {
        self.counts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .by_endpoint
            .clone()
    }

    /// The summary line's text: `endpoint=count` pairs and the total.
    fn summary(counts: &BTreeMap<&'static str, u64>) -> (u64, String) {
        let total = counts.values().sum();
        let line = counts
            .iter()
            .map(|(endpoint, count)| format!("{endpoint}={count}"))
            .collect::<Vec<_>>()
            .join(" ");
        (total, line)
    }

    /// Log the hourly summary when an hour has passed, and start counting
    /// again. Quiet when nothing was called.
    pub fn log_summary_if_due(&self) {
        let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        if counts.since.elapsed() < SUMMARY_EVERY {
            return;
        }
        let minutes = counts.since.elapsed().as_secs() / 60;
        let taken = std::mem::take(&mut counts.by_endpoint);
        counts.since = Instant::now();
        drop(counts);
        if taken.is_empty() {
            return;
        }
        let (total, line) = Self::summary(&taken);
        tracing::info!(total, minutes, calls = %line, "spotify calls this hour");
    }

    /// A call succeeded: the growing wait starts over.
    pub fn succeeded(&self) {
        let mut backoff = self.backoff.lock().unwrap_or_else(|p| p.into_inner());
        if backoff.last_wait.is_some() {
            *backoff = Backoff::default();
        }
    }

    /// Spotify refused a call to `endpoint`. Answers the wait to hold:
    /// what Spotify asked for, or, when the first call after the last wait
    /// is refused again, the longer of that and twice the last wait (at
    /// most [`MAX_WAIT`]). A refusal while a wait still runs (a call that
    /// was already on its way) changes nothing. Logs a WARN line and the
    /// call summary.
    pub fn refused(
        &self,
        endpoint: &str,
        refusal: Option<Refusal>,
        retry_after: Option<Duration>,
    ) -> Duration {
        let kind = if refusal.as_ref().is_some_and(Refusal::is_quota) {
            RefusalKind::Quota
        } else {
            RefusalKind::RateLimit
        };
        let retry_after = retry_after.or_else(|| refusal.as_ref().and_then(|r| r.retry_after));
        let asked = retry_after.unwrap_or(Duration::ZERO).min(MAX_WAIT);
        let now = Instant::now();
        let wait = {
            let mut backoff = self.backoff.lock().unwrap_or_else(|p| p.into_inner());
            match backoff.until {
                Some(until) if now < until => {
                    // Already waiting: the same refusal, seen twice.
                    return until.saturating_duration_since(now).max(asked);
                }
                _ => {}
            }
            let wait = match backoff.last_wait {
                Some(previous) => asked.max(previous.saturating_mul(2)).min(MAX_WAIT),
                None => asked,
            };
            if !wait.is_zero() {
                backoff.last_wait = Some(wait);
                backoff.until = Some(now + wait);
            }
            wait
        };
        let note = RefusalNote {
            kind,
            endpoint: endpoint.to_owned(),
            retry_after,
            wait,
        };
        *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Some(note);
        let (total, line) = Self::summary(&self.counts());
        tracing::warn!(
            reason = refusal.as_ref().map_or("", |r| r.reason.as_str()),
            kind = kind.label(),
            endpoint,
            retry_after_secs = retry_after.map(|wait| wait.as_secs()),
            wait_secs = wait.as_secs(),
            "spotify refused a call (429)"
        );
        tracing::warn!(total, calls = %line, "spotify calls this hour so far");
        wait
    }

    /// The last refusal, if any.
    pub fn last_refusal(&self) -> Option<RefusalNote> {
        self.last.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Put back a refusal kept from before a restart: its kind and the
    /// wait it set (so the next refusal doubles it), ending in `left`.
    pub fn restore(&self, note: RefusalNote, left: Duration) {
        {
            let mut backoff = self.backoff.lock().unwrap_or_else(|p| p.into_inner());
            if !note.wait.is_zero() {
                backoff.last_wait = Some(note.wait);
                backoff.until = Some(Instant::now() + left);
            }
        }
        *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Some(note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quota(secs: u64) -> Option<Refusal> {
        Some(Refusal {
            reason: "QUOTA_EXCEEDED".to_owned(),
            retry_after: Some(Duration::from_secs(secs)),
        })
    }

    #[test]
    fn a_quota_refusal_is_named_and_waits_what_spotify_asked() {
        let usage = SpotifyUsage::default();
        usage.count("/me/playlists");
        let wait = usage.refused("/me/playlists", quota(600), None);
        assert_eq!(wait, Duration::from_secs(600));
        let note = usage.last_refusal().expect("kept");
        assert_eq!(note.kind, RefusalKind::Quota);
        assert_eq!(note.endpoint, "/me/playlists");
        assert_eq!(note.kind.label(), "Spotify quota used up");
    }

    #[test]
    fn a_refusal_without_a_reason_is_the_rate_limit() {
        let usage = SpotifyUsage::default();
        usage.refused("/me/tracks", None, Some(Duration::from_secs(5)));
        let note = usage.last_refusal().expect("kept");
        assert_eq!(note.kind, RefusalKind::RateLimit);
        assert_eq!(note.kind.label(), "Spotify rate limit");
    }

    #[test]
    fn a_second_refusal_after_the_wait_doubles_it_up_to_a_day() {
        let usage = SpotifyUsage::default();
        // A wait that has already ended, so the next refusal is the first
        // call after it.
        usage.restore(
            RefusalNote {
                kind: RefusalKind::Quota,
                endpoint: "/me/playlists".to_owned(),
                retry_after: Some(Duration::from_secs(3600)),
                wait: Duration::from_secs(3600),
            },
            Duration::ZERO,
        );
        let wait = usage.refused("/me/playlists", quota(60), None);
        assert_eq!(wait, Duration::from_secs(7200), "twice the last wait");
        // Spotify asking for longer wins.
        let usage = SpotifyUsage::default();
        usage.restore(
            RefusalNote {
                kind: RefusalKind::Quota,
                endpoint: "x".to_owned(),
                retry_after: None,
                wait: Duration::from_secs(100),
            },
            Duration::ZERO,
        );
        assert_eq!(
            usage.refused("x", quota(1000), None),
            Duration::from_secs(1000)
        );
        // Capped at a day.
        let usage = SpotifyUsage::default();
        usage.restore(
            RefusalNote {
                kind: RefusalKind::Quota,
                endpoint: "x".to_owned(),
                retry_after: None,
                wait: Duration::from_secs(20 * 3600),
            },
            Duration::ZERO,
        );
        assert_eq!(usage.refused("x", quota(60), None), MAX_WAIT);
    }

    #[test]
    fn a_success_resets_the_growing_wait() {
        let usage = SpotifyUsage::default();
        usage.restore(
            RefusalNote {
                kind: RefusalKind::Quota,
                endpoint: "x".to_owned(),
                retry_after: None,
                wait: Duration::from_secs(3600),
            },
            Duration::ZERO,
        );
        usage.succeeded();
        assert_eq!(usage.refused("x", quota(60), None), Duration::from_secs(60));
    }

    #[test]
    fn a_refusal_during_a_running_wait_does_not_double_it() {
        let usage = SpotifyUsage::default();
        let first = usage.refused("x", quota(600), None);
        let second = usage.refused("x", quota(600), None);
        assert_eq!(first, Duration::from_secs(600));
        assert!(second <= Duration::from_secs(600), "{second:?}");
    }

    #[test]
    fn calls_are_counted_by_endpoint() {
        let usage = SpotifyUsage::default();
        usage.count("/me/playlists");
        usage.count("/me/playlists");
        usage.count("/playlists/{id}/items");
        let counts = usage.counts();
        assert_eq!(counts.get("/me/playlists"), Some(&2));
        assert_eq!(counts.get("/playlists/{id}/items"), Some(&1));
    }
}
