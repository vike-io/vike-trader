//! `LeaseFacts::exposure_cap_armed`: the answer the control door's LIVE-account gate reads
//! (`plan_submit_symbol`: a live account takes a new symbol only with an account-wide ceiling armed).
//! The production binding is `bind_exposure_cap`'s closure in `tradehub_cli/live_runtime.rs`; what
//! this proves is the seam's own contract.

use super::*;

/// Unbound, NO account counts as capped: a live account is refused a new symbol rather than admitted
/// on a ceiling nobody checked.
#[test]
fn an_unbound_registry_arms_no_account() {
    let facts = LeaseFacts::default();
    assert!(!facts.exposure_cap_armed("bybit"));
    assert!(!facts.exposure_cap_armed("okx#ALT"));
}

/// Bound, the closure answers per ROUTE KEY: the account with a ceiling is armed, its sibling is not,
/// and the binding is made once (a second bind does not replace the first).
#[test]
fn a_bound_registry_answers_per_route_key_and_binds_once() {
    let facts = LeaseFacts::default();
    facts.bind_exposure_cap(|route_key| route_key == "bybit#ALT");
    assert!(facts.exposure_cap_armed("bybit#ALT"));
    assert!(!facts.exposure_cap_armed("bybit"), "the default account has no ceiling of its own");
    facts.bind_exposure_cap(|_| true);
    assert!(!facts.exposure_cap_armed("bybit"), "the first binding stands");
}

