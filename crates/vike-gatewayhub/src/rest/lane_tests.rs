use vike_secrets::venue_setting::seeded_venue_settings;

use super::*;
use crate::registry::{RosterLane, owned};
use vike_venues::venue_urls::VenueUrls;

fn no_store() -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(None, vike_secrets::CredentialScope::Full)
}

/// OANDA's production spec over no store, on the release seed's rows.
fn oanda() -> Box<dyn CredentialedKlinesSpec> {
    Box::new(vike_oanda::credentialed_klines::CredentialedKlinesSpecOanda::new(
        no_store(),
        &VenueUrls::seed("oanda"),
    ))
}

/// IG's production spec over no store, on the release seed's rows.
fn ig() -> Box<dyn CredentialedKlinesSpec> {
    Box::new(vike_ig::credentialed_klines::CredentialedKlinesSpecIg::new(
        no_store(),
        &VenueUrls::seed("ig"),
    ))
}

/// cTrader's production spec over no store and no rows, its accounts read as production reads them.
fn ctrader() -> Box<dyn CredentialedKlinesSpec> {
    use crate::data::accounts::AccountSourceStore;

    let accounts = AccountSourceStore::source(no_store());
    Box::new(vike_ctrader::credentialed_klines::CredentialedKlinesSpecCtrader::new(
        no_store(),
        Arc::new(move |venue: &str| accounts.directory(venue)),
        VenueUrls::default(),
    ))
}

/// **The advertisement is the registry**: what the production lane DISPATCHES is what the typed
/// table declares, lane by lane and in order (the client's rosters are pinned to the same table by
/// `kline_sources`' and `catalog`'s own tests).
#[test]
fn the_production_lane_advertises_the_registrys_rosters_in_order() {
    let facts = RestLaneBridge::new(false, &seeded_venue_settings()).facts();
    assert_eq!(facts.klines, owned(RosterLane::Klines));
    assert_eq!(facts.funding, owned(RosterLane::Funding));
    assert_eq!(facts.ticks, owned(RosterLane::Ticks));
    assert_eq!(facts.catalog, owned(RosterLane::Catalog));
    assert_eq!(facts.family, owned(RosterLane::Family));
    assert_eq!(facts.market_rules, owned(RosterLane::MarketRules));
    assert!(facts.options && facts.presence);
    assert!(facts.serves_any_fetch());
    assert!(
        facts.credentialed.is_empty(),
        "no credential store was handed over, so no credentialed lane is advertised"
    );
}

/// A credential store arms the credentialed lanes, and the registry's credentialed venues (oanda's, ig's
/// and ctrader's, in that order) are the ones advertised; a lane armed alone advertises its own venue alone.
#[test]
fn a_credential_store_arms_the_credentialed_lane() {
    let lane = RestLaneBridge::new(false, &seeded_venue_settings()).with_credential_store(
        no_store(),
        &seeded_venue_settings(),
        None,
    );
    assert_eq!(lane.facts().credentialed, owned(RosterLane::CredentialedKlines));
    assert_eq!(lane.facts().credentialed, ["oanda", "ig", "ctrader"]);

    let only_ig = RestLaneBridge::new(false, &seeded_venue_settings()).with_credentialed(ig());
    assert_eq!(only_ig.facts().credentialed, ["ig"]);
    let only_oanda =
        RestLaneBridge::new(false, &seeded_venue_settings()).with_credentialed(oanda());
    assert_eq!(only_oanda.facts().credentialed, ["oanda"]);
    let only_ctrader =
        RestLaneBridge::new(false, &seeded_venue_settings()).with_credentialed(ctrader());
    assert_eq!(only_ctrader.facts().credentialed, ["ctrader"]);
}

