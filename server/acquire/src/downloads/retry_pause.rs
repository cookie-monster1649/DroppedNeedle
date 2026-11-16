//! Fork: a retry that would only repeat the same failure waits.
//!
//! The retry pass starts a successor for every failed task under the retry
//! ceiling. When nothing has changed, a song nobody shares fails the same
//! way each time, and every successor is one more failed row. So once an
//! ask's last two downloads ended with the same outcome, and that outcome
//! is about the copies on offer (nobody shares it, the settings refuse
//! what is shared, a hold for a person), the next retry waits until one
//! of these is true:
//!
//! - the download settings changed since the newer of the two started
//!   (the quality settings, the source order, a plugin turned on): the
//!   retry pass stamps each successor with the settings' mark, and a
//!   different mark now lets the retry go;
//! - [`REPEAT_PAUSE_SECS`] passed since the newer one ended, so new shares
//!   get their chance.
//!
//! A failure on our side (a full disk, MusicBrainz down) never pauses: the
//! usual back-off already covers it.

use crate::reason::{Reason, ReasonKind};

/// How long a retry whose outcome repeats waits when nothing changed.
pub const REPEAT_PAUSE_SECS: f64 = 7.0 * 86_400.0;

/// One finished download of an ask, newest first.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    /// Its outcome code, when it has one.
    pub outcome: Option<String>,
    /// When it ended (Unix seconds).
    pub ended_at: f64,
    /// The settings' mark the retry pass stamped on it, when it did.
    pub settings_mark: Option<i64>,
}

/// Whether an outcome says something about the copies on offer, so the
/// same search would answer the same way.
fn about_the_copies(code: &str) -> bool {
    Reason::from_code(code)
        .is_some_and(|reason| matches!(reason.kind(), ReasonKind::Source | ReasonKind::Hold))
}

/// When the next retry of an ask may start, given its finished downloads
/// newest first and the settings' mark now. `None` when nothing holds it
/// back.
pub fn resume_at(history: &[Finished], settings_mark: i64) -> Option<f64> {
    let [newest, previous, ..] = history else {
        return None;
    };
    let code = newest.outcome.as_deref()?;
    if previous.outcome.as_deref() != Some(code) || !about_the_copies(code) {
        return None;
    }
    // Without a stamp the newest predates this rule or was a person's
    // ask; either way it ran under settings that may differ, so only a
    // stamped download proves nothing changed.
    if newest.settings_mark != Some(settings_mark) {
        return None;
    }
    Some(newest.ended_at + REPEAT_PAUSE_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(outcome: &str, ended_at: f64, mark: Option<i64>) -> Finished {
        Finished {
            outcome: Some(outcome.to_owned()),
            ended_at,
            settings_mark: mark,
        }
    }

    #[test]
    fn two_identical_outcomes_under_the_same_settings_pause() {
        let history = [
            finished("track_not_found", 1_000.0, Some(7)),
            finished("track_not_found", 500.0, None),
        ];
        assert_eq!(resume_at(&history, 7), Some(1_000.0 + REPEAT_PAUSE_SECS));
    }

    #[test]
    fn a_settings_change_lets_the_retry_go() {
        let history = [
            finished("nothing_allowed", 1_000.0, Some(7)),
            finished("nothing_allowed", 500.0, Some(7)),
        ];
        assert_eq!(resume_at(&history, 8), None);
    }

    #[test]
    fn different_outcomes_or_one_failure_never_pause() {
        let changed = [
            finished("nothing_allowed", 1_000.0, Some(7)),
            finished("track_not_found", 500.0, Some(7)),
        ];
        assert_eq!(resume_at(&changed, 7), None);
        assert_eq!(
            resume_at(&[finished("track_not_found", 1.0, Some(7))], 7),
            None
        );
        assert_eq!(resume_at(&[], 7), None);
    }

    #[test]
    fn failures_on_our_side_never_pause() {
        let history = [
            finished("local_fault", 1_000.0, Some(7)),
            finished("local_fault", 500.0, Some(7)),
        ];
        assert_eq!(resume_at(&history, 7), None);
        let unstamped = [
            finished("track_not_found", 1_000.0, None),
            finished("track_not_found", 500.0, None),
        ];
        assert_eq!(resume_at(&unstamped, 7), None, "no proof nothing changed");
    }
}
