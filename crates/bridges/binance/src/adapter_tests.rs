use std::cell::Cell;

use super::*;
use vike_bridge_core::venue_adapter::{ArmingNotRunningCause, missing_time_field};
use vike_bridge_core::venue_adapter_fixture::{
    AdapterFixture, ExecutionClientInert, assert_one_key_set_resolves_its_tier,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::key_permissions::ALLOW_WITHDRAW_KEYS_ENV;
use vike_venues::credential_keys::{API_KEY, API_SECRET, attribution_var_for};

/// A fixture whose started account holds the complete pair (`API_KEY` = `"k"`, `API_SECRET` =
/// `"s"`) — the account's fields, by the field words `api_credentials` reads (decision 0156 item 6:
/// no name is composed, so none is spelled here) — and the release's addresses (a fresh box's rows).
fn paired() -> AdapterFixture {
    seeded(AdapterFixture::new(&[(API_KEY, "k"), (API_SECRET, "s")]))
}

/// A fixture whose started account holds the key and NOT the secret — a half-written pair.
fn half_paired() -> AdapterFixture {
    seeded(AdapterFixture::new(&[(API_KEY, "k")]))
}

/// `fx` carrying the release's `venue_url` rows for binance, as a fresh box's `AdapterInputs::settings` does.
fn seeded(mut fx: AdapterFixture) -> AdapterFixture {
    fx.settings = vike_secrets::venue_setting::VenueSettings::default().with_urls(urls());
    fx
}

/// The release's binance rows.
fn urls() -> vike_venues::venue_urls::VenueUrls {
    vike_venues::venue_urls::VenueUrls::seed(VENUE)
}

/// The addresses those rows give an account at `tier`.
fn hosts(tier: AccountTier) -> BinanceUrls {
    BinanceUrls::resolve(&urls(), tier).expect("the seed holds every binance row")
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The REAL body binance answered, captured from the CI box on 2026-08-09 (copied from `vike-tradehub-core`'s
/// clock-test fixtures, which read the same capture).
fn clock_body() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/binance.json"))
        .expect("the captured fixture is valid JSON")
}

/// A key pair for the gate's own tests; its values reach no venue.
fn creds() -> Credentials {
    Credentials::for_test("test-key", "test-secret", None)
}

/// A KNOWN withdraw-capable key — the only input that can refuse at all.
fn withdraw_capable() -> Result<KeyPermissions, String> {
    Ok(KeyPermissions { can_withdraw: Some(true), ..KeyPermissions::UNKNOWN })
}

/// The tier the startup credential probe binds for `fx` at the account's tier `live`, or `None`
/// when it offers no probe.
fn probed_tier(fx: &AdapterFixture, live: AccountTier) -> Option<AccountTier> {
    match VenueAdapterBinance.credential_probe(&fx.inputs(live)) {
        Some(CredentialProbe::RecordsIdentity { bound_tier, .. }) => Some(bound_tier),
        Some(CredentialProbe::ReadOnly(_)) => {
            panic!("binance's probe is the RecordsIdentity shape")
        }
        None => None,
    }
}

/// The COIN-M row read of a start whose symbol is not a COIN-M name: never asked.
fn no_coin_m(_: &str, symbol: &str) -> Result<crate::execution::rest::coin_m::CoinMMount, String> {
    panic!("{symbol} is not a COIN-M name: its start reads no dapi row")
}

/// The rows `vike-tradehub-core`'s tables carried for binance, pinned as a matrix: a change to any of
/// them is a deliberate edit here too.
#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = VenueAdapterBinance.declaration();
    assert_eq!(VenueAdapterBinance.venue(), "binance");
    assert!(d.addresses_accounts && d.process_exclusive.is_none());
    assert!(d.takes_reconcile_trigger, "the resync supervisor pokes the reconcile driver");
    assert_eq!(d.grid_source, GridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentityRule::Undeterminable {
            why: "the store holds an HMAC api key/secret pair and no account identifier; which \
                  account a key belongs to is answerable only by an authenticated call",
        }
    );
    assert_eq!(
        d.clock,
        ClockDeclaration::Wired {
            endpoint: "GET /api/v3/time (public)",
            auth: ClockAuthentication::Public,
            risk: ClockRisk::SignedTimestamp,
        }
    );
}

/// THE D1 MATRIX (docs/decisions/0095, decision 0156 item 6): the account's tier is the network,
/// and its key pair is the one it holds. A complete pair arms the account's own tier — `live` is
/// mainnet, every lower tier demo; no pair (or half of one) is paper, and under a `live` tier the
/// cause names the missing LIVE key set.
#[test]
fn resolve_reads_exactly_the_tier_the_ceiling_names() {
    let absent = [AdapterFixture::new(&[]), half_paired()];
    assert_one_key_set_resolves_its_tier(&VenueAdapterBinance, absent, &paired());
}

/// The hosts at EVERY tier, `Paper` included: `live` dials mainnet's rows and every lower tier the
/// demo network's, through the one lookup the start, the leverage write and the mode heal share.
#[test]
fn every_tier_dials_the_hosts_of_the_network_it_names() {
    for (tier, bound) in [
        (AccountTier::Live, AccountTier::Live),
        (AccountTier::Demo, AccountTier::Demo),
        (AccountTier::Paper, AccountTier::Demo),
    ] {
        assert_eq!(hosts_of(&paired().inputs(tier)).ok(), Some(hosts(bound)), "{tier}");
    }
}

/// Review Focus 2 at this venue: a complete pair under a tier below `live` resolves the DEMO tier
/// and nothing above it — `Paper` reads as `Demo`, never as `Live`.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    for below in [AccountTier::Demo, AccountTier::Paper] {
        assert_eq!(
            VenueAdapterBinance.resolve(&paired().inputs(below)),
            ArmingResolution::Armed { tier: AccountTier::Demo, held_below_live: None },
            "tier={below}"
        );
    }
}

