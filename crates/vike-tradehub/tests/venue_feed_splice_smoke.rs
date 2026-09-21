//! The daemon's venue-feed SPLICE — "a subscribed bar/quote actually reaches the core" — proven end to
//! end against the REAL binary and the REAL venue. A live smoke per keyless-data venue (`#[ignore]`d,
//! run explicitly), plus a deterministic startup-refusal case per credentialed-data venue (NOT
//! ignored — network-free, they gate in CI):
//!
//! ```sh
//! cargo test -p vike-tradehub --test venue_feed_splice_smoke -- --ignored --nocapture
//! ```
//!
//! # The gap this closes
//!
//! `crates/vike-tradehub/src/tradehub_cli.rs`'s `wire_venue_feeds` is the splice between a bridge's live
//! feed and the core (`vike_core::CoreLaneSink` onto the ingest lanes). Every venue arm in it
//! rests on pure `*_plan`/`*_arming` unit tests plus the compiler, and the paper-seam daemon tests
//! (`crates/vike-tradehub/tests/daemon/multi_mount_profile.rs`) deliberately build NO venue feed —
//! they hand-feed the core's own `bar_sender`. So before this file, no test anywhere reached the
//! path *venue socket → venue decode → `LiveDataSink` → core lane → mounted strategy* for ANY
//! venue: a cross-labelled subscription, a sink wired to the wrong lane, or an arm that subscribes
//! nothing at all would disclose a healthy mount and feed the core nothing — the
//! silent-do-nothing this repo keeps designing out (`CexTicks`' doc records the exact class).
//!
//! # Why a SMOKE and not a scripted CI test — the evidence
//!
//! The deterministic route was looked at first and was structurally unavailable when this file
//! was written; the first blocker has since been CLOSED, and the other two are why the closure
//! took the shape it did:
//!
//! - CLOSED (#1438): `wire_venue_feeds` used to construct each venue's feed object ITSELF, so
//!   the only caller-injectable seam was its `wrap` parameter, which wraps the SINK, never the
//!   stream. The `FeedCtors` trait in `crates/vike-tradehub/src/tradehub_cli.rs` is exactly the
//!   feed-construction parameter this bullet once said was missing, and
//!   `crates/vike-tradehub/src/feed_splice_seam_tests.rs` is the deterministic splice test it
//!   exists for — scripted frames through the REAL arm, in the default CI lane. This smoke keeps
//!   the half no script can prove: the dial, TLS, reconnect, and the venue's real frames.
//! - The shared driver DOES have a stream-generic entry (`vike_bridge_core::market_pump`'s
//!   `run_market_feed_on`) and `vike_bridge_core::scripted`'s `ScriptedStream` implements
//!   `MarketStream` — but no venue `Feeds` accepts a caller-supplied stream or connect closure:
//!   `crates/bridges/deribit/src/market_feed.rs`'s lane mains (`bars_main` and its siblings) each
//!   hardcode the dial (`run_market_feed` at `crate::options_feed::MAINNET_WS`), and the bars lane
//!   additionally performs a REST warmup (`fetch_seed`) against a hardcoded mainnet base no WS
//!   double covers.
//! - The venue's own injection seam (`Feeds::spawn_with`'s `body` parameter) is private, and
//!   substituting the body replaces the WHOLE lane — parse, fold, sink emission — with a stand-in:
//!   a green fake, not a test of the splice.
//!
//! So the honest test is the skipping-but-real one: the shipped binary, the live gate, the real
//! venue. Deribit was the first venue because its market plane is KEYLESS public MAINNET JSON-RPC
//! — no credential to fake, no account touched; the extension below covers the rest of
//! `LIVE_WIRED_VENUES` in the two shapes the venue set actually splits into.
//!
//! # Venue coverage — every live-wired arm, in exactly two shapes
//!
//! The live-wired set splits on ONE line — whether the venue's MARKET plane authenticates — and
//! that line decides what a smoke can honestly prove while exec must stay paper:
//!
//! - **KEYLESS-DATA venues** — deribit, hyperliquid, binance, bybit, okx, aster, and polymarket
//!   under this crate's `polymarket` feature — get the full splice, the deribit shape verbatim:
//!   an EMPTY temp store keeps every venue's exec on the paper fallback (absent credentials ARE
//!   the workspace live gate) while the venue's public data plane streams for real. Each case's
//!   own doc carries the venue-specific facts (lanes, network verdict, tick grid, and which
//!   stderr words prove the paper seam).
//! - **CREDENTIALED-DATA venues** — alpaca, ctrader, oanda, ig — cannot have their splice proven
//!   by an EMPTY-store run of this smoke, STRUCTURALLY: split-plane I9 deliberately
//!   resolves each feed's credentials through the SAME loader, tier and vars map
//!   `vike_mount::make_engine`'s exec arm reads (each `*_plan`'s doc in
//!   `crates/vike-tradehub/src/tradehub_cli.rs` states it per venue), so any store that lets the feed
//!   mount ORDINARILY also arms REAL demo exec — and the mounted `buy_hold` would place a real
//!   order on the demo account, which a validation smoke must never do. What IS provable end to
//!   end over the shipped binary with an empty store is the other half of the same design: it
//!   REFUSES the live mount at `venue_feed_plan`, before any core thread or venue socket exists,
//!   naming the exact keys — the silent-do-nothing conversion those gates exist for. Those cases
//!   are deterministic and network-free, so they are NOT `#[ignore]`d and gate in CI.
//!   The data-plane-only credential seam this bullet once said did not exist NOW EXISTS:
//!   `data_only = true` in the profile has `live_mount` WITHHOLD that venue's keys from the exec
//!   mount after its plan resolved them (the `withhold_exec_credentials` doc in
//!   `crates/vike-tradehub/src/tradehub_cli.rs` is the authority), so a credentialed feed can mount over
//!   paper exec BY DECLARATION. Oanda's splice is proven DETERMINISTICALLY through it
//!   (`crates/vike-tradehub/src/feed_splice_seam_tests.rs`'s data-only case — scripted frames,
//!   fake keys, the arming-state assert); alpaca/ctrader/ig still rest on their
//!   `*_plan`/`*_arming` unit tests plus the refusal cases here, and a LIVE data-only smoke per
//!   credentialed venue (real demo keys + `data_only = true`, real feed, exec provably paper) is
//!   the weekday follow-up this file's calendar section already scopes.
//!
//! # Weekend / calendar honesty
//!
//! Every splice case here runs on a data plane that trades around the clock (crypto perps and
//! prediction books), so no case in this file is weekday-gated. The venues WITH trading
//! calendars (alpaca equities; oanda/ig/ctrader FX, whose quote lanes go silent over the
//! weekend) are exactly the credentialed-data venues above — and their cases are network-free
//! refusals, which the calendar cannot touch.
//!
//! # The paper seam, and why no order can reach any venue
//!
//! The daemon starts over an EMPTY temp settings store: absent credentials ARE the live gate, so
//! `build_node` mounts every venue's EXEC on the paper exchange even under `VIKE_TRADEHUB_LIVE=1`.
//! Each splice case ASSERTS that disclosure before trusting anything else — the CEX-shaped arms in
//! their own words ("EXEC IS PAPER for {venue}"), the hyperliquid/polymarket arms through
//! `live_venues={}` on the same log line, `build_node`'s own record that NO venue got a live
//! client — so if credentials somehow leaked in, the run fails loudly rather than trading.
//!
//! ⚠ The READY BANNER is now held to that same record instead of to the profile. It reads
//! `LIVE (venue=none)` here — the live gate is on, nothing armed — and `prove_live_splice` asserts
//! it does NOT name the case's own venue. It used to assert the opposite (`LIVE (venue=<venue>)`)
//! beside the `live_venues={}` needle below, i.e. it pinned two contradictory claims about one
//! startup as a contract; that contract is what let the shipped daemon print
//! `"mode":"LIVE (venue=bybit)"` on the CI box over a NINE-venue arming record. See the driver.
//! (Deribit's authed sockets are hardcoded testnet besides — the two-networks split in
//! `crates/bridges/deribit/CLAUDE.md` — so that venue could not reach a mainnet account even with
//! keys present.)
//!
//! # What one green run proves, and what it does not
//!
//! Proven, in one pass (the deribit chain; every splice case proves the same chain with its own
//! venue's arm in the middle): profile → live gate → `venue_feed_plan`'s venue arm → `live_mount`
//! → `wire_venue_feeds` → the venue's real `Feeds` subscribing every one of this mount's lanes
//! without error (each subscribe is `?`-checked — a refusal fails the mount and the ready banner
//! never prints) → a real venue event through the venue's own decode → the sink → the core's
//! ingest lanes → the runtime dispatching it on this mount's own key → the mounted `buy_hold`
//! submitting → the order visible in the stdout summary — and then the bounded teardown joining
//! every feed socket inside the profile deadline, exit 0.
//!
//! `buy_hold` acts on the FIRST event its mount receives on either of two verbs (`BuyHold`'s
//! `on_bar` and `on_quote_tick`), so the trigger in practice is the venue-throttled L1 quote —
//! milliseconds after the quote lane connects — with the 1m close (`BarFolder` — the successor
//! bucket's first push IS the close signal) as the fallback on a quiet book. Every case is
//! deliberately lane-agnostic: WHICH lane delivered first is not pinned, only that a live venue
//! event crossed the whole splice. The label-agreement half is load-bearing either way: the venue
//! lanes stamp their venue string and the caller's own symbol spelling on every emission, and the
//! runtime dispatches quotes on exactly that `(venue, symbol)` pair (bars on the interval-keyed
//! triple), so a cross-labelled subscription (the `cex_feed_wiring_pin.rs` failure class) times
//! the case out — the frames still stream, and the core dispatches none of them to the mount.
//!
//! NOT proven: the credentialed-data venues' LIVE splice (see Venue coverage above — their
//! refusal cases prove the gate, not the feed; oanda's splice is proven scripted through the
//! data-only seam, and the live dial stays the weekday follow-up), and any venue arm on a day
//! its route refuses.
//!
//! ⚠ The splice cases need outbound wss/https to each venue's public data hosts (each case's doc
//! names the venue's own caveats). There is deliberately NO self-skip on network failure: these
//! smokes are keyless, so running one at all is the operator's statement that the network is
//! expected to be there — a dead route should FAIL the run, not green-skip it (the keyless
//! `deribit_klines_live_smoke` precedent).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long to wait for startup facts (ready banner, feed-subscribed disclosure) — and, on the
/// credentialed-data venues, for the startup REFUSAL itself. These are network-free — a keyless
/// subscribe only spawns threads, and a refusal happens before any thread — so this bound is
/// about a loaded box, not the venue.
const STARTUP_PATIENCE: Duration = Duration::from_secs(60);

