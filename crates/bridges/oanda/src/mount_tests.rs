use super::*;
use vike_bridge_core::credentials::Environment;
use vike_bridge_core::venue_mount_fixture::{MountFixture, found_tier_events};
use vike_log::capture::captured;
use vike_model::accounts::account_keys::{AccountLabel, account_key};

const KEYS: &[(&str, &str)] =
    &[("OANDA_DEMO_API_KEY", "tok"), ("OANDA_DEMO_ACCOUNT_ID", "101-004-1-001")];

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

fn keyed(label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in KEYS {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    fx.account = label.clone();
    fx
}

/// The LIVE-named `(api_key, account_id)` pair, COMPOSED by this crate's own naming function and
/// never spelled: a bare `OANDA_LIVE_*` literal in a `src/` file reads to
/// `crates/vike-ops/tests/settings/settings_registry.rs` as an `Injected` map read, against this crate's
/// rows for those names, which are declared `TestOnly` (`declared_layer_matches_the_path`).
fn live_names() -> (String, String) {
    crate::config::oanda_env_var_names(Environment::Live)
}

#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = OandaVenueMount.declaration();
    assert_eq!(OandaVenueMount.venue(), "oanda");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::NoGrid);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named {
            prefix: "OANDA",
            demo_tiers: &["DEMO"],
            live_tiers: &["LIVE", "MAINNET"],
            name_suffixes: &["ACCOUNT_ID"],
            evm_key_suffixes: &[],
        }
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