/// The LABEL picks no key: the start reads the credentials it is handed — the started account's
/// own, which `vike-tradehub-core` resolved from the account row — so a labelled account arms on its pair
/// and does not run without one, exactly as the default account does, at both tiers.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    for (live, unarmed) in [
        (AccountTier::Demo, ArmingNotRunningCause::NoCredentials),
        (AccountTier::Live, ArmingNotRunningCause::LiveCredentialsAbsent),
    ] {
        let mut alt_paired = paired();
        alt_paired.account = alt();
        assert_eq!(
            VenueAdapterBinance.resolve(&alt_paired.inputs(live)),
            ArmingResolution::Armed { tier: live, held_below_live: None },
            "live={live}: ALT arms on its own keys"
        );
        let mut alt_empty = AdapterFixture::new(&[]);
        alt_empty.account = alt();
        assert_eq!(
            VenueAdapterBinance.resolve(&alt_empty.inputs(live)),
            ArmingResolution::NotRunning(unarmed),
            "live={live}: ALT without keys of its own is paper"
        );
    }
}

/// No complete key pair — none at all, or a key without its secret — at either tier: paper,
/// nothing to reconcile, nothing recorded, and OFFLINE. The doubles panic if reached, which proves
/// the withdraw gate's signed read, the grid pre-fetch and the spawn all sit behind the credential
/// gate; the real `start` then meets the same gate, and the `.P` symbol shows the perp lane refuses
/// the same way.
#[test]
fn without_a_key_set_for_the_ceilings_tier_the_start_is_paper_and_offline() {
    let (tx, _rx) = vike_execution::event_channel(8);
    let empty = AdapterFixture::new(&[]);
    let half = half_paired();
    for (fx, live) in [
        (&empty, AccountTier::Demo),
        (&empty, AccountTier::Live),
        (&half, AccountTier::Demo),
        (&half, AccountTier::Live),
    ] {
        let out = start_with(
            fx.request(live, "BTCUSDT.P", &tx),
            |_, _| panic!("a mount with no key set issues no signed read"),
            |_, _, _| panic!("a mount with no key set fetches no grid"),
            |_, _| panic!("a mount with no key set reads no COIN-M row"),
            |_| panic!("a mount with no key set spawns nothing"),
        );
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "live={live}");
        assert!(out.reconcile.is_none() && out.identity.is_none(), "live={live}");
        let out = VenueAdapterBinance.start(fx.request(live, "BTCUSDT.P", &tx));
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "live={live}");
        assert!(out.reconcile.is_none() && out.identity.is_none(), "live={live}");
        assert!(VenueAdapterBinance.credential_probe(&fx.inputs(live)).is_none(), "live={live}");
    }
}

