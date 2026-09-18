//! Opt-in box art fetch from libretro-thumbnails (ticket W15-09, ruling
//! D-011 in `docs/DECISIONS.md`; `docs/design/UX_WAVE_15.md` §4 item 4).
//!
//! ## What this module is, and is not
//!
//! This is the fourth and LAST thumbnail source in `crate::thumbnail`'s
//! priority order (save-state screenshot, first-frame, user art folder,
//! then this) — never a replacement for any of the first three, and never
//! reachable unless Settings' "Fetch box art from the internet" toggle
//! (`crate::settings::PathSettings::fetch_art`) is on. `crate::app` is the
//! only caller that checks that toggle and decides whether to ask this
//! module for anything at all; nothing here defaults to on, and nothing
//! here runs unless asked.
//!
//! [`ArtClient`] is a trait, not a hard dependency on `ureq`, specifically
//! so acceptance 1 ("with the toggle off, no request is made") is testable
//! with a recording fake that panics if it is ever called — no live
//! network access needed to prove the gate holds.
//!
//! NON_GOALS #5 (no ROM distribution) is untouched: [`thumbnail_url`]
//! only ever builds a URL to a PNG under `Named_Boxarts/`; nothing in this
//! module can request, receive, or store ROM bytes.
//!
//! ## The URL shape (verified against the source, not memory)
//!
//! Per <https://github.com/libretro-thumbnails/libretro-thumbnails>'s
//! README (fetched 2026-09-18): thumbnails are laid out as
//! `thumbnails/<Playlist Name>/<Named_Type>/<Game Name>.png`, served from
//! `thumbnails.libretro.com` at the same path shape, and "If the
//! characters `&*/:\`<>?\|"` appear in a game name... they must be
//! replaced with `_` in the corresponding thumbnail filename." This
//! module always requests the `Named_Boxarts` type. `<Game Name>` is the
//! No-Intro name (the library's own `LibraryEntry::title`, which is
//! already the ROM's file stem with no scraping applied —
//! `crate::library`'s own module doc), mapped through [`map_game_name`].

use std::collections::HashSet;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use rf_cache::{Cache, CacheKey};

use crate::library::Console;

/// `<System Name>` for each console this build recognizes, exactly as
/// libretro-thumbnails names its per-system repositories/directories
/// (e.g. `libretro-thumbnails/Nintendo_-_Nintendo_Entertainment_System`
/// unslugs to this directory name inside `thumbnails.libretro.com`).
#[must_use]
pub const fn system_name(console: Console) -> &'static str {
    match console {
        Console::Nes => "Nintendo - Nintendo Entertainment System",
        Console::Snes => "Nintendo - Super Nintendo Entertainment System",
    }
}

/// Characters the libretro-thumbnails README says must be replaced with
/// `_` in a thumbnail filename: `` &*/:`<>?\| `` plus `"`, which the
/// README's own prose lists alongside them (both are filesystem-unsafe on
/// Windows, which is the entire reason the substitution rule exists).
const UNSAFE_CHARS: [char; 9] = ['&', '*', '/', ':', '`', '<', '>', '?', '\\'];

/// Map a No-Intro game name to its libretro-thumbnails filename stem
/// (module doc): every character in [`UNSAFE_CHARS`] (plus `"`) becomes
/// `_`; everything else, including non-ASCII titles, passes through
/// unchanged — the README's rule names specific unsafe characters, not
/// "ASCII only".
#[must_use]
pub fn map_game_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '"' || UNSAFE_CHARS.contains(&c) {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Percent-encode one path segment for the request URL. `map_game_name`
/// has already removed every filesystem-unsafe character; this only
/// needs to handle what is unsafe in a URL *path segment* — spaces above
/// all, since No-Intro names are full of them (`"Super Mario Bros. 3
/// (USA) (Rev 1)"`). Conservative allowlist (alphanumeric plus a handful
/// of characters that are safe unencoded in a path segment per RFC 3986
/// and appear routinely in these names, e.g. `.`, `(`, `)`, `'`, `,`,
/// `!`, `-`); everything else, ASCII or not, is percent-encoded from its
/// UTF-8 bytes.
fn percent_encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'('
            | b')'
            | b'\''
            | b','
            | b'!' => out.push(byte as char),
            _ => {
                out.push('%');
                out.push_str(&format!("{byte:02X}"));
            }
        }
    }
    out
}