/// How long to wait for a LIVE venue event to reach the core. In practice the venue-throttled
/// L1 quote lands within seconds of the quote lane connecting; the slowest honest trigger is the
/// 1m bar close (the successor bucket's first push, per `BarFolder`), which lands shortly after
/// the next minute boundary — this bound is several times that, for a loaded box or a slow dial.
const EVENT_PATIENCE: Duration = Duration::from_secs(300);

/// How long to wait for the daemon to exit after the `shutdown` word — must comfortably exceed the
/// profile's `shutdown_deadline_ms` below.
const EXIT_PATIENCE: Duration = Duration::from_secs(60);

/// How long to wait for an EXITED daemon's last stderr/stdout to flush through the reader
/// threads — the pipes are closed, so this is about scheduler latency, not the venue.
const DRAIN_PATIENCE: Duration = Duration::from_secs(10);

/// The symbol `build_node` mounts `venue`'s engine on, read from the table `live_mount`'s own
/// safety gate reads (`vike_run::WIRED_MARKETS`) — never a hand copy that can rot.
fn wired_symbol(venue: &str) -> &'static str {
    vike_run::WIRED_MARKETS
        .iter()
        .find(|(v, _)| *v == venue)
        .map(|(_, symbol)| *symbol)
        .unwrap_or_else(|| panic!("build_node wires a {venue} market"))
}

