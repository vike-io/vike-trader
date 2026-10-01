use std::cell::Cell;

use super::*;
use vike_bridge_core::key_permissions::ALLOW_WITHDRAW_KEYS_ENV;
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_model::account_keys::{AccountLabel, account_key};
use vike_model::credential_keys::{
    API_KEY_SUFFIX, API_SECRET_SUFFIX, attribution_var_for, key_owner, starter_keys,
};

/// This venue's key pair at `tier` (key, then secret), under the names
/// `load_credentials_for_account` reads — taken from the credential grid's own enumeration and
/// classification (`starter_keys`, `key_owner`), neither spelled out nor composed here.
/// `crates/vike-ops/tests/settings_registry.rs` is why on both counts: its literal harvest reads a
/// key name spelled in a `src/` file as a variable this crate names, and its `generated_key_sites`
/// reads a CALL to one of the grid's builders as "this crate reads the whole grid" — counted
/// against `mount.rs`, since the gate folds this `#[cfg(test)]` module back into its owner — and
/// demands a registry row for every name in it. Asking the table module is neither: that module is
/// excluded from the set by construction.
fn pair(tier: Environment, key: &str, secret: &str) -> Vec<(String, String)> {
    [(API_KEY_SUFFIX, key), (API_SECRET_SUFFIX, secret)]
        .into_iter()
        .map(|(suffix, value)| {
            let name = starter_keys(VENUE)
                .into_iter()
                .find(|k| k.ends_with(suffix) && key_owner(k) == Some((VENUE, Some(tier.as_str()))))
                .expect("the credential grid names this venue's key at this tier");
            (name, value.to_string())
        })
        .collect()
}

fn demo_pair() -> Vec<(String, String)> {
    pair(Environment::Demo, "dk", "ds")
}

fn live_pair() -> Vec<(String, String)> {
    pair(Environment::Live, "lk", "ls")
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// A fixture holding every pair of `sets` under `label`'s key names, asking for `label`.
fn keyed(label: &AccountLabel, sets: &[Vec<(String, String)>]) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in sets.iter().flatten() {
        fx.vars.insert(account_key(k, label), v.clone());
    }
    fx.account = label.clone();
    fx
}

/// The REAL body binance answered, captured from the CI box on 2026-08-09 (copied from `vike-mount`'s
/// clock-test fixtures, which read the same capture).
fn clock_body() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/binance.json"))
        .expect("the captured fixture is valid JSON")
}

/// A key pair for the gate's own tests; its values reach no venue.
fn creds() -> Credentials {
    Credentials {
        api_key: "test-key".to_string(),
        api_secret: "test-secret".to_string(),
        passphrase: None,
    }
}

/// A KNOWN withdraw-capable key — the only input that can refuse at all.
fn withdraw_capable() -> Result<KeyPermissions, String> {
    Ok(KeyPermissions { can_withdraw: Some(true), ..KeyPermissions::UNKNOWN })
}

/// The tier the startup credential probe binds for `fx` under the ceiling, or `None` when it
/// offers no probe.
fn probed_tier(fx: &MountFixture, live: bool) -> Option<Tier> {
    match BinanceVenueMount.credential_probe(&fx.inputs(live)) {
        Some(CredentialProbe::RecordsIdentity { bound_tier, .. }) => Some(bound_tier),
        Some(CredentialProbe::ReadOnly(_)) => {
            panic!("binance's probe is the RecordsIdentity shape")
        }
        None => None,
    }
}

/// An exec client that is never driven — the offline stand-in for the spawn, whose real thread
/// would dial the venue the moment it started.
struct Unspawned;

