//! `AlpacaVenueAdapter`'s tests, offline. First the declaration rows and `resolve`: an account below
//! `live` holding the whole trio arms demo-only, half a trio does not run, and a `live` account's
//! complete trio does not run as `LiveTierNotWired`. Then what the arm says about that live account:
//! one `error!` naming the account, never a credential value, while a demo account starts exactly
//! as before and says nothing about a live tier.

use super::*;
use vike_bridge_core::venue_adapter_fixture::{AdapterFixture, found_tier_events};
use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::AccountLabel;

/// The whole trio `load_alpaca_config_from` requires, by FIELD.
const KEYS: &[(&str, &str)] =
    &[("CLIENT_ID", "cid"), ("CLIENT_SECRET", "csecret"), ("ACCOUNT_ID", "acct-1")];

/// The rows `vike-tradehub-core`'s tables carried for alpaca, pinned as the capability-map playbook pins
/// a matrix: a change to any of them is a deliberate edit here too.
#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = AlpacaVenueAdapter.declaration();
    assert_eq!(AlpacaVenueAdapter.venue(), "alpaca");
    assert!(d.addresses_accounts);
    assert!(d.process_exclusive.is_none());
    assert!(!d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named { name_fields: &["ACCOUNT_ID"], evm_key_fields: &[] }
    );
    // The reason is the text an operator reads in the preflight's clock row, so it is pinned
    // whole rather than by shape.
    assert_eq!(
        d.clock,
        ClockDecl::NotWired {
            reason: "its /v1/clock needs the OAuth2 client-credentials Bearer, i.e. a second token \
                     exchange before any measurement, and alpaca stamps no timestamp on a request",
            unmeasured_risk: None,
        }
    );
}

/// The arming-probe row: the whole trio on an account below `live` arms a demo-only arm; a `live`
/// account's whole trio is `LiveTierNotWired`; anything less stays `NoCredentials` at every tier.
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed = Resolution::Armed {
        tier: AccountTier::Demo,
        held_below_live: Some(HeldBelowLive::DemoOnlyArm),
    };
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        assert_eq!(AlpacaVenueAdapter.resolve(&AdapterFixture::new(KEYS).inputs(tier)), armed);
    }
    assert_eq!(
        AlpacaVenueAdapter.resolve(&AdapterFixture::new(KEYS).inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired),
        "a live account's trio is a set this arm will not use — and the cause says that, rather \
         than that the account holds nothing"
    );
    for tier in AccountTier::ALL {
        assert_eq!(
            AlpacaVenueAdapter.resolve(&AdapterFixture::new(&[]).inputs(tier)),
            Resolution::Paper(PaperCause::NoCredentials),
            "{tier}"
        );
        assert_eq!(
            AlpacaVenueAdapter.resolve(&AdapterFixture::new(&KEYS[..2]).inputs(tier)),
            Resolution::Paper(PaperCause::NoCredentials),
            "half a key set is no key set ({tier})"
        );
    }
}

/// Review Focus 2 at this venue: no tier makes this arm resolve `Live` — it has no live arm.
#[test]
fn no_tier_ever_resolves_live() {
    for tier in AccountTier::ALL {
        assert!(
            !matches!(
                AlpacaVenueAdapter.resolve(&AdapterFixture::new(KEYS).inputs(tier)),
                Resolution::Armed { tier: AccountTier::Live, .. }
            ),
            "{tier}"
        );
    }
}