/// The presence words the lane itself answers: a keyless or unserved venue needs nothing; the
/// credentialed venue with no way to look is `NotChecked` (nothing was read, which is not `Absent`);
/// with a store handle over no settings directory it is `Absent`.
#[test]
fn the_presence_words_say_what_was_and_was_not_read() {
    let bare = RestLaneBridge::new(false, &seeded_venue_settings());
    assert_eq!(bare.credential_presence("binance"), CredentialPresence::NotNeeded);
    assert_eq!(bare.credential_presence("no-such-venue"), CredentialPresence::NotNeeded);
    assert_eq!(bare.credential_presence("oanda"), CredentialPresence::NotChecked);
    assert_eq!(bare.credential_presence("ig"), CredentialPresence::NotChecked);
    assert_eq!(bare.credential_presence("ctrader"), CredentialPresence::NotChecked);
    let armed = RestLaneBridge::new(false, &seeded_venue_settings()).with_credential_store(
        no_store(),
        &seeded_venue_settings(),
        None,
    );
    assert_eq!(armed.credential_presence("oanda"), CredentialPresence::Absent);
    assert_eq!(armed.credential_presence("ig"), CredentialPresence::Absent);
    assert_eq!(armed.credential_presence("ctrader"), CredentialPresence::Absent);
    assert_eq!(armed.credential_presence("binance"), CredentialPresence::NotNeeded);
}

/// A credentialed fetch the lane was not built for is a REFUSAL (the venue was not asked), for an
/// unarmed lane, for another venue and for a lane that is not klines.
#[test]
fn a_credentialed_fetch_the_lane_cannot_serve_is_refused() {
    let scope = ConnectionMemos::new();
    let ctx = FetchContext::new();
    let klines = |venue: &str| FetchWhat::Klines {
        venue: venue.to_string(),
        symbol: "EUR_USD".to_string(),
        interval: "5s".to_string(),
        start_ms: 0,
        end_ms: 1,
    };
    let unarmed = RestLaneBridge::new(false, &seeded_venue_settings());
    let armed = RestLaneBridge::new(false, &seeded_venue_settings()).with_credential_store(
        no_store(),
        &seeded_venue_settings(),
        None,
    );
    for (lane, what) in [
        (&unarmed, klines("oanda")),
        (&unarmed, klines("ig")),
        (&unarmed, klines("ctrader")),
        (&armed, klines("binance")),
        (&armed, klines("alpaca")),
        (&armed, FetchWhat::CatalogList { venue: "oanda".to_string() }),
        (&armed, FetchWhat::CatalogList { venue: "ig".to_string() }),
        (&armed, FetchWhat::CatalogList { venue: "ctrader".to_string() }),
    ] {
        let out = lane.fetch(&scope, &what, true, &ctx);
        assert!(matches!(out, Err(SourceError::Refused(_))), "{what:?}: {out:?}");
    }
    assert!(scope.is_empty(), "a refusal must not create the connection's credential slot");
}

/// A tick fetch for another venue and a family for another venue are refused, not routed.
#[test]
fn a_venue_the_lane_has_no_lane_for_is_refused() {
    let lane = RestLaneBridge::new(false, &seeded_venue_settings());
    let scope = ConnectionMemos::new();
    let ticks = FetchWhat::Ticks {
        venue: "binance".to_string(),
        symbol: "EURUSD".to_string(),
        start_ms: 0,
        end_ms: 1,
    };
    let out = lane.fetch(&scope, &ticks, false, &FetchContext::new());
    assert!(matches!(out, Err(SourceError::Refused(_))), "{out:?}");
    let err = lane.resolve_family(&scope, "binance", "btc-updown-5m", 0).expect_err("not served");
    assert!(err.contains("`binance`") && err.contains("polymarket"), "{err}");
}