/// The arming row as it was: only `Practice` arms, and a LIVE-named key set is REFUSED — it
/// answers paper even when the practice keys are present too (`mountable_tier_for_account`'s
/// "a live-named key set WINS" rule).
#[test]
fn resolve_is_the_arms_own_gate_including_the_live_tier_refusal() {
    let armed =
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) };
    for live in [false, true] {
        assert_eq!(
            OandaVenueMount.resolve(&MountFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
        assert_eq!(OandaVenueMount.resolve(&keyed(&AccountLabel::Default).inputs(live)), armed);
        let mut live_named = keyed(&AccountLabel::Default);
        live_named.vars.insert(live_names().0, "real".to_string());
        assert_eq!(
            OandaVenueMount.resolve(&live_named.inputs(live)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "the refusal names the tier it refused — `error!` says it exists, so the probe must \
             not say 'no credentials' beside it (live={live})"
        );
    }
}

#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let (key, account) = live_names();
    let mut fx = MountFixture::new(&[]);
    fx.vars.insert(key, "t".to_string());
    fx.vars.insert(account, "a".to_string());
    for live in [false, true] {
        assert!(!matches!(
            OandaVenueMount.resolve(&fx.inputs(live)),
            Resolution::Armed { tier: Tier::Live, .. }
        ));
        assert_eq!(
            OandaVenueMount.resolve(&fx.inputs(live)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "a live-named pair alone arms nothing, and the cause names the live tier (live={live})"
        );
    }
}

/// **The rule that makes oanda the one arm that refuses LOUDLY, held in both of its halves.** Any
/// non-blank live-named variable — HALF a pair included — is a live key set the arm will not use, so
/// it is [`PaperCause::LiveTierNotWired`]; a store with no live-named variable at all is the
/// ordinary unconfigured state and keeps [`PaperCause::NoCredentials`]. (The siblings that adopted
/// the same cause detect a COMPLETE live key set instead, because their loaders call half a set
/// absent; this arm's refusal predates them and is deliberately the broader one.)
#[test]
fn any_live_named_variable_is_the_live_tier_and_none_is_not() {
    let (key, account) = live_names();
    for only in [key, account] {
        let mut half = MountFixture::new(&[]);
        half.vars.insert(only, "x".to_string());
        for live in [false, true] {
            assert_eq!(
                OandaVenueMount.resolve(&half.inputs(live)),
                Resolution::Paper(PaperCause::LiveTierNotWired),
                "half a live pair is a half-written LIVE intent (live={live})"
            );
        }
    }
    for live in [false, true] {
        assert_eq!(
            OandaVenueMount.resolve(&MountFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "nothing live-named: the ordinary unconfigured state (live={live})"
        );
    }
}

/// Scoped to the ACCOUNT asking: a neighbour's live-named key renames neither this account's cause
/// nor its arming.
#[test]
fn the_live_tier_cause_is_scoped_to_the_account_that_holds_the_key() {
    let mut alt_live = keyed(&AccountLabel::Default);
    alt_live.vars.insert(account_key(&live_names().0, &alt()), "real".to_string());
    // The DEFAULT account still arms: the live-named key is ALT's.
    assert!(matches!(OandaVenueMount.resolve(&alt_live.inputs(true)), Resolution::Armed { .. }));
    // ALT asks for a name with a live key and no practice pair of its own.
    alt_live.account = alt();
    assert_eq!(
        OandaVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
}

#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        OandaVenueMount.resolve(&alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
    assert!(matches!(
        OandaVenueMount.resolve(&keyed(&alt()).inputs(true)),
        Resolution::Armed { .. }
    ));
    let mut default_asks_alt_keys = keyed(&alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        OandaVenueMount.resolve(&default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// An unconfigured store, and a store that wrote BOTH tiers — the practice pair plus a
/// LIVE-named key — each mount PAPER. The second row is the live-tier refusal's own pin at the
/// MOUNT: `resolve_is_the_arms_own_gate_including_the_live_tier_refusal` pins only `resolve`, and
/// `mount` asks `mountable_tier_for_account` for itself. A Demo-first loader — the silent ignore
/// the module doc names as the original bug — arms the practice account from that store and fails
/// here on `exec`. That failing run dials fxPractice with the fixture's fake token, the trade-off
/// `an_oanda_live_key_set_refuses_the_mount_instead_of_trading_the_practice_account` also
/// documents; under the correct mount the refusal is decided before any config exists, so
/// neither row reaches the network.
///
/// Reconciliation is ON in the request, and neither store needs the reconcile factory: the mount
/// returns without it and nothing panics. `out.recon` being `None` cannot show more than that
/// offline — the factory itself answers `None` on any connect failure, so a factory that was
/// called and one that was not look the same here.
#[test]
fn an_unconfigured_or_both_tiers_store_mounts_paper() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut both_tiers = keyed(&AccountLabel::Default);
    both_tiers.vars.insert(live_names().0, "real".to_string());
    for (store, fx) in [
        ("an unconfigured store", MountFixture::new(&[])),
        ("a store holding both tiers", both_tiers),
    ] {
        let mut req = fx.request(true, "EUR_USD", &tx);
        req.recon_enabled = true;
        let out = OandaVenueMount.mount(req);
        assert!(matches!(out.exec, ExecOutcome::Paper), "{store} must mount PAPER");
        assert!(out.recon.is_none() && out.identity.is_none(), "{store}");
    }
}

/// A LABELLED account is mounted only when it armed, so one holding only a LIVE-named key never
/// reaches `mount` and said nothing at all — `vike-mount` asks the arm to speak for it instead, and
/// the arm says exactly what `mount` says for the default account: the one refusal sentence, once.
#[test]
fn an_account_that_is_never_mounted_is_spoken_for_in_the_same_words() {
    let unmounted = AccountLabel::parse("UNMNT").expect("a legal label");
    let (live_key, _) = live_names();
    let mut live_only = MountFixture::new(&[]);
    live_only.account = unmounted.clone();
    live_only
        .vars
        .insert(account_key(&live_key, &unmounted), "SECRET-VALUE-do-not-log".to_string());
    let (_, events) =
        captured(|| OandaVenueMount.report_unmounted_account(&live_only.inputs(true)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("oanda"), Some("UNMNT")), "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a refusal carries `found_tier`, never `tier`: {e:?}");
    assert!(
        e.message.contains(&account_key(&live_key, &unmounted)),
        "names the LIVE-named variable it found, label-composed: {}",
        e.message
    );
    assert!(!format!("{e:?}").contains("SECRET-VALUE"), "never a value: {e:?}");
    // The REMEDY in that sentence is for THIS account: `vike-cli secrets set` is an upsert, so
    // naming the unlabelled practice keys would tell the operator to overwrite the DEFAULT
    // account's practice token with the labelled account's. The backtick closing the name is what
    // tells the unlabelled key from the labelled one that merely contains it.
    let (demo_key, demo_account) = crate::config::oanda_env_var_names(Environment::Demo);
    for (default, own) in [
        (&demo_key, account_key(&demo_key, &unmounted)),
        (&demo_account, account_key(&demo_account, &unmounted)),
    ] {
        assert!(
            e.message.contains(&format!("vike-cli secrets set {own}`")),
            "the remedy names the labelled account's own `{own}`: {}",
            e.message
        );
        assert!(
            !e.message.contains(&format!("vike-cli secrets set {default}`")),
            "the remedy must not name the default account's `{default}`: {}",
            e.message
        );
    }

    // A practice-only labelled account, and an empty store, are not refusals.
    for quiet in [keyed(&unmounted), MountFixture::new(&[])] {
        let (_, events) =
            captured(|| OandaVenueMount.report_unmounted_account(&quiet.inputs(true)));
        assert!(events.is_empty(), "silent: {events:?}");
    }
}