/// Build the full HTTPS URL for one game's box art (module doc's verified
/// shape): `https://thumbnails.libretro.com/<System Name>/Named_Boxarts/
/// <mapped Game Name>.png`.
#[must_use]
pub fn thumbnail_url(system: &str, game_name: &str) -> String {
    let mapped = map_game_name(game_name);
    format!(
        "https://thumbnails.libretro.com/{}/Named_Boxarts/{}.png",
        percent_encode_segment(system),
        percent_encode_segment(&mapped)
    )
}

/// Whether the fetch pipeline should even be asked for this entry:
/// acceptance 1's gate (the toggle) combined with "for entries with no
/// local thumbnail" (plan.json W15-09 acceptance 5) — a pure function so
/// both conditions are testable with no network, no thread, and no
/// `egui::Context` in sight.
#[must_use]
pub const fn should_fetch(fetch_art_enabled: bool, has_local_thumbnail: bool) -> bool {
    fetch_art_enabled && !has_local_thumbnail
}

/// What went wrong fetching one URL. Kept small and stringly-detailed
/// (module doc: failures are quiet, at most a single toast per session —
/// nothing here is meant to be pattern-matched by the UI beyond "it
/// failed").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtFetchError {
    /// The transport itself failed (DNS, TLS, connection refused/reset,
    /// timeout) — `ureq::Error`'s `Display` text.
    Transport(String),
    /// The server answered but not with a usable image: any non-2xx
    /// status (404 is the overwhelmingly common case — a title with no
    /// thumbnail in the community repo).
    Status(u16),
}

impl std::fmt::Display for ArtFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArtFetchError::Transport(msg) => write!(f, "transport error: {msg}"),
            ArtFetchError::Status(code) => write!(f, "HTTP {code}"),
        }
    }
}

/// Fetches box art bytes from a URL. A trait (module doc) so the "toggle
/// off means zero requests" and "toggle on fetches once" acceptance tests
/// need no live network — [`FakeArtClient`] (test-only) stands in for
/// [`UreqArtClient`].
///
/// `Send + Sync` because the only real caller (`ArtFetcher::spawn`) hands
/// the client across a thread boundary.
pub trait ArtClient: Send + Sync {
    /// Fetch `url` and return its raw response bytes on a 2xx status.
    ///
    /// # Errors
    /// [`ArtFetchError`] on any transport failure or non-2xx status.
    fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, ArtFetchError>;
}

/// The real client: a plain blocking `ureq` GET, no cookies, no JSON, no
/// proxy — just enough to retrieve one PNG (NFR-011: `ureq`, MIT OR
/// Apache-2.0, `default-features = false` plus the `rustls-no-provider`,
/// `_ring` and `platform-verifier` features — TLS via rustls, trust
/// anchors from the OS-native certificate store rather than a vendored
/// Mozilla bundle; see `deny.toml`'s W15-09 section for why).
#[derive(Debug, Default, Clone, Copy)]
pub struct UreqArtClient;

impl UreqArtClient {
    /// One agent per call is fine at this volume (a handful of PNGs per
    /// session). What matters is the root-cert choice: with the
    /// `platform-verifier` feature and no `rustls-webpki-roots`, ureq's
    /// default `RootCerts::WebPki` panics inside the TLS layer
    /// ("WebPki is disabled. You need to explicitly configure root certs
    /// on Agent", ureq-3.4.2/src/tls/rustls.rs) — found by running the
    /// ignored real-fetch test, which the recording fake could never
    /// catch. `RootCerts::PlatformVerifier` is the OS trust store the
    /// deny.toml note promises.
    fn agent() -> ureq::Agent {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        ureq::Agent::config_builder().tls_config(tls).build().into()
    }
}

