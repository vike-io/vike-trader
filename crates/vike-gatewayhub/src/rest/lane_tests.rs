use super::*;
use crate::registry::{Lane, owned};

fn no_store() -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(None, vike_secrets::CredentialScope::Full)
}

/// **The advertisement is the registry**: what the production lane DISPATCHES is what the typed
/// table declares, lane by lane and in order (the client's rosters are pinned to the same table by
/// `kline_sources`' and `catalog`'s own tests).
#[test]
fn the_production_lane_advertises_the_registrys_rosters_in_order() {
    let facts = VenueRest::new(false).facts();
    assert_eq!(facts.klines, owned(Lane::Klines));
    assert_eq!(facts.funding, owned(Lane::Funding));
    assert_eq!(facts.ticks, owned(Lane::Ticks));
    assert_eq!(facts.catalog, owned(Lane::Catalog));
    assert_eq!(facts.family, owned(Lane::Family));
    assert!(facts.options && facts.presence);
    assert!(facts.serves_any_fetch());
    assert!(
        facts.credentialed.is_empty(),
        "no credential store was handed over, so no credentialed lane is advertised"
    );
}

/// A credential store arms the credentialed lane, and the registry's single credentialed venue is
/// the one advertised.
#[test]
fn a_credential_store_arms_the_credentialed_lane() {
    let lane = VenueRest::new(false).with_credential_store(no_store());
    assert_eq!(lane.facts().credentialed, owned(Lane::CredentialedKlines));
    assert!(lane.oanda().is_some());
}

/// The presence words the lane itself answers: a keyless or unserved venue needs nothing; the
/// credentialed venue with no way to look is `NotChecked` (nothing was read, which is not `Absent`);
/// with a store handle over no settings directory it is `Absent`.
#[test]
fn the_presence_words_say_what_was_and_was_not_read() {
    let bare = VenueRest::new(false);
    assert_eq!(bare.credential_presence("binance"), CredentialPresence::NotNeeded);
    assert_eq!(bare.credential_presence("no-such-venue"), CredentialPresence::NotNeeded);
    assert_eq!(bare.credential_presence(OANDA_VENUE), CredentialPresence::NotChecked);
    let armed = VenueRest::new(false).with_credential_store(no_store());
    assert_eq!(armed.credential_presence(OANDA_VENUE), CredentialPresence::Absent);
    assert_eq!(armed.credential_presence("binance"), CredentialPresence::NotNeeded);
}

/// A credentialed fetch the lane was not built for is a REFUSAL (the venue was not asked), for an
/// unarmed lane, for another venue and for a lane that is not klines.
#[test]
fn a_credentialed_fetch_the_lane_cannot_serve_is_refused() {
    let scope = ConnectionScope::new();
    let ctx = FetchCtx::new();
    let klines = |venue: &str| FetchWhat::Klines {
        venue: venue.to_string(),
        symbol: "EUR_USD".to_string(),
        interval: "5s".to_string(),
        start_ms: 0,
        end_ms: 1,
    };
    let unarmed = VenueRest::new(false);
    let armed = VenueRest::new(false).with_credential_store(no_store());
    for (lane, what) in [
        (&unarmed, klines(OANDA_VENUE)),
        (&armed, klines("binance")),
        (&armed, FetchWhat::CatalogList { venue: OANDA_VENUE.to_string() }),
    ] {
        let out = lane.fetch(&scope, &what, true, &ctx);
        assert!(matches!(out, Err(FetchFailure::Refused(_))), "{what:?}: {out:?}");
    }
    assert!(scope.is_empty(), "a refusal must not create the connection's credential slot");
}

/// A tick fetch for another venue and a family for another venue are refused, not routed.
#[test]
fn a_venue_the_lane_has_no_lane_for_is_refused() {
    let lane = VenueRest::new(false);
    let scope = ConnectionScope::new();
    let ticks = FetchWhat::Ticks {
        venue: "binance".to_string(),
        symbol: "EURUSD".to_string(),
        start_ms: 0,
        end_ms: 1,
    };
    let out = lane.fetch(&scope, &ticks, false, &FetchCtx::new());
    assert!(matches!(out, Err(FetchFailure::Refused(_))), "{out:?}");
    let err = lane.resolve_family(&scope, "binance", "btc-updown-5m", 0).expect_err("not served");
    assert!(err.contains("`binance`") && err.contains("polymarket"), "{err}");
}

/// The status seam: empty until the root wires a snapshot, then exactly the wired rows.
#[test]
fn the_budget_seam_is_empty_until_the_root_wires_a_snapshot() {
    assert!(VenueRest::new(false).budget_counters().is_empty());
    let row = BudgetCounter {
        venue: "binance".to_string(),
        host: "fapi".to_string(),
        scope: "ip".to_string(),
        class: "bulk".to_string(),
        used: 7,
        cap: 2_400,
        window_ms: 60_000,
        would_throttle: 0,
    };
    let wired = row.clone();
    let lane = VenueRest::new(false).with_budget_snapshot(Arc::new(move || vec![wired.clone()]));
    assert_eq!(lane.budget_counters(), vec![row]);
}
