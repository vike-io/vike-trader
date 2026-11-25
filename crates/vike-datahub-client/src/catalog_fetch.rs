//! `catalog_fetch` — **ask the backend's datahub for one venue's instrument list over the OBSERVE
//! key, and when it will not answer, say why in a sentence rather than in an empty list.** The
//! client of [`Request::VenueCatalog`](crate::proto::Request::VenueCatalog): one blocking call,
//! five refusal shapes, and one sentence each. Its callers are the Options poll's index
//! (`crate::tool_feeds::poll`) and `vike-app-core`'s catalog refresh, whose module
//! `data::catalog_refresh::server_route` keeps the refresh's own verbs.
//!
//! # ⚠ The refusals are the point, and none of them is an error
//!
//! Four different facts about the world come back as a SUCCESSFUL response, and a client that
//! folded any of them into "the fetch failed" would tell an operator to retry something that
//! cannot change:
//!
//! | what came back | the fact | who can act |
//! |---|---|---|
//! | `CatalogOutcome::NotArmed` | the server's operator REFUSED the lane (it is ON by default since `docs/decisions/0066`) | the server's operator (`vike-cli config set flags.venue_catalog_off false` + restart) |
//! | `Refused(NotServed)` | this server's table has no provider for the venue | whoever builds that table |
//! | `Refused(NeedsCredentials)` | listing spends the operator's identity (0062 decision 3) | **nobody** |
//! | `Refused(NoBulkList)` | the VENUE publishes no list | **nobody** |
//!
//! ...plus one refused CLIENT-side before a frame is written: a server that does not advertise
//! [`FEATURE_VENUE_CATALOG`]. That is [`CatalogFetchReport::ServerUnsupported`], and it exists so
//! an un-advertised capability can never render as an empty instrument list — which for `ig` and
//! `ibkr` would be indistinguishable from the truth.
//!
//! **The sentences are the SERVER's**, not ours: every refusal renders through
//! [`CatalogListing::describe`], so the daemon, the CLI and the desktop cannot describe one refusal
//! three different ways.
//!
//! # ⚠ What this module does NOT do: retry, poll, or batch
//!
//! `crates/vike-datahub/src/catalog.rs` gates a request in five steps and the BUCKET is the fifth —
//! arming, the venue's nature and the table are all consulted before it, and the TTL memo is
//! checked before the bucket inside `CatalogLane::admit`. So **every refusal above spends no
//! token**, and the one path that does is a provider FAILURE mid-fetch (the lane deliberately
//! memoizes only successes: caching a blip would turn a venue being briefly down into six hours of
//! refusal).
//!
//! That is why there is no retry loop here, and why the ONE caller that asks without a press is
//! bounded below that lane's refill: the Options poll thread (the Options half of
//! `crate::tool_feeds::poll::DatahubPoller`'s `run_once`) asks for `deribit` alone, when
//! `vike_options::index_refresh_due` says so — every [`crate::catalog::CATALOG_VENUE_REFILL`] while its
//! index is EMPTY (the refill itself, passed as the retry, so even a run of failures cannot drain the
//! burst) and at most every ten minutes once it holds one, when the grid shows a name it lacks (inside
//! the server's TTL that is a memo hit and spends nothing). `vike-app-core`'s `REFRESH_COOLDOWN_MS` is
//! that same constant, so a human pressing as fast as the button allows tracks the refill rate and can
//! never drain the two-token burst.
//! `vike-app-core`'s `DatahubCatalogRefresh::spawn_initial` stays LOCAL-ONLY for the same reason — a
//! startup that routed eight cold venues to a server would be precisely the poll this paragraph
//! forbids, wearing a first run.

use vike_hub_protocol::auth::Scope;
use vike_model::Instrument;

use crate::catalog::{CatalogListing, CatalogOutcome};
use crate::dial::{DatahubDial, NO_BACKEND_NOTE};
use crate::{DatahubClient, FEATURE_VENUE_CATALOG};