/// **The production tick lane is bounded at the WIRE's span** (`GATEWAY_TICKS_MAX_SPAN_MS`, which the bridge
/// cannot name and is handed here): one millisecond past it is refused before the venue, and a window
/// exactly at it is asked of the venue (no `dukascopy` rows here, so the venue call answers the row's
/// refusal and nothing is dialled).
#[test]
fn the_production_tick_lane_is_bounded_at_the_wires_span() {
    let lane = RestLaneBridge::new(false, &BTreeMap::new());
    let scope = ConnectionMemos::new();
    let ticks = |end_ms: i64| FetchWhat::Ticks {
        venue: TICK_VENUE.to_string(),
        symbol: "EURUSD".to_string(),
        start_ms: 0,
        end_ms,
    };
    let past = lane.fetch(&scope, &ticks(GATEWAY_TICKS_MAX_SPAN_MS), false, &FetchContext::new());
    assert!(
        matches!(past, Err(SourceError::Refused(ref t)) if t.text().contains("at most 744")),
        "{past:?}"
    );
    let at = lane.fetch(&scope, &ticks(GATEWAY_TICKS_MAX_SPAN_MS - 1), false, &FetchContext::new());
    assert!(
        matches!(at, Err(SourceError::Fetch(ref t)) if t.text().contains("live history rest")),
        "{at:?}"
    );
}

/// The status seam: empty until the root wires a snapshot, then exactly the wired rows.
#[test]
fn the_budget_seam_is_empty_until_the_root_wires_a_snapshot() {
    assert!(RestLaneBridge::new(false, &seeded_venue_settings()).budget_counters().is_empty());
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
    let lane = RestLaneBridge::new(false, &seeded_venue_settings())
        .with_budget_snapshot(Arc::new(move || vec![wired.clone()]));
    assert_eq!(lane.budget_counters(), vec![row]);
}

/// **The credentialed advertisement keeps the registry's order whatever order the lanes are armed in**,
/// and arming a venue twice replaces its lane rather than adding a second one.
#[test]
fn the_credentialed_advertisement_keeps_the_registrys_order_whatever_the_arming_order() {
    let reversed = RestLaneBridge::new(false, &seeded_venue_settings())
        .with_credentialed(ctrader())
        .with_credentialed(ig())
        .with_credentialed(oanda());
    assert_eq!(reversed.facts().credentialed, owned(RosterLane::CredentialedKlines));
    let two = RestLaneBridge::new(false, &seeded_venue_settings())
        .with_credentialed(ctrader())
        .with_credentialed(oanda());
    assert_eq!(two.facts().credentialed, ["oanda", "ctrader"]);
    let twice = RestLaneBridge::new(false, &seeded_venue_settings())
        .with_credentialed(oanda())
        .with_credentialed(oanda());
    assert_eq!(twice.facts().credentialed, ["oanda"]);
}

/// **A cancelled credentialed fetch asks no venue and creates no connection slot**, for every credentialed
/// venue: the cancel is seen before anything is built or read, and the words say so.
#[test]
fn a_cancelled_credentialed_fetch_creates_no_slot_for_any_venue() {
    let armed = RestLaneBridge::new(false, &seeded_venue_settings()).with_credential_store(
        no_store(),
        &seeded_venue_settings(),
        None,
    );
    let scope = ConnectionMemos::new();
    let ctx = FetchContext::new();
    ctx.cancel();
    for venue in owned(RosterLane::CredentialedKlines) {
        let what = FetchWhat::Klines {
            venue: venue.clone(),
            symbol: "CS.D.EURUSD.MINI.IP".to_string(),
            interval: "1h".to_string(),
            start_ms: 0,
            end_ms: 1,
        };
        let out = armed.fetch(&scope, &what, true, &ctx);
        assert!(
            matches!(&out, Err(SourceError::Fetch(t))
                if t.text() == "gatewayhub fetch cancelled before the venue was asked"),
            "{venue}: {out:?}"
        );
    }
    assert!(scope.is_empty(), "a cancelled fetch must not create the connection's slot");
}

/// The IG ledger's clock in [`a_closed_ig_connection_says_what_the_week_has_left_once`]: a fixed instant.
fn ig_ledger_now() -> i64 {
    1_792_776_143_000
}