impl ExecutionClient for Unspawned {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// The rows `vike-mount`'s tables carried for binance, pinned as a matrix: a change to any of
/// them is a deliberate edit here too.
#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = BinanceVenueMount.declaration();
    assert_eq!(BinanceVenueMount.venue(), "binance");
    assert!(d.addresses_accounts && d.process_exclusive.is_none());
    assert!(d.takes_recon_trigger, "the resync supervisor pokes the reconcile driver");
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentity::Undeterminable {
            why: "the store holds an HMAC api key/secret pair and no account identifier; which \
                  account a key belongs to is answerable only by an authenticated call",
        }
    );
    assert_eq!(
        d.clock,
        ClockDecl::Wired {
            endpoint: "GET /api/v3/time (public)",
            auth: ClockAuth::Public,
            risk: ClockRisk::SignedTimestamp,
        }
    );
    // …and its row of the fallback-grid table.
    assert_eq!(
        fallback_properties(),
        SymbolProperties {
            tick_size: 0.01,
            step_size: 0.00001,
            min_qty: 0.00001,
            max_qty: 100_000.0,
            min_notional: 5.0,
            ..Default::default()
        }
    );
}

/// THE D1 MATRIX (docs/decisions/0095): the ceiling alone picks the key tier. A `live` ceiling
/// reads the LIVE pair and nothing else — without it the venue is paper for want of the LIVE key
/// set, never armed on the demo pair — and every lower ceiling reads DEMO.
#[test]
fn resolve_reads_exactly_the_tier_the_ceiling_names() {
    let no_credentials = Resolution::Paper(PaperCause::NoCredentials);
    let no_live_key_set = Resolution::Paper(PaperCause::LiveCredentialsAbsent);
    let empty = MountFixture::new(&[]);
    let demo_only = keyed(&AccountLabel::Default, &[demo_pair()]);
    let live_only = keyed(&AccountLabel::Default, &[live_pair()]);
    assert_eq!(BinanceVenueMount.resolve(&empty.inputs(false)), no_credentials);
    assert_eq!(BinanceVenueMount.resolve(&empty.inputs(true)), no_live_key_set);
    assert_eq!(
        BinanceVenueMount.resolve(&demo_only.inputs(false)),
        Resolution::Armed { tier: Tier::Demo, held_below_live: None }
    );
    assert_eq!(
        BinanceVenueMount.resolve(&demo_only.inputs(true)),
        no_live_key_set,
        "a live ceiling never falls back to the demo pair"
    );
    assert_eq!(
        BinanceVenueMount.resolve(&live_only.inputs(true)),
        Resolution::Armed { tier: Tier::Live, held_below_live: None }
    );
    assert_eq!(
        BinanceVenueMount.resolve(&live_only.inputs(false)),
        no_credentials,
        "a lower ceiling never reads the live pair"
    );
}

/// Review Focus 2 at this venue: with BOTH pairs present, a ceiling below `live` resolves the
/// DEMO tier and nothing above it.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let both = keyed(&AccountLabel::Default, &[demo_pair(), live_pair()]);
    assert_eq!(
        BinanceVenueMount.resolve(&both.inputs(false)),
        Resolution::Armed { tier: Tier::Demo, held_below_live: None }
    );
}

/// The account 2×2, under BOTH ceilings: a labelled account reads its OWN keys at the ceiling's
/// tier and never the default account's, and the default account never reads a labelled one's —
/// at the demo tier, and at the live tier, where a second engine signing on another account's key
/// would be signing on real money.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    for (live, set, armed, unarmed) in [
        (false, demo_pair(), Tier::Demo, PaperCause::NoCredentials),
        (true, live_pair(), Tier::Live, PaperCause::LiveCredentialsAbsent),
    ] {
        let keys = std::slice::from_ref(&set);
        let mut alt_asks_default_keys = keyed(&AccountLabel::Default, keys);
        alt_asks_default_keys.account = alt();
        assert_eq!(
            BinanceVenueMount.resolve(&alt_asks_default_keys.inputs(live)),
            Resolution::Paper(unarmed),
            "live={live}: ALT must not arm off the default account's keys"
        );
        assert_eq!(
            BinanceVenueMount.resolve(&keyed(&alt(), keys).inputs(live)),
            Resolution::Armed { tier: armed, held_below_live: None },
            "live={live}: ALT arms on its own keys"
        );
        let mut default_asks_alt_keys = keyed(&alt(), keys);
        default_asks_alt_keys.account = AccountLabel::Default;
        assert_eq!(
            BinanceVenueMount.resolve(&default_asks_alt_keys.inputs(live)),
            Resolution::Paper(unarmed),
            "live={live}: the default account must not arm off ALT's keys"
        );
    }
}