/// How one `ServerBacked` fetch ended. A plain value, so every sentence below is unit-testable
/// without a socket — `vike-app-core`'s `ChartSeedReport` shape, deliberately.
///
/// ⚠ `PartialEq` but NOT `Eq`, and it is forced rather than chosen: [`Instrument`] carries a
/// `vike_model::SymbolProperties` whose tick/lot fields are `f64`, so nothing reachable from a
/// listing can be `Eq`. The same note [`CatalogOutcome`] carries.
#[derive(Clone, Debug, PartialEq)]
pub enum CatalogFetchReport {
    /// No datahub address was resolved at all, so nothing was dialled. Not a failure — it is the
    /// ordinary state of a desktop with no backend connected.
    NoBackend,
    /// The TCP dial (or the handshake riding it) failed; nothing was sent.
    ConnectFailed { addr: String, error: String },
    /// The server advertises no [`FEATURE_VENUE_CATALOG`]. Refused CLIENT-side, before a frame.
    ServerUnsupported { addr: String, features: Vec<String> },
    /// The venue answered. `truncated` is the server's own flag and is carried rather than
    /// dropped — a picker silently missing a venue's tail is a bug report nobody can reproduce.
    Listed { instruments: Vec<Instrument>, truncated: bool, cached: bool },
    /// A SUCCESSFUL response that fetched nothing, for a reason that is not a failure. `why` is
    /// the SERVER-side type's own sentence, never one assembled here.
    Refused { why: String },
    /// The request was sent and the server answered with an error, or a provider failed mid-fetch.
    /// ⚠ **The one path a retry can cost the server a token** — see the module doc.
    Failed { addr: String, error: String },
}

impl CatalogFetchReport {
    /// Did this fetch produce a list the cache may adopt? Only [`Self::Listed`] can.
    #[must_use]
    pub fn listed(&self) -> bool {
        matches!(self, Self::Listed { .. })
    }
}

/// **Ask the datahub for one venue's instrument list, blocking** — call it from a worker or poll
/// thread, never the UI thread.
///
/// The capability check happens BEFORE a frame is written, and it happens twice over: once here so
/// the report can name the ADVERTISED SET (which [`DatahubClient::venue_catalog`]'s own `Err`
/// string cannot be parsed for) and once inside that method, which is the enforcement. Neither leg
/// re-spells the feature string — [`FEATURE_VENUE_CATALOG`] is the one constant both read, and
/// [`DatahubClient::serves_venue_catalog`] is the named predicate rather than a fourth inline
/// `features().iter().any(..)`.
///
/// The reply is bounded by [`CATALOG_REPLY_TIMEOUT`]: a peer that vanished mid-fetch ends the call
/// as [`CatalogFetchReport::Failed`] naming the timeout, instead of parking the worker (and, for
/// the Options tool's index, its poll thread) for good.
pub fn fetch_venue_catalog(dial: Option<&DatahubDial>, venue: &str) -> CatalogFetchReport {
    fetch_venue_catalog_within(dial, venue, CATALOG_REPLY_TIMEOUT)
}

/// How long ONE catalog fetch may go without a reply byte before it is reported as failed.
///
/// Shorter than the server's worst honest fetch on purpose, because clipping one costs NOTHING: a
/// cold fetch is a venue's REST calls (each capped by `vike_transport::http`'s 30 s timeout;
/// deribit is five of them, polymarket pages), and the server's lane memoizes a SUCCESS before it
/// writes the answer (`crates/vike-datahub/src/server/venue_catalog.rs`'s `venue_catalog_verb`), so
/// the press or poll after a clipped fetch is a memo hit that spends no token. Thirty seconds
/// covers a warm answer by orders of magnitude and a cold one-request venue whole; a longer bound
/// only lengthens how long a dead link holds the worker.
pub const CATALOG_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// [`fetch_venue_catalog`] with the reply bound as a PARAMETER — the seam the tests drive in
/// milliseconds; production passes [`CATALOG_REPLY_TIMEOUT`].
pub(crate) fn fetch_venue_catalog_within(
    dial: Option<&DatahubDial>,
    venue: &str,
    reply_timeout: std::time::Duration,
) -> CatalogFetchReport {
    let Some(dial) = dial else { return CatalogFetchReport::NoBackend };
    let dialled = DatahubClient::connect_with_keys(&dial.address, dial.keys.as_ref(), Scope::Read);
    let mut client = match dialled {
        Ok(mut c) => {
            c.set_reply_timeout(Some(reply_timeout));
            c
        }
        Err(e) => {
            return CatalogFetchReport::ConnectFailed {
                addr: dial.address.clone(),
                error: e.to_string(),
            };
        }
    };
    if !client.serves_venue_catalog() {
        return CatalogFetchReport::ServerUnsupported {
            addr: dial.address.clone(),
            features: client.features().to_vec(),
        };
    }
    match client.venue_catalog(venue) {
        Ok(listing) => match listing.outcome {
            CatalogOutcome::Listed { instruments, truncated, cached } => {
                CatalogFetchReport::Listed { instruments, truncated, cached }
            }
            other => CatalogFetchReport::Refused { why: describe_outcome(venue, &other) },
        },
        Err(error) => CatalogFetchReport::Failed { addr: dial.address.clone(), error },
    }
}

