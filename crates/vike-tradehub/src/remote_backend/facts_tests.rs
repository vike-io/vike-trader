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