/// The account's tier alone decides the network the start binds, and its own pair is what it
/// signs with: the pair it hands the grid pre-fetch and the spawn, the network both bind, and the
/// bound tier handed to the identity record (inert for binance today) — the tier `resolve`
/// reports. Driven through the start's own body with only its three network steps replaced, so a
/// tier written into that body as a literal, or a key read from anywhere but the account, fails it.
#[test]
fn the_ceiling_alone_picks_the_key_set_the_network_and_the_bound_tier() {
    let (tx, _rx) = vike_execution::event_channel(8);
    let account = paired();
    // The tier the hosts bind (`bound_tier`'s answer), and the tier the start reports, are
    // the account's own.
    for live in [AccountTier::Demo, AccountTier::Live] {
        let (want_env, want_tier) = (hosts(live), live);
        let mut fetched_with = None;
        let mut spawned_with = None;
        let out = start_with(
            account.request(live, "BTCUSDT", &tx),
            |_, _| Ok(KeyPermissions::UNKNOWN),
            |env, c, _| {
                fetched_with = Some((env, c.api_key.expose().clone()));
                None
            },
            no_coin_m,
            |s| {
                spawned_with = Some((s.hosts, s.creds.api_key.expose().clone()));
                Box::new(ExecutionClientInert)
            },
        );
        let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
            panic!("live={live}: the account's key pair arms the mount");
        };
        assert_eq!(
            exec.bound_tier, want_tier,
            "the tier handed to the identity record (inert for binance today)"
        );
        assert_eq!(
            VenueAdapterBinance.resolve(&account.inputs(live)),
            ArmingResolution::Armed { tier: want_tier, held_below_live: None },
            "…is the tier the arming screen reports"
        );
        assert_eq!(fetched_with, Some((want_env, "k".to_string())), "the grid pre-fetch");
        assert_eq!(spawned_with, Some((want_env, "k".to_string())), "the exec spawn");
    }
}

/// THE TIER at the one startup read that SIGNS — the property `vike-tradehub-core`'s `startup_tests.rs`
/// pinned for this venue under this name: the probe signs with the account's pair and binds the
/// account's own tier, so a `demo` account never sends a signed read to the live network, and an
/// account without a complete pair is not probed at all. Pure — the reconcile client is a signer
/// plus a transport.
#[test]
fn a_demo_ceiling_never_signs_against_the_real_money_tier() {
    assert_eq!(probed_tier(&paired(), AccountTier::Demo), Some(AccountTier::Demo));
    assert_eq!(probed_tier(&paired(), AccountTier::Live), Some(AccountTier::Live));
    for live in [AccountTier::Demo, AccountTier::Live] {
        assert_eq!(probed_tier(&AdapterFixture::new(&[]), live), None, "live={live}: no pair");
        assert_eq!(probed_tier(&half_paired(), live), None, "live={live}: half a pair");
    }
}

