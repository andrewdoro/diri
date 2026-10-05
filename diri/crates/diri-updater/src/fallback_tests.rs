//! The mirror fallback end to end, through real curl: GitHub stand-ins that
//! fail in each way blocked networks fail, and a fake mirror served over real
//! HTTPS by `openssl s_server` with a throwaway certificate only these tests
//! trust.

use std::io::Write as _;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::*;

const TAG: &str = "v9.9.9";
const FILE: &str = "diri-9.9.9-universal.zip";
const ARCHIVE: &[u8] = b"pretend this is a notarized diri.app, zipped";

/// An HTTPS server for the files under its root.
struct TlsServer {
    host: String,
    cacert: PathBuf,
    child: Child,
    _keys: tempfile::TempDir,
}

impl TlsServer {
    fn start(root: &Path) -> Self {
        let keys = tempfile::tempdir().expect("key dir");
        let cert = keys.path().join("cert.pem");
        let key = keys.path().join("key.pem");
        let status = Command::new("/usr/bin/openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            ])
            .args(["-subj", "/CN=127.0.0.1"])
            .args(["-addext", "subjectAltName=IP:127.0.0.1"])
            .args(["-addext", "basicConstraints=critical,CA:FALSE"])
            .args(["-addext", "extendedKeyUsage=serverAuth"])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("openssl runs");
        assert!(status.success(), "test certificate");

        let port = free_port();
        let child = Command::new("/usr/bin/openssl")
            .args(["s_server", "-WWW", "-quiet", "-accept", &port.to_string()])
            .arg("-cert")
            .arg(&cert)
            .arg("-key")
            .arg(&key)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("openssl s_server starts");
        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline, "s_server never listened");
            std::thread::sleep(Duration::from_millis(20));
        }
        Self {
            host: format!("127.0.0.1:{port}"),
            cacert: cert,
            child,
            _keys: keys,
        }
    }
}

impl Drop for TlsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a free port")
        .port()
}

/// Accepts connections and never says a word: a route that black-holes TLS.
fn silent_host() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let host = format!("127.0.0.1:{}", listener.local_addr().expect("addr").port());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    host
}

/// Answers the TLS handshake with plain text: an intercepting middlebox.
fn plaintext_host() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let host = format!("127.0.0.1:{}", listener.local_addr().expect("addr").port());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
        }
    });
    host
}

/// Nothing listens on the discard port.
const REFUSING_HOST: &str = "127.0.0.1:9";

fn sha256(bytes: &[u8]) -> String {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("bytes");
    std::fs::write(&path, bytes).expect("write");
    let output = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(&path)
        .output()
        .expect("shasum");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("digest")
        .to_owned()
}

fn feed_json(release_url: &str) -> String {
    serde_json::json!({
        "feed_version": 1,
        "releases": [{
            "version": "9.9.9",
            "url": release_url,
            "size": ARCHIVE.len(),
            "sha256": sha256(ARCHIVE),
        }]
    })
    .to_string()
}

fn github_url(github: &str) -> String {
    format!("https://{github}/cristicretu/diri/releases/download/{TAG}/{FILE}")
}

/// A release host's file tree: the feed, and the archive at the mirror's path.
fn site(feed: &str, archive: &[u8]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("site root");
    std::fs::write(root.path().join("appcast.json"), feed).expect("feed");
    let release = root.path().join("releases/download").join(TAG);
    std::fs::create_dir_all(&release).expect("release dir");
    std::fs::write(release.join(FILE), archive).expect("archive");
    root
}

struct Fixture {
    updater: Updater,
    _mirror: TlsServer,
    _site: tempfile::TempDir,
    _home: tempfile::TempDir,
}

