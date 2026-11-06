//! What the arm says about a LIVE account it does not use: one named line, never a value.

use super::*;

// ── what the arm says about a LIVE account it will not use ──────────────────────────────────────
//
// Every account label below is unique to its test, out of habit from when the unused-beside-demo
// `warn!` was said once per process per `(venue, account)`; it is said once per start now, and a
// shared label would no longer change a result. (That `warn!` is gone with the second key set: an
// account holds ONE tier's fields, so no live account is ever stored "beside" a demo one.)

/// A `demo` account — the ordinary case — says nothing about a live tier.
#[test]
fn a_demo_account_alone_adds_no_line_about_the_live_tier() {
    let alone = AccountLabel::parse("ALONE").expect("a legal label");
    let fx = keyed(&alone);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (_, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Demo, "AAPL.SMART.USD", &tx),
            |_, _| None,
            |_, _| panic!("a socket mount never builds a cpapi reconcile client"),
            |_, _| Ok(Box::new(Inert)),
            |_, _| None,
        )
    });
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
}

/// A `live` account whose `ACCOUNT` was forgotten — a client id written, the account not — is a
/// typo or a half-finished edit, and the loader calls it absent, so it printed `NoCredentials`
/// under the words of an empty store. The start names the FIELD it lacks (names only) and still
/// lands on paper. The same fields on a `demo` account say nothing: the half-written report is the
/// live tier's, as it always was.
#[test]
fn a_half_written_live_tier_names_the_missing_account_and_the_start_is_still_paper() {
    let half = AccountLabel::parse("HALF").expect("a legal label");
    let mut fx = AdapterFixture::new(&[("CLIENT_ID", SECRET)]);
    fx.account = half;
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Live, "AAPL.SMART.USD", &tx),
            |_, _| panic!("no account: the reconcile factory must not be called"),
            |_, _| panic!("(cpapi) no account: the reconcile factory must not be called"),
            |_, _| panic!("no account: nothing may be dialled"),
            |_, _| panic!("no account: no grid may be fetched"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("ibkr"), Some("HALF")), "{e:?}");
    assert_eq!(e.field("missing_keys"), Some("ACCOUNT"), "{e:?}");
    assert!(e.message.contains("`ACCOUNT` unset"), "names the missing field: {}", e.message);
    assert!(!e.message.contains("CLIENT_ID"), "names only what is MISSING: {}", e.message);
    assert!(!format!("{e:?}").contains(SECRET), "no credential VALUE: {e:?}");
    assert_eq!(
        IbkrVenueAdapter.resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the cause the arming screen prints is unchanged"
    );

    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Demo, "AAPL.SMART.USD", &tx),
            |_, _| panic!("no account: the reconcile factory must not be called"),
            |_, _| panic!("(cpapi) no account: the reconcile factory must not be called"),
            |_, _| panic!("no account: nothing may be dialled"),
            |_, _| panic!("no account: no grid may be fetched"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(found_tier_events(&events).is_empty(), "a demo account's set says nothing: {events:?}");
}

/// A LABELLED account is started only when it armed, so a `live` one that did not arm never reaches
/// `start` and said nothing at all — `vike-tradehub-core` asks the arm to speak for it instead, and the arm
/// says exactly what `start` says for the default account.
#[test]
fn an_account_that_is_never_started_is_spoken_for_in_the_same_words() {
    let unmounted = AccountLabel::parse("UNMNT").expect("a legal label");
    let mut live_only = AdapterFixture::new(&[("ACCOUNT", SECRET)]);
    live_only.account = unmounted;
    let (_, events) = captured(|| {
        IbkrVenueAdapter.report_not_running_account(&live_only.inputs(AccountTier::Live))
    });
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");
    assert!(events.iter().all(|e| !format!("{e:?}").contains(SECRET)), "{events:?}");

    let (_, events) = captured(|| {
        IbkrVenueAdapter
            .report_not_running_account(&AdapterFixture::new(&[]).inputs(AccountTier::Live));
    });
    assert!(events.is_empty(), "an empty store is the ordinary state and is silent: {events:?}");
}