/// Kills the child if an assertion unwinds, so a failing smoke never leaves a daemon (and its
/// live venue sockets) running on the box.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The project root one case's daemon is started in: an OWNED temp directory with the EMPTY
/// `settings/` store already inside it, removed when the returned guard drops.
///
/// ⚠ Returns the `TempDir` GUARD, and the caller must HOLD it for as long as the child runs —
/// [`Daemon`] binds it in a field declared AFTER `child`, so the reaper kills the daemon before
/// the directory it is running in goes away.
///
/// This used to be `env::temp_dir().join(format!("vike_tradehub_{tag}_{pid}_{nanos}"))`, which
/// satisfies `crates/vike-ops/tests/temp_path_gate.rs` (the name is not fixed) and was still wrong
/// twice over — the two defects `crates/vike-tradehub/src/config.rs`'s `own_script` records, with
/// this helper as the worst live producer of them. MEASURED on the CI box, 2026-08-25:
///
/// * **Nothing ever deleted one.** `/tmp` held 44,840 leaked test directories of this shape, and
///   this helper alone was adding ~45 a day — one per venue case per run, forever.
/// * **A PID is REUSED**, and the CI box runs these tests as TWO users (`the CI user` for CI, `the operator`
///   for the verification lanes). When a pid collides with a directory the OTHER user made, the
///   `create_dir_all` SUCCEEDS (it is already there) and the write into it fails
///   `PermissionDenied` — a live, intermittent flake arriving through the very idiom
///   `temp_path_gate`'s own message offers as the remedy. Uniquifying on a pid prevents collision
///   WITHIN a run; it prevents nothing ACROSS users over time, and it leaks either way.
///
/// `tempfile` fixes both halves at once: unique by construction, and self-deleting even when an
/// assertion unwinds. The tag stays in the NAME via `tempfile::Builder::prefix`, so a directory
/// seen mid-run is still attributable to the case that owns it.
fn temp_dir(tag: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("vike_tradehub_{tag}_"))
        .tempdir()
        .expect("create the temp project root");
    std::fs::create_dir_all(dir.path().join("settings")).expect("create the temp settings dir");
    dir
}

/// The started daemon: the child, the project root it runs in, its stdout protocol lines,
/// everything it has logged to stderr, and the stdout lines already consumed (kept for failure
/// dumps).
struct Daemon {
    child: Reaper,
    /// The temp project root, held so its `Drop` removes the tree at the end of the case.
    /// ⚠ Declared AFTER `child` deliberately: fields drop in declaration order, so the reaper
    /// kills and reaps the daemon BEFORE the directory it was started in is removed — a running
    /// process's working directory cannot be deleted at all on Windows, and deleting it out from
    /// under a live daemon is a race on unix.
    _root: tempfile::TempDir,
    lines: Receiver<String>,
    seen: Vec<String>,
    stderr: Arc<Mutex<String>>,
}

impl Daemon {
    /// Start the shipped binary in LIVE mode over a single-mount `venue` `buy_hold` profile, with
    /// an EMPTY temp settings store (⇒ every venue's exec on the paper fallback — or, on a
    /// credentialed-data venue, the plan REFUSAL) and stdin piped open (the `shutdown` word is
    /// the teardown trigger). The removed-ceiling variables are cleared because a set one is a
    /// deliberate startup REFUSAL, and the log filters are cleared so the default `info` console
    /// level carries the arming disclosures these tests assert on.
    fn start(venue: &str, symbol: &str, tick_size: f64) -> Self {
        let root = temp_dir("feed_splice");
        let dir = root.path();
        let profile = dir.join("tradehub.toml");
        std::fs::write(
            &profile,
            format!(
                "venue = \"{venue}\"\n\
                 symbol = \"{symbol}\"\n\
                 interval = \"1m\"\n\
                 interval_ms = 60000\n\
                 tick_size = {tick_size}\n\
                 \n\
                 [strategy]\n\
                 name = \"buy_hold\"\n\
                 \n\
                 [strategy.params]\n\
                 size = 0.001\n\
                 \n\
                 [daemon]\n\
                 summary_ms = 1000\n\
                 shutdown_deadline_ms = 20000\n"
            ),
        )
        .expect("write the daemon profile");

        let mut child = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
            .arg("--config")
            .arg(&profile)
            .current_dir(dir)
            .stdin(Stdio::piped()) // held open; the `shutdown` word drives the teardown
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("VIKE_TRADEHUB_LIVE", "1")
            .env("VIKE_SETTINGS_DIR", dir.join("settings"))
            .env("VIKE_STATE_ROOT", dir.join("state"))
            .env("VIKE_LOG_DIR", dir.join("logs"))
            .env("VIKE_LOG_FILE_LEVEL", "off")
            .env_remove("VIKE_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_ADDR")
            .env_remove("VIKE_TRADEHUB_CONTROL")
            .env_remove("VIKE_RUN_PROFILE")
            .env_remove("VIKE_RECONCILE")
            .env_remove("RUST_LOG")
            .env_remove("VIKE_LOG")
            // Determinism over an inherited shell: the network verdicts are read off the REAL
            // process env (`cex_mainnet_enabled` and the hyperliquid plan arm both read process
            // first), and polymarket's exec/egress gates are process-env flags too — a value
            // leaking in from the runner's shell must not flip a smoke's network or arm anything.
            .env_remove("BINANCE_MAINNET")
            .env_remove("BYBIT_MAINNET")
            .env_remove("OKX_MAINNET")
            .env_remove("HYPERLIQUID_MAINNET")
            .env_remove("POLY_EXEC")
            .env_remove("POLY_RECONCILE")
            .env_remove("POLY_WS_PROXY_ENABLED")
            // ...and polymarket's egress must be PINNED direct, not merely un-configured: the
            // resolution DEFAULTS to the arbdub SOCKS tunnel at 127.0.0.1:1080 when nothing is
            // set (`crates/bridges/polymarket/src/egress.rs`'s `proxy_url_with`, default-ON
            // because the venue is US-geo-blocked), and a test box runs no tunnel — measured on
            // the CI box as every daemon-side CLOB dial dying with "Connection refused" while the
            // mount looked healthy. `"direct"` short-circuits the whole resolution (both lanes:
            // the WS gate can only ever use the same endpoint). Inert for every other venue;
            // the polymarket case's doc carries the run-from-a-non-US-box consequence.
            .env("POLY_SOCKS_PROXY", "direct")
            // A set removed variable is a deliberate startup REFUSAL (`vike_config::REMOVED_ENV`),
            // and the refusal cases below must fail on the CREDENTIAL refusal, never a stale one.
            .env_remove("VIKE_SECRETS_PASSPHRASE")
            .spawn()
            .expect("spawn vike-tradehub");

        // Both pipes drained on their own threads — an undrained pipe fills its kernel buffer and
        // the daemon then blocks writing to it, which would look exactly like a dead feed.
        let (tx, lines) = mpsc::channel();
        let stdout = child.stdout.take().expect("piped stdout");
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr);
        let err_pipe = child.stderr.take().expect("piped stderr");
        std::thread::spawn(move || {
            for line in BufReader::new(err_pipe).lines().map_while(Result::ok) {
                let mut buf = sink.lock().expect("stderr buffer");
                buf.push_str(&line);
                buf.push('\n');
            }
        });

