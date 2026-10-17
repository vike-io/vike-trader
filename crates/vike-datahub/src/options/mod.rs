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
//! verb reads, one [`OptionsSource`] per venue) and [`select_expiries`] (the request's one
//! restriction). Only what NAMES the venue and the folds sits behind `options-serve`:
//! `book` (the state and every fold onto it) and `deribit` (the poll thread, the two feeds and
//! the option index, all fetched through the gatewayhub, which owns the venue connection: decision
//! 0164). It is `crate::catalog`'s rule: the TYPE is feature-free, the constructor naming the
//! venue is not.
//!
//! # The bounds, every one the server's (the REST cadence the operator's, per box)
//!
//! | bound | where |
//! |---|---|
//! | which venues | [`OptionsLane`]'s membership: one entry, `deribit`, built by `deribit::real_options_lane` |
//! | which underlyings | `deribit::UNDERLYINGS` (BTC, ETH, SOL: the venue provider's own list, held here because the set is known without a venue call) |
//! | how many expiries are held, and asked | [`POLLED_EXPIRIES`] = `vike_datahub_client::options::MAX_OPTIONS_EXPIRIES` |
//! | the strike window | `deribit::STRIKE_WINDOW` (±30 around spot, the widest display preset) |
//! | the REST cadence | `deribit::POLL_INTERVAL`, or the box's `config.datahub_options_poll_secs` row (`deribit::poll_interval`) |
//! | the 100 ms ticker focus set | `deribit::FOCUS_UNDERLYINGS` × `deribit::FOCUS_STRIKES` front-expiry strikes |
//!
//! A request SELECTS from what is held: no request moves any row of that table, so the venue cost is
//! the server's whatever clients ask (`docs/decisions/0052`'s rule, and
//! `vike_datahub_client::proto::required_scope`'s arm for the verb).

use vike_marketdata::UnderlyingChains;

#[cfg(feature = "options-serve")]
pub mod book;
#[cfg(feature = "options-serve")]
pub mod deribit;

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
    /// The underlyings this venue lists, in the venue's own spelling and CANONICAL order (BTC, ETH,
    /// SOL, then the rest) — a fixed set, known without a venue call, and what every reply carries
    /// as `OptionsChainReply::served_underlyings`.
    fn underlyings(&self) -> Vec<String>;

    /// `underlying`'s current snapshot restricted by [`select_expiries`] to `expiries`, or WHY there
    /// is none yet (the first venue pass has not landed, or every pass so far failed). Called only
    /// with an underlying [`Self::underlyings`] lists.
    fn snapshot(&self, underlying: &str, expiries: &[String]) -> Result<UnderlyingChains, String>;
}

/// The table the verb reads: one [`OptionsSource`] per venue. Its mere EXISTENCE is the arming —
/// a build without `options-serve` mounts none and the verb answers `NotArmed`.
pub struct OptionsLane {
    sources: Vec<(String, Box<dyn OptionsSource>)>,
}

impl OptionsLane {
    /// A lane over `sources`, keyed by venue slug. A later entry for a slug already present is
    /// unreachable (lookups take the first).
    pub fn new(sources: Vec<(String, Box<dyn OptionsSource>)>) -> Self {
        Self { sources }
    }

    /// The venues this lane serves, in table order — what an `UnknownVenue` refusal names.
    pub fn supported(&self) -> Vec<String> {
        self.sources.iter().map(|(v, _)| v.clone()).collect()
    }

    /// The source serving `venue`, matched exactly.
    pub fn get(&self, venue: &str) -> Option<&dyn OptionsSource> {
        self.sources.iter().find(|(v, _)| v == venue).map(|(_, s)| s.as_ref())
    }
}

/// **The request's one restriction**: `full`'s chains narrowed to `asked` — the FRONT expiry
/// (`default_expiry`) when `asked` is empty; an asked expiry `full` does not hold is OMITTED, never
/// an error — with the FULL `expiries` strip and `default_expiry` kept, so a client can offer every
/// expiry this server holds. Clones only the chains it keeps.
pub fn select_expiries(full: &UnderlyingChains, asked: &[String]) -> UnderlyingChains {
    let front = [full.default_expiry.clone()];
    let wanted: &[String] = if asked.is_empty() { &front } else { asked };
    let chains =
        wanted.iter().filter_map(|e| full.chains.get(e).map(|c| (e.clone(), c.clone()))).collect();
    UnderlyingChains {
        default_expiry: full.default_expiry.clone(),
        expiries: full.expiries.clone(),
        chains,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_marketdata::{Expiry, OptionChain, UnderlyingKind};

    fn chain(date: &str) -> OptionChain {
        OptionChain {
            underlying: "BTC".into(),
            underlying_kind: UnderlyingKind::Crypto,
            underlying_price: Some(100_000.0),
            expiry: Expiry { date: date.into(), dte: 1, label: date.into() },
            asof_ms: 0,
            source: "deribit".into(),
            rows: Vec::new(),
        }
    }

    fn bundle() -> UnderlyingChains {
        let dates = ["2026-06-26", "2026-07-31", "2026-09-25"];
        UnderlyingChains {
            default_expiry: dates[0].into(),
            expiries: dates
                .iter()
                .map(|d| Expiry { date: (*d).into(), dte: 1, label: (*d).into() })
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

    #[test]
    fn a_lane_finds_its_venue_by_exact_slug() {
        struct Empty;
        impl OptionsSource for Empty {
            fn underlyings(&self) -> Vec<String> {
                vec!["BTC".into()]
            }
            fn snapshot(&self, _: &str, _: &[String]) -> Result<UnderlyingChains, String> {
                Err("never".into())
            }
        }
        let lane =
            OptionsLane::new(vec![("deribit".into(), Box::new(Empty) as Box<dyn OptionsSource>)]);
        assert_eq!(lane.supported(), ["deribit"]);
        assert!(lane.get("deribit").is_some());
        assert!(lane.get("Deribit").is_none() && lane.get("okx").is_none());
    }
}