/// No key set for the ceiling's tier — none at all, a `live` ceiling over DEMO keys only, a lower
/// ceiling over LIVE keys only: paper, nothing to reconcile, nothing recorded, and OFFLINE. The
/// doubles panic if reached, which proves the withdraw gate's signed read, the grid pre-fetch and
/// the spawn all sit behind the credential gate; the real `mount` then meets the same gate, and
/// the `.P` symbol shows the perp lane refuses the same way.
#[test]
fn without_a_key_set_for_the_ceilings_tier_the_mount_is_paper_and_offline() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let empty = MountFixture::new(&[]);
    let demo_only = keyed(&AccountLabel::Default, &[demo_pair()]);
    let live_only = keyed(&AccountLabel::Default, &[live_pair()]);
    for (fx, live) in [(&empty, false), (&empty, true), (&demo_only, true), (&live_only, false)] {
        let out = mount_with(
            fx.request(live, "BTCUSDT.P", &tx),
            |_| panic!("a mount with no key set issues no signed read"),
            |_, _, _| panic!("a mount with no key set fetches no grid"),
            |_| panic!("a mount with no key set spawns nothing"),
        );
        assert!(matches!(out.exec, ExecOutcome::Paper), "live={live}");
        assert!(out.recon.is_none() && out.identity.is_none(), "live={live}");
        let out = BinanceVenueMount.mount(fx.request(live, "BTCUSDT.P", &tx));
        assert!(matches!(out.exec, ExecOutcome::Paper), "live={live}");
        assert!(out.recon.is_none() && out.identity.is_none(), "live={live}");
        assert!(BinanceVenueMount.credential_probe(&fx.inputs(live)).is_none(), "live={live}");
    }
}

/// With BOTH key sets present, the ceiling alone decides what the mount signs with and binds:
/// the pair it hands the grid pre-fetch and the spawn, the network both bind, and the bound tier
/// handed to the identity record (inert for binance today) — the tier `resolve` reports. Driven
/// through the mount's own body with only its three network steps replaced, so a tier written into
/// that body as a literal, or a key set read at the wrong tier, fails it.
#[test]
fn the_ceiling_alone_picks_the_key_set_the_network_and_the_bound_tier() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let both = keyed(&AccountLabel::Default, &[demo_pair(), live_pair()]);
    for (live, key, want_env, want_tier) in
        [(false, "dk", Environment::Demo, Tier::Demo), (true, "lk", Environment::Live, Tier::Live)]
    {
        let mut fetched_with = None;
        let mut spawned_with = None;
        let out = mount_with(
            both.request(live, "BTCUSDT", &tx),
            |_| Ok(KeyPermissions::UNKNOWN),
            |env, c, _| {
                fetched_with = Some((env, c.api_key.clone()));
                None
            },
            |s| {
                spawned_with = Some((s.env, s.creds.api_key.clone()));
                Box::new(Unspawned)
            },
        );
        let ExecOutcome::Live(exec) = out.exec else {
            panic!("live={live}: a key set at the ceiling's tier arms the mount");
        };
        assert_eq!(
            exec.bound_tier, want_tier,
            "the tier handed to the identity record (inert for binance today)"
        );
        assert_eq!(
            BinanceVenueMount.resolve(&both.inputs(live)),
            Resolution::Armed { tier: want_tier, held_below_live: None },
            "…is the tier the arming screen reports"
        );
        assert_eq!(fetched_with, Some((want_env, key.to_string())), "the grid pre-fetch");
        assert_eq!(spawned_with, Some((want_env, key.to_string())), "the exec spawn");
    }
}

