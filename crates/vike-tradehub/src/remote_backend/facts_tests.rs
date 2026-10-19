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