/// The pure verdict core (moved from `vike-tradehub-core`'s `fee_schedule_tests.rs`): a KNOWN
/// withdraw-capable key REFUSES, the override forces `Allow`, and a trade-only key arms.
#[test]
fn a_known_withdraw_capable_key_refuses_unless_overridden() {
    assert_eq!(binance_withdraw_verdict(withdraw_capable(), false), CredentialWithdrawGate::Refuse);
    assert_eq!(binance_withdraw_verdict(withdraw_capable(), true), CredentialWithdrawGate::Allow);
    let trade_only = KeyPermissions {
        can_withdraw: Some(false),
        can_trade: Some(true),
        ip_restricted: Some(true),
    };
    assert_eq!(binance_withdraw_verdict(Ok(trade_only), false), CredentialWithdrawGate::Allow);
}

/// FAIL-OPEN on introspection (moved): a fetch error, or an all-Unknown body, is NOT evidence of a
/// withdraw-capable key, so it never refuses.
#[test]
fn an_unknown_or_failed_key_probe_never_refuses() {
    let err = Err("venue error -2015: Invalid API-key".to_string());
    assert_eq!(binance_withdraw_verdict(err, false), CredentialWithdrawGate::Allow);
    assert_eq!(
        binance_withdraw_verdict(Ok(KeyPermissions::UNKNOWN), false),
        CredentialWithdrawGate::Allow
    );
}

/// The DEMO path answers `Allow` WITHOUT the signed read (moved from `vike-tradehub-core`'s
/// `fee_schedule_tests.rs`, where it could not fail — a demo gate that DID read would still have
/// answered `Allow`, since any failed read is Unknown): `sapi` is mainnet-only, so there is nothing
/// to introspect on the demo host. The read here answers a withdraw-capable key and counts its
/// calls; the mainnet half is the control that proves the same read WOULD refuse, so the demo
/// `Allow` can only come from never asking.
#[test]
fn a_demo_start_never_probes_key_permissions() {
    let calls = Cell::new(0);
    let read = |_: &Credentials| {
        calls.set(calls.get() + 1);
        withdraw_capable()
    };
    assert_eq!(binance_withdraw_gate(false, &creds(), false, read), CredentialWithdrawGate::Allow);
    assert_eq!(calls.get(), 0, "a demo mount must not issue the signed read");
    assert_eq!(binance_withdraw_gate(true, &creds(), false, read), CredentialWithdrawGate::Refuse);
    assert_eq!(calls.get(), 1, "a mainnet mount issues it exactly once");
}

/// THE GATE IN THE START: a mainnet start whose key is KNOWN withdraw-capable does not run, and the
/// refusal comes before anything is built — no grid pre-fetch, no reconcile client, no spawn.
/// `flags.allow_withdraw_keys`, folded into the map as `"1"`, lets the same key arm.
#[test]
fn a_withdraw_capable_key_stops_a_mainnet_start_before_anything_is_built() {
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut fx = paired();
    let mut asked_at = None;
    let out = start_with(
        fx.request(AccountTier::Live, "BTCUSDT", &tx),
        |spot_rest, _| {
            asked_at = Some(spot_rest);
            withdraw_capable()
        },
        |_, _, _| panic!("a refused mount fetches no grid"),
        no_coin_m,
        |_| panic!("a refused mount spawns nothing"),
    );
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
    assert_eq!(asked_at, Some(hosts(AccountTier::Live).spot_rest), "sapi is the LIVE spot host's");
    fx.vars.insert(ALLOW_WITHDRAW_KEYS_ENV.to_string(), "1".to_string());
    let out = start_with(
        fx.request(AccountTier::Live, "BTCUSDT", &tx),
        |_, _| withdraw_capable(),
        |_, _, _| None,
        no_coin_m,
        |_| Box::new(ExecutionClientInert),
    );
    assert!(
        matches!(out.exec, VenueAdapterExecutionOutcome::Running(_)),
        "the override arms the same key"
    );
}

