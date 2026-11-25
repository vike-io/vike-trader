//! The late-currency watch's decisions (`Late`), with no network and no clock but the one handed in: which
//! lease is watched at all, what a reply must say to count, and the cadence of the asks.

use super::*;

const ROUTE: &str = "fxcm#late-currency-unit";

fn live(currency: Option<&str>, assumed: bool, tier: AccountTier) -> AdapterReply {
    AdapterReply::Running {
        bound_tier: tier,
        grid: None,
        contract_value: None,
        margin_mode: None,
        leg_grids: Vec::new(),
        reconcile: false,
        leverage_cap: None,
        account_currency: currency.map(str::to_string),
        account_currency_assumed: assumed,
        native_time_release: false,
        broker_code_len: 0,
    }
}

/// The pair fxcm's pound account states (its `currency_symbol` fact), which the start hands the watch for a lease on
/// the symbol the same row states.
const POUND_PAIR: Option<&str> = Some("EURGBP");

fn armed() -> Late {
    Late::armed(true, false, ROUTE, PAUSES, POUND_PAIR)
        .expect("an fxcm lease the venue gave no word on")
}

/// **Only a converting lease the venue gave no word on, whose account states a pair, is watched.** A currency the
/// venue STATED at the start is never watched (a stated currency that changes stays refused, as before), nor is an
/// account that does not convert, nor one with no pair to move to (it states none, or the lease is on a mount somebody
/// chose: the start then hands the watch no pair).
#[test]
fn only_a_converting_lease_the_venue_gave_no_word_on_with_a_stated_pair_is_watched() {
    assert!(Late::armed(true, false, ROUTE, PAUSES, POUND_PAIR).is_some());
    assert!(
        Late::armed(true, true, ROUTE, PAUSES, POUND_PAIR).is_none(),
        "the venue stated its currency at the start: nothing to wait for"
    );
    assert!(
        Late::armed(false, false, ROUTE, PAUSES, POUND_PAIR).is_none(),
        "an account that does not convert anchors no ledger"
    );
    assert!(
        Late::armed(true, false, ROUTE, PAUSES, None).is_none(),
        "no pair to follow a currency with"
    );
}

/// **Silence is not the venue's word**: no currency, a blank one, the gatewayhub's own assumed USD and a client
/// bound at another tier each leave the watch open.
#[test]
fn an_absent_blank_or_assumed_currency_is_not_heard() {
    let mut late = armed();
    for reply in [
        live(None, false, AccountTier::Demo),
        live(Some("  "), false, AccountTier::Demo),
        live(Some("USD"), true, AccountTier::Demo),
        live(Some("GBP"), true, AccountTier::Demo),
        live(Some("GBP"), false, AccountTier::Live),
        AdapterReply::NotRunning { block_word: "adapter failed".into(), refusal_text: "no".into() },
    ] {
        assert_eq!(
            late.hear("fxcm", AccountTier::Demo, "EURUSD", &reply),
            Heard::Nothing,
            "{reply:?}"
        );
        assert!(!late.is_over(), "{reply:?}");
    }
}

/// **A pound the venue states late moves the mount to the stated pound pair, as a fresh start would**: the rule is
/// `moved_mount`'s, so the watch and a boot cannot disagree. The case of the code is no matter.
#[test]
fn a_pound_stated_late_moves_the_mount_to_the_pound_pair_and_ends_the_watch() {
    let mut late = armed();
    let heard = late.hear(
        "fxcm",
        AccountTier::Demo,
        "EURUSD",
        &live(Some("gbp"), false, AccountTier::Demo),
    );
    assert_eq!(heard, Heard::Moves { currency: "GBP".to_string(), pair: "EURGBP".to_string() });
    assert!(late.is_over());
    assert!(
        !late.due(Instant::now() + Duration::from_secs(3_600)),
        "nothing is asked once it is over"
    );
}

/// **A currency no pair follows ends the watch and moves nothing**: USD (the assumption was right; the balance hold
/// is the toggle's), and any other money (held and refused, as before: the stated pair does not count in it).
#[test]
fn a_currency_no_pair_follows_is_said_and_moves_nothing() {
    for currency_value in ["USD", "EUR", "JPY", "CHF"] {
        let mut late = armed();
        let heard = late.hear(
            "fxcm",
            AccountTier::Demo,
            "EURUSD",
            &live(Some(currency_value), false, AccountTier::Demo),
        );
        assert_eq!(
            heard,
            Heard::Stays { currency: currency_value.to_string() },
            "{currency_value}"
        );
        assert!(late.is_over(), "{currency_value}");
    }
}

/// **The asks follow FXCM's own login retry**: the first is 30 s after the start, then 1, 2 and 5 minutes, then every
/// 10, measured from the ask before.
#[test]
fn the_asks_follow_the_login_retry_and_the_last_pause_repeats() {
    let mut late = armed();
    let start = Instant::now();
    assert!(!late.due(start), "not at the start");
    assert!(late.due(start + Duration::from_secs(31)), "30 s after it");
    let mut now = start + Duration::from_secs(31);
    for pause in [60, 120, 300, 600, 600, 600] {
        late.asked(now);
        assert!(!late.due(now + Duration::from_secs(pause - 1)), "{pause} s is the pause");
        assert!(late.due(now + Duration::from_secs(pause)), "{pause} s is the pause");
        now += Duration::from_secs(pause);
    }
}

/// **A request is taken once, and a new lease's start forgets one an older lease left.**
#[test]
fn a_remount_request_is_taken_once_and_an_old_one_is_forgotten_at_the_next_start() {
    let key = "fxcm#late-currency-request-unit";
    assert!(!take_remount(key), "nothing was asked");
    request_remount(key);
    assert!(take_remount(key));
    assert!(!take_remount(key), "taken once");
    request_remount(key);
    forget_remount(key);
    assert!(!take_remount(key), "a start that begins now builds another engine");
}
