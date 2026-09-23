//! `catalog_wire` — **the refresh button's SECOND route: ask the backend's datahub for a venue
//! this binary links no bridge for**, and when it will not answer, say why in a sentence rather
//! than in an empty list.
//!
//! # Where this sits
//!
//! [`catalog_refresh`](crate::catalog_refresh) owns the cache, the per-venue budget and the fold;
//! it had exactly one fetch — a `vike_catalog::CatalogProvider` this binary LINKS — and rulings 1
//! and 2 of the 2026-09-09 rename design left the desktop linking one venue bridge. So that
//! module's own ⚠ ("less than you expect") was the whole truth: thirteen of the fourteen roster
//! rows said *not in this build* and offered nothing.
//!
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` built the
//! other side — `vike_datahub_client::proto::Request::VenueCatalog`, an Observe verb — and
//! `vike_catalog::catalog_source_for` is the routing decision it was declared for. This module is
//! the client of that verb: one blocking call, five refusal shapes, and one sentence each.
//!
//! # Why this may use the desktop's OWN key
//!
//! The identical argument [`crate::chart_seed`] makes, and it is the reason that module rather
//! than [`crate::backfill_wire`] is the template here. `Request::VenueCatalog` is
//! `VerbScope::Read` (0062's decision 1, asserted in
//! `crates/vike-datahub/tests/auth_roundtrip.rs`), so the datahub OBSERVE pair the desktop already
//! resolves for its store reads reaches it. `Request::Backfill` is `VerbScope::Write`, which is
//! why `run_wire_backfill` dials unauthenticated and cannot work against a keyed server at all.
//!
//! # ⚠ The refusals are the point, and none of them is an error
//!
//! Four different facts about the world come back as a SUCCESSFUL response, and a client that
//! folded any of them into "the fetch failed" would tell an operator to retry something that
//! cannot change:
//!
//! | what came back | the fact | who can act |
//! |---|---|---|
//! | `CatalogOutcome::NotArmed` | the server's operator REFUSED the lane (it is ON by default since `docs/decisions/0066`) | the server's operator (delete `venue_catalog_off` + restart) |
//! | `Refused(NotServed)` | this server's BUILD carries no provider for the venue | whoever rebuilds it |
//! | `Refused(NeedsCredentials)` | listing spends the operator's identity (0062 decision 3) | **nobody** |
//! | `Refused(NoBulkList)` | the VENUE publishes no list | **nobody** |
//!
//! ...plus one refused CLIENT-side before a frame is written: a server that does not advertise
//! [`FEATURE_VENUE_CATALOG`]. That is [`CatalogFetchReport::ServerUnsupported`], and it exists so
//! an un-advertised capability can never render as an empty instrument list — which for `ig` and
//! `ibkr` would be indistinguishable from the truth.
//!
//! **The sentences are the SERVER's**, not ours: every refusal renders through
//! `vike_datahub_client::catalog::CatalogListing::describe`, so the daemon, the CLI and the desktop
//! cannot describe one refusal three different ways.
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
//! That is why there is no retry loop here and nothing in this tree polls the verb.
//! [`crate::catalog_refresh::REFRESH_COOLDOWN_MS`] is 60 s, which is that lane's
//! `CATALOG_VENUE_REFILL` exactly, so a human pressing as fast as the button allows tracks the
//! refill rate and can never drain the two-token burst.
//! `crate::catalog_refresh::CatalogRefresh::spawn_initial` stays LOCAL-ONLY for the same reason — a
//! startup that routed eight cold venues to a server would be precisely the poll this paragraph
//! forbids, wearing a first run.

use vike_catalog::Instrument;
use vike_datahub_client::catalog::{CatalogListing, CatalogOutcome, CatalogRefusal};
use vike_datahub_client::{DatahubClient, FEATURE_VENUE_CATALOG};
use vike_node_proto::auth::{NodeKeys, Scope};

/// **What a routed venue says when there is no datahub to ask** — the ONE spelling of it, shared
/// by the row's Status cell and by the disabled button's hover
/// ([`crate::catalog_refresh::RefreshBlock::NoBackend`]), so the two cannot describe the same state
/// differently. It names BOTH fixes, because either one works and a sentence naming one would send
/// half the operators down a path their box does not use.
pub const NO_BACKEND_NOTE: &str = "no backend is connected and `config.datahub_addr` is unset, so there is no datahub to ask — \
     connect a backend that advertises one, or set that key.";

/// **Where a `ServerBacked` refresh dials, and what it signs with** — the
/// [`crate::chart_seed::SeedDial`] twin, and here for the identical reason: only a BINARY may read
/// the process environment or the credential store, so the composition root resolves this once and
/// hands the whole value down.
///
/// ⚠ It carries `key_name` as well as the resolved keys because the desktop's datahub address AND
/// its observe-key name are both properties of the ACTIVE BACKEND RECORD, which a switch replaces
/// — see [`crate::catalog_refresh::CatalogRefresh::dial_is_stale`], which compares the pair and is
/// what keeps the resolution off the frame path.
#[derive(Clone, Debug)]
pub struct CatalogDial {
    /// The resolved datahub address — `crate::datahub_resolve::resolve_datahub_addr`'s answer.
    pub addr: String,
    /// The observe-key NAME this dial was resolved for; the staleness compare's other half.
    pub key_name: String,
    /// The desktop's OBSERVE pair. `None` dials unauthenticated, which is correct against a
    /// key-less loopback dev server and fails at the handshake against a keyed one, naming what is
    /// missing.
    pub keys: Option<NodeKeys>,
}

impl CatalogDial {
    /// Build one from the facts a COMPOSITION ROOT owns — the resolved address, the settings
    /// directory, the process-environment sweep and the active backend record's key NAME.
    ///
    /// The exact shape of [`crate::chart_seed::SeedDial::resolve`], including the notice handling:
    /// key-resolution notices are LOGGED rather than returned, because this is the fourth caller of
    /// the same resolution in one process and a decision-0051 legacy-store notice the operator has
    /// already seen three times is not worth a fourth surface.
    pub fn resolve(
        addr: String,
        settings_dir: Option<&str>,
        env: &std::collections::HashMap<String, String>,
        key_name: &str,
    ) -> Self {
        let (keys, notices) =
            crate::backend_registry::datahub_observe_keys(settings_dir, env, key_name);
        for n in notices {
            tracing::info!("{n}");
        }
        Self { addr, key_name: key_name.to_string(), keys }
    }
}

/// How one `ServerBacked` fetch ended. A plain value, so every sentence below is unit-testable
/// without a socket — the [`crate::chart_seed::ChartSeedReport`] shape, deliberately.
///
/// ⚠ `PartialEq` but NOT `Eq`, and it is forced rather than chosen: [`Instrument`] carries a
/// `vike_model::SymbolProperties` whose tick/lot fields are `f64`, so nothing reachable from a
/// listing can be `Eq`. The same note `vike_datahub_client::catalog::CatalogOutcome` carries.
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

/// **Ask the datahub for one venue's instrument list, blocking** — call it from the worker thread
/// the refresh already spawns, never the UI thread.
///
/// The capability check happens BEFORE a frame is written, and it happens twice over: once here so
/// the report can name the ADVERTISED SET (which [`DatahubClient::venue_catalog`]'s own `Err`
/// string cannot be parsed for) and once inside that method, which is the enforcement. Neither leg
/// re-spells the feature string — [`FEATURE_VENUE_CATALOG`] is the one constant both read, and
/// [`DatahubClient::serves_venue_catalog`] is the named predicate rather than a fourth inline
/// `features().iter().any(..)`.
pub fn fetch_venue_catalog(dial: Option<&CatalogDial>, venue: &str) -> CatalogFetchReport {
    let Some(dial) = dial else { return CatalogFetchReport::NoBackend };
    let dialled = match &dial.keys {
        Some(k) => DatahubClient::connect_authed(&dial.addr, k, Scope::Read),
        None => DatahubClient::connect(&dial.addr),
    };
    let mut client = match dialled {
        Ok(c) => c,
        Err(e) => {
            return CatalogFetchReport::ConnectFailed {
                addr: dial.addr.clone(),
                error: e.to_string(),
            };
        }
    };
    if !client.serves_venue_catalog() {
        return CatalogFetchReport::ServerUnsupported {
            addr: dial.addr.clone(),
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
        Err(error) => CatalogFetchReport::Failed { addr: dial.addr.clone(), error },
    }
}

/// One outcome's operator sentence, rendered by the SERVER-side type rather than assembled here.
///
/// ⚠ The indirection is the property, not the convenience: `CatalogListing::describe` and
/// `CatalogRefusal::describe` live in `vike-datahub-client` precisely so the daemon's log line, the
/// CLI's output and this row cannot describe one refusal three different ways. A `format!` here
/// would be a fourth spelling nothing compares.
fn describe_outcome(venue: &str, outcome: &CatalogOutcome) -> String {
    CatalogListing { venue: venue.to_string(), outcome: outcome.clone() }.describe()
}

/// A LOCAL refusal's sentence, for a venue [`vike_catalog::catalog_source_for`] routes nowhere.
///
/// ⚠ **No frame is sent for these and none ever should be**: the routing table answers `None`
/// because no server may list the venue (credentialed — 0062's decision 3) or because the venue
/// publishes no list at all. The sentence is still the SERVER-side type's —
/// [`CatalogRefusal::describe`] — so a credentialed venue reads the same words whether it was
/// refused here or at a daemon's door.
#[must_use]
pub fn local_refusal_line(venue: &str, refusal: &CatalogRefusal) -> String {
    refusal.describe(venue)
}

/// One [`CatalogFetchReport`] → the sentence the row's Status cell carries.
///
/// ⚠ **`ServerUnsupported` is the whole point of this function**, exactly as it is in
/// [`crate::chart_seed::render_chart_seed_status`]. Without it a server that will not answer and a
/// venue with no instruments produce the identical blank row, and the operator's next move is to
/// doubt the venue. The line therefore names the EXACT variable and the box it goes on.
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
             delete `venue_catalog_off` from its <project>/settings/flags.toml (and unset \
             VIKE_DATAHUB_VENUE_CATALOG_OFF) and restart it.",
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

    #[test]
    fn a_server_that_does_not_advertise_is_told_apart_from_one_that_is_down() {
        let line = render_catalog_fetch_status(&CatalogFetchReport::ServerUnsupported {
            addr: "<host>:7878".to_string(),
            features: vec!["load_bars".to_string(), "seed_series".to_string()],
        });
        assert!(line.contains("<host>:7878"), "{line}");
        // ⚠ The named switch is the REFUSAL now, not the old arming: since
        // `docs/decisions/0066` a build without `--features catalog-serve` still ADVERTISES (it
        // serves an empty table and answers `NotServed` per venue), so non-advertisement is
        // evidence of a written refusal or an old server and of nothing else. This assertion used
        // to demand `VIKE_DATAHUB_VENUE_CATALOG=1` and `catalog-serve`, and both would now send an
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
            "ig",
            &CatalogOutcome::Refused(CatalogRefusal::NoBulkList {
                why: "searched per query".into(),
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

    #[test]
    fn a_local_refusal_reads_the_same_as_the_servers_would() {
        let refusal = CatalogRefusal::NeedsCredentials;
        assert_eq!(local_refusal_line("oanda", &refusal), refusal.describe("oanda"));
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