/// **A request ends when its connection closes, and the IG lane says what the week has left, ONCE**: a
/// connection whose source was built logs the ledger's closing line (at `info`) when its scope is
/// dropped, however many chunks it fetched; a connection refused before any source was built (an
/// unmeasured interval) logs nothing.
#[test]
fn a_closed_ig_connection_says_what_the_week_has_left_once() {
    use std::time::Duration;

    use vike_bridge_core::credentialed_klines::CredentialedKlinesSource;
    use vike_data::source::SourceError;
    use vike_ig::credentialed_klines::{AllowanceLedger, CredentialedKlinesSpecIg};
    use vike_ig::{Allowance, AllowanceGate, ConfigProvider};
    use vike_marketdata::Bar;

    struct Learner {
        gate: Arc<dyn AllowanceGate>,
    }
    impl CredentialedKlinesSource for Learner {
        fn fetch_until(
            &self,
            _symbol: &str,
            _interval: &str,
            _start_ms: i64,
            _end_ms: i64,
            _stop: &dyn Fn() -> bool,
        ) -> Result<Vec<Bar>, SourceError> {
            self.gate.learn(Allowance { remaining: 7_412, total: 10_000, resets_in_s: 441_000 });
            Ok(Vec::new())
        }
    }
    let spec = CredentialedKlinesSpecIg::with_source(
        no_store(),
        &VenueUrls::seed("ig"),
        Arc::new(
            |_config: ConfigProvider,
             gate: Arc<dyn AllowanceGate>|
             -> Box<dyn CredentialedKlinesSource> { Box::new(Learner { gate }) },
        ),
    )
    .with_ledger(Arc::new(AllowanceLedger::with(Duration::ZERO, ig_ledger_now)));
    let rest =
        RestLaneBridge::new(false, &seeded_venue_settings()).with_credentialed(Box::new(spec));
    let klines = |interval: &str| FetchWhat::Klines {
        venue: "ig".to_string(),
        symbol: "CS.D.EURUSD.MINI.IP".to_string(),
        interval: interval.to_string(),
        start_ms: 0,
        end_ms: 1,
    };
    let ctx = FetchContext::new();

    let ((), lines) = vike_bridge_core::credential_fixture::under_capture(|| {
        let served = ConnectionMemos::new();
        for _ in 0..2 {
            rest.fetch(&served, &klines("1h"), true, &ctx).expect("a chunk");
        }
        drop(served);
        let refused = ConnectionMemos::new();
        rest.fetch(&refused, &klines("1m"), true, &ctx).expect_err("an unmeasured interval");
        drop(refused);
    });

    let ended: Vec<&String> =
        lines.iter().filter(|l| l.contains("ig history request ended")).collect();
    assert_eq!(ended.len(), 1, "one line for the one connection that fetched: {lines:?}");
    assert!(
        ended[0].starts_with("INFO")
            && ended[0].contains(
                "IG allowance left: 7412 of 10000 points. The allowance resets in 5 days 2 hours"
            ),
        "{}",
        ended[0]
    );
}