/// THE TIER at the one startup read that SIGNS — the property `vike-mount`'s `startup_tests.rs`
/// pinned for this venue under this name: the probe reads the key set the ceiling names and binds
/// that tier, so a lower ceiling never sends a signed read to the live account and a `live` one
/// never signs with the demo pair. Pure — the reconcile client is a signer plus a transport.
#[test]
fn a_demo_ceiling_never_signs_against_the_real_money_tier() {
    let both = keyed(&AccountLabel::Default, &[demo_pair(), live_pair()]);
    let demo_only = keyed(&AccountLabel::Default, &[demo_pair()]);
    let live_only = keyed(&AccountLabel::Default, &[live_pair()]);
    assert_eq!(probed_tier(&both, false), Some(Tier::Demo));
    assert_eq!(probed_tier(&both, true), Some(Tier::Live));
    assert_eq!(probed_tier(&demo_only, false), Some(Tier::Demo));
    assert_eq!(probed_tier(&live_only, true), Some(Tier::Live));
    assert_eq!(
        probed_tier(&demo_only, true),
        None,
        "a live ceiling never signs with the demo pair"
    );
    assert_eq!(probed_tier(&live_only, false), None, "a lower ceiling never reads the live pair");
}

/// The pure verdict core (moved from `vike-mount`'s `fee_schedule_tests.rs`): a KNOWN
/// withdraw-capable key REFUSES, the override forces `Allow`, and a trade-only key arms.
#[test]
fn a_known_withdraw_capable_key_refuses_unless_overridden() {
    assert_eq!(binance_withdraw_verdict(withdraw_capable(), false), WithdrawGate::Refuse);
    assert_eq!(binance_withdraw_verdict(withdraw_capable(), true), WithdrawGate::Allow);
    let trade_only = KeyPermissions {
        can_withdraw: Some(false),
        can_trade: Some(true),
        ip_restricted: Some(true),
    };
    assert_eq!(binance_withdraw_verdict(Ok(trade_only), false), WithdrawGate::Allow);
}

/// FAIL-OPEN on introspection (moved): a fetch error, or an all-Unknown body, is NOT evidence of a
/// withdraw-capable key, so it never refuses.
#[test]
fn an_unknown_or_failed_key_probe_never_refuses() {
    let err = Err("venue error -2015: Invalid API-key".to_string());
    assert_eq!(binance_withdraw_verdict(err, false), WithdrawGate::Allow);
    assert_eq!(binance_withdraw_verdict(Ok(KeyPermissions::UNKNOWN), false), WithdrawGate::Allow);
}

/// The DEMO path answers `Allow` WITHOUT the signed read (moved from `vike-mount`'s
/// `fee_schedule_tests.rs`, where it could not fail — a demo gate that DID read would still have
/// answered `Allow`, since any failed read is Unknown): `sapi` is mainnet-only, so there is nothing
/// to introspect on the demo host. The read here answers a withdraw-capable key and counts its
/// calls; the mainnet half is the control that proves the same read WOULD refuse, so the demo
/// `Allow` can only come from never asking.
#[test]
fn a_demo_mount_never_probes_key_permissions() {
    let calls = Cell::new(0);
    let read = |_: &Credentials| {
        calls.set(calls.get() + 1);
        withdraw_capable()
    };
    assert_eq!(binance_withdraw_gate(false, &creds(), false, read), WithdrawGate::Allow);
    assert_eq!(calls.get(), 0, "a demo mount must not issue the signed read");
    assert_eq!(binance_withdraw_gate(true, &creds(), false, read), WithdrawGate::Refuse);
    assert_eq!(calls.get(), 1, "a mainnet mount issues it exactly once");
}

