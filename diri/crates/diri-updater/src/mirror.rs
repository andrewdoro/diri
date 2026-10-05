//! The update mirror: a second route to the same feed and archives, for
//! networks where GitHub is blocked, throttled, or crawling.
//!
//! The mirror is a route, not an authority. It serves byte-for-byte copies of
//! GitHub's release assets (`updates.diri.sh`, a Cloudflare Worker proxying
//! only the feed and `releases/download/<tag>/<file>`), and nothing it says is
//! trusted more than what GitHub says:
//!
//! - A feed read from the mirror goes through the same parser and the same
//!   newer-only eligibility rules, and its release URLs must still name the
//!   GitHub releases host. The client derives the mirror's archive URL from
//!   that GitHub URL itself, so the mirror cannot point a download anywhere,
//!   not even elsewhere on its own host.
//! - An archive read from the mirror is held to the feed's size and SHA-256,
//!   then to the Developer ID + notarization pin and the promised version in
//!   [`crate::Updater::stage`] — exactly the checks a GitHub download gets. A
//!   mirror that serves other bytes is rejected; one that serves nothing only
//!   withholds updates, which a blocked GitHub was doing anyway.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::error::{Result, UpdateError};

/// Host of the Cloudflare Worker in `updates-mirror/`.
pub const DEFAULT_MIRROR_HOST: &str = "updates.diri.sh";
/// Path prefix of a release asset on GitHub, which the mirror mirrors.
const GITHUB_RELEASE_PATH: &str = "/cristicretu/diri/releases/download/";
/// How long the feed request to GitHub runs alone before the mirror is asked
/// too. A healthy GitHub answers the feed (two redirects and a few KB) in
/// about a second; a blocked one would otherwise hold the check for the full
/// connect timeout.
pub(crate) const HEDGE_AFTER: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mirror {
    pub host: String,
}

impl Default for Mirror {
    fn default() -> Self {
        Self {
            host: DEFAULT_MIRROR_HOST.to_owned(),
        }
    }
}

impl Mirror {
    /// `None` for anything that is not a bare `host[:port]`, so an override
    /// cannot smuggle a path, userinfo, or curl config syntax into a URL.
    pub fn new(host: &str) -> Option<Self> {
        let valid = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'));
        valid.then(|| Self {
            host: host.to_owned(),
        })
    }

    pub fn feed_url(&self) -> String {
        format!("https://{}/appcast.json", self.host)
    }

    /// The mirror's copy of a release archive the feed names on GitHub.
    ///
    /// `None` unless `url` is exactly `https://<releases_host>/cristicretu/
    /// diri/releases/download/<tag>/<file>` with plain path segments — the
    /// shape `scripts/release.sh` writes. Anything else stays GitHub-only.
    pub fn archive_url(&self, url: &str, releases_host: &str) -> Option<String> {
        let rest = url
            .strip_prefix("https://")?
            .strip_prefix(releases_host)?
            .strip_prefix(GITHUB_RELEASE_PATH)?;
        let (tag, file) = rest.split_once('/')?;
        if !is_plain_segment(tag) || !is_plain_segment(file) {
            return None;
        }
        Some(format!(
            "https://{}/releases/download/{tag}/{file}",
            self.host
        ))
    }
}

fn is_plain_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !segment.starts_with('.')
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Which route served an updater request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    GitHub,
    Mirror,
}

impl Source {
    /// Stable telemetry label (`source`).
    pub fn label(self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::Mirror => "mirror",
        }
    }
}

/// How one updater step reached its answer, for telemetry. Every field is a
/// closed set of labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceReport {
    /// The route whose answer the step returned: the one that succeeded, or
    /// GitHub's when every route failed (its error is the one surfaced).
    pub source: Source,
    /// Why GitHub's answer was not used: its error kind, `slow` when the
    /// mirror answered the feed first while GitHub was still pending, or
    /// `skipped` when the archive went to the mirror first because the feed
    /// had just come from it.
    pub github_error: Option<&'static str>,
    /// The mirror's error kind, when it was tried and failed.
    pub mirror_error: Option<&'static str>,
}

impl SourceReport {
    pub(crate) fn github() -> Self {
        Self {
            source: Source::GitHub,
            github_error: None,
            mirror_error: None,
        }
    }

    pub(crate) fn note_failure(&mut self, source: Source, error: &UpdateError) {
        match source {
            Source::GitHub => self.github_error = Some(error.kind()),
            Source::Mirror => self.mirror_error = Some(error.kind()),
        }
    }
}