        Daemon { child: Reaper(child), _root: root, lines, seen: Vec::new(), stderr }
    }

    fn log(&self) -> String {
        self.stderr.lock().expect("stderr buffer").clone()
    }

    fn dump(&self) -> String {
        format!("stdout so far:\n{}\nstderr so far:\n{}", self.seen.join("\n"), self.log())
    }

    /// Consume stdout lines until `hit` returns true for one, or `patience` runs out — a dead
    /// daemon closes the pipe, so this never hangs past the deadline.
    fn wait_stdout(&mut self, patience: Duration, what: &str, mut hit: impl FnMut(&str) -> bool) {
        let deadline = Instant::now() + patience;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(line) => {
                    let found = hit(&line);
                    self.seen.push(line);
                    if found {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!("no {what} within {patience:?}; {}", self.dump())
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("the daemon died before {what}; {}", self.dump())
                }
            }
        }
    }

    /// Wait until the accumulated stderr contains `needle` — the arming disclosures ride the
    /// tracing console layer, not the stdout protocol.
    fn wait_stderr_contains(&self, patience: Duration, needle: &str) {
        let deadline = Instant::now() + patience;
        while !self.log().contains(needle) {
            assert!(
                Instant::now() < deadline,
                "stderr never contained {needle:?} within {patience:?}; {}",
                self.dump()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Poll until the daemon exits (`why` names what the exit is expected FOR in the failure
    /// message), or fail with the full output dump.
    fn wait_exit(&mut self, patience: Duration, why: &str) -> std::process::ExitStatus {
        let deadline = Instant::now() + patience;
        loop {
            match self.child.0.try_wait().expect("try_wait") {
                Some(status) => return status,
                None if Instant::now() >= deadline => {
                    panic!("the daemon was still running {patience:?} {why}; {}", self.dump())
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    /// Pull whatever stdout lines remain into `seen`. Terminates: the reader thread drops its
    /// sender once the (exited) daemon's pipe closes, so the loop ends on `Disconnected` — the
    /// timeout only bounds a slow pipe drain on a loaded box.
    fn drain_stdout(&mut self) {
        while let Ok(line) = self.lines.recv_timeout(DRAIN_PATIENCE) {
            self.seen.push(line);
        }
    }

    /// Bounded teardown through the stdio control word: every feed socket stops + joins inside
    /// the profile deadline and the process exits cleanly.
    fn shutdown_cleanly(mut self) {
        {
            let stdin = self.child.0.stdin.as_mut().expect("piped stdin");
            stdin.write_all(b"shutdown\n").expect("write the shutdown word");
            stdin.flush().expect("flush stdin");
        }
        let status = self.wait_exit(EXIT_PATIENCE, "after the shutdown word");
        assert!(status.success(), "the daemon must exit cleanly after `shutdown`, got {status:?}");
    }
}

/// The whole splice for one KEYLESS-DATA venue, one pass — the module doc's chain with `venue`'s
/// own `wire_venue_feeds` arm in the middle. `disclosures` are the stderr facts that must land
/// before the splice is trusted: the arm's own feed-subscribed words plus this mount's
/// paper-exec proof (each test's doc says which words, and why those).
fn prove_live_splice(venue: &str, symbol: &str, tick_size: f64, disclosures: &[&str]) {
    let mut daemon = Daemon::start(venue, symbol, tick_size);

    // 1. THE READY BANNER — and what it must say is what ARMED, which over this harness's EMPTY
    //    store is nothing at all.
    //
    // ⚠ THIS ASSERTION IS INVERTED FROM WHAT IT WAS, AND THE INVERSION IS THE POINT. It used to
    // demand `LIVE (venue={venue})` — the venue this smoke's PROFILE mounts — while step 2 below
    // demanded `live_venues={}` from the SAME startup. Those are contradictory statements about one
    // process: the first says this venue trades live, the second is `build_node`'s own record that
    // no venue got a live exec client. Pinning both as a contract is what kept the banner printing
    // a set the mount had never armed, and made a green test the evidence that it was correct. The
    // shape reached production: the CI box's daemon logged a NINE-venue `live_venues` record under
    // `"mode":"LIVE (venue=bybit)"`, its profile's one venue.
    //
    // So the two must now AGREE, and the agreement is asserted in both directions: the banner names
    // the empty-set sentinel, and it does NOT name this case's venue. Nothing about the splice this
    // driver exists to prove is relaxed — steps 2-4 are untouched, and step 3 still requires a real
    // venue event to cross the whole wiring into the mounted strategy.
    let mut banner = String::new();
    daemon.wait_stdout(STARTUP_PATIENCE, "ready banner", |line| {
        let hit = line.contains("\"kind\":\"ready\"");
        if hit {
            banner = line.to_string();
        }
        hit
    });
    assert!(
        banner.contains("\"mode\":\"LIVE (venue=none)\""),
        "the live gate is ON over an EMPTY credential store, so nothing can have armed and the \
         banner must say so: {banner}"
    );
    assert!(
        !banner.contains(&format!("venue={venue}")),
        "the banner named this smoke's PROFILE venue ({venue}) — that is the set the profile \
         mounts, not the set that armed, and over an empty store nothing armed: {banner}"
    );

    // ...and the banner's claim must match `build_node`'s OWN record from the same startup. This is
    // the cross-check the old contract made impossible: one authority on stdout, one on stderr,
    // asserted to agree rather than each asserted alone.
    daemon.wait_stderr_contains(STARTUP_PATIENCE, "live_venues={}");

    // 2. The feed arm ran and the exec seam is disclosed PAPER — the assertion that turns a
    //    credential leak into a loud failure instead of a trade. Every needle is the arm's own
    //    wording (or `build_node`'s own `live_venues` record, rendered on the same log line — the
    //    cases that name it are repeating the driver's universal assert above as their own
    //    documented paper proof, which is why their docs still cite it).
    for needle in disclosures {
        daemon.wait_stderr_contains(STARTUP_PATIENCE, needle);
    }

    // 3. THE SPLICE: a live venue event reaches the core and the mounted strategy acts.
    //    `buy_hold` submits exactly once, on the first quote or bar close its mount receives —
    //    and this daemon has no other feed and no other order source (no operator command is
    //    sent, reconcile is off), so `orders >= 1` in the summary is equivalent to "a live venue
    //    feed event arrived at the core through the daemon's own wiring, labelled for this
    //    mount's dispatch key". A REST warmup cannot fake it: the runtime's `BarSeed` arm
    //    stores history and drives no strategy — only a live-lane event does.
    daemon.wait_stdout(
        EVENT_PATIENCE,
        "summary with the strategy's order (a live venue event reached the core)",
        |line| {
            if !line.contains("\"kind\":\"summary\"") {
                return false;
            }
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => return false,
            };
            let orders = v["orders"].as_u64().unwrap_or(0);
            let positions = v["positions"].as_u64().unwrap_or(0);
            orders >= 1 || positions >= 1
        },
    );

    // 4. Bounded teardown through the stdio control word.
    daemon.shutdown_cleanly();
}

/// DERIBIT — the first venue proven (PR #1432), unchanged in substance: KEYLESS public MAINNET
/// JSON-RPC on the widest arm this daemon mounts (bars + quotes + trades + the lossless L2 book —
/// `crates/vike-tradehub/src/tradehub_cli.rs`'s `deribit_arming` states the lane set). tick_size 0.5 is
/// the venue's own BTC-PERPETUAL grid. The paper proof is the arm's own words — this arm
/// discloses "EXEC IS PAPER" explicitly, and deribit's authed sockets are hardcoded testnet
/// besides.
#[test]
#[ignore = "live smoke: the shipped daemon dials real Deribit MAINNET public market data \
            (keyless; exec stays paper over an empty credential store); run explicitly with \
            --ignored"]
fn a_live_deribit_event_reaches_the_core_through_the_daemons_own_feed_wiring() {
    prove_live_splice(
        "deribit",
        wired_symbol("deribit"),
        0.5,
        &["deribit feed subscribed (keyless MAINNET bars", "EXEC IS PAPER for deribit"],
    );
}

/// HYPERLIQUID — the daemon's original live venue, and the second keyless-data splice.
///
/// - Lanes: the HL arm in `wire_venue_feeds` subscribes bars + quotes only (no book pump — the
///   feed is `vike_hyperliquid::market_feed::Feeds` alone), so the trigger is the venue's L1
///   quote with the 1m close as the quiet-book fallback — exactly `buy_hold`'s two verbs.
/// - Network: with `HYPERLIQUID_MAINNET` removed from the child env, `venue_feed_plan`'s HL arm
///   resolves TESTNET (`vike_bridge_core::mainnet::mainnet_for`'s default), so the case pins the
///   DEFAULT shape — `network=Testnet` on the arm's own log line — rather than arming the
///   mainnet flag to chase a busier book.
/// - Paper proof: this arm's disclosure carries no "EXEC IS PAPER" phrase (only the CEX-shaped
///   arms say it in words), so the proof is `live_venues={}` on the same line —
///   `vike_run::build_node`'s OWN record that no venue got a live client.
/// - tick_size only passes validation here: the HL arm sizes no book grid from it.
#[test]
#[ignore = "live smoke: the shipped daemon dials real Hyperliquid TESTNET public market data \
            (keyless; exec stays paper over an empty credential store); run explicitly with \
            --ignored"]
fn a_live_hyperliquid_event_reaches_the_core_through_the_daemons_own_feed_wiring() {
    prove_live_splice(
        "hyperliquid",
        wired_symbol("hyperliquid"),
        1.0,
        &["Hyperliquid feed subscribed (bars + quotes)", "network=Testnet", "live_venues={}"],
    );
}

/// BINANCE — the first CEX-arm splice: the kline lane (`vike_binance::market_feed::Feeds`) plus
/// the SPOT `@bookTicker`+`@trade`+`@depth` pump (`spawn_binance_market_data`), both keyless
/// public MAINNET (the venue's demo tier serves no public market WS — `venue_feed_plan`'s CEX
/// block records the demo-exec-over-mainnet-prices shape).
///
/// - Paper proof: the CEX arm says it in words, with the tier `make_engine` would actually
///   consult — `BINANCE_MAINNET` is removed from the child env, so `cex_arming` resolves DEMO
///   and the disclosure reads "no DEMO credentials".
/// - tick_size 0.01 is the venue's BTCUSDT spot grid; it sizes the pump's `L2Book` and the
///   maker grid alike (`cex_plan`'s tick refusal is about exactly this number).
/// - ⚠ Geography can refuse this venue at the network level (the FUTURES WS is known silently
///   blocked from DE boxes; this mount's pump is SPOT, which has been reachable). A step-3
///   timeout under a healthy subscribe disclosure is evidence about the route, not the splice —
///   record it, never fake it.
#[test]
#[ignore = "live smoke: the shipped daemon dials real Binance MAINNET public spot market data \
            (keyless; exec stays paper over an empty credential store); run explicitly with \
            --ignored"]
fn a_live_binance_event_reaches_the_core_through_the_daemons_own_feed_wiring() {
    prove_live_splice(
        "binance",
        wired_symbol("binance"),
        0.01,
        &[
            "binance feed subscribed (klines + quote/trade/book",
            "EXEC IS PAPER for binance: no DEMO credentials",
            "live_venues={}",
        ],
    );
}

/// BYBIT — the second CEX-arm splice. ⚠ The two lanes deliberately reach DIFFERENT instruments
/// under one `BTCUSDT` string (`venue_feed_plan`'s bybit arm carries the whole argument): the
/// pump (`spawn_bybit_market_data`) connects the LINEAR PERP public WS — matching the perp
/// engine `build_node` mounts — while the kline lane's `perp_split` maps the suffix-less symbol
/// to SPOT klines, whose close occupies only `PriceBoard::resolve`'s last rung. Lane-agnostic as
/// ever, the case is satisfied by whichever lane delivers first (in practice the perp L1 quote).
///
/// - Paper proof: the CEX arm's own words at the DEMO tier (`BYBIT_MAINNET` removed from the
///   child env), plus `live_venues={}`.
/// - tick_size 0.1 is the venue's BTCUSDT linear-perp grid.
#[test]
#[ignore = "live smoke: the shipped daemon dials real Bybit MAINNET public linear-perp market \
            data (keyless; exec stays paper over an empty credential store); run explicitly \
            with --ignored"]
fn a_live_bybit_event_reaches_the_core_through_the_daemons_own_feed_wiring() {
    prove_live_splice(
        "bybit",
        wired_symbol("bybit"),
        0.1,
        &[
            "bybit feed subscribed (klines + quote/trade/book",
            "EXEC IS PAPER for bybit: no DEMO credentials",
            "live_venues={}",
        ],
    );
}

/// OKX — the third CEX-arm splice: the SWAP instrument (`BTC-USDT-SWAP`), one symbol on both
/// lanes (klines + the `bbo-tbt`/`trades`/`books5` pump `spawn_okx_market_data` subscribes),
/// keyless public MAINNET.
///
/// - Paper proof: the CEX arm's own words at the DEMO tier (`OKX_MAINNET` removed from the
///   child env), plus `live_venues={}`.
/// - tick_size 0.1 is the venue's BTC-USDT-SWAP price grid. (The venue's CONTRACT-vs-BASE qty
///   split lives on the exec side and cannot matter here: exec is the paper book.)
#[test]
#[ignore = "live smoke: the shipped daemon dials real OKX MAINNET public swap market data \
            (keyless; exec stays paper over an empty credential store); run explicitly with \
            --ignored"]
fn a_live_okx_event_reaches_the_core_through_the_daemons_own_feed_wiring() {
    prove_live_splice(
        "okx",
        wired_symbol("okx"),
        0.1,
        &[
            "okx feed subscribed (klines + quote/trade/book",
            "EXEC IS PAPER for okx: no DEMO credentials",
            "live_venues={}",
        ],
    );
}

/// ASTER — the fourth CEX-arm splice, the binance fork on the `.P` perp spelling
/// (`BTCUSDT.P`; both lanes split it to the exchange's wire symbol while every emission keeps
/// the mounted spelling `accepts_symbol` dispatches on — `vike_aster::market_data`'s
/// `pump_wire`).
///
/// - Data plane: keyless MAINNET reads BY CHOICE (`wire_venue_feeds` pins `Environment::Live`
///   on both lanes; a testnet exists, and the arm's comment carries why it is not used).
/// - ⚠ The account-safety rule for this venue is a CREDENTIAL rule (root CLAUDE.md's aster
///   paragraph: exclude aster from validation runs by WITHHOLDING its creds), and this case IS
///   that rule: the EMPTY store is what makes an authenticated mainnet call impossible — the
///   run touches only the public market stream, and exec discloses paper at the TESTNET tier
///   (aster has no `{VENUE}_MAINNET` flag; absent LIVE creds resolve the testnet tier, so the
///   disclosure reads "no TESTNET credentials").
/// - tick_size 0.1 is the venue's BTCUSDT perp grid (the binance-futures fork's tick).
#[test]
#[ignore = "live smoke: the shipped daemon dials real Aster MAINNET public perp market data \
            (keyless reads; exec stays paper over an empty credential store — no ASTER_* key \
            exists in it, so no authenticated call is possible); run explicitly with --ignored"]
fn a_live_aster_event_reaches_the_core_through_the_daemons_own_feed_wiring() {
    prove_live_splice(
        "aster",
        wired_symbol("aster"),
        0.1,
        &[
            "aster feed subscribed (klines + quote/trade/book",
            "EXEC IS PAPER for aster: no TESTNET credentials",
            "live_venues={}",
        ],
    );
}

/// Resolve one QUOTED outcome token + its venue tick off the public CLOB, dialling DIRECT — the
/// `discover_token` shape `crates/bridges/polymarket/tests/poly_exec_mount_smoke.rs` uses:
/// `/sampling-markets` (the liquidity-rewards program — actively quoted books by construction),
/// then a `/midpoint` probe per candidate accepting a mid away from the pinned extremes. A token
/// id baked into this file would be stale within the minute — `vike_run::WIRED_MARKETS`'
/// polymarket row is EMPTY for exactly that reason (the mount is account-wide; only a profile
/// names a token) — and the plain `/markets` page-1 pick was MEASURED insufficient on the CI box:
/// its first market was a dead book (the same token on both runs), the subscribe succeeded, and
/// no event ever derived a quote. A midpoint near 0 or 1 is treated as dead too: a settled
/// market keeps a one-sided book pinned at the extreme, and this smoke needs a book that will
/// actually derive an L1 quote.
#[cfg(feature = "polymarket")]
fn resolve_live_polymarket_market() -> (String, f64) {
    use vike_bridge_core::transport::RestTransport;
    let transport = vike_bridge_core::transport::UreqTransport::new("polymarket");
    let page = transport
        .public(vike_polymarket::CLOB_BASE, "/sampling-markets", &[])
        .expect("GET /sampling-markets");
    let markets = vike_polymarket::parse_markets(&page);
    let market = markets
        .iter()
        .filter(|m| !m.tokens.is_empty())
        .find(|m| {
            transport
                .public(
                    vike_polymarket::CLOB_BASE,
                    "/midpoint",
                    &[("token_id", m.tokens[0].token_id.clone())],
                )
                .ok()
                .and_then(|v| v.get("mid")?.as_str()?.parse::<f64>().ok())
                .is_some_and(|mid| (0.05..=0.95).contains(&mid))
        })
        .expect("a sampling market whose first token has a mid-range midpoint");
    let tick = if market.tick_size.is_finite() && market.tick_size > 0.0 {
        market.tick_size
    } else {
        0.001
    };
    (market.tokens[0].token_id.clone(), tick)
}

/// POLYMARKET — the feature-gated splice (this crate's `polymarket` feature; the nested
/// polymarket CI lane is what compiles this case). Not the CEX shape and not the HL shape: ONE
/// `subscribe_book` drives everything through `vike_run::MakerSink` — the book-derived L1 quote
/// onto the tick lane (the trigger here: the CLOB WS answers a subscribe with a book snapshot,
/// so the first quote derives within seconds) and synth bars from its `TickBarSynthesizer` as
/// the fallback cadence.
///
/// - Paper proof: `live_venues={}` (this arm's disclosure carries no per-state exec phrase —
///   its static tail names the full gate stack real orders would additionally need:
///   POLY_EXEC=1 + POLY_PRIVATE_KEY + the Dublin egress; all three are absent here, and
///   `POLY_EXEC` is explicitly removed from the child env).
/// - The symbol is a LIVE token resolved at run time ([`resolve_live_polymarket_market`]), so
///   the case cannot rot with a market's resolution.
/// - ⚠ Geography and the ROUTE: Polymarket is US-geo-blocked and the venue's egress therefore
///   DEFAULTS to the arbdub SOCKS tunnel (`crates/bridges/polymarket/src/egress.rs`'s
///   `proxy_url_with` — default ON at 127.0.0.1:1080 with nothing set). A test lane runs no
///   tunnel, and the first the CI box run proved the failure shape: a healthy-looking mount whose
///   every daemon-side CLOB dial dies with "Connection refused". The harness pins
///   `POLY_SOCKS_PROXY=direct`, so this smoke tests the VENUE, not the route — run it from a
///   box the venue's WS actually serves; a US box needs the Dublin route, which this case
///   deliberately does not arm.
///
/// ⚠ RUN RECORD 2026-08-22 (the CI box, direct egress): NOT yet proven — the venue's market WS
/// delivered NOTHING from that box while its REST answered fine. The daemon run showed the
/// full healthy shape (ready banner, feed disclosure, `live_venues={}`) and then a frozen
/// snapshot seq with `orders:0` for the whole 300s window; the failure was isolated to the
/// ROUTE, not this wiring, by running the bridge's own proven book smoke
/// (`crates/bridges/polymarket/tests/market_ticks_live_smoke.rs`'s
/// `market_ws_delivers_book_and_quote`) on the same box with the same direct egress — it
/// failed identically: "no book+quote after 30s (books=0, quotes=0) — token quiet or WS
/// down?" on a sampling-market token with a mid-range midpoint. With the Dublin tunnel
/// parked, no box we run today reaches the CLOB WS, so this case awaits a working route; a
/// green run of the bridge smoke from a box is the precondition for expecting this one green
/// there.
#[cfg(feature = "polymarket")]
#[test]
#[ignore = "live smoke: the shipped daemon dials the real Polymarket CLOB public market channel \
            (keyless; exec stays paper over an empty credential store — POLY_EXEC unset besides); \
            run explicitly with --ignored, from a non-US box"]
fn a_live_polymarket_event_reaches_the_core_through_the_daemons_own_feed_wiring() {
    let (token, tick) = resolve_live_polymarket_market();
    prove_live_splice(
        "polymarket",
        &token,
        tick,
        &["Polymarket feed subscribed (book → derived L1 quote)", "live_venues={}"],
    );
}

/// **THE READY BANNER, BLACK-BOX, OVER THE SHIPPED BINARY — the one CI-gated proof that the string
/// `main` prints is the ARMING RECORD and not the profile's mount set.**
///
/// Everything else that gates this property gates a piece of it: `main.rs`'s `ready_mode_line` unit
/// tests gate the RENDERING, and `feed_splice_seam_tests`'s
/// `the_ready_banner_names_the_arming_record_not_the_venue_the_profile_mounts` gates that a REAL
/// mount's record differs from the profile's set and that the renderer follows the record. Neither
/// can reach `main`'s own binding of the two, and that binding is exactly where the defect lived —
/// `live_mount`'s third return was discarded and the banner was built from `mount_venues`. So this
/// runs the real daemon and reads its real stdout.
///
/// The profile names ONE venue (hyperliquid). The store is EMPTY, so nothing arms, and the two sets
/// therefore differ: a banner built from the profile reads `LIVE (venue=hyperliquid)` — which is
/// what the pre-fix binary printed here, beside its own `live_venues={}` — and a banner built from
/// the record reads `LIVE (venue=none)`. Both directions are asserted, because "does not name
/// hyperliquid" alone would also pass on a banner that named some other venue.
///
/// # Why this one is NOT `#[ignore]`d while every splice case above is
///
/// The splice cases wait for a live venue EVENT and are therefore network-bound. This case waits
/// only for a startup line: a keyless subscribe spawns threads and returns (this file's
/// `STARTUP_PATIENCE` doc states it — the network work happens inside each lane's own thread, the
/// deribit REST warmup included), so the banner prints whether or not the dial ever succeeds. The
/// daemon's feed threads DO attempt a hyperliquid TESTNET public dial as a side effect; nothing here
/// waits on it or asserts anything about it, and `HYPERLIQUID_MAINNET` is removed from the child env
/// by the harness, so no mainnet host and no account is involved.
///
/// It is torn down by the [`Reaper`] rather than through the `shutdown` word ON PURPOSE: a clean
/// teardown joins the feed sockets, which on a box with no route puts a bounded-deadline exit code
/// in the path of an assertion about a startup string. The bounded teardown has its own coverage
/// (`sigterm_stop`, and the splice cases' `shutdown_cleanly`); this case must not depend on it.
#[test]
fn the_ready_banner_names_what_armed_not_the_venue_the_profile_mounts() {
    // The PRECONDITION is keyed on the PROFILE, not on the banner under test: the whole premise is
    // that the profile names a real venue which the banner must then decline to name. If this venue
    // ever stopped being live-wired, `wired_symbol` panics here rather than letting the case pass
    // because the string it was hunting for happened to be absent.
    let venue = "hyperliquid";
    let mut daemon = Daemon::start(venue, wired_symbol(venue), 1.0);

    let mut banner = String::new();
    daemon.wait_stdout(STARTUP_PATIENCE, "ready banner", |line| {
        let hit = line.contains("\"kind\":\"ready\"");
        if hit {
            banner = line.to_string();
        }
        hit
    });

    assert!(
        !banner.contains(&format!("venue={venue}")),
        "the ready banner named the venue the PROFILE mounts. That is the defect this test exists \
         for: over an EMPTY credential store nothing can have armed, and the banner is supposed to \
         name what armed. Banner: {banner}"
    );
    assert!(
        banner.contains("\"mode\":\"LIVE (venue=none)\""),
        "the live gate is ON and the store is EMPTY, so the banner must say exactly that: {banner}"
    );

    // ...and the banner must AGREE with `build_node`'s own record from this same startup. Two
    // authorities, two streams, asserted against each other — the cross-check whose absence let one
    // of them print a nine-venue process as a one-venue one on the CI box.
    daemon.wait_stderr_contains(STARTUP_PATIENCE, "live_venues={}");
}

/// The credentialed-data venues' end-to-end case: an EMPTY store REFUSES the live mount at
/// `venue_feed_plan`, surfaced by `main` as a startup FAILURE — before the ready banner, before
/// any core thread or venue socket. Deterministic and network-free, so the cases built on this
/// are NOT `#[ignore]`d: they gate in CI (the module doc's Venue coverage section carries why
/// the refusal, and not the splice, is what this file can prove for these venues).
///
/// `needles` are the venue's own `*_plan` refusal words: the gate must name the venue's
/// credential story AND the exact keys an operator would add. The no-ready assertion is what
/// separates "refused at the gate" from every softer failure — in particular ctrader's
/// demote-to-paper (`ctrader_arming`'s remedy: an exec connect failure AFTER a credentialed
/// mount wears "EXEC IS PAPER … restart the daemon", and the daemon keeps running); a plan
/// refusal EXITS, having mounted nothing.
fn prove_credentialed_refusal(venue: &str, tick_size: f64, needles: &[&str]) {
    let mut daemon = Daemon::start(venue, wired_symbol(venue), tick_size);

    // The refusal exits the daemon — `main` surfaces `live_mount`'s error and returns FAILURE.
    let status =
        daemon.wait_exit(STARTUP_PATIENCE, "waiting for the empty store's credential refusal");
    assert!(
        !status.success(),
        "an empty store must REFUSE a live {venue} mount (credentialed data plane), got \
         {status:?}; {}",
        daemon.dump()
    );

    // ...in `live_mount`'s framing and the venue plan's own words, naming the exact keys.
    daemon.wait_stderr_contains(DRAIN_PATIENCE, "live mount failed");
    for needle in needles {
        daemon.wait_stderr_contains(DRAIN_PATIENCE, needle);
    }

    // ...and it happened at the PLAN: the mount never came up, so the ready banner never printed
    // (and with it, no summary and no order path of any kind).
    daemon.drain_stdout();
    assert!(
        !daemon.seen.iter().any(|l| l.contains("\"kind\":\"ready\"")),
        "a refused {venue} mount must never print the ready banner; {}",
        daemon.dump()
    );
}

/// ALPACA — credentialed-data venue: the REFUSAL case, not the splice. The SANDBOX OAuth2 trio
/// that would let the data WS mount is the SAME trio `vike_mount::make_engine`'s alpaca arm arms
/// SANDBOX exec with (`alpaca_plan`'s doc: same loader, same tier, same map — never a second env
/// read), so a data-credentialed run of THIS default-profile smoke cannot keep the
/// exec-stays-paper property (a `data_only = true` profile can — the module doc's Venue coverage
/// section names the seam and its deterministic oanda proof). The
/// empty store must refuse per `alpaca_plan`, naming the trio. (Weekend note: moot here —
/// network-free — and the equities calendar would only ever gate a future splice case.)
#[test]
fn alpaca_live_mount_refuses_an_empty_store_naming_the_sandbox_trio() {
    prove_credentialed_refusal(
        "alpaca",
        0.01, // AAPL's cent grid — past the tick refusal so the CREDENTIAL refusal answers
        &["alpaca's market data is CREDENTIALED", "ALPACA_SANDBOX_CLIENT_ID"],
    );
}

/// CTRADER — credentialed-data venue: the REFUSAL case, not the splice (`ctrader_plan` resolves
/// `CtraderConfig::from_vars(Demo, …)` — the exec arm's own loader and tier, so on the default
/// profile data credentials arm demo exec; the `data_only` seam is the declared exception the
/// module doc's Venue coverage section scopes). The refusal must name both halves
/// of the venue's credential story: the Spotware app registration pair and the DEMO token tier.
///
/// This venue is also WHY the shared driver asserts no-ready rather than merely non-zero exit:
/// ctrader's exec connects SYNCHRONOUSLY inside `make_engine` and a connect/auth failure DEMOTES
/// the venue to paper for the session while the daemon keeps running (`ctrader_arming`'s
/// remedy). "Mounted and demoted" prints the ready banner and an "EXEC IS PAPER" disclosure;
/// "refused at the gate" exits before either. The two must never be conflated, and the driver's
/// assertion set distinguishes them structurally.
#[test]
fn ctrader_live_mount_refuses_an_empty_store_naming_the_demo_tokens() {
    prove_credentialed_refusal(
        "ctrader",
        0.00001, // EURUSD's pipette grid — past the tick refusal so the credential one answers
        &["ctrader's market data is CREDENTIALED", "CTRADER_DEMO_ACCESS_TOKEN"],
    );
}

/// OANDA — credentialed-data venue: the REFUSAL case, not the splice (`oanda_plan` resolves
/// `load_oanda_config_from(Demo, …)` — the exec arm's own loader and tier over the one map, so
/// on the default profile data credentials arm PRACTICE exec; both feed lanes are Bearer-authed
/// against the same account, there is no keyless oanda lane at all). This venue's SPLICE is the
/// one already proven deterministically through the `data_only` seam
/// (`crates/vike-tradehub/src/feed_splice_seam_tests.rs`); the refusal case here still gates the
/// empty-store half. The refusal must name the venue's pair. The
/// profile's `1m` maps in `vike_oanda::granularity`, so the credential refusal — not the
/// interval one — is what answers.
#[test]
fn oanda_live_mount_refuses_an_empty_store_naming_the_demo_pair() {
    prove_credentialed_refusal(
        "oanda",
        0.00001, // EURUSD's pipette grid — past the tick refusal so the credential one answers
        &["oanda's market data is CREDENTIALED", "OANDA_DEMO_API_KEY"],
    );
}

/// IG — credentialed-data venue: the REFUSAL case, not the splice (`ig_plan` resolves
/// `load_ig_config_from(Demo, …)` — the exec arm's own loader and tier; every Lightstreamer
/// subscription opens its own `IgSession` login, so there is no keyless IG lane). The refusal
/// must name the venue's trio. The profile's `1m` maps in `vike_ig::market_data::ig_scale`
/// (the deliberately-narrow STREAMING ladder), so the credential refusal — not the interval one
/// — is what answers. (Weekend note: moot here — network-free; the FX calendar would only ever
/// gate a future splice case's quote lane.)
#[test]
fn ig_live_mount_refuses_an_empty_store_naming_the_demo_trio() {
    prove_credentialed_refusal(
        "ig",
        0.00001, // the epic's pip-fraction grid — past the tick refusal so the credential one answers
        &["ig's market data is CREDENTIALED", "IG_DEMO_API_KEY"],
    );
}