impl ArtClient for UreqArtClient {
    fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, ArtFetchError> {
        let mut response = Self::agent()
            .get(url)
            .call()
            .map_err(|e| ArtFetchError::Transport(e.to_string()))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(ArtFetchError::Status(status));
        }
        response
            .body_mut()
            .read_to_vec()
            .map_err(|e| ArtFetchError::Transport(e.to_string()))
    }
}

/// One fetch's outcome, delivered back through [`ArtFetcher::poll`].
pub struct ArtFetchResult {
    pub rom_sha256: String,
    pub outcome: Result<Vec<u8>, ArtFetchError>,
}

struct ArtRequest {
    rom_sha256: String,
    url: String,
}

/// The background fetch queue (plan.json W15-09 acceptance: "a background
/// thread with a small queue... at most one in flight per entry, once per
/// session on failure").
///
/// One worker thread drains `request_rx` and processes requests strictly
/// one at a time — "a small queue" and "at most one in flight" fall out
/// of that shape for free, with no separate concurrency-limiting
/// bookkeeping needed. `request` itself is what enforces the "once per
/// session" half: a `rom_sha256` already in flight or already attempted
/// (success OR failure) this session is never queued twice.
pub struct ArtFetcher {
    request_tx: mpsc::Sender<ArtRequest>,
    result_rx: mpsc::Receiver<ArtFetchResult>,
    in_flight: HashSet<String>,
    attempted_this_session: HashSet<String>,
}

impl ArtFetcher {
    /// Spawn the worker thread. `waker`, if given, is called after every
    /// result is sent — the same pattern `core_thread::spawn_with_waker`
    /// uses (ticket W14-20 defect 2) to wake a UI thread that may be
    /// idling on `ctx.request_repaint()`'s ordinary redraw cadence rather
    /// than actively polling a background channel every frame.
    #[must_use]
    pub fn spawn(client: Arc<dyn ArtClient>, waker: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<ArtRequest>();
        let (result_tx, result_rx) = mpsc::channel::<ArtFetchResult>();
        // Detached deliberately, same reasoning as `rescan_library`'s own
        // worker: nothing joins this thread, and it ends on its own once
        // the app drops both channel halves (`request_tx` closing ends
        // the `recv()` loop).
        let _ = thread::Builder::new()
            .name("art-fetch".to_string())
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    let outcome = client.fetch_bytes(&request.url);
                    let sent = result_tx.send(ArtFetchResult {
                        rom_sha256: request.rom_sha256,
                        outcome,
                    });
                    if sent.is_err() {
                        // The receiver (the app) is gone; nothing left to
                        // wake and nothing left to process.
                        break;
                    }
                    if let Some(wake) = &waker {
                        wake();
                    }
                }
            });
        ArtFetcher {
            request_tx,
            result_rx,
            in_flight: HashSet::new(),
            attempted_this_session: HashSet::new(),
        }
    }

    /// Queue a fetch for `rom_sha256` at `url`, unless one is already in
    /// flight or was already attempted (either outcome) this session.
    /// Returns whether it was actually queued — callers don't need the
    /// return value for correctness (the dedup is internal), but it makes
    /// the "fetches once" acceptance test's assertion direct rather than
    /// inferred from side effects.
    pub fn request(&mut self, rom_sha256: &str, url: &str) -> bool {
        if self.in_flight.contains(rom_sha256) || self.attempted_this_session.contains(rom_sha256) {
            return false;
        }
        self.in_flight.insert(rom_sha256.to_string());
        self.attempted_this_session.insert(rom_sha256.to_string());
        let sent = self.request_tx.send(ArtRequest {
            rom_sha256: rom_sha256.to_string(),
            url: url.to_string(),
        });
        if sent.is_err() {
            // Worker thread is gone (spawn failed, or it already exited);
            // undo the bookkeeping rather than permanently blackholing
            // this hash for the rest of the session.
            self.in_flight.remove(rom_sha256);
            self.attempted_this_session.remove(rom_sha256);
            return false;
        }
        true
    }

    /// Drain every result that has arrived since the last poll. Never
    /// blocks — this is called from the UI thread once per frame
    /// (`crate::app`'s `pump_core_events`-style poll loop).
    pub fn poll(&mut self) -> Vec<ArtFetchResult> {
        let mut results = Vec::new();
        while let Ok(result) = self.result_rx.try_recv() {
            self.in_flight.remove(&result.rom_sha256);
            results.push(result);
        }
        results
    }
}