/// THE GATE IN THE MOUNT: a mainnet mount whose key is KNOWN withdraw-capable stays paper, and the
/// refusal comes before anything is built — no grid pre-fetch, no reconcile client, no spawn — as
/// the legacy arm's guard sent the venue to the paper arm. `flags.allow_withdraw_keys`, folded into
/// the map as `"1"`, lets the same key arm.
#[test]
fn a_withdraw_capable_key_keeps_a_mainnet_mount_paper_before_anything_is_built() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut fx = keyed(&AccountLabel::Default, &[live_pair()]);
    let out = mount_with(
        fx.request(true, "BTCUSDT", &tx),
        |_| withdraw_capable(),
        |_, _, _| panic!("a refused mount fetches no grid"),
        |_| panic!("a refused mount spawns nothing"),
    );
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
    fx.vars.insert(ALLOW_WITHDRAW_KEYS_ENV.to_string(), "1".to_string());
    let out = mount_with(
        fx.request(true, "BTCUSDT", &tx),
        |_| withdraw_capable(),
        |_, _, _| None,
        |_| Box::new(Unspawned),
    );
    assert!(matches!(out.exec, ExecOutcome::Live(_)), "the override arms the same key");
}

/// Decision 0095: the override is `flags.allow_withdraw_keys`, which the composition root folds
/// into the credential map it hands the mount — and that map is the gate's ONLY source; no process
/// environment is consulted. Moved from `vike-mount`'s `arming.rs` (`withdraw_override_tests`),
/// now read through this mount's own `withdraw_override` over `MountInputs::secrets`. The fold's
/// half is `crates/vike-tradehub/src/tradehub_cli_tests.rs`'s
/// `the_withdraw_override_is_the_row_alone`: the two meet at the string the fold writes.
#[test]
fn the_folded_row_is_the_only_source() {
    let verdict = |fx: &MountFixture| {
        binance_withdraw_verdict(withdraw_capable(), withdraw_override(&fx.inputs(true)))
    };
    assert_eq!(verdict(&MountFixture::new(&[(ALLOW_WITHDRAW_KEYS_ENV, "1")])), WithdrawGate::Allow);
    assert_eq!(
        verdict(&MountFixture::new(&[(ALLOW_WITHDRAW_KEYS_ENV, "0")])),
        WithdrawGate::Refuse
    );
    assert_eq!(verdict(&MountFixture::new(&[])), WithdrawGate::Refuse, "the default is REFUSE");
}

/// An armed mount carries a reconcile handle on BOTH lanes of the one venue id and at BOTH tiers,
/// with the global reconcile gate OFF. The legacy arm built it unconditionally — it is pure to
/// construct, and `vike-mount`'s shared tail reads the fee schedule through it whatever the gate
/// says — so a mount that gated it on `recon_enabled` would change what a mount with
/// reconciliation off does. (`vike-mount`'s `recon_client_tests` asserted this venue through
/// `build_recon_client`'s `"binance"` arm, which this move deleted — the dispatch itself went with
/// okx's port, the last of the trio; a bare call of the factory, which always answers `Some`,
/// could not fail.)
#[test]
fn an_armed_mount_carries_a_reconcile_handle_on_both_lanes_at_both_tiers() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let both = keyed(&AccountLabel::Default, &[demo_pair(), live_pair()]);
    for symbol in ["BTCUSDT", "BTCUSDT.P"] {
        for live in [false, true] {
            let req = both.request(live, symbol, &tx);
            assert!(!req.recon_enabled, "precondition: the reconcile gate is off");
            let out = mount_with(
                req,
                |_| Ok(KeyPermissions::UNKNOWN),
                |_, _, _| None,
                |_| Box::new(Unspawned),
            );
            assert!(matches!(out.exec, ExecOutcome::Live(_)), "{symbol} live={live}");
            assert!(out.recon.is_some(), "{symbol} live={live}: no reconcile handle");
        }
    }
}