/// An updater whose "GitHub" is `github` and whose mirror serves `feed` and
/// `archive`.
fn fixture(github: &str, feed: &str, archive: &[u8]) -> Fixture {
    let site = site(feed, archive);
    let mirror = TlsServer::start(site.path());
    let home = tempfile::tempdir().expect("home");
    let config = UpdaterConfig {
        feed_url: format!(
            "https://{github}/cristicretu/diri/releases/latest/download/appcast.json"
        ),
        releases_host: github.to_owned(),
        mirror: Some(Mirror::new(&mirror.host).expect("mirror host")),
        current_version: Version::new(0, 1, 0),
        bundle: home.path().join("diri.app"),
        cache_dir: home.path().join("updates"),
        installed_signature: SignatureInfo::default(),
    };
    let updater = Updater::with_http(
        config,
        test_http(&mirror.cacert),
        Duration::from_millis(500),
    );
    Fixture {
        updater,
        _mirror: mirror,
        _site: site,
        _home: home,
    }
}

fn test_http(cacert: &Path) -> Http {
    Http {
        connect_timeout_seconds: 3,
        feed_timeout_seconds: 5,
        download_timeout_seconds: 10,
        cacert: Some(cacert.to_path_buf()),
        ..Http::new()
    }
}

fn check_from_mirror(github: &str, expected_github_error: &'static str) -> Fixture {
    let fixture = fixture(github, &feed_json(&github_url(github)), ARCHIVE);
    let release = fixture
        .updater
        .check(None)
        .expect("the mirror answers the check")
        .expect("9.9.9 is offered");
    assert_eq!(release.version, "9.9.9");
    assert_eq!(
        fixture.updater.take_source(),
        Some(SourceReport {
            source: Source::Mirror,
            github_error: Some(expected_github_error),
            mirror_error: None,
        })
    );
    fixture
}

#[test]
fn a_refused_github_is_replaced_by_the_mirror_for_feed_and_archive() {
    let fixture = check_from_mirror(REFUSING_HOST, "connect");
    let release = fixture
        .updater
        .check(None)
        .expect("check")
        .expect("release");
    fixture.updater.take_source();

    let archive = fixture
        .updater
        .download(&release, |_| {})
        .expect("the mirror's archive matches the feed");
    assert_eq!(std::fs::read(&archive).expect("archive"), ARCHIVE);
    assert_eq!(
        fixture.updater.take_source(),
        Some(SourceReport {
            source: Source::Mirror,
            github_error: Some("skipped"),
            mirror_error: None,
        }),
        "the feed came from the mirror, so the archive goes there first"
    );
}

#[test]
fn an_unresolvable_github_is_replaced_by_the_mirror() {
    check_from_mirror("diri-updates-test.invalid", "dns");
}

#[test]
fn a_tls_intercepted_github_is_replaced_by_the_mirror() {
    check_from_mirror(&plaintext_host(), "tls");
}

#[test]
fn a_black_holed_github_costs_only_the_hedge_delay() {
    let started = Instant::now();
    check_from_mirror(&silent_host(), "slow");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "fell back after {:?}, not after GitHub's timeout",
        started.elapsed()
    );
}

#[test]
fn a_download_github_drops_falls_back_to_the_mirror() {
    // The feed came from GitHub (simulated by never fetching it), so GitHub
    // is tried first and the mirror only after its route fails.
    let fixture = fixture(REFUSING_HOST, "{}", ARCHIVE);
    let release = Feed::parse(&feed_json(&github_url(REFUSING_HOST)))
        .expect("feed")
        .releases[0]
        .clone();
    let archive = fixture.updater.download(&release, |_| {}).expect("mirror");
    assert_eq!(std::fs::read(&archive).expect("archive"), ARCHIVE);
    assert_eq!(
        fixture.updater.take_source(),
        Some(SourceReport {
            source: Source::Mirror,
            github_error: Some("connect"),
            mirror_error: None,
        })
    );
}

#[test]
fn a_tampered_mirror_archive_is_rejected_and_deleted() {
    let mut tampered = ARCHIVE.to_vec();
    tampered[0] ^= 0xff;
    let fixture = fixture(
        REFUSING_HOST,
        &feed_json(&github_url(REFUSING_HOST)),
        &tampered,
    );
    let release = fixture
        .updater
        .check(None)
        .expect("check")
        .expect("release");

    let error = fixture
        .updater
        .download(&release, |_| {})
        .expect_err("bytes that do not match the feed are never staged");
    assert!(matches!(error, UpdateError::Integrity(_)), "{error}");
    assert!(
        !fixture
            .updater
            .release_dir(&release)
            .join("diri.zip")
            .exists(),
        "the rejected archive is removed"
    );
    let report = fixture.updater.take_source().expect("report");
    assert_eq!(report.source, Source::Mirror);
}