/// **A closed cTrader connection drops what it read the grant through**: the source (and the provider
/// that holds the connection's grant memo) lives as long as the connection's scope and no longer.
#[test]
fn a_closed_ctrader_connection_drops_its_grant_provider() {
    use std::sync::{Mutex, PoisonError, Weak};

    use vike_ctrader::credentialed_klines::CredentialedKlinesSpecCtrader;
    use vike_ctrader::history::{HistoryConfigError, HistoryConfigProvider};
    use vike_ctrader::transport::connection::ConnectionConfig;
    use vike_data::source::SourceError;
    use vike_marketdata::Bar;
    use vike_secrets::account_directory::AccountDirectory;

    type ProviderFn = dyn Fn() -> Result<ConnectionConfig, HistoryConfigError> + Send + Sync;
    /// A source that holds its provider, as the real one does, and asks it nothing.
    struct Quiet {
        _config: HistoryConfigProvider,
    }
    impl KlineSource for Quiet {
        fn venue(&self) -> &str {
            "ctrader"
        }
        fn fetch(&self, _: &str, _: &str, _: i64, _: i64) -> Result<Vec<Bar>, SourceError> {
            Ok(Vec::new())
        }
    }
    let providers: Arc<Mutex<Vec<Weak<ProviderFn>>>> = Arc::new(Mutex::new(Vec::new()));
    let held = Arc::clone(&providers);
    let spec = CredentialedKlinesSpecCtrader::with_source(
        no_store(),
        Arc::new(|_venue: &str| AccountDirectory::unread()),
        VenueUrls::seed("ctrader"),
        Arc::new(move |config: HistoryConfigProvider| -> Box<dyn KlineSource> {
            held.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::downgrade(&config));
            Box::new(Quiet { _config: config })
        }),
    );
    let rest =
        RestLaneBridge::new(false, &seeded_venue_settings()).with_credentialed(Box::new(spec));
    let what = FetchWhat::Klines {
        venue: "ctrader".to_string(),
        symbol: "EURUSD".to_string(),
        interval: "1m".to_string(),
        start_ms: 0,
        end_ms: 60_000,
    };
    let built = || providers.lock().unwrap_or_else(PoisonError::into_inner).len();
    let alive = || {
        let held = providers.lock().unwrap_or_else(PoisonError::into_inner);
        held.iter().filter(|p| p.upgrade().is_some()).count()
    };

    let scope = ConnectionMemos::new();
    rest.fetch(&scope, &what, true, &FetchContext::new()).expect("a fetch");
    rest.fetch(&scope, &what, true, &FetchContext::new()).expect("a second fetch");
    assert_eq!((built(), alive()), (1, 1), "one source per connection, alive while it is open");
    drop(scope);
    assert_eq!(alive(), 0, "the grant's provider outlived the connection that read it");
}

/// **A credentialed fetch for ctrader reaches the ctrader lane and no other**, through the real
/// `RestLaneBridge` dispatch (moved here from the cTrader lane's own suite with the lane's venue code): the
/// lane armed alone answers its own venue's klines as `FetchRows::Bars`, reading the grant for the
/// connection that asked, and refuses every other venue's (the venue was not asked) reading nothing.
#[test]
fn a_credentialed_fetch_for_ctrader_reaches_the_ctrader_lane_only() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use vike_ctrader::credentialed_klines::CredentialedKlinesSpecCtrader;
    use vike_ctrader::history::HistoryConfigProvider;
    use vike_data::source::SourceError;
    use vike_marketdata::Bar;
    use vike_secrets::account_directory::AccountDirectory;

    /// A source that asks its provider the way the real one does when a session opens, and answers an
    /// empty window whatever the provider said.
    struct Asking {
        config: HistoryConfigProvider,
    }
    impl KlineSource for Asking {
        fn venue(&self) -> &str {
            "ctrader"
        }
        fn fetch(&self, _: &str, _: &str, _: i64, _: i64) -> Result<Vec<Bar>, SourceError> {
            let _ = (self.config)();
            Ok(Vec::new())
        }
    }
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    let spec = CredentialedKlinesSpecCtrader::with_source(
        no_store(),
        Arc::new(move |_venue: &str| {
            counted.fetch_add(1, Ordering::SeqCst);
            AccountDirectory::unread()
        }),
        VenueUrls::seed("ctrader"),
        Arc::new(|config: HistoryConfigProvider| -> Box<dyn KlineSource> {
            Box::new(Asking { config })
        }),
    );
    let rest =
        RestLaneBridge::new(false, &seeded_venue_settings()).with_credentialed(Box::new(spec));
    let klines = |venue: &str| FetchWhat::Klines {
        venue: venue.to_string(),
        symbol: "EURUSD".to_string(),
        interval: "1m".to_string(),
        start_ms: 0,
        end_ms: 60_000,
    };
    let scope = ConnectionMemos::new();
    let served = rest.fetch(&scope, &klines("ctrader"), true, &FetchContext::new());
    assert!(matches!(&served, Ok(FetchRows::Bars(bars)) if bars.is_empty()), "{served:?}");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "the lane read the grant for the connection that asked"
    );
    for other in ["oanda", "ig"] {
        let refused = rest.fetch(&scope, &klines(other), true, &FetchContext::new());
        assert!(matches!(refused, Err(SourceError::Refused(_))), "{other}: {refused:?}");
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1, "a refusal for another venue read nothing");
}