/// The stay-paper half of the same fact at the START: a live account's trio does NOT RUN, offline,
/// with nothing to reconcile — the cause and the loudness say why, the outcome is paper.
#[test]
fn a_live_accounts_trio_starts_paper_offline() {
    let fx = AdapterFixture::new(KEYS);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(AccountTier::Live, "AAPL", &tx);
    req.recon_enabled = true;
    let out = AlpacaVenueAdapter.start(req);
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// Absent credentials: paper, nothing to reconcile, no probe row — and nothing was built.
#[test]
fn with_no_credentials_the_start_is_paper_and_offline() {
    let fx = AdapterFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let out = AlpacaVenueAdapter.start(fx.request(AccountTier::Demo, "AAPL", &tx));
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
    assert!(AlpacaVenueAdapter.credential_probe(&fx.inputs(AccountTier::Demo)).is_none());
}

/// With a demo account's trio the probe row exists, built without the network (the OAuth2 mint
/// happens on the first request). A `live` account's trio is never sent to the sandbox hosts: no
/// probe.
#[test]
fn credentialed_alpaca_offers_a_read_only_probe() {
    let fx = AdapterFixture::new(KEYS);
    assert!(matches!(
        AlpacaVenueAdapter.credential_probe(&fx.inputs(AccountTier::Demo)),
        Some(CredentialProbe::ReadOnly(_))
    ));
    assert!(AlpacaVenueAdapter.credential_probe(&fx.inputs(AccountTier::Live)).is_none());
}

// ── what the arm says about a LIVE account it will not use ──────────────────────────────────────

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// The whole trio for `label`, every value [`SECRET`].
fn secret_trio(label: &str) -> AdapterFixture {
    let mut fx = AdapterFixture::new(&[
        ("CLIENT_ID", SECRET),
        ("CLIENT_SECRET", SECRET),
        ("ACCOUNT_ID", SECRET),
    ]);
    fx.account = AccountLabel::parse(label).expect("a legal label");
    fx
}

/// The offline doubles `start_with` takes: no grid, no reconcile client, a recording exec client.
fn start_offline(fx: &AdapterFixture, tier: AccountTier) -> (AdapterOutcome, Vec<CapturedEvent>) {
    let (tx, _rx) = vike_exec::event_channel(8);
    captured(|| {
        start_with(
            fx.request(tier, "AAPL", &tx),
            |_, _| None,
            |_, _| None,
            |_, _| Box::new(vike_exec::testing::RecordingClient::default()),
        )
    })
}

/// A demo account's trio — the ordinary case — starts the sandbox, bound at DEMO, announces the
/// start at the tier it bound, and says nothing about a live tier.
#[test]
fn a_demo_account_starts_the_sandbox_and_says_nothing_about_a_live_tier() {
    let (out, events) = start_offline(&secret_trio("ALONE"), AccountTier::Demo);
    let ExecOutcome::Live(exec) = out.exec else { panic!("a demo account's trio mounts") };
    assert_eq!(exec.bound_tier, AccountTier::Demo);
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
    assert!(
        events.iter().any(|e| e.field("tier") == Some("demo")),
        "the mount announcement is said, at the tier it bound: {events:?}"
    );
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// A `live` account holding the whole trio used to read as an empty store. It does NOT RUN — none
/// of the three network steps is taken — and says so once, at `error!`, naming the account and
/// never a value.
#[test]
fn a_live_accounts_trio_is_said_once_and_the_start_is_paper() {
    let fx = secret_trio("LIVEACCT");
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Live, "AAPL", &tx),
            |_, _| panic!("a live account mounts no sandbox, so no grid fetch"),
            |_, _| panic!("a live account mounts no sandbox, so no reconcile client"),
            |_, _| panic!("a live account mounts no sandbox, so no exec client"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "exactly one line about the live account: {events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("alpaca"), Some("LIVEACCT")), "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a diagnostic carries `found_tier`, never `tier`: {e:?}");
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// Half a trio on a `live` account is what the loader calls absent: paper, `NoCredentials`, and
/// silent — the arm names no field it does not have a line for.
#[test]
fn a_half_written_set_is_no_credentials_and_says_nothing() {
    let mut fx = AdapterFixture::new(&KEYS[..2]);
    fx.account = AccountLabel::parse("HALF").expect("a legal label");
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Live, "AAPL", &tx),
            |_, _| panic!("no complete trio, so no grid fetch"),
            |_, _| panic!("no complete trio, so no reconcile client"),
            |_, _| panic!("no complete trio, so no exec client"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
    assert_eq!(
        AlpacaVenueAdapter.resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// A LABELLED account is started only when it armed, so a `live` one never reaches `start` and said
/// nothing at all — `vike-tradehub-core` asks the arm to speak for it instead, and the arm says exactly
/// what `start` says for the default account. A half-written, an empty and a demo account are
/// silent.
#[test]
fn an_account_that_is_never_started_is_spoken_for_in_the_same_words() {
    let live = secret_trio("UNMNT");
    let (_, events) =
        captured(|| AlpacaVenueAdapter.report_not_running_account(&live.inputs(AccountTier::Live)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");

    let mut half = AdapterFixture::new(&KEYS[..1]);
    half.account = AccountLabel::parse("UNMNT").expect("a legal label");
    for (quiet, tier) in [
        (&half, AccountTier::Live),
        (&AdapterFixture::new(&[]), AccountTier::Live),
        (&live, AccountTier::Demo),
    ] {
        let (_, events) =
            captured(|| AlpacaVenueAdapter.report_not_running_account(&quiet.inputs(tier)));
        assert!(events.is_empty(), "silent ({tier}): {events:?}");
    }
}