#[test]
fn an_oversized_mirror_archive_is_rejected() {
    let mut padded = ARCHIVE.to_vec();
    padded.extend_from_slice(&[0; 4096]);
    let fixture = fixture(
        REFUSING_HOST,
        &feed_json(&github_url(REFUSING_HOST)),
        &padded,
    );
    let release = fixture
        .updater
        .check(None)
        .expect("check")
        .expect("release");
    let error = fixture
        .updater
        .download(&release, |_| {})
        .expect_err("more bytes than the feed declared");
    assert!(matches!(error, UpdateError::Integrity(_)), "{error}");
}

#[test]
fn a_mirror_feed_cannot_point_downloads_at_another_host() {
    // A mirror rewriting the archive URL — to itself or anywhere else — is
    // refused: archive URLs must name GitHub; the client derives the mirror
    // path on its own.
    let site = site("{}", ARCHIVE);
    let mirror = TlsServer::start(site.path());
    let hijacked = format!("https://{}/releases/download/{TAG}/{FILE}", mirror.host);
    std::fs::write(site.path().join("appcast.json"), feed_json(&hijacked)).expect("feed");
    let home = tempfile::tempdir().expect("home");
    let updater = Updater::with_http(
        UpdaterConfig {
            feed_url: format!("https://{REFUSING_HOST}/appcast.json"),
            releases_host: REFUSING_HOST.to_owned(),
            mirror: Some(Mirror::new(&mirror.host).expect("host")),
            current_version: Version::new(0, 1, 0),
            bundle: home.path().join("diri.app"),
            cache_dir: home.path().join("updates"),
            installed_signature: SignatureInfo::default(),
        },
        test_http(&mirror.cacert),
        Duration::from_millis(500),
    );
    let release = updater.check(None).expect("check").expect("release");
    let error = updater
        .download(&release, |_| {})
        .expect_err("the mirror named its own URL");
    assert!(matches!(error, UpdateError::UntrustedUrl(_)), "{error}");
}

#[test]
fn a_healthy_github_serves_the_archive_and_the_mirror_is_never_read() {
    // GitHub serves the genuine archive; the mirror would serve tampered
    // bytes, so success proves the mirror was not consulted.
    let github_site = tempfile::tempdir().expect("github root");
    let release_dir = github_site
        .path()
        .join("cristicretu/diri/releases/download")
        .join(TAG);
    std::fs::create_dir_all(&release_dir).expect("dir");
    std::fs::write(release_dir.join(FILE), ARCHIVE).expect("archive");
    let github = TlsServer::start(github_site.path());

    let mut tampered = ARCHIVE.to_vec();
    tampered[0] ^= 0xff;
    let fixture = fixture(&github.host, "{}", &tampered);
    let release = Feed::parse(&feed_json(&github_url(&github.host)))
        .expect("feed")
        .releases[0]
        .clone();
    let updater = Updater::with_http(
        fixture.updater.config.clone(),
        Http {
            // Trust both throwaway certificates.
            cacert: Some(bundle_of(
                &[&github.cacert, &fixture._mirror.cacert],
                &fixture._home,
            )),
            ..fixture.updater.http.clone()
        },
        Duration::from_millis(500),
    );
    let archive = updater
        .download(&release, |_| {})
        .expect("GitHub serves it");
    assert_eq!(std::fs::read(&archive).expect("archive"), ARCHIVE);
    assert_eq!(updater.take_source(), Some(SourceReport::github()));
}

fn bundle_of(certs: &[&Path], directory: &tempfile::TempDir) -> PathBuf {
    let path = directory.path().join("ca-bundle.pem");
    let mut bundle = Vec::new();
    for cert in certs {
        bundle.extend(std::fs::read(cert).expect("cert"));
    }
    std::fs::write(&path, bundle).expect("bundle");
    path
}