/// Runs `github`, and `mirror` too once GitHub has failed on its route or has
/// not answered within `hedge_after`; returns the first success.
///
/// When both fail the result is GitHub's error: it is the canonical host, and
/// the mirror failing as well says nothing new to the person reading it. A
/// request that loses the race is not cancelled — its thread finishes on its
/// own within curl's time limit and its answer is dropped.
pub(crate) fn race<T: Send + 'static>(
    github: impl FnOnce() -> Result<T> + Send + 'static,
    mirror: Option<impl FnOnce() -> Result<T> + Send + 'static>,
    hedge_after: Duration,
) -> (Result<T>, SourceReport) {
    let mut report = SourceReport::github();
    let (sender, receiver) = mpsc::channel();
    {
        let sender = sender.clone();
        thread::spawn(move || {
            let _ = sender.send((Source::GitHub, github()));
        });
    }
    let stopped = || UpdateError::Io(std::io::Error::other("update request thread stopped"));
    let Some(mirror) = mirror else {
        drop(sender);
        let result = receiver
            .recv()
            .map_or_else(|_| Err(stopped()), |(_, result)| result);
        if let Err(error) = &result {
            report.note_failure(Source::GitHub, error);
        }
        return (result, report);
    };

    let mut github_error = None;
    let mut pending = 1;
    match receiver.recv_timeout(hedge_after) {
        Ok((_, Ok(value))) => return (Ok(value), report),
        Ok((_, Err(error))) if !error.is_route_failure() => {
            report.note_failure(Source::GitHub, &error);
            return (Err(error), report);
        }
        Ok((_, Err(error))) => {
            report.note_failure(Source::GitHub, &error);
            github_error = Some(error);
        }
        Err(mpsc::RecvTimeoutError::Timeout) => pending = 2,
        Err(mpsc::RecvTimeoutError::Disconnected) => github_error = Some(stopped()),
    }
    thread::spawn(move || {
        let _ = sender.send((Source::Mirror, mirror()));
    });

    let mut mirror_error = None;
    for _ in 0..pending {
        let Ok((source, result)) = receiver.recv() else {
            break;
        };
        match result {
            Ok(value) => {
                report.source = source;
                if source == Source::Mirror && report.github_error.is_none() {
                    report.github_error = Some("slow");
                }
                return (Ok(value), report);
            }
            Err(error) => {
                report.note_failure(source, &error);
                match source {
                    Source::GitHub => github_error = Some(error),
                    Source::Mirror => mirror_error = Some(error),
                }
            }
        }
    }
    report.source = Source::GitHub;
    let error = github_error.or(mirror_error).unwrap_or_else(stopped);
    (Err(error), report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NetworkFailure;
    use std::time::Instant;

    const GITHUB: &str = "github.com";

    fn mirror() -> Mirror {
        Mirror::default()
    }

    #[test]
    fn archive_urls_map_onto_the_mirror_path() {
        assert_eq!(
            mirror().archive_url(
                "https://github.com/cristicretu/diri/releases/download/v0.9.2/diri-0.9.2-universal.zip",
                GITHUB,
            ),
            Some(
                "https://updates.diri.sh/releases/download/v0.9.2/diri-0.9.2-universal.zip"
                    .to_owned()
            )
        );
    }

    #[test]
    fn only_plain_github_release_assets_have_a_mirror_copy() {
        for url in [
            "https://evil.test/cristicretu/diri/releases/download/v1/diri.zip",
            "https://github.com/someone/else/releases/download/v1/diri.zip",
            "https://github.com/cristicretu/diri/releases/download/v1/../../x.zip",
            "https://github.com/cristicretu/diri/releases/download/v1/a/b.zip",
            "https://github.com/cristicretu/diri/releases/download/v1/diri.zip?x=1",
            "https://github.com/cristicretu/diri/releases/download/v1/diri.zip#x",
            "https://github.com/cristicretu/diri/releases/download/v1/",
            "https://github.com/cristicretu/diri/releases/download/v1/.hidden",
            "https://github.com@evil.test/cristicretu/diri/releases/download/v1/diri.zip",
            "http://github.com/cristicretu/diri/releases/download/v1/diri.zip",
            "https://github.com/cristicretu/diri/releases/download/v1/di\"ri.zip",
        ] {
            assert_eq!(mirror().archive_url(url, GITHUB), None, "{url}");
        }
    }

    #[test]
    fn mirror_hosts_are_bare_authorities() {
        assert!(Mirror::new("updates.diri.sh").is_some());
        assert!(Mirror::new("127.0.0.1:8443").is_some());
        for host in ["", "evil.test/x", "a@b", "a\"b", "a b", "a\nb"] {
            assert!(Mirror::new(host).is_none(), "{host:?}");
        }
    }

    fn network(failure: NetworkFailure) -> UpdateError {
        UpdateError::network(failure, "test")
    }

    type Answer = Box<dyn FnOnce() -> Result<&'static str> + Send>;

    fn answer(delay: Duration, result: Result<&'static str>) -> Answer {
        Box::new(move || {
            thread::sleep(delay);
            result
        })
    }

    const HEDGE: Duration = Duration::from_millis(100);

    #[test]
    fn a_healthy_github_never_touches_the_mirror() {
        let (result, report) = race(
            answer(Duration::ZERO, Ok("github")),
            Some(answer(Duration::ZERO, Err(network(NetworkFailure::Dns)))),
            HEDGE,
        );
        assert_eq!(result.expect("github"), "github");
        assert_eq!(report, SourceReport::github());
    }

    #[test]
    fn route_failures_fall_back_to_the_mirror() {
        for failure in [
            NetworkFailure::Dns,
            NetworkFailure::Connect,
            NetworkFailure::Timeout,
            NetworkFailure::Tls,
            NetworkFailure::Other,
            NetworkFailure::RateLimited(429),
            NetworkFailure::Http(502),
        ] {
            let (result, report) = race(
                answer(Duration::ZERO, Err(network(failure))),
                Some(answer(Duration::ZERO, Ok("mirror"))),
                HEDGE,
            );
            assert_eq!(result.expect("mirror"), "mirror", "{failure:?}");
            assert_eq!(report.source, Source::Mirror);
            assert_eq!(report.github_error, Some(failure.kind()));
            assert_eq!(report.mirror_error, None);
        }
    }

    #[test]
    fn answers_that_are_not_about_the_route_do_not_fall_back() {
        let (result, report) = race(
            answer(Duration::ZERO, Err(network(NetworkFailure::NotFound))),
            Some(answer(Duration::ZERO, Ok("mirror"))),
            HEDGE,
        );
        assert!(matches!(
            result,
            Err(UpdateError::Network {
                failure: NetworkFailure::NotFound,
                ..
            })
        ));
        assert_eq!(report.source, Source::GitHub);
        assert_eq!(report.github_error, Some("not_found"));
        assert_eq!(report.mirror_error, None, "the mirror was never asked");
    }

    #[test]
    fn a_slow_github_is_hedged_without_waiting_for_its_timeout() {
        let started = Instant::now();
        let (result, report) = race(
            answer(Duration::from_secs(5), Ok("github")),
            Some(answer(Duration::ZERO, Ok("mirror"))),
            HEDGE,
        );
        assert_eq!(result.expect("mirror"), "mirror");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(report.source, Source::Mirror);
        assert_eq!(report.github_error, Some("slow"));
    }

    #[test]
    fn a_slow_github_still_wins_if_the_mirror_fails() {
        let (result, report) = race(
            answer(Duration::from_millis(300), Ok("github")),
            Some(answer(Duration::ZERO, Err(network(NetworkFailure::Dns)))),
            HEDGE,
        );
        assert_eq!(result.expect("github"), "github");
        assert_eq!(report.source, Source::GitHub);
        assert_eq!(report.github_error, None);
        assert_eq!(report.mirror_error, Some("dns"));
    }

    #[test]
    fn when_both_fail_github_error_is_surfaced_and_both_are_reported() {
        let (result, report) = race(
            answer(Duration::ZERO, Err(network(NetworkFailure::Timeout))),
            Some(answer(
                Duration::ZERO,
                Err(network(NetworkFailure::Connect)),
            )),
            HEDGE,
        );
        assert!(matches!(
            result,
            Err(UpdateError::Network {
                failure: NetworkFailure::Timeout,
                ..
            })
        ));
        assert_eq!(
            report,
            SourceReport {
                source: Source::GitHub,
                github_error: Some("timeout"),
                mirror_error: Some("connect"),
            }
        );
    }

    #[test]
    fn without_a_mirror_github_answers_alone() {
        let (result, report) = race(
            answer(
                Duration::from_millis(200),
                Err(network(NetworkFailure::Dns)),
            ),
            None::<Answer>,
            HEDGE,
        );
        assert!(result.is_err());
        assert_eq!(report.github_error, Some("dns"));
        assert_eq!(report.mirror_error, None);
    }
}