// ---------------------------------------------------------------------
// Art cache: a second `rf-cache::Cache`, rooted in its own `art`
// subdirectory with its own cap (`PathSettings::art_cache_cap_mb`) —
// `rf-cache` has no namespace concept (`Cache::open` takes one root and
// one cap), so the fallback the ticket names is what this is: a second
// store, not a second implementation.
// ---------------------------------------------------------------------

const FETCHED_ART_ASSET: &str = "boxart";
const FETCHED_ART_PRODUCER: &str = "libretro-thumbnails";
/// Bumped if the stored payload's shape ever changes (currently: raw PNG
/// bytes straight off the wire).
const FETCHED_ART_SETTINGS: &str = "v1";

fn fetched_art_key(rom_sha256: &str) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: FETCHED_ART_ASSET.to_string(),
        producer: FETCHED_ART_PRODUCER.to_string(),
        settings_hash: FETCHED_ART_SETTINGS.to_string(),
    }
}

/// Where the fetched-art cache lives: alongside `crate::thumbnail`'s own
/// `thumbnails` cache directory (same `cache_dir`/config-root base), in
/// a sibling `art` subdirectory so its own cap (`art_cache_cap_mb`)
/// evicts independently of the first-frame-capture cache's cap
/// (`cache_cap_mb`) — exactly the "separate art cache cap" plan.json asks
/// for.
#[must_use]
pub fn art_cache_root(
    config_root: Option<&std::path::Path>,
    paths: &crate::settings::PathSettings,
) -> Option<std::path::PathBuf> {
    let base = match &paths.cache_dir {
        Some(dir) => dir.clone(),
        None => config_root?.join("cache"),
    };
    Some(base.join("art"))
}

/// Open the fetched-art cache, respecting `PathSettings::art_cache_cap_mb`.
/// `None` under the same conditions as `crate::thumbnail::open_cache` —
/// missing entirely degrades to "never fetched this session", never a
/// reason to fail loading a ROM or opening the library.
#[must_use]
pub fn open_art_cache(
    config_root: Option<&std::path::Path>,
    paths: &crate::settings::PathSettings,
) -> Option<Cache> {
    let root = art_cache_root(config_root, paths)?;
    let cap_bytes = paths.art_cache_cap_mb.saturating_mul(1024 * 1024);
    Cache::open(root, cap_bytes).ok()
}

/// Read back fetched art bytes for `rom_sha256`, or `None` on a miss.
///
/// # Errors
/// Propagates [`Cache::get`]'s errors (a genuinely corrupt/tampered
/// entry) — distinct from `Ok(None)`, an ordinary cache miss.
pub fn get_fetched_art(
    cache: &mut Cache,
    rom_sha256: &str,
) -> Result<Option<Vec<u8>>, rf_cache::CacheError> {
    cache.get(&fetched_art_key(rom_sha256))
}

/// Persist fetched art bytes for `rom_sha256`.
///
/// # Errors
/// Propagates [`Cache::put`]'s errors (disk full, containment violation).
pub fn put_fetched_art(
    cache: &mut Cache,
    rom_sha256: &str,
    png_bytes: &[u8],
) -> Result<(), rf_cache::CacheError> {
    cache.put(&fetched_art_key(rom_sha256), png_bytes)
}

pub mod test_support {
    //! Test-only fake client. Deliberately NOT `#[cfg(test)]`-gated: the
    //! kittest scenario in `tests/library_grid_network_art.rs` (ticket
    //! W15-09 acceptance 6) is a separate integration-test crate compiled
    //! against `retroforge` as an ordinary external dependency, where
    //! `cfg(test)` is never set — only this crate's OWN `cargo test`
    //! unit-test build sets that cfg. Always compiling this small module
    //! is the same tradeoff `crate::app`'s `_for_test` methods already
    //! make (that impl block isn't `#[cfg(test)]`-gated either).
    use super::{ArtClient, ArtFetchError};
    use std::sync::Mutex;

