use super::*;
use vike_bridge_core::venue_mount_fixture::{MountFixture, found_tier_events};
use vike_log::capture::captured;
use vike_model::accounts::account_keys::AccountLabel;

/// The whole pair `load_oanda_config_from` requires, by FIELD.
const KEYS: &[(&str, &str)] = &[("API_KEY", "tok"), ("ACCOUNT_ID", "101-004-1-001")];

#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = OandaVenueMount.declaration();
    assert_eq!(OandaVenueMount.venue(), "oanda");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::NoGrid);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named { name_fields: &["ACCOUNT_ID"], evm_key_fields: &[] }
    );
    // The whole row, reason included: it is the operator-facing sentence the clock leg reports
    // as `NotChecked`, and this declaration is where it lives.
    assert_eq!(
        d.clock,
        ClockDecl::NotWired {
            reason: "its only time field is the pricing snapshot's publication tick, quantized to \
                     a 1 s grid — a check over it would flap across a ±900 ms band on a healthy \
                     host",
            unmeasured_risk: None,
        }
    );
}

/// The arming row: only `Practice` arms — an account below `live` holding its whole pair — and a
/// `live` account's whole pair is REFUSED as `LiveTierNotWired`. Anything less is `NoCredentials`
/// at every tier.
#[test]
fn resolve_is_the_arms_own_gate_including_the_live_tier_refusal() {
    let armed = Resolution::Armed {
        tier: AccountTier::Demo,
        held_below_live: Some(HeldBelowLive::DemoOnlyArm),
    };
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        assert_eq!(OandaVenueMount.resolve(&MountFixture::new(KEYS).inputs(tier)), armed);
    }
    assert_eq!(
        OandaVenueMount.resolve(&MountFixture::new(KEYS).inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired),
        "the refusal names the tier it refused — `error!` says it exists, so the probe must not \
         say 'no credentials' beside it"
    );
    for tier in AccountTier::ALL {
        assert_eq!(
            OandaVenueMount.resolve(&MountFixture::new(&[]).inputs(tier)),
            Resolution::Paper(PaperCause::NoCredentials),
            "nothing stored: the ordinary unconfigured state ({tier})"
        );
        for half in [&KEYS[..1], &KEYS[1..]] {
            assert_eq!(
                OandaVenueMount.resolve(&MountFixture::new(half).inputs(tier)),
                Resolution::Paper(PaperCause::NoCredentials),
                "half a pair is what the loader calls absent ({tier})"
            );
        }
    }
}

#[test]
fn no_tier_ever_resolves_live() {
    for tier in AccountTier::ALL {
        assert!(
            !matches!(
                OandaVenueMount.resolve(&MountFixture::new(KEYS).inputs(tier)),
                Resolution::Armed { tier: AccountTier::Live, .. }
            ),
            "{tier}"
        );
    }
}

/// An unconfigured account, and a `live` account holding its whole pair, each mount PAPER. The
/// second row is the live-tier refusal's own pin at the MOUNT:
/// `resolve_is_the_arms_own_gate_including_the_live_tier_refusal` pins only `resolve`, and `mount`
/// asks `mountable_tier` for itself. A mount that served the pair from the practice hosts — the
/// silent ignore the module doc names as the original bug — arms the practice account here and
/// fails on `exec`. That failing run dials fxPractice with the fixture's fake token; under the
/// correct mount the refusal is decided before any config is used, so neither row reaches the
/// network.
///
/// Reconciliation is ON in the request, and neither account needs the reconcile factory: the mount
/// returns without it and nothing panics. `out.recon` being `None` cannot show more than that
/// offline — the factory itself answers `None` on any connect failure, so a factory that was
/// called and one that was not look the same here.
#[test]
fn an_unconfigured_or_live_account_mounts_paper() {
    let (tx, _rx) = vike_exec::event_channel(8);
    for (account, fx, tier) in [
        ("an unconfigured account", MountFixture::new(&[]), AccountTier::Demo),
        ("a live account holding its pair", MountFixture::new(KEYS), AccountTier::Live),
    ] {
        let mut req = fx.request(tier, "EUR_USD", &tx);
        req.recon_enabled = true;
        let out = OandaVenueMount.mount(req);
        assert!(matches!(out.exec, ExecOutcome::Paper), "{account} must mount PAPER");
        assert!(out.recon.is_none() && out.identity.is_none(), "{account}");
    }
}

/// A LABELLED account is mounted only when it armed, so a `live` one holding its pair never reaches
/// `mount` and said nothing at all — `vike-mount` asks the arm to speak for it instead, and the arm
/// says exactly what `mount` says for the default account: the one refusal sentence, once, naming
/// THAT account and never a credential name or value.
#[test]
fn an_account_that_is_never_mounted_is_spoken_for_in_the_same_words() {
    let unmounted = AccountLabel::parse("UNMNT").expect("a legal label");
    let mut live = MountFixture::new(&[
        ("API_KEY", "SECRET-VALUE-do-not-log"),
        ("ACCOUNT_ID", "SECRET-VALUE-do-not-log"),
    ]);
    live.account = unmounted.clone();
    let (_, events) =
        captured(|| OandaVenueMount.report_unmounted_account(&live.inputs(AccountTier::Live)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("oanda"), Some("UNMNT")), "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a refusal carries `found_tier`, never `tier`: {e:?}");
    assert!(e.message.contains("account UNMNT"), "names the account it speaks for: {}", e.message);
    assert!(
        e.message.contains("LIVE") && e.message.contains("PAPER"),
        "names the tier and where the venue stays: {}",
        e.message
    );
    assert!(!format!("{e:?}").contains("SECRET-VALUE"), "never a value: {e:?}");

    // A practice account, a half-written live account, and an empty one are not refusals.
    let mut half = MountFixture::new(&KEYS[..1]);
    half.account = unmounted;
    for (quiet, tier) in [
        (&live, AccountTier::Demo),
        (&half, AccountTier::Live),
        (&MountFixture::new(&[]), AccountTier::Live),
    ] {
        let (_, events) =
            captured(|| OandaVenueMount.report_unmounted_account(&quiet.inputs(tier)));
        assert!(events.is_empty(), "silent ({tier}): {events:?}");
    }
}