/// Decision 0095: the override is `flags.allow_withdraw_keys`, which the composition root folds
/// into the credential map it hands the start — and that map is the gate's ONLY source. Moved from
/// `vike-tradehub-core`'s `arming.rs` (`withdraw_override_tests`), now read through this start's own
/// `withdraw_override` over `AdapterInputs::secrets`. The fold's half is
/// `crates/vike-tradehub/src/tradehub_cli/tests/precedence.rs`'s `the_withdraw_override_is_the_row_alone`: the
/// two meet at the string the fold writes.
///
/// ⚠ **Driven through [`start_with`], i.e. through [`binance_withdraw_gate`] itself.** This test
/// used to call `binance_withdraw_verdict` over `withdraw_override`, the two halves of the gate
/// but not the gate: re-pointing the start's call at another source, or having the gate ignore the
/// override it is handed, left it green. Here every case runs the whole mainnet start over a key
/// KNOWN to withdraw, so the outcome (live, or no client and NOT RUNNING) is the gate's own verdict on the
/// folded row alone, and the probe counter proves the gate really ran — a mainnet start issues
/// the signed read exactly once whichever way it then decides.
///
/// ⚠ What no in-process test can show is that the PROCESS ENVIRONMENT is not consulted: the
/// workspace forbids `unsafe`, so `set_var` is not available to plant `VIKE_ALLOW_WITHDRAW_KEYS`.
/// That half is held by the settings registry (`every_read_variable_is_declared` fails on an
/// undeclared `env::var`, and the venue-settings rule
/// `no_venue_setting_is_read_from_the_environment` refuses a bridge read of any venue variable).
#[test]
fn the_folded_row_is_the_only_source() {
    let (tx, _rx) = vike_execution::event_channel(8);
    // `(armed live, signed reads issued)` for a mainnet start of a withdraw-capable key whose
    // folded row is `row` (`None` = no row at all).
    let start_probed = |row: Option<&str>| {
        let mut fx = paired();
        if let Some(value) = row {
            fx.vars.insert(ALLOW_WITHDRAW_KEYS_ENV.to_string(), value.to_string());
        }
        let probes = Cell::new(0);
        let out = start_with(
            fx.request(AccountTier::Live, "BTCUSDT", &tx),
            |_, _| {
                probes.set(probes.get() + 1);
                withdraw_capable()
            },
            |_, _, _| None,
            no_coin_m,
            |_| Box::new(ExecutionClientInert),
        );
        (matches!(out.exec, VenueAdapterExecutionOutcome::Running(_)), probes.get())
    };
    assert_eq!(start_probed(Some("1")), (true, 1), "the folded `1` lets the key arm");
    assert_eq!(start_probed(Some("0")), (false, 1), "a folded `0` keeps the guard");
    assert_eq!(start_probed(None), (false, 1), "no row: the default is REFUSE");
    // Only the exact `1` the fold writes counts — the fuzzy spellings an environment flag was once
    // read with do not.
    for spelling in ["true", "yes", " 1", "1 ", ""] {
        assert_eq!(start_probed(Some(spelling)), (false, 1), "{spelling:?} must not arm the key");
    }
}

/// An armed start carries a reconcile handle on BOTH lanes of the one venue id and at BOTH tiers,
/// with the global reconcile gate OFF. The legacy arm built it unconditionally — it is pure to
/// construct, and `vike-tradehub-core`'s shared tail reads the fee schedule through it whatever the gate
/// says — so a start that gated it on `recon_enabled` would change what a start with
/// reconciliation off does. (`vike-tradehub-core`'s `recon_client_tests` asserted this venue through
/// `build_recon_client`'s `"binance"` arm, which this move deleted — the dispatch itself went with
/// okx's port, the last of the trio; a bare call of the factory, which always answers `Some`,
/// could not fail.)
#[test]
fn an_armed_start_carries_a_reconcile_handle_on_both_lanes_at_both_tiers() {
    let (tx, _rx) = vike_execution::event_channel(8);
    let account = paired();
    for symbol in ["BTCUSDT", "BTCUSDT.P"] {
        for live in [AccountTier::Demo, AccountTier::Live] {
            let req = account.request(live, symbol, &tx);
            assert!(!req.reconcile_enabled, "precondition: the reconcile gate is off");
            let out = start_with(
                req,
                |_, _| Ok(KeyPermissions::UNKNOWN),
                |_, _, _| None,
                no_coin_m,
                |_| Box::new(ExecutionClientInert),
            );
            assert!(
                matches!(out.exec, VenueAdapterExecutionOutcome::Running(_)),
                "{symbol} live={live}"
            );
            assert!(out.reconcile.is_some(), "{symbol} live={live}: no reconcile handle");
        }
    }
}

