//! The `OptionsChain` verb: one underlying's option-chain snapshot, answered from the mounted
//! options lane and never from the store. `handle_request` (the parent module) routes the request
//! here and passes the lane and nothing else — no `store` handle, the `venue_catalog` shape.

use vike_datahub_client::options::{
    MAX_OPTIONS_EXPIRIES, OptionsChainOutcome, OptionsChainReply, OptionsRefusal,
    validate_options_request,
};
use vike_datahub_client::proto::Response;

use crate::options::OptionsLane;

/// **Serve one underlying's option-chain snapshot** — the `Request::OptionsChain` handler.
///
/// # ⚠ Almost every outcome is a SUCCESS, `venue_catalog_verb`'s shape
///
/// An unarmed build, a venue or underlying the lane does not serve, too many expiries and a
/// snapshot that has not landed are all `Response::OptionsChain` values; only a MALFORMED request is
/// [`Response::Error`]. The Options tool asks every second, and a stream of errors is the wrong shape
/// for a routine answer.
///
/// # The gate order, and why each step is where it is
///
/// 1. **SHAPE** ([`validate_options_request`]) — the one genuinely exceptional input, refused before
///    anything is consulted.
/// 2. **ARMING** — before the venue, so an unarmed server says so about EVERY venue rather than
///    leaking which ones it would have served.
/// 3. **THE VENUE** — the lane's table, naming what it serves.
/// 4. **THE UNDERLYING** — the provider's own fixed list, naming what it lists.
/// 5. **THE EXPIRY COUNT** — [`MAX_OPTIONS_EXPIRIES`], after the two lookups so a request wrong on
///    both counts is told the more useful thing first.
/// 6. **THE SNAPSHOT** — the only step that touches the lane's state; `NotReady` when none is held
///    yet. On the Deribit source the first call here also STARTS the lane
///    (`crate::options::deribit`'s module doc).
pub(super) fn options_chain_verb(
    venue: &str,
    underlying: &str,
    expiries: &[String],
    lane: Option<&OptionsLane>,
) -> Response {
    // Gate 1.
    if let Err(why) = validate_options_request(venue, underlying, expiries) {
        return Response::Error(format!("options chain: {why}"));
    }
    // Every outcome carries the venue's SERVED set (`OptionsChainReply::served_underlyings`), so a
    // client's selector learns it from any answer — empty where the server serves the venue nothing.
    let reply = |served_underlyings: Vec<String>, outcome| {
        Response::OptionsChain(OptionsChainReply {
            venue: venue.to_string(),
            underlying: underlying.to_string(),
            served_underlyings,
            outcome,
        })
    };
    // Gate 2 — a SUCCESS, not a refusal.
    let Some(lane) = lane else {
        return reply(Vec::new(), OptionsChainOutcome::NotArmed);
    };
    // Gate 3.
    let Some(source) = lane.get(venue) else {
        let supported = lane.supported();
        return reply(
            Vec::new(),
            OptionsChainOutcome::Refused(OptionsRefusal::UnknownVenue { supported }),
        );
    };
    let served = source.underlyings();
    let refused = |r| reply(served.clone(), OptionsChainOutcome::Refused(r));
    // Gate 4.
    if !served.iter().any(|u| u == underlying) {
        return refused(OptionsRefusal::UnknownUnderlying { supported: served.clone() });
    }
    // Gate 5.
    if expiries.len() > MAX_OPTIONS_EXPIRIES {
        return refused(OptionsRefusal::TooManyExpiries { max: MAX_OPTIONS_EXPIRIES });
    }
    // Gate 6.
    match source.snapshot(underlying, expiries) {
        Ok(chains) => reply(served.clone(), OptionsChainOutcome::Chains(chains)),
        Err(why) => refused(OptionsRefusal::NotReady { why }),
    }
}

#[cfg(test)]
mod tests {
    //! Every gate, over a scripted source — no venue, no feature: these run on the DEFAULT build.

    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use vike_marketdata::{Expiry, OptionChain, UnderlyingChains, UnderlyingKind};

    use super::*;
    use crate::options::{OptionsSource, select_expiries};

    const DATES: [&str; 3] = ["2026-06-26", "2026-07-31", "2026-09-25"];

    /// A source holding one BTC bundle over [`DATES`] (or nothing, when `held` is false), and
    /// recording every snapshot call so a test can prove a refused request never reached it.
    struct Scripted {
        held: bool,
        calls: Arc<AtomicUsize>,
    }

