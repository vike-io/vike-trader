//! **The options lane: option-chain snapshots this process HOLDS and folds from the venue's own
//! feeds — what [`Request::OptionsChain`](vike_datahub_client::proto::Request::OptionsChain)
//! answers from.**
//!
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md`'s shape applied to the venue: the
//! desktop's Options tool used to open Deribit's REST and two Deribit sockets itself; it now asks
//! this daemon, which does the venue work ONCE for every client (through the gatewayhub, which owns
//! the connection since decision 0164). The answer SHAPE and the request
//! validator both ends need are `crates/vike-datahub-client/src/options.rs`'s; what lives here is
//! everything only the server can have.
//!
//! # What is feature-free and what is not
//!
//! The verb must DECODE and ANSWER on every build — a build without the lane answers
//! `OptionsChainOutcome::NotArmed` — so the SEAM is feature-free: [`OptionsLane`] (the table the
//! verb reads, its [`OptionsSource`]s) and [`select_expiries`] (the request's one
//! restriction). Only the source and the folds sit behind `options-serve`: `book` (the state and
//! every fold onto it) and `gateway` (the poll thread, the two feeds and the option index, all
//! fetched through the gatewayhub, which owns the venue connection: decision 0164). Neither names a
//! venue: which venue the lane serves, and which underlyings, is the gatewayhub's listing.
//!
//! # The bounds, every one the server's (the REST cadence the operator's, per box)
//!
//! | bound | where |
//! |---|---|
//! | which venues | [`OptionsLane`]'s membership: one source, `gateway::real_options_lane`, serving the venue the gatewayhub's listing names |
//! | which underlyings | the gatewayhub's listing, the venue's own (asked every pass; none served while it cannot be had) |
//! | how many expiries are held, and asked | [`POLLED_EXPIRIES`] = `vike_datahub_client::options::MAX_OPTIONS_EXPIRIES` |
//! | the strike window | `gateway::STRIKE_WINDOW` (±30 around spot, the widest display preset) |
//! | the REST cadence | `gateway::POLL_INTERVAL`, or the box's `config.datahub_options_poll_secs` row (`gateway::poll_interval`) |
//! | the 100 ms ticker focus set | `gateway::DEFAULT_FOCUS_UNDERLYINGS` × `gateway::DEFAULT_FOCUS_STRIKES` front-expiry strikes |
//!
//! A request SELECTS from what is held: no request moves any row of that table, so the venue cost is
//! the server's whatever clients ask (`docs/decisions/0052`'s rule, and
//! `vike_datahub_client::proto::required_scope`'s arm for the verb).

use vike_marketdata::OptionChainSet;

#[cfg(feature = "options-serve")]
pub mod book;
#[cfg(feature = "options-serve")]
pub mod gateway;

/// How many expiries per underlying the lane HOLDS — the nearest ones, as the desktop's own poll
/// kept them. It is also what a request may ask for: a request naming more could only name expiries
/// this server does not hold.
pub const POLLED_EXPIRIES: usize = 8;

// The two numbers are ONE bound seen from both ends: the client crate's cap is what a request may
// name, this is what the server holds. Held equal at compile time so neither moves alone.
const _: () = assert!(
    POLLED_EXPIRIES == vike_datahub_client::options::MAX_OPTIONS_EXPIRIES,
    "POLLED_EXPIRIES (what this server holds per underlying) and MAX_OPTIONS_EXPIRIES (what a \
     request may name) are one bound: move both, and re-read the client constant's doc."
);

/// **One venue's option-chain snapshots, as the verb reads them.** A trait so the verb's every gate
/// is tested over a scripted source on the DEFAULT build, and so the TYPE carries no bridge.
pub trait OptionsSource: Send + Sync {
    /// The venue slug this source serves, or `None` while it does not know it yet (a source over the
    /// gatewayhub learns it from the gatewayhub's first listing, and keeps it).
    fn venue(&self) -> Option<String>;

    /// The underlyings this venue lists, in the venue's own spelling and the listing's ORDER (most
    /// listed options first) — what every reply carries as `OptionsChainReply::served_underlyings` —
    /// or WHY none is served (the listing has not been had yet, or the last ask for it failed). It is
    /// answered without a venue call, on the connection's own thread.
    fn underlyings(&self) -> Result<Vec<String>, String>;

    /// `underlying`'s current snapshot restricted by [`select_expiries`] to `expiries`, or WHY there
    /// is none yet (the first venue pass has not landed, or every pass so far failed). Called only
    /// with an underlying [`Self::underlyings`] lists.
    fn snapshot(&self, underlying: &str, expiries: &[String]) -> Result<OptionChainSet, String>;

    /// Read the venue's listing again NOW (`Request::OptionsRefresh`): the underlyings it lists, which
    /// [`Self::underlyings`] answers from then on, or why it could not be had. A call that may wait on
    /// a venue read, on the connection's own thread.
    fn refresh(&self) -> Result<Vec<String>, String>;
}

/// The table the verb reads: its [`OptionsSource`]s. Its mere EXISTENCE is the arming — a build
/// without `options-serve` mounts none and the verb answers `NotArmed`.
pub struct OptionsLane {
    sources: Vec<Box<dyn OptionsSource>>,
}

impl OptionsLane {
    /// A lane over `sources`, each naming its own venue ([`OptionsSource::venue`]). A later source for
    /// a venue already present is unreachable (lookups take the first).
    pub fn new(sources: Vec<Box<dyn OptionsSource>>) -> Self {
        Self { sources }
    }

    /// The venues this lane serves, in table order — what an `UnknownVenue` refusal names. A source
    /// that does not know its venue yet names none.
    pub fn supported(&self) -> Vec<String> {
        self.sources.iter().filter_map(|s| s.venue()).collect()
    }

    /// The source serving `venue`, matched exactly — or, when none does, the first source that does not
    /// know its venue yet: it answers for ANY venue until it knows (its `underlyings` says why it
    /// serves none, which is the answer such a request gets).
    pub fn get(&self, venue: &str) -> Option<&dyn OptionsSource> {
        self.sources
            .iter()
            .find(|s| s.venue().as_deref() == Some(venue))
            .or_else(|| self.sources.iter().find(|s| s.venue().is_none()))
            .map(|s| s.as_ref())
    }
}

/// **The request's one restriction**: `full`'s chains narrowed to `asked` — the FRONT expiry
/// (`default_expiry`) when `asked` is empty; an asked expiry `full` does not hold is OMITTED, never
/// an error — with the FULL `expiries` strip and `default_expiry` kept, so a client can offer every
/// expiry this server holds. Clones only the chains it keeps.
pub fn select_expiries(full: &OptionChainSet, asked: &[String]) -> OptionChainSet {
    let front = [full.default_expiry.clone()];
    let wanted: &[String] = if asked.is_empty() { &front } else { asked };
    let chains =
        wanted.iter().filter_map(|e| full.chains.get(e).map(|c| (e.clone(), c.clone()))).collect();
    OptionChainSet {
        default_expiry: full.default_expiry.clone(),
        expiries: full.expiries.clone(),
        chains,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_marketdata::{OptionChain, OptionChainExpiry, OptionUnderlyingKind};

    fn chain(date: &str) -> OptionChain {
        OptionChain {
            underlying: "BTC".into(),
            underlying_kind: OptionUnderlyingKind::Crypto,
            underlying_price: Some(100_000.0),
            expiry: OptionChainExpiry { date: date.into(), dte: 1, label: date.into() },
            asof_ms: 0,
            source: "deribit".into(),
            rows: Vec::new(),
        }
    }

    fn bundle() -> OptionChainSet {
        let dates = ["2026-06-26", "2026-07-31", "2026-09-25"];
        OptionChainSet {
            default_expiry: dates[0].into(),
            expiries: dates
                .iter()
                .map(|d| OptionChainExpiry { date: (*d).into(), dte: 1, label: (*d).into() })
                .collect(),
            chains: dates.iter().map(|d| ((*d).to_string(), chain(d))).collect(),
        }
    }

    #[test]
    fn no_asked_expiry_is_the_front_one_and_the_strip_is_kept_whole() {
        let got = select_expiries(&bundle(), &[]);
        assert_eq!(got.chains.keys().collect::<Vec<_>>(), ["2026-06-26"]);
        assert_eq!(got.expiries.len(), 3, "the FULL strip rides every answer");
        assert_eq!(got.default_expiry, "2026-06-26");
    }

    #[test]
    fn an_unheld_expiry_is_omitted_and_the_held_ones_answer() {
        let asked = ["2026-09-25".to_string(), "2027-01-29".to_string(), "2026-07-31".to_string()];
        let got = select_expiries(&bundle(), &asked);
        assert_eq!(got.chains.keys().collect::<Vec<_>>(), ["2026-07-31", "2026-09-25"]);
        assert_eq!(got.expiries.len(), 3);
    }

    /// A source naming `venue` (or none yet).
    struct Named(Option<&'static str>);

    impl OptionsSource for Named {
        fn venue(&self) -> Option<String> {
            self.0.map(str::to_string)
        }
        fn underlyings(&self) -> Result<Vec<String>, String> {
            Ok(vec!["BTC".into()])
        }
        fn snapshot(&self, _: &str, _: &[String]) -> Result<OptionChainSet, String> {
            Err("never".into())
        }
        fn refresh(&self) -> Result<Vec<String>, String> {
            self.underlyings()
        }
    }

    fn lane_of(sources: Vec<Named>) -> OptionsLane {
        OptionsLane::new(
            sources.into_iter().map(|s| Box::new(s) as Box<dyn OptionsSource>).collect(),
        )
    }

    #[test]
    fn a_lane_finds_its_venue_by_exact_slug() {
        let lane = lane_of(vec![Named(Some("deribit"))]);
        assert_eq!(lane.supported(), ["deribit"]);
        assert!(lane.get("deribit").is_some());
        assert!(lane.get("Deribit").is_none() && lane.get("okx").is_none());
    }

    /// A source that does not know its venue yet names none and answers for any venue; once another
    /// source names the venue asked, that one answers.
    #[test]
    fn a_source_that_does_not_know_its_venue_yet_answers_for_any() {
        let lane = lane_of(vec![Named(None)]);
        assert!(lane.supported().is_empty());
        assert!(lane.get("deribit").is_some() && lane.get("okx").is_some());
        let lane = lane_of(vec![Named(None), Named(Some("okx"))]);
        assert_eq!(lane.supported(), ["okx"]);
        assert_eq!(lane.get("okx").and_then(|s| s.venue()).as_deref(), Some("okx"));
        assert_eq!(lane.get("deribit").map(|s| s.venue()), Some(None));
    }
}
