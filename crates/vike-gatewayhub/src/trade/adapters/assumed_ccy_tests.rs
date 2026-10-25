//! The gatewayhub's USD default for an account whose venue states no currency (`assumed_ccy`): a stated currency
//! passes through byte for byte, a silent read is USD flagged ASSUMED and said once per state change, and the
//! start and each venue's rate source share ONE memory of what was said.

use vike_log::capture::{CapturedEvent, captured};

use super::*;

const SILENT: &str = "the account summary carries no currency";

fn silent() -> CcyRead {
    CcyRead::Silent(SILENT.to_string())
}

/// The warn lines of an assumption among `events`.
fn assumption_warns(events: &[CapturedEvent]) -> Vec<&str> {
    events
        .iter()
        .filter(|e| e.level == tracing::Level::WARN && e.message.contains("assuming USD"))
        .map(|e| e.message.as_str())
        .collect()
}

/// **A venue that STATES a currency is passed through untouched** (the golden): the same code, no flag, no line,
/// whichever reader asks, for the currencies the owner's accounts and the odd ones alike.
#[test]
fn a_stated_currency_passes_through_unflagged_and_says_nothing() {
    let said = AssumedSaid::default();
    let (_, events) = captured(|| {
        for (venue, ccy) in [
            ("oanda", "USD"),
            ("ctrader", "EUR"),
            ("ig", "USD"),
            ("dukascopy", "GBP"),
            ("ibkr", "JPY"),
        ] {
            assert_eq!(
                start_currency(&said, venue, "DEFAULT", CcyRead::Stated(ccy.to_string())),
                (Some(ccy.to_string()), false),
                "{venue}"
            );
            assert_eq!(
                source_currency(&said, venue, "DEFAULT", Ok(ccy.to_string())),
                (ccy.to_string(), false),
                "{venue}"
            );
        }
    });
    assert!(events.is_empty(), "a stated currency says nothing: {events:?}");
}

/// **A silent read is USD, ASSUMED**, for the start's reply and for a rate source alike, said at `warn` in FXCM's
/// words naming the venue and why; a read that FAILED is the same silence (the ruling: "or the read fails").
#[test]
fn a_silent_read_is_assumed_usd_flagged_and_warned_in_fxcms_words() {
    let said = AssumedSaid::default();
    let (told, events) = captured(|| start_currency(&said, "ctrader", "DEFAULT", silent()));
    assert_eq!(told, (Some("USD".to_string()), true));
    let warns = assumption_warns(&events);
    assert_eq!(warns.len(), 1, "{events:?}");
    assert!(
        warns[0].starts_with("ctrader account currency not reported by the venue: assuming USD"),
        "{warns:?}"
    );
    assert!(warns[0].contains(SILENT), "the venue's own reason travels in the line: {warns:?}");
    assert!(warns[0].contains("keeps refusing a symbol quoted in another currency"));

    let (told, events) = captured(|| {
        source_currency(&said, "oanda", "DEFAULT", Err("oanda /v3/summary: timed out".into()))
    });
    assert_eq!(told, ("USD".to_string(), true));
    let warns = assumption_warns(&events);
    assert!(warns.len() == 1 && warns[0].starts_with("oanda account currency"), "{events:?}");
}

/// **One warn per account per state change**: the same silence again says nothing more; the venue stating the
/// currency in between is a change (said at `info`), and the next silence is said again. The start and a rate
/// source of one account share the memory (one line, not two), and two accounts do not.
#[test]
fn the_assumption_is_warned_once_per_account_and_state_change() {
    let said = AssumedSaid::default();
    let (_, events) = captured(|| {
        start_currency(&said, "ctrader", "A", silent());
        start_currency(&said, "ctrader", "A", silent());
        source_currency(&said, "ctrader", "A", Err(SILENT.into()));
    });
    assert_eq!(assumption_warns(&events).len(), 1, "{events:?}");

    let (_, events) = captured(|| start_currency(&said, "ctrader", "B", silent()));
    assert_eq!(assumption_warns(&events).len(), 1, "another account is its own: {events:?}");

    let (_, events) =
        captured(|| start_currency(&said, "ctrader", "A", CcyRead::Stated("USD".into())));
    assert!(
        events.iter().any(|e| e.level == tracing::Level::INFO
            && e.message.contains("now states the account's currency (USD)")),
        "{events:?}"
    );
    // ...and once stated it stays stated (review 2, HIGH-1): a later silence is answered with the currency the
    // venue gave, flagged nothing and said nothing, so there is no "change back" to say
    let (again, events) = captured(|| start_currency(&said, "ctrader", "A", silent()));
    assert_eq!(again, (Some("USD".to_string()), false));
    assert!(assumption_warns(&events).is_empty(), "{events:?}");
}

/// What the lane's reconcile balance reads is what the last START told the node: an account is ASSUMED from the
/// first start whose read was silent until a start states a currency, and only that account.
#[test]
fn an_account_is_assumed_from_its_first_silent_start_until_a_start_states_it() {
    let said = AssumedSaid::default();
    assert!(!said.is_assumed("oanda", "A"), "nothing said yet");
    start_currency(&said, "oanda", "A", silent());
    assert!(said.is_assumed("oanda", "A"));
    assert!(!said.is_assumed("oanda", "B"), "another account");
    assert!(!said.is_assumed("ig", "A"), "another venue");
    start_currency(&said, "oanda", "A", CcyRead::Stated("USD".into()));
    assert!(!said.is_assumed("oanda", "A"), "a start stated it");
    // a rate source that is silent and no start behind it tells the node nothing: nothing is withheld for it
    source_currency(&said, "oanda", "B", Err(SILENT.into()));
    assert!(!said.is_assumed("oanda", "B"), "only a start writes what the balance gate reads");
}