/// What the spawn is handed is what the legacy arm threaded: the request's reconnect trigger, the
/// store's Broker/Link id, the `[risk]` leverage and the folded TRADE_LITE switch — each at its
/// byte-identical default when nothing sets it — plus the engine symbol with its `.P` kept (the
/// lane is the adapter's to split), never a fallback grid; and the outcome carries the pre-fetched
/// grid. A start past the budget refusal is reached nowhere offline but here.
#[test]
fn the_spawn_is_handed_what_the_request_and_the_store_say() {
    let (tx, _rx) = vike_execution::event_channel(8);
    // Nothing set: every argument at its default.
    let bare = paired();
    let mut spawned = None;
    let out = start_with(
        bare.request(AccountTier::Demo, "BTCUSDT.P", &tx),
        |_, _| panic!("a demo mount issues no signed read"),
        |_, _, _| None,
        no_coin_m,
        |s| {
            spawned = Some(s);
            Box::new(ExecutionClientInert)
        },
    );
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::Running(_)));
    let s = spawned.expect("the mount spawned its exec client");
    assert!(s.on_reconcile.is_none() && s.link_id.is_none() && !s.trade_lite_fill);
    assert_eq!(s.leverage, 2.0, "unset `[risk] max_leverage` is the historical 2x");
    // A DEMO start values BNB on the MAINNET ticker, as it always did (a price, not an account), in the quote
    // the MAINNET list names.
    assert_eq!(s.ticker_rest, "https://api.binance.com");
    assert_eq!(s.usdm_list_rest, "https://fapi.binance.com");
    assert_eq!(s.hosts, hosts(AccountTier::Demo));

    // Everything set.
    let (trigger, _poked) = std::sync::mpsc::channel();
    let mut fx = paired();
    // The attribution name a writer fills for this venue's mechanic (`BINANCE_BROKER_CODE`) — a
    // tier-less DEPLOYMENT name, so it rides `AdapterInputs::secrets`, not the account's fields —
    // asked of the table module rather than spelled out: the literal harvest of
    // `crates/vike-ops/tests/settings_secrets/settings_registry.rs` reads a name spelled in a
    // `src/` file as a variable this crate names.
    let link_id_name =
        attribution_var_for(VENUE).expect("binance attributes by an order client id prefix");
    fx.vars.insert(link_id_name, "LINK1".to_string());
    // `venue.binance.trade_lite_fill` — a `venue_setting` row, which the start reads from
    // `AdapterInputs::settings` (decision 0095, Task 7).
    fx.settings = vike_secrets::venue_setting::VenueSettings::from_rows(
        VENUE,
        &[vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: None,
            field: "TRADE_LITE_FILL".to_string(),
            value: "1".to_string(),
        }],
    )
    .with_urls(urls());
    let profile = vike_model::ProfileRisk { max_leverage: Some(5.0), ..Default::default() };
    let mut req = fx.request(AccountTier::Demo, "BTCUSDT.P", &tx);
    req.reconcile_trigger = Some(trigger);
    req.risk_profile = Some(&profile);
    let grid = SymbolProperties { tick_size: 0.1, ..Default::default() };
    let mut spawned = None;
    let out = start_with(
        req,
        |_, _| panic!("a demo mount issues no signed read"),
        |_, _, symbol| {
            assert_eq!(symbol, "BTCUSDT.P", "the pre-fetch routes the lane itself");
            Some(grid)
        },
        no_coin_m,
        |s| {
            spawned = Some(s);
            Box::new(ExecutionClientInert)
        },
    );
    let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
        panic!("a demo key set arms the mount")
    };
    assert_eq!(exec.grid, Some(grid), "the outcome carries the pre-fetched grid");
    assert!(exec.contract_value.is_none() && exec.margin_mode.is_none());
    assert!(exec.leg_grids.is_empty());
    let s = spawned.expect("the mount spawned its exec client");
    assert_eq!(s.symbol, "BTCUSDT.P");
    assert!(s.on_reconcile.is_some(), "the resync supervisor gets the reconnect trigger");
    assert_eq!(s.link_id.as_deref(), Some("LINK1"));
    assert_eq!(s.leverage, 5.0);
    assert!(s.trade_lite_fill);
    assert!(s.properties_rec.is_none());
}