/// One outcome's operator sentence, rendered by the SERVER-side type rather than assembled here.
///
/// ⚠ The indirection is the property, not the convenience: [`CatalogListing::describe`] and
/// `CatalogRefusal::describe` live in this crate precisely so the daemon's log line, the CLI's
/// output and the desktop's row cannot describe one refusal three different ways. A `format!` in a
/// caller would be a fourth spelling nothing compares.
pub fn describe_outcome(venue: &str, outcome: &CatalogOutcome) -> String {
    CatalogListing { venue: venue.to_string(), outcome: outcome.clone() }.describe()
}

/// One [`CatalogFetchReport`] → the sentence a catalog row's Status cell (and the Options poll's
/// index note) carries.
///
/// ⚠ **`ServerUnsupported` is the whole point of this function**, exactly as it is in
/// `vike-app-core`'s `render_chart_seed_status`. Without it a server that will not answer and a
/// venue with no instruments produce the identical blank row, and the operator's next move is to
/// doubt the venue. The line therefore names the EXACT settings row and the box it is written on.
#[must_use]
pub fn render_catalog_fetch_status(report: &CatalogFetchReport) -> String {
    match report {
        CatalogFetchReport::NoBackend => NO_BACKEND_NOTE.to_string(),
        CatalogFetchReport::ConnectFailed { addr, error } => {
            format!("the datahub at {addr} could not be reached ({error}) — nothing was requested.")
        }
        CatalogFetchReport::ServerUnsupported { addr, features } => format!(
            "the datahub at {addr} will not list any venue: it does not advertise \
             \"{FEATURE_VENUE_CATALOG}\" (it advertised [{}]) — nothing was sent. The catalog is \
             ON by default, so either that server predates the verb or its operator refused it: \
             run `vike-cli config set flags.venue_catalog_off false` on that box and restart it.",
            features.join(", ")
        ),
        CatalogFetchReport::Listed { instruments, truncated, cached } => {
            let n = instruments.len();
            let src = if *cached { " (from that server's cache)" } else { "" };
            if *truncated {
                format!(
                    "{n} instruments{src} — TRUNCATED at the wire cap, so the tail of this venue's \
                     universe is missing."
                )
            } else {
                format!("{n} instruments{src}.")
            }
        }
        CatalogFetchReport::Refused { why } => why.clone(),
        CatalogFetchReport::Failed { addr, error } => {
            format!("the datahub at {addr} could not list it: {error}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::CatalogRefusal;

    #[test]
    fn a_server_that_does_not_advertise_is_told_apart_from_one_that_is_down() {
        let line = render_catalog_fetch_status(&CatalogFetchReport::ServerUnsupported {
            addr: "<host>:7878".to_string(),
            features: vec!["load_bars".to_string(), "seed_series".to_string()],
        });
        assert!(line.contains("<host>:7878"), "{line}");
        // ⚠ The named switch is the REFUSAL now, not the old arming: since
        // `docs/decisions/0066` every build ADVERTISES unless the refusal is written, so
        // non-advertisement is
        // evidence of a written refusal or an old server and of nothing else. This assertion used
        // to demand `VIKE_DATAHUB_VENUE_CATALOG=1` and a build feature, and both would now send an
        // operator to something that cannot be the cause.
        assert!(line.contains("venue_catalog_off"), "the switch is named: {line}");
        assert!(line.contains("restart"), "and the action: {line}");
        assert!(line.contains("ON by default"), "and which way the default points: {line}");
        assert!(line.contains("load_bars, seed_series"), "the advertised set is shown: {line}");
        assert!(line.contains("nothing was sent"), "{line}");

        let down = render_catalog_fetch_status(&CatalogFetchReport::ConnectFailed {
            addr: "<host>:7878".to_string(),
            error: "connection refused".to_string(),
        });
        assert!(down.contains("could not be reached"), "{down}");
        assert!(
            !down.contains("venue_catalog_off"),
            "a dead server is not a config problem: {down}"
        );
    }

    #[test]
    fn a_no_backend_report_sends_nothing_and_names_both_fixes() {
        assert_eq!(fetch_venue_catalog(None, "binance"), CatalogFetchReport::NoBackend);
        let line = render_catalog_fetch_status(&CatalogFetchReport::NoBackend);
        assert!(line.contains("config.datahub_addr"), "{line}");
        assert!(line.contains("connect a backend"), "{line}");
        assert!(!line.contains("0 instruments"), "an absent server is not an empty venue: {line}");
    }

    /// The four SUCCESSFUL non-listing answers all render the SERVER-side type's own words, and
    /// none of them reads as a count.
    #[test]
    fn every_refusal_renders_the_servers_own_sentence_and_never_a_count() {
        let unarmed = describe_outcome("okx", &CatalogOutcome::NotArmed);
        assert!(unarmed.contains("venue_catalog_off"), "{unarmed}");
        assert!(unarmed.contains("REFUSED"), "a refused server is not broken: {unarmed}");

        let served = describe_outcome(
            "dukascopy",
            &CatalogOutcome::Refused(CatalogRefusal::NotServed {
                supported: vec!["binance".into(), "okx".into()],
            }),
        );
        assert!(served.contains("binance, okx"), "the supported set is named: {served}");

        let creds =
            describe_outcome("alpaca", &CatalogOutcome::Refused(CatalogRefusal::NeedsCredentials));
        assert!(creds.contains("credentials"), "{creds}");
        // ⚠ This one survives the flip UNCHANGED and on purpose: no SERVER switch arms a
        // credentialed venue in either direction, so the sentence must name neither the old arming
        // nor the new refusal. `crates/vike-datahub-client/src/catalog.rs`'s
        // `a_credential_refusal_never_suggests_arming_anything` is the same fence at the source.
        assert!(!creds.contains("VIKE_DATAHUB_VENUE_CATALOG"), "no switch arms this: {creds}");
        assert!(!creds.contains("venue_catalog_off"), "and no refusal governs it either: {creds}");

        let none = describe_outcome(
            "ibkr",
            &CatalogOutcome::Refused(CatalogRefusal::NoBulkList {
                why: "contract-search shaped".into(),
            }),
        );
        assert!(none.contains("publishes no bulk instrument list"), "{none}");

        for line in [&unarmed, &served, &creds, &none] {
            assert!(
                !line.contains("0 instruments"),
                "a refusal must never read as a count: {line}"
            );
        }
    }

    /// A truncated listing SAYS SO. A picker silently missing a venue's tail is a bug report
    /// nobody can reproduce, which is why the server carries the flag at all.
    #[test]
    fn a_truncated_listing_is_reported_rather_than_silently_short() {
        let line = render_catalog_fetch_status(&CatalogFetchReport::Listed {
            instruments: Vec::new(),
            truncated: true,
            cached: false,
        });
        assert!(line.contains("TRUNCATED"), "{line}");
        let quiet = render_catalog_fetch_status(&CatalogFetchReport::Listed {
            instruments: Vec::new(),
            truncated: false,
            cached: true,
        });
        assert!(!quiet.contains("TRUNCATED"), "{quiet}");
        assert!(quiet.contains("from that server's cache"), "a memo hit says so: {quiet}");
    }

    /// A datahub that completes the handshake and then never answers the catalog request (the
    /// half-open shape) ends the fetch as `Failed`, naming the timeout, within the bound.
    #[test]
    fn a_silent_datahub_fails_the_fetch_within_the_bound_and_says_so() {
        use crate::PROTO_VERSION;
        use crate::proto::Request;
        use vike_hub_protocol::frame::{read_frame, write_frame};
        use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr").to_string();
        let (done, wait_done) = std::sync::mpsc::channel::<()>();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _: HandshakeFrame = read_frame(&mut stream).expect("Hello");
            let features = vec![FEATURE_VENUE_CATALOG.to_string()];
            write_frame(
                &mut stream,
                &HandshakeReply::Welcome { proto_version: PROTO_VERSION, features, nonce: None },
            )
            .expect("Welcome");
            let asked: Request = read_frame(&mut stream).expect("the catalog request arrives");
            assert!(matches!(asked, Request::VenueCatalog { .. }), "{asked:?}");
            // Silence, held open until the client has given up.
            wait_done.recv().expect("the client returns");
        });
        let dial = DatahubDial { address: addr.clone(), keys: None, epoch: 0 };
        let bound = std::time::Duration::from_millis(200);
        let t0 = std::time::Instant::now();
        let report = fetch_venue_catalog_within(Some(&dial), "deribit", bound);
        let waited = t0.elapsed();
        done.send(()).expect("release the peer");
        peer.join().expect("peer");

        assert!(waited >= bound && waited < std::time::Duration::from_secs(10), "{waited:?}");
        let CatalogFetchReport::Failed { addr: failed_at, error } = &report else {
            panic!("a silent datahub is a FAILED fetch, got {report:?}");
        };
        assert_eq!(failed_at, &addr);
        assert!(error.contains("no reply byte for 200ms"), "names the timeout: {error}");
        let line = render_catalog_fetch_status(&report);
        assert!(line.contains("could not list it"), "{line}");
    }

    #[test]
    fn only_a_listing_may_be_adopted() {
        assert!(
            CatalogFetchReport::Listed { instruments: Vec::new(), truncated: false, cached: false }
                .listed()
        );
        for r in [
            CatalogFetchReport::NoBackend,
            CatalogFetchReport::Refused { why: "x".into() },
            CatalogFetchReport::Failed { addr: "a".into(), error: "e".into() },
        ] {
            assert!(!r.listed(), "{r:?}");
        }
    }
}