/// **A RATE SOURCE's read never clears what the START told the node** (review 2, HIGH-1): the engine was anchored
/// to an assumed USD at its start (the summary timed out), and the lister now reads the summary for itself and
/// gets EUR. The engine is still on the assumed USD until it is started again, so the reconcile balance (the
/// account's real EUR cash) must stay withheld; before, the lister's read marked the account "stated" and the next
/// pass adopted the EUR figure as dollars.
#[test]
fn a_rate_sources_read_never_clears_what_the_start_told() {
    let said = AssumedSaid::default();
    assert_eq!(start_currency(&said, "oanda", "A", silent()), (Some("USD".to_string()), true));
    assert!(said.is_assumed("oanda", "A"));
    assert_eq!(
        source_currency(&said, "oanda", "A", Ok("EUR".into())),
        ("EUR".to_string(), false),
        "the source passes what the venue stated"
    );
    assert!(said.is_assumed("oanda", "A"), "the engine is still the start's assumed USD");
    // the START that reads the venue next is what moves this belt: EUR, stated (the NODE's gate over its own
    // engine's binding does not follow it: `gateway_restart_wire`'s re-teach test)
    assert_eq!(start_currency(&said, "oanda", "A", silent()), (Some("EUR".to_string()), false));
    assert!(!said.is_assumed("oanda", "A"));
}

/// **A currency the venue STATED is never downgraded to an assumption by a later silent read** (review 2, HIGH-1
/// (c), and its M5/M6): a transient failure of the lister, or of a later start, on an account the venue already
/// told us is EUR is answered EUR, unflagged and unwarned, whoever asks and whoever stated it first. It must not
/// flip a correctly stated account to "assumed" (which withholds its balance and says "assuming USD" falsely),
/// nor, for ig, walk past the refusal of a non-USD account.
#[test]
fn a_stated_currency_is_never_downgraded_by_a_later_silent_read() {
    let said = AssumedSaid::default();
    let (got, events) = captured(|| {
        let started = start_currency(&said, "oanda", "A", CcyRead::Stated("EUR".into()));
        let lister = source_currency(&said, "oanda", "A", Err(SILENT.into()));
        let again = start_currency(&said, "oanda", "A", silent());
        (started, lister, again)
    });
    assert_eq!(got.0, (Some("EUR".to_string()), false));
    assert_eq!(
        got.1,
        ("EUR".to_string(), false),
        "the lister's failure is answered with the stated EUR"
    );
    assert_eq!(got.2, (Some("EUR".to_string()), false), "so is a later silent start");
    assert!(!said.is_assumed("oanda", "A"), "never flipped to assumed");
    assert!(assumption_warns(&events).is_empty(), "no false 'assuming USD': {events:?}");

    // whoever stated it first: a rate source states GBP, then a START that reads nothing is told GBP
    let (got, events) = captured(|| {
        source_currency(&said, "ctrader", "B", Ok("GBP".into()));
        start_currency(&said, "ctrader", "B", silent())
    });
    assert_eq!(got, (Some("GBP".to_string()), false));
    assert!(!said.is_assumed("ctrader", "B"));
    assert!(assumption_warns(&events).is_empty(), "{events:?}");

    // the memory is per account: another account of the same venue is still assumed when it is silent
    assert_eq!(start_currency(&said, "oanda", "C", silent()), (Some("USD".to_string()), true));
}

/// **The oanda lister's currency, WITH credentials** (review 2, MEDIUM-3: no test reached this branch): a summary
/// that fails is the ruling's USD, assumed and said once; one that states a currency passes untouched; and with
/// NO credentials nothing was asked, so there is nothing to assume over and the refusal stands.
#[test]
fn the_oanda_lister_assumes_over_a_silent_summary_only_with_credentials() {
    let said = AssumedSaid::default();
    let (got, events) = captured(|| {
        lister_currency(&said, "oanda", "L1", true, Err("oanda /v3/summary: timed out".into()))
    });
    assert_eq!(got, Ok("USD".to_string()));
    assert_eq!(assumption_warns(&events).len(), 1, "{events:?}");
    assert_eq!(
        lister_currency(&said, "oanda", "L2", true, Ok("EUR".into())),
        Ok("EUR".to_string())
    );
    assert_eq!(
        lister_currency(&said, "oanda", "L3", false, Err("oanda: no usable credentials".into())),
        Err("oanda: no usable credentials".to_string()),
        "no credentials: refused, never assumed"
    );
    assert_eq!(
        lister_currency(&said, "oanda", "L4", false, Ok("EUR".into())),
        Ok("EUR".to_string())
    );
}

/// A currency the lane cannot hand the node (the mounted pair counts money in another one) has nothing to
/// assume over: the old answer, and no line.
#[test]
fn a_declined_read_tells_nothing_and_assumes_nothing() {
    let said = AssumedSaid::default();
    let (told, events) =
        captured(|| start_currency(&said, "dukascopy", "DEFAULT", CcyRead::Declined));
    assert_eq!(told, (None, false));
    assert!(events.is_empty(), "{events:?}");
}