/// The directory a start reads: one ACTIVE `live` row of `venue`/`label`, with `cap` as its `max_exposure`.
fn rows_with(
    venue: &str,
    label: &AccountLabel,
    cap: Option<f64>,
    active: bool,
) -> AccountDirectory {
    vike_tradehub_core::AdapterPolicy::default()
        .with_account_row(vike_secrets::Account {
            id: 1,
            venue: venue.to_string(),
            tier: vike_model::AccountTier::Live,
            label: label.text().map(str::to_string),
            venue_account_id: None,
            parent_id: None,
            active,
            last_verified_at: None,
            max_exposure: cap.and_then(vike_secrets::MaxExposure::new),
            leverage: None,
        })
        .accounts
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The ceiling an engine is built under is the box-wide row OR the account's own ACTIVE row; rows that
/// were not read arm nothing.
#[test]
fn a_ceiling_is_the_policy_row_or_the_accounts_own_active_row() {
    let armed = |policy, rows: &AccountDirectory| rows_arm_a_cap(policy, rows, "bybit", &alt());
    assert!(armed(true, &AccountDirectory::unread()), "the box-wide row needs no account row");
    assert!(!armed(false, &AccountDirectory::unread()), "unread rows arm nothing");
    assert!(armed(false, &rows_with("bybit", &alt(), Some(5000.0), true)));
    assert!(!armed(false, &rows_with("bybit", &alt(), None, true)));
    assert!(
        !armed(false, &rows_with("bybit", &alt(), Some(5000.0), false)),
        "an inactive row binds nothing"
    );
    assert!(!armed(false, &rows_with("bybit", &AccountLabel::Default, Some(5000.0), true)));
}

/// **An account hot-started with a ceiling row the boot never saw is armed for the door** (an account
/// inactive at boot, or whose `max_exposure` was set later): the boot's rows say "no cap", the engine
/// the start built carries one, and a live account must not be refused every new symbol for want of a
/// restart decision 0169 rules out.
#[test]
fn a_hot_started_account_with_a_cap_the_boot_did_not_see_is_armed() {
    let facts = LeaseFacts::default();
    facts.bind_exposure_cap(|_| false);
    assert!(!facts.exposure_cap_armed("bybit#ALT"));
    facts.engine_starting("bybit#ALT", true);
    assert!(facts.exposure_cap_armed("bybit#ALT"));
    assert!(!facts.exposure_cap_armed("bybit"), "a sibling keeps the boot's answer");
}

/// **A hot-restarted account whose ceiling was removed is NOT armed for the door** (the unsafe
/// direction): the boot's rows said "capped", the new engine has no ceiling, and a live account must
/// not be admitted sixteen symbols on the boot's word. A later start with a ceiling arms it again.
#[test]
fn a_hot_restarted_account_whose_cap_is_gone_is_not_armed_by_the_boots_rows() {
    let facts = LeaseFacts::default();
    facts.bind_exposure_cap(|_| true);
    assert!(facts.exposure_cap_armed("bybit#ALT"));
    facts.engine_starting("bybit#ALT", false);
    assert!(
        !facts.exposure_cap_armed("bybit#ALT"),
        "the engine's own state outranks the boot's rows"
    );
    facts.engine_starting("bybit#ALT", true);
    assert!(facts.exposure_cap_armed("bybit#ALT"));
}

/// **Stopping an account forgets what the node admitted into its engine** (decision 0169 stop then
/// start): the next engine has none, so a stale entry would make `release` return early and `price` take
/// the uncounted path past [`MAX_WATCHED`]. A symbol no other account of the venue admitted goes to the
/// idle reaper (its pump is not permanent); one another account admitted keeps its pump.
#[test]
fn a_stopped_account_forgets_its_admitted_symbols_and_hands_their_pumps_to_the_reaper() {
    let facts = LeaseFacts::default();
    facts.admitted.lock().unwrap().extend([
        key("bybit#ALT", "SOLUSDT"),
        key("bybit", "SOLUSDT"),
        key("bybit#ALT", "ETHUSDT"),
    ]);
    facts.engine_stopped("bybit#ALT");
    assert_eq!(*facts.admitted.lock().unwrap(), HashSet::from([key("bybit", "SOLUSDT")]));
    assert_eq!(entry(&facts, "bybit", "SOLUSDT"), None, "the default account still admits it");
    assert!(matches!(entry(&facts, "bybit", "ETHUSDT"), Some(Sub::Idle(_))));
}

/// **Stop, start without a ceiling, start with one — the door follows the engine, never a stale answer.**
/// A stopped account reads "no ceiling" at once (the boot's rows, which said capped, no longer speak for
/// it); a start announces its ceiling BEFORE the engine exists; a start that builds nothing puts the
/// previous answer back; and every stop and start moves the account's generation.
#[test]
fn the_door_follows_stop_and_start_and_never_a_stale_ceiling() {
    let facts = LeaseFacts::default();
    facts.bind_exposure_cap(|_| true);
    assert_eq!(facts.generation("bybit#ALT"), 0);
    assert!(facts.exposure_cap_armed("bybit#ALT"), "the boot's engine, capped by the boot's rows");

    facts.engine_stopped("bybit#ALT");
    assert!(!facts.exposure_cap_armed("bybit#ALT"), "stopped: the door refuses a live new symbol");
    assert_eq!(
        facts.engines.lock().unwrap()["bybit#ALT"].cap,
        Some(false),
        "the cap entry is cleared"
    );
    assert_eq!(facts.generation("bybit#ALT"), 1);

    let before = facts.engine_starting("bybit#ALT", false);
    assert!(!facts.exposure_cap_armed("bybit#ALT"), "started without a ceiling: refuses");
    assert_eq!(facts.generation("bybit#ALT"), 2);

    facts.engine_start_refused("bybit#ALT", before);
    assert!(!facts.exposure_cap_armed("bybit#ALT"), "a refused start restores the stopped answer");

    facts.engine_starting("bybit#ALT", true);
    assert!(facts.exposure_cap_armed("bybit#ALT"), "started with a ceiling: admits");
    assert_eq!(facts.generation("bybit#ALT"), 3, "every start moves it");
    assert_eq!(facts.generation("bybit"), 0, "a sibling's generation is its own");
}

fn key(venue: &str, symbol: &str) -> (String, String) {
    (venue.to_string(), symbol.to_string())
}

fn entry(facts: &LeaseFacts, venue: &str, symbol: &str) -> Option<Sub> {
    facts.watched.lock().unwrap().get(&key(venue, symbol)).copied()
}

// ---- the negative cache is bounded ----------------------------------------------------------------

/// One refusal per distinct refused name is remembered, and no more than [`MAX_FAILED`] at once: a
/// full map first drops what expired, a refusal that still does not fit is not remembered (the next
/// order asks again), and a refusal already in the map is always refreshed.
#[test]
fn the_negative_cache_never_outgrows_its_cap_and_drops_the_expired_first() {
    let mut failed: HashMap<(String, String), (Instant, String)> = HashMap::new();
    for i in 0..MAX_FAILED {
        remember_refusal(&mut failed, key("bybit", &format!("S{i}")), "no".into());
    }
    assert_eq!(failed.len(), MAX_FAILED);
    remember_refusal(&mut failed, key("bybit", "ONE-MORE"), "no".into());
    assert_eq!(failed.len(), MAX_FAILED, "all fresh: the newcomer is not remembered");
    assert!(!failed.contains_key(&key("bybit", "ONE-MORE")));

    remember_refusal(&mut failed, key("bybit", "S0"), "still no".into());
    assert_eq!(failed[&key("bybit", "S0")].1, "still no", "a known name is refreshed");
    assert_eq!(failed.len(), MAX_FAILED);

    let long_ago = Instant::now().checked_sub(NEGATIVE_TTL + Duration::from_secs(1)).unwrap();
    for (at, _) in failed.values_mut() {
        *at = long_ago;
    }
    remember_refusal(&mut failed, key("bybit", "ONE-MORE"), "no".into());
    assert_eq!(failed.len(), 1, "the expired were dropped, the newcomer stands");
}

// ---- the price pumps of symbols nobody admitted are bounded ------------------------------------------

/// A symbol the door let go of keeps its place (its pump is open) until the pump is closed, and a full
/// set is refused: the bound is on OPEN pumps, not on in-flight asks. Asking again takes an idle one back.
#[test]
fn a_released_pump_keeps_its_place_until_it_is_closed_and_a_re_ask_takes_it_back() {
    let facts = LeaseFacts::default();
    for i in 0..MAX_WATCHED {
        let k = key("bybit", &format!("S{i}"));
        assert!(matches!(facts.claim_pump(&k), Ok(Claim::Subscribe)));
        facts.watched.lock().unwrap().insert(k, Sub::Open);
    }
    let full = facts.claim_pump(&key("bybit", "NEW")).err().expect("a full set refuses");
    assert!(full.contains(&MAX_WATCHED.to_string()), "{full}");

    facts.release("bybit", "bybit", "S0");
    assert!(
        matches!(entry(&facts, "bybit", "S0"), Some(Sub::Idle(_))),
        "released: idle, still counted"
    );
    assert!(
        facts.claim_pump(&key("bybit", "NEW")).is_err(),
        "a released pump still holds its place (no feed is bound here to close it)"
    );
    assert!(matches!(facts.claim_pump(&key("bybit", "S0")), Ok(Claim::Held)));
    assert_eq!(entry(&facts, "bybit", "S0"), Some(Sub::Open), "the re-ask put it back in use");
}

/// A pump the reaper is closing right now answers "none yet" (the retry subscribes afresh), and an
/// ADMITTED symbol's entry is dropped at once, never left to be reaped.
#[test]
fn a_closing_pump_asks_for_a_retry_and_an_admitted_symbol_leaves_the_set() {
    let facts = LeaseFacts::default();
    let k = key("bybit", "ETHUSDT");
    facts.watched.lock().unwrap().insert(k.clone(), Sub::Closing);
    assert!(matches!(facts.claim_pump(&k), Ok(Claim::Closing)));

    facts.watched.lock().unwrap().insert(k, Sub::Idle(Instant::now()));
    facts.admitted("bybit", "bybit", "ETHUSDT");
    assert_eq!(entry(&facts, "bybit", "ETHUSDT"), None, "admitted: the engine pump, not counted");
    assert_eq!(facts.close_idle_pumps(), 0);
}

/// `release` moves only a pump IN USE to idle, and not at all for a symbol its account admitted.
#[test]
fn release_idles_a_pump_in_use_and_never_one_an_account_admitted() {
    let facts = LeaseFacts::default();
    facts.watched.lock().unwrap().insert(key("bybit", "A"), Sub::Opening);
    facts.release("bybit", "bybit", "A");
    assert_eq!(entry(&facts, "bybit", "A"), Some(Sub::Opening), "an ask in flight is not idle");

    facts.watched.lock().unwrap().insert(key("bybit", "B"), Sub::Open);
    facts.admitted.lock().unwrap().insert(("bybit".to_string(), "B".to_string()));
    facts.release("bybit", "bybit", "B");
    assert_eq!(entry(&facts, "bybit", "B"), Some(Sub::Open), "an admitted symbol is never idled");
}

/// A symbol ANOTHER account of the venue admitted is the engine pump too: `admitted_on_venue` sees it
/// across route keys and labels, and not across venues.
#[test]
fn an_admission_by_any_account_of_the_venue_protects_the_pump() {
    let facts = LeaseFacts::default();
    facts.admitted.lock().unwrap().insert(("bybit#ALT".to_string(), "ETHUSDT".to_string()));
    assert!(facts.admitted_on_venue("bybit", "ETHUSDT"));
    assert!(!facts.admitted_on_venue("bybit", "SOLUSDT"));
    assert!(!facts.admitted_on_venue("binance", "ETHUSDT"));
}

// ---- an idle pump under a venue gap ------------------------------------------------------------------

/// A scripted dial: it records the quote lanes the feed opens and closes, and fails nothing.
struct Lanes(Arc<Mutex<Vec<String>>>, u64);

impl vike_data::DataClient for Lanes {
    fn subscribe_bars(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn subscribe_quotes(
        &mut self,
        symbol: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        self.1 += 1;
        self.0.lock().unwrap().push(format!("quotes {symbol}"));
        Ok(vike_data::SubscriptionId(self.1))
    }
    fn subscribe_trades(
        &mut self,
        _: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn subscribe_book(
        &mut self,
        _: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn unsubscribe(&mut self, id: vike_data::SubscriptionId) {
        self.0.lock().unwrap().push(format!("unsubscribe {}", id.0));
    }
    fn shutdown(&mut self) {}
}

/// What [`idle_pump_under_a_gap`] builds: the registry, the feed and tap it keeps alive, and the dial's log.
type IdlePump =
    (LeaseFacts, crate::feeds::QuoteFeed, Arc<crate::feeds::PriceTap>, Arc<Mutex<Vec<String>>>);

/// A registry whose `venue` has a price feed with `symbol`'s quote lane open and a VENUE-source gap open
/// on it (a stream whose dial keeps failing: `GapStart` from every refused open, never a `Live`), and the
/// door's entry for it idle since long enough to be reaped.
fn idle_pump_under_a_gap(venue: &str, symbol: &str) -> IdlePump {
    use vike_model::StreamStatus;
    let log = Arc::new(Mutex::new(Vec::new()));
    let tap = Arc::new(crate::feeds::PriceTap::default());
    let feed = crate::feeds::QuoteFeed::new(
        crate::feeds::Tracked::new(Box::new(Lanes(Arc::clone(&log), 0))),
        Arc::clone(&tap),
    );
    let watcher = feed.watcher();
    watcher.subscribe_ticks(symbol, 0.00001).expect("the lane opens");
    tap.note_status(venue, symbol, "quotes", StreamStatus::GapStart { at_ts_ms: 1 });
    assert!(watcher.gap_open(venue, symbol));
    let facts = LeaseFacts::default();
    facts.bind_prices(HashMap::from([(venue.to_string(), watcher)]));
    let long_ago = Instant::now().checked_sub(IDLE_TTL + Duration::from_secs(1)).unwrap();
    facts.watched.lock().unwrap().insert(key(venue, symbol), Sub::Idle(long_ago));
    (facts, feed, tap, log)
}

/// **A pair whose stream cannot connect does not keep its place for ever** (oanda: a fence-passing pair whose
/// `/pricing/stream` open keeps failing, the venue's stream cap or a stuck 429, redials every few seconds and
/// keeps a venue gap open). OANDA's row is `SessionBounded`, so no link dead-man window exists for the gap to
/// strand: the reaper closes the idle pump, the lane is unsubscribed (its redials stop), the tap forgets the
/// gap, and the place under [`MAX_WATCHED`] is free again.
#[test]
fn an_idle_pump_under_a_gap_is_reaped_where_no_dead_man_window_exists() {
    let (facts, _feed, tap, log) = idle_pump_under_a_gap("oanda", "GBPUSD");
    assert_eq!(facts.close_idle_pumps(), 1, "the place was freed");
    assert_eq!(entry(&facts, "oanda", "GBPUSD"), None);
    assert_eq!(
        *log.lock().unwrap(),
        ["quotes GBPUSD".to_string(), "unsubscribe 1".to_string()],
        "the lane was closed, so the stream stops redialling"
    );
    assert!(!tap.gap_open("oanda", "GBPUSD"), "a re-subscribe starts clean");

    // …and a full set of such pumps no longer refuses the next pair.
    let (facts, _feed, _tap, _log) = idle_pump_under_a_gap("oanda", "GBPUSD");
    for i in 0..MAX_WATCHED - 1 {
        facts.watched.lock().unwrap().insert(key("oanda", &format!("S{i}")), Sub::Open);
    }
    assert!(matches!(facts.claim_pump(&key("oanda", "NEW")), Ok(Claim::Subscribe)));
}

/// **An armed venue keeps the rule**: binance's `Armed` row means the core holds a window for an open gap,
/// and a pump closed under it would leave that window without its `Live` (cancel + HALT after the grace).
#[test]
fn an_idle_pump_under_a_gap_is_kept_where_a_dead_man_window_exists() {
    let (facts, _feed, tap, log) = idle_pump_under_a_gap("binance", "ETHUSDT");
    assert_eq!(facts.close_idle_pumps(), 0, "held by the gap");
    assert!(matches!(entry(&facts, "binance", "ETHUSDT"), Some(Sub::Idle(_))));
    assert_eq!(*log.lock().unwrap(), ["quotes ETHUSDT".to_string()], "nothing was closed");
    assert!(tap.gap_open("binance", "ETHUSDT"));
    for venue in ["hyperliquid", "deribit", "alpaca"] {
        assert!(gap_holds_pump(venue), "{venue}: an Inert row may be armed for a hub link");
    }
    assert!(gap_holds_pump("binance") && !gap_holds_pump("oanda") && !gap_holds_pump("ig"));
}

// ---- a started lane twin's price lane is opened ahead of its first order ------------------------------

/// A registry with `binance`'s price feed bound to a scripted dial (it records the lanes opened), and the
/// facts of the twin's own symbol already looked up on its lease (the cache stands in for the gatewayhub).
fn registry_with_twin_facts() -> (LeaseFacts, crate::feeds::QuoteFeed, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let feed = crate::feeds::QuoteFeed::new(
        crate::feeds::Tracked::new(Box::new(Lanes(Arc::clone(&log), 0))),
        Arc::new(crate::feeds::PriceTap::default()),
    );
    let facts = LeaseFacts::default();
    facts.bind_prices(HashMap::from([("binance".to_string(), feed.watcher())]));
    keep_twin_facts(&facts);
    (facts, feed, log)
}

/// Keep the twin's own symbol's facts as `facts`' answer (the cache stands in for the gatewayhub).
fn keep_twin_facts(facts: &LeaseFacts) {
    facts.answers.lock().unwrap().insert(
        key("binance#PERP", "BTCUSDT.P"),
        (
            Instant::now(),
            SymbolAdmission {
                route_key: "binance#PERP".into(),
                symbol: "BTCUSDT.P".into(),
                grid: SymbolGrid {
                    tick_size: Some(0.1),
                    lot_size: Some(0.001),
                    ..SymbolGrid::default()
                },
                contract_value: None,
                initial_mark: None,
                quote_ccy: None,
                quote_rate: None,
                rate_as_of_ms: None,
                im_requirement: None,
            },
        ),
    );
}

/// **`warm_price` opens the twin's own symbol's pump once, before any order asks**: the venue's feed dial gets
/// the symbol's lane, the door's set counts it as in use, and a second warm (a restart of the twin) opens
/// nothing more. Without it the first opening order after a hot start paid the facts look-up, the datahub dial
/// and the pump's first frame inside the door's hold.
#[test]
fn warming_a_lane_twins_price_opens_its_pump_once() {
    let (facts, _feed, log) = registry_with_twin_facts();
    facts.warm_price("binance#PERP", "binance", "BTCUSDT.P");
    assert_eq!(*log.lock().unwrap(), ["quotes BTCUSDT.P".to_string()], "the lane is open");
    assert_eq!(entry(&facts, "binance", "BTCUSDT.P"), Some(Sub::Open), "counted, in use");

    facts.warm_price("binance#PERP", "binance", "BTCUSDT.P");
    assert_eq!(log.lock().unwrap().len(), 1, "a second warm finds the pump open");
}

/// **Another account's refused order cannot idle the twin's price lane** (v0.1.80 review b). A second binance
/// account asking for `BTCUSDT.P` and refused releases the symbol, and `release` moved the shared pump to idle
/// because nothing there knew the twin held it: 30 s later the reaper closed the lane under the running twin.
/// Built on the twin's `warmed` record, the lane survives the release and the reaper while the twin runs, and
/// goes back to the reaper when the twin stops.
#[test]
fn another_accounts_refused_order_cannot_idle_a_running_twins_price_lane() {
    let (facts, _feed, log) = registry_with_twin_facts();
    facts.set_idle_ttl(Duration::ZERO);
    facts.warm_price("binance#PERP", "binance", "BTCUSDT.P");

    facts.release("binance#ALT", "binance", "BTCUSDT.P");
    assert_eq!(
        entry(&facts, "binance", "BTCUSDT.P"),
        Some(Sub::Open),
        "not idled by the other account"
    );
    assert_eq!(facts.close_idle_pumps(), 0, "nothing for the reaper to close");
    assert_eq!(*log.lock().unwrap(), ["quotes BTCUSDT.P".to_string()], "the lane stays open");

    facts.engine_stopped("binance#PERP");
    assert!(matches!(entry(&facts, "binance", "BTCUSDT.P"), Some(Sub::Idle(_))));
    assert_eq!(facts.close_idle_pumps(), 1, "the stopped twin hands its lane to the reaper");
    assert_eq!(
        *log.lock().unwrap(),
        ["quotes BTCUSDT.P".to_string(), "unsubscribe 1".to_string()],
        "…which closes it"
    );
}

/// A warm that cannot be done (no lease for the account, no price feed for the venue) is best effort: it
/// opens nothing, leaves no place held under [`MAX_WATCHED`], and the door's own ask stands.
#[test]
fn a_warm_that_cannot_be_done_opens_nothing_and_holds_no_place() {
    let (facts, _feed, log) = registry_with_twin_facts();
    facts.warm_price("binance#PERP", "binance", "ETHUSDT.P");
    assert!(
        log.lock().unwrap().is_empty(),
        "no facts and no lease for that symbol: nothing opened"
    );
    assert_eq!(entry(&facts, "binance", "ETHUSDT.P"), None, "a failed ask forgets its place");

    let unbound = LeaseFacts::default();
    unbound.warm_price("binance#PERP", "binance", "BTCUSDT.P");
    assert_eq!(entry(&unbound, "binance", "BTCUSDT.P"), None, "no price feed for the venue");
}

/// **A lane twin that stops hands back the pump it warmed.** The twin's own symbol is admitted by nobody, so
/// `engine_stopped` found nothing of it in `admitted` and the pump held its place under [`MAX_WATCHED`] and a
/// datahub subscription until the session ended. Now the stop idles it, the reaper closes it, and the place is
/// free; a pump another twin warmed too, or an account admitted, stays open for its holder.
#[test]
fn a_stopped_lane_twin_hands_back_the_pump_it_warmed() {
    let (facts, _feed, log) = registry_with_twin_facts();
    facts.warm_price("binance#PERP", "binance", "BTCUSDT.P");
    assert_eq!(entry(&facts, "binance", "BTCUSDT.P"), Some(Sub::Open));
    facts.engine_stopped("binance#PERP");
    assert!(
        matches!(entry(&facts, "binance", "BTCUSDT.P"), Some(Sub::Idle(_))),
        "the stop gave the pump to the reaper"
    );
    assert!(facts.warmed.lock().unwrap().is_empty(), "nothing is kept warm for a stopped engine");
    facts.set_idle_ttl(Duration::ZERO);
    assert_eq!(facts.close_idle_pumps(), 1, "the place was freed");
    assert_eq!(entry(&facts, "binance", "BTCUSDT.P"), None);
    assert_eq!(
        *log.lock().unwrap(),
        ["quotes BTCUSDT.P".to_string(), "unsubscribe 1".to_string()],
        "the datahub subscription was closed"
    );

    // Started again: the next warm opens it afresh (the restart path of an account follow).
    facts.warm_price("binance#PERP", "binance", "BTCUSDT.P");
    assert_eq!(entry(&facts, "binance", "BTCUSDT.P"), Some(Sub::Open));
    assert_eq!(log.lock().unwrap().len(), 3);
}

/// **A stop releases only a pump nobody else holds**: a sibling twin that warmed the same symbol keeps it
/// until the LAST one stops, and an account that admitted the symbol keeps it for good. A stop of an
/// account that warmed nothing touches nothing.
#[test]
fn a_stopped_twin_leaves_a_pump_another_twin_or_an_admission_holds() {
    let (facts, _feed, _log) = registry_with_twin_facts();
    facts.warm_price("binance#PERP", "binance", "BTCUSDT.P");
    facts.warmed.lock().unwrap().insert(("binance#ALT".into(), "BTCUSDT.P".into()));
    facts.engine_stopped("binance#PERP");
    assert_eq!(entry(&facts, "binance", "BTCUSDT.P"), Some(Sub::Open), "the sibling twin holds it");
    facts.engine_stopped("binance#OTHER");
    assert_eq!(entry(&facts, "binance", "BTCUSDT.P"), Some(Sub::Open), "it warmed nothing");
    facts.engine_stopped("binance#ALT");
    assert!(
        matches!(entry(&facts, "binance", "BTCUSDT.P"), Some(Sub::Idle(_))),
        "the last one left"
    );

    let (facts, _feed, _log) = registry_with_twin_facts();
    facts.warm_price("binance#PERP", "binance", "BTCUSDT.P");
    facts.admitted.lock().unwrap().insert(("binance#ALT".into(), "BTCUSDT.P".into()));
    facts.engine_stopped("binance#PERP");
    assert_eq!(entry(&facts, "binance", "BTCUSDT.P"), Some(Sub::Open), "an admission holds it");
}

/// A dial whose quote subscribe first runs `during`: the account's engine stopping WHILE the warm is in flight.
struct StopsDuringOpen {
    log: Arc<Mutex<Vec<String>>>,
    during: Option<Box<dyn FnOnce() + Send>>,
}

impl vike_data::DataClient for StopsDuringOpen {
    fn subscribe_bars(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn subscribe_quotes(
        &mut self,
        symbol: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        if let Some(during) = self.during.take() {
            during();
        }
        self.log.lock().unwrap().push(format!("quotes {symbol}"));
        Ok(vike_data::SubscriptionId(1))
    }
    fn subscribe_trades(
        &mut self,
        _: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn subscribe_book(
        &mut self,
        _: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn unsubscribe(&mut self, id: vike_data::SubscriptionId) {
        self.log.lock().unwrap().push(format!("unsubscribe {}", id.0));
    }
    fn shutdown(&mut self) {}
}

/// **FORCED INTERLEAVING: a stop that lands while the warm is subscribing does not strand the pump.** The
/// warm's own thread opens the pump AFTER the stop has already looked for what to hand back; it sees the
/// engine's generation moved and hands the pump back itself, instead of leaving a lane no stop will ever
/// find. MUTATION PROOF: drop the generation check in `keep_warmed` and the pump stays `Open`.
#[test]
fn a_stop_that_lands_while_the_warm_subscribes_does_not_strand_the_pump() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let facts = Arc::new(LeaseFacts::default());
    let stopper = Arc::clone(&facts);
    let dial = StopsDuringOpen {
        log: Arc::clone(&log),
        during: Some(Box::new(move || stopper.engine_stopped("binance#PERP"))),
    };
    let feed = crate::feeds::QuoteFeed::new(
        crate::feeds::Tracked::new(Box::new(dial)),
        Arc::new(crate::feeds::PriceTap::default()),
    );
    facts.bind_prices(HashMap::from([("binance".to_string(), feed.watcher())]));
    keep_twin_facts(&facts);

    facts.warm_price("binance#PERP", "binance", "BTCUSDT.P");
    assert!(
        matches!(entry(&facts, "binance", "BTCUSDT.P"), Some(Sub::Idle(_))),
        "the warm handed the pump back: its engine had stopped"
    );
    assert!(facts.warmed.lock().unwrap().is_empty());
    facts.set_idle_ttl(Duration::ZERO);
    assert_eq!(facts.close_idle_pumps(), 1);
    assert_eq!(
        *log.lock().unwrap(),
        ["quotes BTCUSDT.P".to_string(), "unsubscribe 1".to_string()],
        "the reaper closed it"
    );
}

// ---- the boot window ---------------------------------------------------------------------------------

fn unavailable() -> SymbolFactsError {
    SymbolFactsError::Unavailable("no answer".into())
}

/// Outside the window a round trip gets no bound of its own; inside it gets the boot bound; after the
/// guard drops it gets none again (the runtime door keeps its behaviour). A registry told no boot bound
/// opens no window at all.
#[test]
fn only_the_boot_window_bounds_a_round_trip() {
    let facts = LeaseFacts::default();
    let seen = Mutex::new(Vec::new());
    let ask = |facts: &LeaseFacts| {
        let _ = facts.ask("bybit", |bound| {
            seen.lock().unwrap().push(bound);
            Ok(())
        });
    };
    ask(&facts);
    {
        let _no_wait = facts.begin_boot();
        ask(&facts);
    }
    facts.set_boot_wait(Duration::from_secs(7));
    {
        let _window = facts.begin_boot();
        ask(&facts);
    }
    ask(&facts);
    assert_eq!(*seen.lock().unwrap(), [None, None, Some(Duration::from_secs(7)), None]);
}

/// A lease that burned the whole bound is not asked again inside the window (fail closed, fast), a lease
/// that answered "unavailable" at once is (it was not the wait), another lease is unaffected, and the
/// next window starts clean.
#[test]
fn a_lease_that_missed_the_boot_bound_is_not_asked_again_in_the_window() {
    let facts = LeaseFacts::default();
    facts.set_boot_wait(Duration::from_millis(200));
    {
        let _window = facts.begin_boot();
        assert!(facts.ask("bybit", |_| Err::<(), _>(unavailable())).is_err());
        let mut asked = 0;
        let fast = facts.ask("bybit", |_| {
            asked += 1;
            Ok(())
        });
        assert!(fast.is_ok() && asked == 1, "a prompt `unavailable` is not a miss");

        let slow = facts.ask("bybit", |_| {
            std::thread::sleep(Duration::from_millis(250));
            Err::<(), _>(unavailable())
        });
        assert!(slow.is_err(), "the bound was burned");
        let mut called = false;
        let again = facts.ask("bybit", |_| {
            called = true;
            Ok(())
        });
        match again {
            Err(SymbolFactsError::Unavailable(why)) => {
                assert!(why.contains("not asked again"), "{why}");
            }
            other => panic!("a missed lease is refused without a round trip: {other:?}"),
        }
        assert!(!called, "no round trip was made");
        assert!(facts.ask("okx", |_| Ok(())).is_ok(), "another lease is asked as usual");
    }
    let _next = facts.begin_boot();
    assert!(facts.ask("bybit", |_| Ok(())).is_ok(), "the next window starts clean");
}

/// `SymbolFacts::awaiting_balance`: what the control door holds a hot-started account's opening orders on
/// (decision 0169). Nothing waits until a start ANNOUNCES a wait, so every engine the boot built, and any
/// account a start announced nothing for, reads `None`; an announced wait reads `Some` until the flag its
/// reconcile leg raises goes up; a stop forgets it; the next start of the same account waits afresh.
#[test]
fn only_an_announced_wait_holds_an_account_and_a_raised_flag_or_a_stop_ends_it() {
    let facts = LeaseFacts::default();
    assert_eq!(facts.awaiting_balance("okx#ALT"), None, "the boot announces nothing");

    let synced = Arc::new(AtomicBool::new(false));
    facts.engine_awaiting_balance("okx#ALT", &synced);
    assert!(facts.awaiting_balance("okx#ALT").is_some(), "announced, not landed");
    assert_eq!(facts.awaiting_balance("okx"), None, "a sibling account of the venue is not held");
    assert_eq!(facts.awaiting_balance("bybit#ALT"), None, "nor is another venue's");

    synced.store(true, Ordering::Release);
    assert_eq!(facts.awaiting_balance("okx#ALT"), None, "the balance landed");

    // Stopped before it landed: a stopped account is not waiting for anything.
    let unlanded = Arc::new(AtomicBool::new(false));
    facts.engine_awaiting_balance("okx#ALT", &unlanded);
    assert!(facts.awaiting_balance("okx#ALT").is_some());
    facts.engine_stopped("okx#ALT");
    assert_eq!(facts.awaiting_balance("okx#ALT"), None);

    // Started again: a new engine, a new wait, even though the old flag was raised.
    facts.engine_awaiting_balance("okx#ALT", &Arc::new(AtomicBool::new(false)));
    assert!(facts.awaiting_balance("okx#ALT").is_some());
    // The cap answer the same entry carries (a stop reads `false`) is not touched by a wait.
    assert!(!facts.exposure_cap_armed("okx#ALT"), "the wait leaves the cap answer alone");
}

/// An answer for `oanda`'s EURGBP, priced in GBP when `rate` is `Some`.
fn eurgbp(rate: Option<f64>) -> SymbolAdmission {
    SymbolAdmission {
        route_key: "oanda".into(),
        symbol: "EURGBP".into(),
        grid: SymbolGrid { tick_size: Some(0.00001), lot_size: Some(1.0), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: None,
        quote_ccy: rate.map(|_| "GBP".to_string()),
        quote_rate: rate,
        rate_as_of_ms: rate.map(|_| 1),
        im_requirement: None,
    }
}

/// Keep `admission` as the registry's answer for oanda's EURGBP, looked up `age` ago.
fn keep_aged(facts: &LeaseFacts, age: Duration, admission: SymbolAdmission) {
    let at = Instant::now().checked_sub(age).expect("a clock that has run this long");
    facts.answers.lock().unwrap().insert(key("oanda", "EURGBP"), (at, admission));
}

/// **An answer that carries a conversion rate is served for [`RATE_FRESH_FOR`] and no longer**, where the
/// same answer without a rate lives [`FACTS_TTL`]: the next ask after the window goes back to the lease (here
/// there is none, so the ask says so), and the facts around a rate-less answer are not re-read for an hour.
#[test]
fn a_rate_bearing_answer_is_served_for_the_rate_window_only() {
    let facts = LeaseFacts::default();
    keep_aged(&facts, Duration::from_secs(5), eurgbp(Some(1.35)));
    assert_eq!(facts.admission("oanda", "oanda", "EURGBP"), Ok(eurgbp(Some(1.35))));

    keep_aged(&facts, RATE_FRESH_FOR + Duration::from_secs(1), eurgbp(Some(1.35)));
    let why = facts.admission("oanda", "oanda", "EURGBP").expect_err("the rate is past its window");
    assert!(why.contains("no open trade lease"), "it asked the lease again: {why}");

    keep_aged(&facts, RATE_FRESH_FOR + Duration::from_secs(1), eurgbp(None));
    assert_eq!(
        facts.admission("oanda", "oanda", "EURGBP"),
        Ok(eurgbp(None)),
        "the same age without a rate is still inside the hour"
    );
}

/// **A rate comes due for a refresh at its own window, and a failed re-read is retried in ten seconds, not on
/// every order**: an admitted symbol whose rate is [`RATE_FRESH_FOR`] old is refreshed by the next order (the
/// facts around a rate-less one wait ten minutes); when the re-read fails the entry is moved so that the retry
/// is due [`RATE_RETRY_AFTER`] later.
#[test]
fn a_rate_is_refreshed_at_its_window_and_a_failed_reread_waits_ten_seconds() {
    let facts = LeaseFacts::default();
    facts.admitted.lock().unwrap().insert(key("oanda", "EURGBP"));

    // A rate-less answer 31 s old is not due (ten minutes are): nothing is asked, nothing moves.
    keep_aged(&facts, Duration::from_secs(31), eurgbp(None));
    assert_eq!(facts.refresh("oanda", "oanda", "EURGBP"), None);
    let age_of =
        |facts: &LeaseFacts| facts.answers.lock().unwrap()[&key("oanda", "EURGBP")].0.elapsed();
    assert!(age_of(&facts) >= Duration::from_secs(31), "not touched");

    // A rated answer 31 s old IS due; the lookup fails (no lease), and the entry is moved to wait ten seconds.
    keep_aged(&facts, RATE_FRESH_FOR + Duration::from_secs(1), eurgbp(Some(1.35)));
    assert_eq!(facts.refresh("oanda", "oanda", "EURGBP"), None, "the re-read failed");
    let waited = age_of(&facts);
    let want = RATE_FRESH_FOR - RATE_RETRY_AFTER;
    assert!(
        waited >= want && waited < want + Duration::from_secs(5),
        "due again {RATE_RETRY_AFTER:?} later: the entry is {waited:?} old, wanted about {want:?}"
    );
    assert_eq!(facts.refresh("oanda", "oanda", "EURGBP"), None, "not due yet: no second re-read");
    assert!(age_of(&facts) >= waited, "the second ask did not move it again");
}

// ---- a held foreign-quoted position whose rate nothing refreshes is named ------------------------------

const NOW: i64 = 1_700_000_000_000;

fn ibkr_sap() -> (String, String) {
    key("ibkr", "SAP.SMART.EUR")
}

/// The registry told the journal restored `SAP.SMART.EUR` (EUR) in `ibkr`'s engine, whose engine rate was last
/// observed `age_s` seconds before `NOW` (`None`: the engine holds no EUR row).
fn journaled_sap(age_s: Option<i64>) -> LeaseFacts {
    let facts = LeaseFacts::default();
    facts.note_journaled_foreign("ibkr", &[("SAP.SMART.EUR".to_string(), "EUR".to_string())]);
    facts.bind_rate_rows(move |route_key, ccy| {
        assert_eq!((route_key, ccy), ("ibkr", "EUR"), "asked for the symbol's own currency");
        age_s.map(|age| NOW - age * 1_000)
    });
    facts
}

/// **A held restored position whose rate is older than the window is named, with its age**; a fresh one, a
/// position nothing is held in, and a registry whose rows are not bound are not.
#[test]
fn a_held_restored_position_with_an_old_rate_is_named_with_its_age() {
    let held = |route_key: &str, symbol: &str| (route_key, symbol) == ("ibkr", "SAP.SMART.EUR");
    let none_held = |_: &str, _: &str| false;

    let stale = journaled_sap(Some(342)).unrefreshed_held(&held, NOW);
    assert_eq!(
        stale,
        [Unrefreshed {
            route_key: "ibkr".into(),
            symbol: "SAP.SMART.EUR".into(),
            ccy: "EUR".into(),
            age_s: Some(342)
        }]
    );
    assert!(journaled_sap(Some(60)).unrefreshed_held(&held, NOW).is_empty(), "inside the cadence");
    assert!(journaled_sap(Some(89)).unrefreshed_held(&held, NOW).is_empty(), "inside the window");
    assert_eq!(journaled_sap(Some(91)).unrefreshed_held(&held, NOW).len(), 1, "past the window");
    assert!(journaled_sap(Some(342)).unrefreshed_held(&none_held, NOW).is_empty(), "nothing held");

    let never = journaled_sap(None).unrefreshed_held(&held, NOW);
    assert_eq!(never.len(), 1);
    assert_eq!(never[0].age_s, None, "the engine holds no row for the currency at all");

    let unbound = LeaseFacts::default();
    unbound.note_journaled_foreign("ibkr", &[("SAP.SMART.EUR".to_string(), "EUR".to_string())]);
    assert!(unbound.unrefreshed_held(&held, NOW).is_empty(), "no rows, nothing can be said");
}

/// A symbol this registry rated and admitted is watched the same way as a restored one, and is listed once when
/// the journal names it too.
#[test]
fn an_admitted_rated_symbol_is_watched_and_listed_once() {
    let facts = journaled_sap(Some(300));
    let mut rated = eurgbp(Some(1.10));
    rated.route_key = "ibkr".into();
    rated.symbol = "SAP.SMART.EUR".into();
    rated.quote_ccy = Some("EUR".into());
    facts.answers.lock().unwrap().insert(ibkr_sap(), (Instant::now(), rated));
    facts.admitted.lock().unwrap().insert(ibkr_sap());
    let held = |_: &str, _: &str| true;
    assert_eq!(facts.unrefreshed_held(&held, NOW).len(), 1, "one line, not two");
}

/// The journaled symbols nothing has rated are the ones the timer heals, and a stop of the engine forgets them.
#[test]
fn only_the_journaled_symbols_nothing_has_rated_are_owed_and_a_stop_forgets_them() {
    let facts = journaled_sap(Some(10));
    assert_eq!(facts.owed_symbols(&[]), [ibkr_sap()]);
    assert!(facts.owed_symbols(&[ibkr_sap()]).is_empty(), "rated: the ordinary pass has it");
    facts.engine_stopped("ibkr");
    assert!(facts.owed_symbols(&[]).is_empty(), "a stopped engine owes nothing");
    facts.note_journaled_foreign("ibkr", &[]);
    assert!(facts.journaled.lock().unwrap().is_empty());
}

/// **The owed symbol's backoff doubles from its first wait to its cap and no further** (finding 5 of the second
/// IBKR currency review): 1, 2, 4, 8 minutes, then 15 for every failure after, and it is due exactly when its
/// wait has run.
#[test]
fn the_owed_backoff_doubles_to_fifteen_minutes_and_stays_there() {
    let t = Instant::now();
    let mut state = None;
    let mut waits = Vec::new();
    for _ in 0..7 {
        let next = OwedBackoff::failed(state, t, OWED_RETRY_FIRST);
        waits.push((next.next_at - t).as_secs() / 60);
        state = Some(next);
    }
    assert_eq!(waits, [1, 2, 4, 8, 15, 15, 15]);
    let last = state.expect("failed");
    assert_eq!(last.fails, 7);
    assert!(!last.due(t + Duration::from_secs(15 * 60 - 1)));
    assert!(last.due(t + Duration::from_secs(15 * 60)));
    // The failure count cannot wrap into a zero wait.
    let worn = OwedBackoff { fails: u32::MAX, next_at: t };
    assert_eq!(
        (OwedBackoff::failed(Some(worn), t, OWED_RETRY_FIRST).next_at - t).as_secs() / 60,
        15
    );
}

/// **The watcher's word on the lane twins is what the door reads**, replaced whole at each telling, and silent
/// (false) until the watcher has spoken.
#[test]
fn the_watchers_word_on_the_lane_twins_is_what_the_door_reads() {
    let facts = LeaseFacts::default();
    for key in ["binance#PERP", "bybit#X"] {
        assert!(!facts.real_account_under_twin_label(key) && !facts.lane_twin_closing_only(key));
    }
    facts.set_lane_twin_flags(&["binance#PERP".to_string()], &["bybit#X".to_string()]);
    assert!(facts.real_account_under_twin_label("binance#PERP"));
    assert!(!facts.lane_twin_closing_only("binance#PERP"));
    assert!(
        facts.lane_twin_closing_only("bybit#X") && !facts.real_account_under_twin_label("bybit#X")
    );
    facts.set_lane_twin_flags(&[], &[]);
    assert!(
        !facts.real_account_under_twin_label("binance#PERP")
            && !facts.lane_twin_closing_only("bybit#X")
    );
}

/// **The derived twins the watcher tells the door are the positive evidence a live engine needs**
/// ([`SymbolFacts::derived_lane_twin`]): silent (false) until the watcher has spoken, replaced whole at each
/// telling, and independent of the real/closing flags.
#[test]
fn the_derived_lane_twins_the_watcher_tells_are_what_the_door_reads_as_evidence() {
    let facts = LeaseFacts::default();
    assert!(!facts.derived_lane_twin("binance#PERP"), "silent until the watcher has spoken");
    facts.set_derived_lane_twins(&["binance#PERP".to_string()]);
    assert!(facts.derived_lane_twin("binance#PERP") && !facts.derived_lane_twin("binance#ALT"));
    facts.set_lane_twin_flags(&[], &[]);
    assert!(facts.derived_lane_twin("binance#PERP"), "the other flags do not move it");
    facts.set_derived_lane_twins(&[]);
    assert!(!facts.derived_lane_twin("binance#PERP"), "replaced whole");
}

/// **A gate's hold is listed per route key, in route-key order, and ends with the gate, the start or the engine**
/// ([`SymbolFacts::balance_held`]): silent until a gate speaks, replaced by a later word, withdrawn by `None`, and
/// forgotten by a refused start ([`LeaseFacts::forget_lease`]), a new start ([`LeaseFacts::anchor_currency`]) and a
/// stop ([`LeaseFacts::engine_stopped`]), so a row of an engine that is gone never outlives it.
#[test]
fn a_gates_hold_is_listed_per_account_and_ends_with_its_engine() {
    let facts = LeaseFacts::default();
    assert!(facts.balance_held().is_empty(), "silent until a gate speaks");
    facts.set_balance_held("oanda#ALT", Some("assumed USD"));
    facts.set_balance_held("fxcm", Some("stated EUR not anchored"));
    assert_eq!(
        facts.balance_held(),
        [
            ("fxcm".to_string(), "stated EUR not anchored".to_string()),
            ("oanda#ALT".to_string(), "assumed USD".to_string()),
        ]
    );
    facts.set_balance_held("oanda#ALT", Some("untold foreign stock"));
    assert_eq!(facts.balance_held()[1].1, "untold foreign stock", "a later word replaces");
    facts.set_balance_held("oanda#ALT", None);
    assert_eq!(facts.balance_held().len(), 1, "withdrawn by the gate");

    facts.forget_lease("fxcm");
    assert!(facts.balance_held().is_empty(), "a refused start leaves no row");
    facts.set_balance_held("ig", Some("assumed USD"));
    facts.anchor_currency("ig", Some("USD"));
    assert!(facts.balance_held().is_empty(), "a new start decides its own hold afresh");
    facts.set_balance_held("ig", Some("assumed USD"));
    facts.engine_stopped("ig");
    assert!(facts.balance_held().is_empty(), "a stopped engine holds nothing");
}

// ---- an engine's OWN symbol: the held lane (decision 0197) ----------------------------------------------

/// Keep `symbol`'s facts as `facts`' answer for `route_key` (the cache stands in for the gatewayhub).
fn keep_facts(facts: &LeaseFacts, route_key: &str, symbol: &str) {
    facts.answers.lock().unwrap().insert(
        key(route_key, symbol),
        (
            Instant::now(),
            SymbolAdmission {
                route_key: route_key.into(),
                symbol: symbol.into(),
                grid: SymbolGrid {
                    tick_size: Some(0.1),
                    lot_size: Some(0.001),
                    ..SymbolGrid::default()
                },
                contract_value: None,
                initial_mark: None,
                quote_ccy: None,
                quote_rate: None,
                rate_as_of_ms: None,
                im_requirement: None,
            },
        ),
    );
}

/// A registry whose `venue` has a price feed on a scripted dial (it records the lanes opened and closed) and
/// the facts of the default account's own `symbol` already looked up.
fn registry_for(
    venue: &str,
    symbol: &str,
) -> (LeaseFacts, crate::feeds::QuoteFeed, Arc<Mutex<Vec<String>>>) {
    let (facts, feed, log, _tap) = registry_with_tap(venue, symbol);
    (facts, feed, log)
}

/// What [`registry_with_tap`] builds: the registry, the feed it keeps alive, the dial's log and the venue's tap.
type TappedRegistry =
    (LeaseFacts, crate::feeds::QuoteFeed, Arc<Mutex<Vec<String>>>, Arc<crate::feeds::PriceTap>);

/// [`registry_for`] and the venue's price tap, which says whether the core is handed a mark for a symbol.
fn registry_with_tap(venue: &str, symbol: &str) -> TappedRegistry {
    let log = Arc::new(Mutex::new(Vec::new()));
    let tap = Arc::new(crate::feeds::PriceTap::default());
    let feed = crate::feeds::QuoteFeed::new(
        crate::feeds::Tracked::new(Box::new(Lanes(Arc::clone(&log), 0))),
        Arc::clone(&tap),
    );
    let facts = LeaseFacts::default();
    facts.bind_prices(HashMap::from([(venue.to_string(), feed.watcher())]));
    keep_facts(&facts, venue, symbol);
    (facts, feed, log, tap)
}

fn held_state(facts: &LeaseFacts, route_key: &str, symbol: &str) -> Option<Hold> {
    facts.held.lock().unwrap().get(&key(route_key, symbol)).copied()
}

fn is_open(facts: &LeaseFacts, route_key: &str, symbol: &str) -> bool {
    matches!(held_state(facts, route_key, symbol), Some(Hold::Open { .. }))
}

/// An instant older than [`HOLD_GRACE`].
fn past_the_grace() -> Instant {
    Instant::now().checked_sub(HOLD_GRACE + Duration::from_secs(1)).unwrap()
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wanted(route_key: &str, symbol: &str) -> crate::readmit::Target {
    crate::readmit::Target {
        route_key: route_key.into(),
        venue: route_key.split('#').next().unwrap().into(),
        symbol: symbol.into(),
    }
}

/// A dial whose every quote subscribe is refused, as a venue's stream cap refuses one.
struct Capped(Arc<Mutex<Vec<String>>>);

impl vike_data::DataClient for Capped {
    fn subscribe_bars(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        unreachable!("the keeper subscribes quotes only")
    }
    fn subscribe_quotes(
        &mut self,
        symbol: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        self.0.lock().unwrap().push(format!("refused {symbol}"));
        Err(vike_data::LiveDataError::Subscribe("the venue's stream cap is reached".into()))
    }
    fn subscribe_trades(
        &mut self,
        _: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        unreachable!("the keeper subscribes quotes only")
    }
    fn subscribe_book(
        &mut self,
        _: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        unreachable!("the keeper subscribes quotes only")
    }
    fn unsubscribe(&mut self, _: vike_data::SubscriptionId) {}
    fn shutdown(&mut self) {}
}

/// **A default engine's own symbol gets its lane once, and the lane is the ENGINE's**: the venue's feed dial
/// opens it a single time (a second hold finds it open), and it takes no place under [`MAX_WATCHED`], so the
/// door's foreign symbols are all still free beside it.
#[test]
fn holding_an_engines_own_symbol_opens_its_lane_once_and_takes_no_watched_place() {
    let (facts, _feed, log) = registry_for("oanda", "EURUSD");
    facts.hold_price("oanda", "oanda", "EURUSD");
    assert_eq!(*log.lock().unwrap(), ["quotes EURUSD".to_string()], "the lane is open");
    assert!(is_open(&facts, "oanda", "EURUSD"));
    assert_eq!(
        entry(&facts, "oanda", "EURUSD"),
        None,
        "held, not watched: no place under the bound"
    );

    facts.hold_price("oanda", "oanda", "EURUSD");
    assert_eq!(log.lock().unwrap().len(), 1, "a second hold finds the lane open");
    for i in 0..MAX_WATCHED {
        let k = key("oanda", &format!("S{i}"));
        assert!(matches!(facts.claim_pump(&k), Ok(Claim::Subscribe)), "place {i} is free");
        facts.watched.lock().unwrap().insert(k, Sub::Open);
    }
}

/// **Flat releases the lane to the idle reaper, after the grace and not before**: a lane the engine was seen
/// holding something in a moment ago stays; one not wanted for longer than [`HOLD_GRACE`] goes `Idle`, and the
/// reaper then closes the datahub subscription. A lane still in the keeper's `wanted` list never ages out.
#[test]
fn a_flat_engine_hands_its_lane_to_the_idle_reaper_after_the_grace() {
    let (facts, _feed, log) = registry_for("oanda", "EURUSD");
    facts.hold_price("oanda", "oanda", "EURUSD");

    facts.keep_lanes(&[]);
    assert!(is_open(&facts, "oanda", "EURUSD"), "inside the grace: kept");

    facts
        .held
        .lock()
        .unwrap()
        .insert(key("oanda", "EURUSD"), Hold::Open { seen: past_the_grace() });
    facts.keep_lanes(&[wanted("oanda", "EURUSD")]);
    assert!(
        is_open(&facts, "oanda", "EURUSD"),
        "still held: the sighting is stamped, not released"
    );
    let Some(Hold::Open { seen }) = held_state(&facts, "oanda", "EURUSD") else { panic!() };
    assert!(seen.elapsed() < HOLD_GRACE);

    facts
        .held
        .lock()
        .unwrap()
        .insert(key("oanda", "EURUSD"), Hold::Open { seen: past_the_grace() });
    facts.keep_lanes(&[]);
    assert_eq!(held_state(&facts, "oanda", "EURUSD"), None, "flat past the grace: released");
    assert!(matches!(entry(&facts, "oanda", "EURUSD"), Some(Sub::Idle(_))), "…to the reaper");
    facts.set_idle_ttl(Duration::ZERO);
    assert_eq!(facts.close_idle_pumps(), 1);
    assert_eq!(
        *log.lock().unwrap(),
        ["quotes EURUSD".to_string(), "unsubscribe 1".to_string()],
        "the datahub subscription is closed"
    );
}

/// **Nothing held, nothing subscribed**: a keeper tick over an empty view, and a registry nobody holds
/// anything in, open no lane and leave no entry.
#[test]
fn nothing_held_subscribes_nothing() {
    let (facts, _feed, log) = registry_for("oanda", "EURUSD");
    facts.keep_lanes(&[]);
    assert!(log.lock().unwrap().is_empty());
    assert!(facts.held.lock().unwrap().is_empty());
    assert!(facts.watched.lock().unwrap().is_empty());
}

/// **A venue ceiling hit fails SOFT**: the dial refuses (OANDA's stream cap, IG's lane cap, the strategy lane's
/// key limit all arrive as a refused subscribe), the entry is `Refused`, no place is held, nothing panics and no
/// order is touched; the lane is not asked again inside [`HOLD_RETRY_AFTER`], and the retry after it is the
/// quiet one (its refusal was said when it first happened).
#[test]
fn a_ceiling_hit_fails_soft_says_it_once_and_is_not_asked_again_at_once() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let feed = crate::feeds::QuoteFeed::new(
        crate::feeds::Tracked::new(Box::new(Capped(Arc::clone(&log)))),
        Arc::new(crate::feeds::PriceTap::default()),
    );
    let facts = LeaseFacts::default();
    facts.bind_prices(HashMap::from([("oanda".to_string(), feed.watcher())]));
    keep_facts(&facts, "oanda", "EURUSD");

    facts.hold_price("oanda", "oanda", "EURUSD");
    assert!(matches!(held_state(&facts, "oanda", "EURUSD"), Some(Hold::Refused { .. })));
    assert_eq!(entry(&facts, "oanda", "EURUSD"), None, "a refused lane holds no place");
    assert_eq!(log.lock().unwrap().len(), 1);

    facts.hold_price("oanda", "oanda", "EURUSD");
    assert_eq!(log.lock().unwrap().len(), 1, "not asked again inside the retry pause");

    facts.held.lock().unwrap().insert(
        key("oanda", "EURUSD"),
        Hold::Refused { at: Instant::now().checked_sub(HOLD_RETRY_AFTER).unwrap() },
    );
    assert_eq!(
        facts.claim_hold("oanda", "oanda", "EURUSD"),
        Some((0, true)),
        "the retry knows its refusal was said already"
    );
    facts.unclaim_hold("oanda", "EURUSD");

    // A tick that no longer wants it forgets the refusal, so the next holding says it afresh.
    facts.held.lock().unwrap().insert(key("oanda", "EURUSD"), Hold::Refused { at: Instant::now() });
    facts.keep_lanes(&[]);
    assert_eq!(held_state(&facts, "oanda", "EURUSD"), None);
}

/// A venue this node has no price feed for at all is the same soft failure.
#[test]
fn a_venue_with_no_price_feed_stays_unpriced_without_a_fuss() {
    let facts = LeaseFacts::default();
    keep_facts(&facts, "polymarket", "TOKEN");
    facts.hold_price("polymarket", "polymarket", "TOKEN");
    assert!(matches!(held_state(&facts, "polymarket", "TOKEN"), Some(Hold::Refused { .. })));
    assert!(facts.watched.lock().unwrap().is_empty());
}

/// **Alpaca is out by name** (the owner's deferral), whatever the account's label, and so are the engines that a
/// mount prices already (default or labelled) and a stopped one. None of them opens a lane, marks a symbol or
/// leaves an entry; a stopped engine that starts again may.
#[test]
fn alpaca_a_stopped_engine_and_a_mounted_symbol_hold_no_lane() {
    assert!(
        NO_HELD_LANE.iter().any(|(venue, why)| *venue == "alpaca" && !why.is_empty()),
        "alpaca is excluded by name, with its reason"
    );
    let (facts, _feed, log, tap) = registry_with_tap("alpaca", "AAPL");
    keep_facts(&facts, "alpaca#ALT", "AAPL");
    facts.hold_price("alpaca", "alpaca", "AAPL");
    facts.hold_price("alpaca#ALT", "alpaca", "AAPL");
    assert!(log.lock().unwrap().is_empty() && facts.held.lock().unwrap().is_empty());
    assert!(!tap.marks_symbol("alpaca", "AAPL"), "a labelled alpaca engine is not marked either");

    let (facts, _feed, log, tap) = registry_with_tap("binance", "BTCUSDT");
    keep_facts(&facts, "binance#ALT", "BTCUSDT");
    facts.bind_mounted(|venue, symbol| (venue, symbol) == ("binance", "BTCUSDT"));
    facts.hold_price("binance", "binance", "BTCUSDT");
    facts.hold_price("binance#ALT", "binance", "BTCUSDT");
    assert!(log.lock().unwrap().is_empty(), "a strategy mount prices the symbol already");
    assert!(facts.held.lock().unwrap().is_empty());
    assert!(!tap.marks_symbol("binance", "BTCUSDT"), "…and the mount's mark concept is left alone");
    keep_facts(&facts, "binance", "ETHUSDT");
    facts.hold_price("binance", "binance", "ETHUSDT");
    assert_eq!(*log.lock().unwrap(), ["quotes ETHUSDT".to_string()], "an unmounted symbol is held");

    let (facts, _feed, log) = registry_for("binance", "BTCUSDT");
    facts.engine_stopped("binance");
    facts.hold_price("binance", "binance", "BTCUSDT");
    assert!(log.lock().unwrap().is_empty(), "a stopped engine is priced by nobody");
    facts.engine_starting("binance", false);
    facts.hold_price("binance", "binance", "BTCUSDT");
    assert_eq!(*log.lock().unwrap(), ["quotes BTCUSDT".to_string()], "started again: held again");
}

/// **A stopped engine hands its held lane back**, and a refused start (which built no engine) leaves it unable
/// to hold one.
#[test]
fn a_stopped_engine_hands_back_the_lane_it_held() {
    let (facts, _feed, log) = registry_for("binance", "BTCUSDT");
    facts.hold_price("binance", "binance", "BTCUSDT");
    assert!(is_open(&facts, "binance", "BTCUSDT"));
    facts.engine_stopped("binance");
    assert!(facts.held.lock().unwrap().is_empty());
    assert!(matches!(entry(&facts, "binance", "BTCUSDT"), Some(Sub::Idle(_))));
    facts.set_idle_ttl(Duration::ZERO);
    assert_eq!(facts.close_idle_pumps(), 1);
    assert_eq!(log.lock().unwrap().len(), 2, "opened, then closed");

    let before = facts.engine_starting("binance", true);
    facts.engine_start_refused("binance", before);
    facts.hold_price("binance", "binance", "BTCUSDT");
    assert_eq!(log.lock().unwrap().len(), 2, "a refused start built no engine to hold anything");
}

/// **Another account's refused order cannot idle or close an engine's held lane** (the twin's rule, for the
/// default engine): the other account's release is a no-op while the lane is held, and when the reaper does
/// pick the entry that account's door ask left, it frees the place and keeps the pump.
#[test]
fn another_accounts_refused_order_cannot_idle_or_close_a_held_lane() {
    let (facts, _feed, log) = registry_for("binance", "BTCUSDT");
    facts.hold_price("binance", "binance", "BTCUSDT");
    facts.set_idle_ttl(Duration::ZERO);

    facts.watched.lock().unwrap().insert(key("binance", "BTCUSDT"), Sub::Open);
    facts.release("binance#ALT", "binance", "BTCUSDT");
    assert_eq!(
        entry(&facts, "binance", "BTCUSDT"),
        Some(Sub::Open),
        "not idled by the other account"
    );

    facts.watched.lock().unwrap().insert(key("binance", "BTCUSDT"), Sub::Idle(Instant::now()));
    assert_eq!(facts.close_idle_pumps(), 1, "the place is freed…");
    assert_eq!(*log.lock().unwrap(), ["quotes BTCUSDT".to_string()], "…and the pump is kept");
    assert!(is_open(&facts, "binance", "BTCUSDT"));
}

/// A pump the reaper has marked `Closing` is not claimed under: the hold stands down for this tick (the
/// keeper asks again), so a lane is never closed under a hold that has just been taken.
#[test]
fn a_pump_being_closed_is_not_claimed() {
    let (facts, _feed, log) = registry_for("binance", "BTCUSDT");
    facts.watched.lock().unwrap().insert(key("binance", "BTCUSDT"), Sub::Closing);
    facts.hold_price("binance", "binance", "BTCUSDT");
    assert!(facts.held.lock().unwrap().is_empty(), "no claim was left behind");
    assert!(log.lock().unwrap().is_empty());
}

/// **FORCED INTERLEAVING: a stop that lands while the lane is opening does not strand the pump.** The opening
/// thread subscribes AFTER the stop has looked for what to hand back; it finds its claim gone and hands the pump
/// back itself. MUTATION PROOF: drop the `ours` check in `open_hold` and the entry is `Open` for a dead engine.
#[test]
fn a_stop_that_lands_while_the_lane_opens_does_not_strand_the_pump() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let facts = Arc::new(LeaseFacts::default());
    let stopper = Arc::clone(&facts);
    let dial = StopsDuringOpen {
        log: Arc::clone(&log),
        during: Some(Box::new(move || stopper.engine_stopped("binance"))),
    };
    let feed = crate::feeds::QuoteFeed::new(
        crate::feeds::Tracked::new(Box::new(dial)),
        Arc::new(crate::feeds::PriceTap::default()),
    );
    facts.bind_prices(HashMap::from([("binance".to_string(), feed.watcher())]));
    keep_facts(&facts, "binance", "BTCUSDT");

    facts.hold_price("binance", "binance", "BTCUSDT");
    assert!(facts.held.lock().unwrap().is_empty(), "the stopped engine holds nothing");
    assert!(
        matches!(entry(&facts, "binance", "BTCUSDT"), Some(Sub::Idle(_))),
        "the pump went back"
    );
    facts.set_idle_ttl(Duration::ZERO);
    assert_eq!(facts.close_idle_pumps(), 1);
    assert_eq!(
        *log.lock().unwrap(),
        ["quotes BTCUSDT".to_string(), "unsubscribe 1".to_string()],
        "the reaper closed it"
    );
}

/// **The door's call never waits**: `hold_lane` claims under a lock and opens the lane on a thread of its own, so
/// the order that asked is not held behind a look-up and a datahub round trip. Without a keeper to hand it that
/// thread (no handle on the registry) it claims nothing.
#[test]
fn the_doors_hold_opens_the_lane_off_its_own_thread() {
    let (facts, _feed, log) = registry_for("oanda", "EURUSD");
    SymbolFacts::hold_lane(&facts, "oanda", "oanda", "EURUSD");
    assert!(facts.held.lock().unwrap().is_empty(), "no keeper, no thread to open it: no claim");
    assert!(log.lock().unwrap().is_empty());

    let facts = Arc::new(facts);
    facts.me.set(Arc::downgrade(&facts)).expect("set once");
    SymbolFacts::hold_lane(&*facts, "oanda", "oanda", "EURUSD");
    wait_for("the lane to open", || is_open(&facts, "oanda", "EURUSD"));
    assert_eq!(*log.lock().unwrap(), ["quotes EURUSD".to_string()]);
    SymbolFacts::hold_lane(&*facts, "oanda", "oanda", "EURUSD");
    assert_eq!(log.lock().unwrap().len(), 1, "an open lane is not opened again");
}

/// **The keeper tick opens what the snapshot shows held**, through the same non-blocking path.
#[test]
fn a_keeper_tick_opens_the_lanes_the_snapshot_shows_held() {
    let (facts, _feed, log) = registry_for("oanda", "EURUSD");
    let facts = Arc::new(facts);
    facts.me.set(Arc::downgrade(&facts)).expect("set once");
    facts.keep_lanes(&[wanted("oanda", "EURUSD")]);
    wait_for("the lane to open", || is_open(&facts, "oanda", "EURUSD"));
    assert_eq!(*log.lock().unwrap(), ["quotes EURUSD".to_string()]);
}

/// **A restored hold is subscribed at the boot**, through the very functions the boot calls: the journal's
/// engine holds an order in its own symbol, `own_hold_targets` names it and `warm_twins` opens its lane with
/// `hold_price`, on a short-lived thread.
#[test]
fn a_restored_hold_subscribes_at_the_boot() {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionEngine, Outbox, RiskGate};

    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(vike_model::RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    engine.submit_order(
        &vike_model::OrderRequest {
            client_order_id: "resting".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(100.0),
            ts: 1,
            ..Default::default()
        },
        1_000,
        &mut Outbox::default(),
    );
    let mut snapshot = engine.snapshot_state();
    snapshot.tier = Some(vike_model::AccountTier::Demo);

    let (facts, _feed, log) = registry_for("binance", "BTCUSDT");
    let facts = Arc::new(facts);
    let boot = crate::readmit::own_hold_targets(&[snapshot]);
    assert_eq!(boot, [wanted("binance", "BTCUSDT")]);
    let handles = crate::readmit::warm_twins(&boot, &crate::readmit::Pace::default(), {
        let facts = Arc::clone(&facts);
        move |t: &crate::readmit::Target| facts.hold_price(&t.route_key, &t.venue, &t.symbol)
    });
    for handle in handles {
        handle.join().expect("the boot's lane opening ends quietly");
    }
    assert_eq!(*log.lock().unwrap(), ["quotes BTCUSDT".to_string()]);
    assert!(is_open(&facts, "binance", "BTCUSDT"));
}

// ---- a LABELLED engine's own symbol: the same lane, marked in the core (decision 0198) -------------------

/// **A labelled engine that holds something gets its own symbol's lane, and the core is handed a mark for it**
/// (`hyperliquid#LIVE`, `bybit#LIVE`): the lane is the venue dial's public QUOTE subscription (the scripted dial
/// is a `DataClient`, which has no order path to open), counted as held and not as watched, and the tap is told
/// to mark the symbol, because the core hands a venue's quote to the venue's default engine only.
#[test]
fn a_labelled_engine_that_holds_something_keeps_its_symbols_lane_and_has_it_marked() {
    let (facts, _feed, log, tap) = registry_with_tap("hyperliquid", "BTC");
    keep_facts(&facts, "hyperliquid#LIVE", "BTC");
    assert!(!tap.marks_symbol("hyperliquid", "BTC"), "nothing is held yet: nothing is marked");

    facts.hold_price("hyperliquid#LIVE", "hyperliquid", "BTC");
    assert_eq!(*log.lock().unwrap(), ["quotes BTC".to_string()], "the public quote lane is open");
    assert!(is_open(&facts, "hyperliquid#LIVE", "BTC"));
    assert!(tap.marks_symbol("hyperliquid", "BTC"), "…and the core is handed a mark for it");
    assert_eq!(
        entry(&facts, "hyperliquid", "BTC"),
        None,
        "held, not watched: no place under the bound"
    );
}

/// **A flat labelled engine releases the lane and the mark**, after the grace and not before, through the same
/// idle path as a default engine's: the reaper then closes the datahub subscription.
#[test]
fn a_flat_labelled_engine_hands_back_its_lane_and_its_mark() {
    let (facts, _feed, log, tap) = registry_with_tap("hyperliquid", "BTC");
    keep_facts(&facts, "hyperliquid#LIVE", "BTC");
    facts.hold_price("hyperliquid#LIVE", "hyperliquid", "BTC");

    facts.keep_lanes(&[]);
    assert!(is_open(&facts, "hyperliquid#LIVE", "BTC"), "inside the grace: kept");
    assert!(tap.marks_symbol("hyperliquid", "BTC"));

    facts
        .held
        .lock()
        .unwrap()
        .insert(key("hyperliquid#LIVE", "BTC"), Hold::Open { seen: past_the_grace() });
    facts.keep_lanes(&[]);
    assert_eq!(
        held_state(&facts, "hyperliquid#LIVE", "BTC"),
        None,
        "flat past the grace: released"
    );
    assert!(!tap.marks_symbol("hyperliquid", "BTC"), "no mark for a symbol nobody holds");
    assert!(matches!(entry(&facts, "hyperliquid", "BTC"), Some(Sub::Idle(_))), "…to the reaper");
    facts.set_idle_ttl(Duration::ZERO);
    assert_eq!(facts.close_idle_pumps(), 1);
    assert_eq!(
        *log.lock().unwrap(),
        ["quotes BTC".to_string(), "unsubscribe 1".to_string()],
        "the datahub subscription is closed"
    );
}

/// **Two accounts of one venue share ONE subscription** (the gatewayhub allows one tick pump per symbol per
/// session): the default engine and the labelled one hold the same symbol, the dial is asked once, the pump is
/// the venue's whoever leaves first, the mark follows the LABELLED holder alone, and the last one out hands the
/// pump to the reaper, which closes it once.
#[test]
fn two_accounts_of_one_venue_share_one_subscription_and_the_last_one_out_closes_it() {
    for default_leaves_first in [true, false] {
        let (facts, _feed, log, tap) = registry_with_tap("bybit", "BTCUSDT");
        keep_facts(&facts, "bybit#LIVE", "BTCUSDT");
        facts.hold_price("bybit", "bybit", "BTCUSDT");
        facts.hold_price("bybit#LIVE", "bybit", "BTCUSDT");
        assert_eq!(
            *log.lock().unwrap(),
            ["quotes BTCUSDT".to_string()],
            "ONE dial subscription for two accounts"
        );
        assert!(tap.marks_symbol("bybit", "BTCUSDT"), "the labelled engine is marked");

        let (first, second) =
            if default_leaves_first { ("bybit", "bybit#LIVE") } else { ("bybit#LIVE", "bybit") };
        facts
            .held
            .lock()
            .unwrap()
            .insert(key(first, "BTCUSDT"), Hold::Open { seen: past_the_grace() });
        facts.keep_lanes(&[wanted(second, "BTCUSDT")]);
        assert_eq!(held_state(&facts, first, "BTCUSDT"), None, "{first} left");
        assert!(is_open(&facts, second, "BTCUSDT"), "{second} still holds it");
        assert_eq!(
            tap.marks_symbol("bybit", "BTCUSDT"),
            default_leaves_first,
            "marked while the labelled engine holds the symbol, and not otherwise"
        );
        facts.set_idle_ttl(Duration::ZERO);
        assert_eq!(facts.close_idle_pumps(), 0, "the pump is still held: not idle");

        facts
            .held
            .lock()
            .unwrap()
            .insert(key(second, "BTCUSDT"), Hold::Open { seen: past_the_grace() });
        facts.keep_lanes(&[]);
        assert!(
            matches!(entry(&facts, "bybit", "BTCUSDT"), Some(Sub::Idle(_))),
            "the last one left"
        );
        assert!(!tap.marks_symbol("bybit", "BTCUSDT"));
        assert_eq!(facts.close_idle_pumps(), 1);
        assert_eq!(
            *log.lock().unwrap(),
            ["quotes BTCUSDT".to_string(), "unsubscribe 1".to_string()],
            "one subscription, closed once"
        );
    }
}

/// **A stopped labelled engine hands back the lane and the mark**, and holds none while it is stopped; started
/// again, it may.
#[test]
fn a_stopped_labelled_engine_hands_back_its_lane_and_its_mark() {
    let (facts, _feed, log, tap) = registry_with_tap("hyperliquid", "BTC");
    keep_facts(&facts, "hyperliquid#LIVE", "BTC");
    facts.hold_price("hyperliquid#LIVE", "hyperliquid", "BTC");
    assert!(tap.marks_symbol("hyperliquid", "BTC"));

    facts.engine_stopped("hyperliquid#LIVE");
    assert!(facts.held.lock().unwrap().is_empty());
    assert!(!tap.marks_symbol("hyperliquid", "BTC"), "a stopped engine is marked by nobody");
    assert!(matches!(entry(&facts, "hyperliquid", "BTC"), Some(Sub::Idle(_))));

    facts.hold_price("hyperliquid#LIVE", "hyperliquid", "BTC");
    assert!(facts.held.lock().unwrap().is_empty(), "a stopped engine holds no lane");
    facts.engine_starting("hyperliquid#LIVE", false);
    facts.hold_price("hyperliquid#LIVE", "hyperliquid", "BTC");
    assert!(is_open(&facts, "hyperliquid#LIVE", "BTC"), "started again: held again");
    assert!(tap.marks_symbol("hyperliquid", "BTC"));
    assert_eq!(log.lock().unwrap().len(), 1, "the pump was never closed: opened once");
}

/// **A ceiling hit on a labelled engine fails soft and marks nothing**: the dial refuses, the entry is `Refused`
/// (said once, asked again after the pause), and the tap is not told to mark a symbol no lane is open for.
#[test]
fn a_labelled_engine_refused_a_lane_is_marked_for_nothing() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let tap = Arc::new(crate::feeds::PriceTap::default());
    let feed = crate::feeds::QuoteFeed::new(
        crate::feeds::Tracked::new(Box::new(Capped(Arc::clone(&log)))),
        Arc::clone(&tap),
    );
    let facts = LeaseFacts::default();
    facts.bind_prices(HashMap::from([("hyperliquid".to_string(), feed.watcher())]));
    keep_facts(&facts, "hyperliquid#LIVE", "BTC");

    facts.hold_price("hyperliquid#LIVE", "hyperliquid", "BTC");
    assert!(matches!(held_state(&facts, "hyperliquid#LIVE", "BTC"), Some(Hold::Refused { .. })));
    assert!(!tap.marks_symbol("hyperliquid", "BTC"), "no lane, no mark");
    assert_eq!(log.lock().unwrap().len(), 1);
}

/// **The lane twin is held by the same rule and keeps its own warm-up**: its hold and its warm share the one
/// subscription, and when the hold is handed back the pump stays, because the running twin warmed it.
#[test]
fn a_lane_twin_that_holds_something_keeps_the_pump_it_warmed() {
    let (facts, _feed, log) = registry_with_twin_facts();
    facts.warm_price("binance#PERP", "binance", "BTCUSDT.P");
    facts.hold_price("binance#PERP", "binance", "BTCUSDT.P");
    assert!(is_open(&facts, "binance#PERP", "BTCUSDT.P"), "the twin holds like any engine");
    assert_eq!(*log.lock().unwrap(), ["quotes BTCUSDT.P".to_string()], "one subscription");

    facts
        .held
        .lock()
        .unwrap()
        .insert(key("binance#PERP", "BTCUSDT.P"), Hold::Open { seen: past_the_grace() });
    facts.keep_lanes(&[]);
    assert_eq!(held_state(&facts, "binance#PERP", "BTCUSDT.P"), None, "flat: the hold is released");
    assert_eq!(
        entry(&facts, "binance", "BTCUSDT.P"),
        Some(Sub::Open),
        "the twin's warm keeps the pump"
    );
    facts.set_idle_ttl(Duration::ZERO);
    assert_eq!(facts.close_idle_pumps(), 0);
    assert_eq!(log.lock().unwrap().len(), 1, "nothing was closed under the running twin");
}