    impl OptionsSource for Scripted {
        fn underlyings(&self) -> Vec<String> {
            vec!["BTC".into(), "ETH".into(), "SOL".into()]
        }
        fn snapshot(&self, u: &str, e: &[String]) -> Result<UnderlyingChains, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if !self.held || u != "BTC" {
                return Err("the first chain pass has not landed yet".into());
            }
            let chain = |d: &str| OptionChain {
                underlying: "BTC".into(),
                underlying_kind: UnderlyingKind::Crypto,
                underlying_price: Some(100_000.0),
                expiry: Expiry { date: d.into(), dte: 1, label: d.into() },
                asof_ms: 0,
                source: "deribit".into(),
                rows: Vec::new(),
            };
            let full = UnderlyingChains {
                default_expiry: DATES[0].into(),
                expiries: DATES
                    .iter()
                    .map(|d| Expiry { date: (*d).into(), dte: 1, label: (*d).into() })
                    .collect(),
                chains: DATES
                    .iter()
                    .map(|d| ((*d).to_string(), chain(d)))
                    .collect::<BTreeMap<_, _>>(),
            };
            Ok(select_expiries(&full, e))
        }
    }

    fn lane(held: bool) -> OptionsLane {
        lane_counting(held).0
    }

    /// [`lane`], plus the count of snapshot calls its source has served.
    fn lane_counting(held: bool) -> (OptionsLane, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let source = Scripted { held, calls: Arc::clone(&calls) };
        (
            OptionsLane::new(vec![("deribit".into(), Box::new(source) as Box<dyn OptionsSource>)]),
            calls,
        )
    }

    fn ask(lane: Option<&OptionsLane>, venue: &str, u: &str, e: &[&str]) -> Response {
        let e: Vec<String> = e.iter().map(|s| (*s).to_string()).collect();
        options_chain_verb(venue, u, &e, lane)
    }

    fn outcome(r: Response) -> OptionsChainOutcome {
        match r {
            Response::OptionsChain(reply) => reply.outcome,
            other => panic!("expected OptionsChain, got {other:?}"),
        }
    }

    #[test]
    fn every_outcome_carries_the_venues_served_set() {
        let served = |r: Response| match r {
            Response::OptionsChain(reply) => reply.served_underlyings,
            other => panic!("expected OptionsChain, got {other:?}"),
        };
        let all = ["BTC", "ETH", "SOL"];
        assert!(served(ask(None, "deribit", "BTC", &[])).is_empty(), "unarmed: nothing served");
        assert!(served(ask(Some(&lane(true)), "okx", "BTC", &[])).is_empty(), "unknown venue");
        for (held, u) in [(true, "BTC"), (true, "DOGE"), (false, "SOL")] {
            assert_eq!(served(ask(Some(&lane(held)), "deribit", u, &[])), all, "{u}, held={held}");
        }
    }

    #[test]
    fn a_malformed_request_is_the_one_error() {
        for (v, u, e) in [
            ("Deribit", "BTC", vec![]),
            ("deribit", "btc", vec![]),
            ("deribit", "BTC", vec!["26JUN26"]),
        ] {
            match ask(Some(&lane(true)), v, u, &e) {
                Response::Error(msg) => assert!(msg.starts_with("options chain:"), "{msg}"),
                other => panic!("{v}/{u}/{e:?}: expected Error, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_unarmed_server_answers_not_armed_for_every_venue() {
        for venue in ["deribit", "okx"] {
            assert_eq!(outcome(ask(None, venue, "BTC", &[])), OptionsChainOutcome::NotArmed);
        }
    }

    #[test]
    fn an_unknown_venue_names_what_is_served() {
        assert_eq!(
            outcome(ask(Some(&lane(true)), "okx", "BTC", &[])),
            OptionsChainOutcome::Refused(OptionsRefusal::UnknownVenue {
                supported: vec!["deribit".into()]
            })
        );
    }

    #[test]
    fn an_unknown_underlying_names_what_is_listed() {
        assert_eq!(
            outcome(ask(Some(&lane(true)), "deribit", "DOGE", &[])),
            OptionsChainOutcome::Refused(OptionsRefusal::UnknownUnderlying {
                supported: vec!["BTC".into(), "ETH".into(), "SOL".into()]
            })
        );
    }

    #[test]
    fn too_many_expiries_is_refused_with_its_number_and_reads_nothing() {
        let (l, calls) = lane_counting(true);
        let many: Vec<String> =
            (1..=MAX_OPTIONS_EXPIRIES + 1).map(|d| format!("2026-08-{d:02}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(
            outcome(ask(Some(&l), "deribit", "BTC", &many)),
            OptionsChainOutcome::Refused(OptionsRefusal::TooManyExpiries {
                max: MAX_OPTIONS_EXPIRIES
            })
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0, "a refused request never reaches the source");
        // Exactly the cap is served.
        let at_cap = &many[..MAX_OPTIONS_EXPIRIES];
        assert!(matches!(
            outcome(ask(Some(&l), "deribit", "BTC", at_cap)),
            OptionsChainOutcome::Chains(_)
        ));
    }

    #[test]
    fn no_expiry_asked_is_the_front_one_with_the_whole_strip() {
        let OptionsChainOutcome::Chains(c) = outcome(ask(Some(&lane(true)), "deribit", "BTC", &[]))
        else {
            panic!("expected chains")
        };
        assert_eq!(c.chains.keys().collect::<Vec<_>>(), [DATES[0]]);
        assert_eq!(c.expiries.len(), DATES.len());
    }

    #[test]
    fn an_unheld_expiry_is_omitted_not_refused() {
        let OptionsChainOutcome::Chains(c) =
            outcome(ask(Some(&lane(true)), "deribit", "BTC", &["2027-03-26", DATES[2]]))
        else {
            panic!("expected chains")
        };
        assert_eq!(c.chains.keys().collect::<Vec<_>>(), [DATES[2]]);
    }

    #[test]
    fn a_snapshot_not_yet_landed_is_not_ready() {
        match outcome(ask(Some(&lane(false)), "deribit", "BTC", &[])) {
            OptionsChainOutcome::Refused(OptionsRefusal::NotReady { why }) => {
                assert!(why.contains("has not landed"), "{why}")
            }
            other => panic!("expected NotReady, got {other:?}"),
        }
    }
}