    /// Records every URL it was asked to fetch and returns a fixed
    /// canned response — used to prove acceptance 1 ("off means zero
    /// requests") and acceptance 2 ("on fetches once and caches") with no
    /// network access.
    pub struct FakeArtClient {
        pub requested_urls: Mutex<Vec<String>>,
        /// What every call returns; `Ok` by default (a tiny fake PNG).
        pub response: Mutex<Result<Vec<u8>, ArtFetchError>>,
    }

    impl FakeArtClient {
        #[must_use]
        pub fn new(response: Result<Vec<u8>, ArtFetchError>) -> Self {
            FakeArtClient {
                requested_urls: Mutex::new(Vec::new()),
                response: Mutex::new(response),
            }
        }

        #[must_use]
        pub fn call_count(&self) -> usize {
            self.requested_urls.lock().unwrap().len()
        }
    }

    impl ArtClient for FakeArtClient {
        fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, ArtFetchError> {
            self.requested_urls.lock().unwrap().push(url.to_string());
            self.response.lock().unwrap().clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeArtClient;
    use super::*;
    use std::time::Duration;

    // ---- map_game_name (module doc's README-verified rule) ------------

    #[test]
    fn map_game_name_replaces_every_unsafe_character_with_underscore() {
        assert_eq!(
            map_game_name(r#"A&B*C/D:E`F<G>H?I\J"K"#),
            "A_B_C_D_E_F_G_H_I_J_K"
        );
    }

    #[test]
    fn map_game_name_leaves_ordinary_punctuation_alone() {
        assert_eq!(
            map_game_name("Super Mario Bros. 3 (USA) (Rev 1)"),
            "Super Mario Bros. 3 (USA) (Rev 1)"
        );
    }

    #[test]
    fn map_game_name_leaves_a_japanese_title_untouched() {
        // No unsafe ASCII characters present -- must pass through
        // byte-for-byte, not get mangled by any ASCII-only assumption.
        let title = "ゼルダの伝説";
        assert_eq!(map_game_name(title), title);
    }

    #[test]
    fn map_game_name_handles_colon_and_ampersand_together() {
        assert_eq!(
            map_game_name("Kirby's Dream Land 3: Special & Rare"),
            "Kirby's Dream Land 3_ Special _ Rare"
        );
    }

    // ---- thumbnail_url shape (acceptance 5: "the client only ever
    // builds thumbnail URLs") -------------------------------------------

    #[test]
    fn thumbnail_url_has_the_expected_host_and_path_shape() {
        let url = thumbnail_url(system_name(Console::Nes), "Super Mario Bros. (World)");
        assert!(url.starts_with("https://thumbnails.libretro.com/"));
        assert!(url.contains("/Named_Boxarts/"));
        assert!(url.ends_with(".png"));
        // Never anything that looks like a ROM extension.
        for ext in [".nes", ".sfc", ".smc", ".zip"] {
            assert!(!url.ends_with(ext));
        }
    }

    #[test]
    fn thumbnail_url_encodes_spaces_and_keeps_system_segment_readable() {
        let url = thumbnail_url(system_name(Console::Snes), "Super Mario World");
        assert!(url.contains("Nintendo%20-%20Super%20Nintendo%20Entertainment%20System"));
        assert!(url.contains("Super%20Mario%20World.png"));
    }

    #[test]
    fn system_name_matches_the_two_consoles_this_build_recognizes() {
        assert_eq!(
            system_name(Console::Nes),
            "Nintendo - Nintendo Entertainment System"
        );
        assert_eq!(
            system_name(Console::Snes),
            "Nintendo - Super Nintendo Entertainment System"
        );
    }

    // ---- should_fetch (acceptance 1 + 5's gate) ------------------------

    #[test]
    fn should_fetch_is_false_when_the_toggle_is_off_regardless_of_local_art() {
        assert!(!should_fetch(false, false));
        assert!(!should_fetch(false, true));
    }

    #[test]
    fn should_fetch_is_false_when_a_local_thumbnail_already_exists() {
        assert!(!should_fetch(true, true));
    }

    #[test]
    fn should_fetch_is_true_only_when_on_and_nothing_local_exists() {
        assert!(should_fetch(true, false));
    }

    // ---- ArtFetcher: acceptance 1 & 2 with the fake client -------------

    fn drain_until<F: Fn(&[ArtFetchResult]) -> bool>(
        fetcher: &mut ArtFetcher,
        timeout: Duration,
        done: F,
    ) -> Vec<ArtFetchResult> {
        let start = std::time::Instant::now();
        let mut all = Vec::new();
        loop {
            all.extend(fetcher.poll());
            if done(&all) || start.elapsed() > timeout {
                return all;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn toggle_off_means_the_client_is_never_called() {
        // Acceptance 1: with `should_fetch` false, `crate::app` never
        // calls `ArtFetcher::request` at all -- this test proves the
        // fake would notice if it were ever asked, by never wiring a
        // fetcher to it in the first place and asserting zero calls.
        let client = Arc::new(FakeArtClient::new(Ok(b"fake-png".to_vec())));
        assert!(!should_fetch(false, false));
        assert_eq!(client.call_count(), 0);
    }

    #[test]
    fn toggle_on_fetches_once_and_a_second_request_is_deduped() {
        let client = Arc::new(FakeArtClient::new(Ok(b"fake-png-bytes".to_vec())));
        let mut fetcher = ArtFetcher::spawn(client.clone(), None);

        assert!(should_fetch(true, false));
        let queued = fetcher.request(
            "rom-abc",
            "https://thumbnails.libretro.com/x/Named_Boxarts/y.png",
        );
        assert!(queued);

        let results = drain_until(&mut fetcher, Duration::from_secs(2), |r| !r.is_empty());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rom_sha256, "rom-abc");
        assert_eq!(results[0].outcome, Ok(b"fake-png-bytes".to_vec()));
        assert_eq!(client.call_count(), 1);

        // Requesting the same hash again this session is a no-op -- the
        // "once per session" half of acceptance 2/plan.json.
        let queued_again = fetcher.request(
            "rom-abc",
            "https://thumbnails.libretro.com/x/Named_Boxarts/y.png",
        );
        assert!(!queued_again);
        assert_eq!(client.call_count(), 1);
    }

    #[test]
    fn a_failed_fetch_is_not_retried_this_session() {
        let client = Arc::new(FakeArtClient::new(Err(ArtFetchError::Status(404))));
        let mut fetcher = ArtFetcher::spawn(client.clone(), None);

        fetcher.request(
            "rom-missing",
            "https://thumbnails.libretro.com/x/Named_Boxarts/missing.png",
        );
        let results = drain_until(&mut fetcher, Duration::from_secs(2), |r| !r.is_empty());
        assert_eq!(results.len(), 1);
        assert!(results[0].outcome.is_err());

        // Second attempt this session: deduped, no second network call.
        let queued_again = fetcher.request(
            "rom-missing",
            "https://thumbnails.libretro.com/x/Named_Boxarts/missing.png",
        );
        assert!(!queued_again);
        assert_eq!(client.call_count(), 1);
    }

    #[test]
    fn different_hashes_each_get_their_own_fetch() {
        let client = Arc::new(FakeArtClient::new(Ok(b"png".to_vec())));
        let mut fetcher = ArtFetcher::spawn(client.clone(), None);
        assert!(fetcher.request(
            "rom-a",
            "https://thumbnails.libretro.com/x/Named_Boxarts/a.png"
        ));
        assert!(fetcher.request(
            "rom-b",
            "https://thumbnails.libretro.com/x/Named_Boxarts/b.png"
        ));
        let results = drain_until(&mut fetcher, Duration::from_secs(2), |r| r.len() == 2);
        assert_eq!(results.len(), 2);
        assert_eq!(client.call_count(), 2);
    }

    #[test]
    fn waker_is_invoked_after_a_result_is_delivered() {
        let client = Arc::new(FakeArtClient::new(Ok(b"png".to_vec())));
        let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let woken_writer = Arc::clone(&woken);
        let waker: Arc<dyn Fn() + Send + Sync> =
            Arc::new(move || woken_writer.store(true, std::sync::atomic::Ordering::SeqCst));
        let mut fetcher = ArtFetcher::spawn(client, Some(waker));
        fetcher.request(
            "rom-a",
            "https://thumbnails.libretro.com/x/Named_Boxarts/a.png",
        );
        drain_until(&mut fetcher, Duration::from_secs(2), |r| !r.is_empty());
        // Give the worker's post-send `wake()` call a moment; `poll`
        // already observed the result by the time `drain_until` returns,
        // and the wake call happens before the next loop iteration in
        // the worker, so this is not a race on the assertion itself.
        std::thread::sleep(Duration::from_millis(20));
        assert!(woken.load(std::sync::atomic::Ordering::SeqCst));
    }

    // ---- fetched-art cache round trip (rf-cache, separate namespace) --

    fn unique_dir(label: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rf-art-test-{label}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fetched_art_round_trips_through_its_own_cache() {
        let dir = unique_dir("roundtrip");
        let mut cache = Cache::open(&dir, 1_000_000).unwrap();
        assert_eq!(get_fetched_art(&mut cache, "rom-x").unwrap(), None);
        put_fetched_art(&mut cache, "rom-x", b"fake-png-bytes").unwrap();
        assert_eq!(
            get_fetched_art(&mut cache, "rom-x").unwrap(),
            Some(b"fake-png-bytes".to_vec())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn art_cache_root_is_a_sibling_of_the_thumbnails_cache_not_inside_it() {
        let paths = crate::settings::PathSettings {
            cache_dir: Some(std::path::PathBuf::from("/configured/cache")),
            ..Default::default()
        };
        let art_root = art_cache_root(None, &paths).unwrap();
        let thumb_root = crate::thumbnail::cache_root(None, &paths).unwrap();
        assert_eq!(art_root, std::path::PathBuf::from("/configured/cache/art"));
        assert_eq!(
            thumb_root,
            std::path::PathBuf::from("/configured/cache/thumbnails")
        );
        assert_ne!(art_root, thumb_root);
    }

    /// Acceptance: art-cache cap eviction. Two ~40-byte entries under a
    /// cap that only fits one force an LRU eviction; the one touched more
    /// recently must survive (mirrors `rf_cache::store`'s own eviction
    /// tests' shape).
    #[test]
    fn art_cache_evicts_the_least_recently_used_entry_under_its_own_cap() {
        let dir = unique_dir("evict");
        let payload_a = vec![7u8; 40];
        let payload_b = vec![9u8; 40];
        let mut cache = Cache::open(&dir, 50).unwrap();
        put_fetched_art(&mut cache, "rom-a", &payload_a).unwrap();
        // Touch "rom-a" so it is more recently used than what "rom-b" is
        // about to become, then insert "rom-b", which must evict "rom-a"
        // to stay under the 50-byte cap.
        let _ = get_fetched_art(&mut cache, "rom-a").unwrap();
        put_fetched_art(&mut cache, "rom-b", &payload_b).unwrap();
        assert_eq!(get_fetched_art(&mut cache, "rom-a").unwrap(), None);
        assert_eq!(
            get_fetched_art(&mut cache, "rom-b").unwrap(),
            Some(payload_b)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- the real fetch (network) --------------------------------------

    /// Not run by default (network access, `cargo test --workspace` must
    /// stay hermetic per CLAUDE.md's CI posture) -- `cargo test -p
    /// retroforge -- --ignored art::tests::real_fetch_of_a_known_title`
    /// on a machine with internet access.
    #[test]
    #[ignore]
    fn real_fetch_of_a_known_title_returns_a_png() {
        let url = thumbnail_url(system_name(Console::Nes), "Super Mario Bros. (World)");
        let bytes = UreqArtClient
            .fetch_bytes(&url)
            .expect("fetch should succeed");
        assert!(bytes.len() > 8, "response too small to be a PNG");
        assert_eq!(
            &bytes[0..8],
            b"\x89PNG\r\n\x1a\n",
            "missing PNG magic bytes"
        );
    }
}