/// **A COIN-M mount reports its row's inverse contract value** (decisions 0170, 0258): `Inverse(contractSize)`
/// and the row's grid, read on the account's network, never the USDⓈ-M grid pre-fetch. The spawn and the
/// reconcile client are built as for any perp mount.
#[test]
fn a_coin_m_mount_reports_its_rows_inverse_value_and_grid() {
    let (tx, _rx) = vike_execution::event_channel(8);
    let list: serde_json::Value = serde_json::from_str(include_str!(
        "../tests/fixtures/exchange_info/dapi_demo_classes_2026-10-30.json"
    ))
    .expect("the committed dapi list");
    for (live, symbol, face) in [
        (AccountTier::Demo, "BTCUSD_PERP.P", 100.0),
        (AccountTier::Live, "ETHUSD_PERP.P", 10.0),
        (AccountTier::Demo, "BTCUSD_261225.P", 100.0),
    ] {
        let mut asked = None;
        let mut spawned = None;
        let out = start_with(
            paired().request(live, symbol, &tx),
            |_, _| Ok(KeyPermissions::UNKNOWN),
            |_, _, _| panic!("a COIN-M mount reads its dapi row, not the USDⓈ-M grid"),
            |dapi, s| {
                asked = Some((dapi, s.to_string()));
                crate::execution::rest::coin_m::mount_of(s, &list)
            },
            |s| {
                spawned = Some(s.symbol.clone());
                Box::new(ExecutionClientInert)
            },
        );
        let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
            panic!("{symbol}: a readable row mounts")
        };
        assert_eq!(
            exec.contract_value,
            Some(vike_model::ContractValue {
                kind: vike_model::NotionalKind::Inverse,
                per_unit: face
            }),
            "{symbol}"
        );
        let grid = exec.grid.expect("the row's grid");
        assert_eq!((grid.tick_size > 0.0, grid.step_size, grid.contract_size), (true, 1.0, 0.0));
        assert_eq!(
            asked,
            Some((hosts(live).dapi_rest, symbol.to_string())),
            "the account's own network"
        );
        assert_eq!(spawned.as_deref(), Some(symbol));
        assert!(out.reconcile.is_some(), "{symbol}: the reconcile client of the mount");
    }
}