/// What the spawn is handed is what the legacy arm threaded: the request's reconnect trigger, the
/// store's Broker/Link id, the `[risk]` leverage and the folded TRADE_LITE switch — each at its
/// byte-identical default when nothing sets it — plus the mounted symbol with its `.P` kept (the
/// lane is the adapter's to split) and the fallback grid; and the outcome carries the pre-fetched
/// grid. A mount past the budget refusal is reached nowhere offline but here.
#[test]
fn the_spawn_is_handed_what_the_request_and_the_store_say() {
    let (tx, _rx) = vike_exec::event_channel(8);
    // Nothing set: every argument at its default.
    let bare = keyed(&AccountLabel::Default, &[demo_pair()]);
    let mut spawned = None;
    let out = mount_with(
        bare.request(false, "BTCUSDT.P", &tx),
        |_| panic!("a demo mount issues no signed read"),
        |_, _, _| None,
        |s| {
            spawned = Some(s);
            Box::new(Unspawned)
        },
    );
    assert!(matches!(out.exec, ExecOutcome::Live(_)));
    let s = spawned.expect("the mount spawned its exec client");
    assert!(s.on_reconcile.is_none() && s.link_id.is_none() && !s.trade_lite_fill);
    assert_eq!(s.leverage, 2.0, "unset `[risk] max_leverage` is the historical 2x");

    // Everything set.
    let (trigger, _poked) = std::sync::mpsc::channel();
    let mut fx = keyed(&AccountLabel::Default, &[demo_pair()]);
    // The attribution name a writer fills for this venue's mechanic (`BINANCE_BROKER_CODE`), asked
    // of the table module for the reason `pair`'s doc gives.
    let link_id_name = attribution_var_for(VENUE).expect("binance attributes by a coid prefix");
    fx.vars.insert(link_id_name, "LINK1".to_string());
    // `venue.binance.trade_lite_fill` — a `venue_setting` row, which the mount reads from
    // `MountInputs::settings` (decision 0095, Task 7).
    fx.settings = vike_secrets::venue_setting::VenueSettings::from_rows(
        VENUE,
        &[vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: None,
            field: "TRADE_LITE_FILL".to_string(),
            value: "1".to_string(),
        }],
    );
    let profile = vike_exec::ProfileRisk { max_leverage: Some(5.0), ..Default::default() };
    let mut req = fx.request(false, "BTCUSDT.P", &tx);
    req.recon_trigger = Some(trigger);
    req.risk_profile = Some(&profile);
    let grid = SymbolProperties { tick_size: 0.1, ..Default::default() };
    let mut spawned = None;
    let out = mount_with(
        req,
        |_| panic!("a demo mount issues no signed read"),
        |_, _, symbol| {
            assert_eq!(symbol, "BTCUSDT.P", "the pre-fetch routes the lane itself");
            Some(grid)
        },
        |s| {
            spawned = Some(s);
            Box::new(Unspawned)
        },
    );
    let ExecOutcome::Live(exec) = out.exec else { panic!("a demo key set arms the mount") };
    assert_eq!(exec.grid, Some(grid), "the outcome carries the pre-fetched grid");
    assert!(exec.contract_size.is_none() && exec.margin_mode.is_none());
    assert!(exec.leg_grids.is_empty());
    let s = spawned.expect("the mount spawned its exec client");
    assert_eq!(s.symbol, "BTCUSDT.P");
    assert_eq!(s.fallback, fallback_properties());
    assert!(s.on_reconcile.is_some(), "the resync supervisor gets the reconnect trigger");
    assert_eq!(s.link_id.as_deref(), Some("LINK1"));
    assert_eq!(s.leverage, 5.0);
    assert!(s.trade_lite_fill);
    assert!(s.properties_rec.is_none());
}

// ---- the clock PARSE, against the real captured body (moved from vike-mount) ------------------

#[test]
fn the_parser_reads_the_real_captured_body() {
    assert_eq!(parse_server_time(&clock_body()), Ok(1_786_242_369_664));
}

/// A body that answered but carries no stamp is an ERROR naming the field, never a zero.
#[test]
fn a_body_without_the_stamp_names_the_missing_field() {
    assert_eq!(parse_server_time(&serde_json::json!({})), Err(missing_time_field("serverTime")));
}