/// **NO ROW, NO MOUNT**: a COIN-M name whose row cannot be read does not run (never valued linear, never the
/// common face guessed), and nothing is spawned.
#[test]
fn a_coin_m_mount_whose_row_cannot_be_read_does_not_run() {
    let (tx, _rx) = vike_execution::event_channel(8);
    let out = start_with(
        paired().request(AccountTier::Demo, "BTCUSD_PERP.P", &tx),
        |_, _| Ok(KeyPermissions::UNKNOWN),
        |_, _, _| panic!("no USDⓈ-M grid for a COIN-M name"),
        |_, _| Err("the dapi list could not be read".to_string()),
        |_| panic!("an unvalued COIN-M mount spawns nothing"),
    );
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// **NO ADDRESS ROW, NO START** (the venue_url table): an account whose rows name no address for its tier does
/// not run, before any network step (the doubles panic if reached), and a demo account whose table holds only the
/// LIVE rows is refused rather than sent to mainnet: channels `trade` and `user` never fall back. Its credential
/// probe FAILS in the row's words (owner ruling B-19) and asks nothing.
#[test]
fn an_account_without_its_address_rows_does_not_run_and_never_reaches_live() {
    let (tx, _rx) = vike_execution::event_channel(8);
    let live_only = vike_venues::venue_urls::VenueUrls::seed_typed(VENUE, UrlType::Live);
    for (rows, tier) in [
        (vike_venues::venue_urls::VenueUrls::default(), AccountTier::Demo),
        (vike_venues::venue_urls::VenueUrls::default(), AccountTier::Live),
        (live_only, AccountTier::Demo),
    ] {
        let mut fx = paired();
        fx.settings = vike_secrets::venue_setting::VenueSettings::default().with_urls(rows);
        let out = start_with(
            fx.request(tier, "BTCUSDT.P", &tx),
            |_, _| panic!("no signed read without an address"),
            |_, _, _| panic!("no grid without an address"),
            no_coin_m,
            |_| panic!("nothing spawns without an address"),
        );
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "{tier:?}");
        assert!(out.reconcile.is_none() && out.identity.is_none(), "{tier:?}");
        match VenueAdapterBinance.credential_probe(&fx.inputs(tier)) {
            Some(CredentialProbe::ReadOnly(probe)) => {
                let why = probe().expect_err("the probe fails");
                let refusal = hosts_of(&fx.inputs(tier)).expect_err("no row").to_string();
                assert_eq!(why, refusal, "{tier:?}: the probe fails in the row's words");
            }
            _ => panic!("{tier:?}: a refused address row is probed by a probe that fails"),
        }
    }
}

// ---- the clock PARSE, against the real captured body (moved from vike-tradehub-core) ------------------

#[test]
fn the_parser_reads_the_real_captured_body() {
    assert_eq!(parse_server_time(&clock_body()), Ok(1_786_242_369_664));
}

/// A body that answered but carries no stamp is an ERROR naming the field, never a zero.
#[test]
fn a_body_without_the_stamp_names_the_missing_field() {
    assert_eq!(parse_server_time(&serde_json::json!({})), Err(missing_time_field("serverTime")));
}

/// The operator's open-orders read is the ADAPTER's (base: the trait's default answered `not offered by binance` to
/// every kind): with no key pair it is told so, at either tier, and a pair on a box with no address row is told the
/// row, both with nothing asked; the balance and the positions keep the shared `not offered` words even with a pair.
/// Offline: a read with a pair and the rows would reach the venue, so this test asks none.
#[test]
fn the_adapter_answers_the_open_orders_read_and_offers_no_other() {
    use vike_model::VenueReadKind::{Balance, OpenOrders, Positions};
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let why = VenueAdapterBinance
            .venue_read(&AdapterFixture::new(&[]).inputs(tier), OpenOrders)
            .unwrap_err();
        assert!(why.contains("no binance api key pair"), "{tier}: {why}");
    }
    let no_rows = AdapterFixture::new(&[(API_KEY, "k"), (API_SECRET, "s")]);
    let why = VenueAdapterBinance.venue_read(&no_rows.inputs(AccountTier::Demo), OpenOrders);
    assert!(why.unwrap_err().contains("no address row"), "the missing row is said");
    for kind in [Balance, Positions] {
        let why = VenueAdapterBinance.venue_read(&paired().inputs(AccountTier::Demo), kind);
        assert_eq!(why.unwrap_err(), vike_model::not_offered_words(VENUE, kind));
    }
}
