use super::*;
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::credential_keys::{API_KEY, API_SECRET, attribution_var_for};

/// A fixture whose mounted account holds the complete pair (`API_KEY` = `"k"`, `API_SECRET` =
/// `"s"`) — the account's fields, by the field words `api_credentials` reads (decision 0156 item 6:
/// no name is composed, so none is spelled here).
fn paired() -> MountFixture {
    MountFixture::new(&[(API_KEY, "k"), (API_SECRET, "s")])
}

/// A fixture whose mounted account holds the key and NOT the secret — a half-written pair.
fn half_paired() -> MountFixture {
    MountFixture::new(&[(API_KEY, "k")])
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The REAL body bybit answered, captured from the CI box on 2026-08-09 (moved from `vike-mount`, whose
/// `fixture()` helper read it).
fn clock_body() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/bybit.json"))
        .expect("the captured fixture is valid JSON")
}

/// An exec client that is never driven: the real one would dial the venue from its own thread the
/// moment it spawned, and these tests read what the mount's body HANDS the spawn.
struct Unspawned;

impl ExecutionClient for Unspawned {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// What `mount_with` hands its spawn step for `req` — `(broker_id, fast_exec, leverage, whether a
/// reconnect trigger rides along)` — with every network step replaced by an offline double.
fn spawned_with(req: MountRequest<'_>) -> (Option<String>, bool, f64, bool) {
    let mut seen = None;
    let _ = mount_with(
        req,
        |_, _, _| None,
        |_, _, _| None,
        |spawn| {
            seen = Some((
                spawn.broker_id,
                spawn.fast_exec,
                spawn.leverage,
                spawn.recon_trigger.is_some(),
            ));
            Box::new(Unspawned)
        },
    );
    seen.expect("the account's key pair spawns the exec client")
}

/// The rows `vike-mount`'s tables carried for bybit, pinned as a matrix.
#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = BybitVenueMount.declaration();
    assert_eq!(BybitVenueMount.venue(), "bybit");
    assert!(d.addresses_accounts && d.process_exclusive.is_none());
    assert!(d.takes_recon_trigger, "the resync supervisor pokes the reconcile driver");
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentity::Undeterminable {
            why: "the store holds an HMAC api key/secret pair and no account identifier; a bybit \
                  sub-account is selected BY THE KEY, which names it nowhere offline",
        }
    );
    assert_eq!(
        d.clock,
        ClockDecl::Wired {
            endpoint: "GET /v5/market/time (public)",
            auth: ClockAuth::Public,
            risk: ClockRisk::SignedTimestamp,
        }
    );
}

/// THE D1 MATRIX (docs/decisions/0095, decision 0156 item 6): the account's tier is the network,
/// and its key pair is the one it holds. A complete pair arms the account's own tier; no pair (or
/// half of one) is paper, and under a `live` tier the cause names the missing LIVE key set.
#[test]
fn resolve_reads_exactly_the_tier_the_ceiling_names() {
    let no_credentials = Resolution::Paper(PaperCause::NoCredentials);
    let no_live_credentials = Resolution::Paper(PaperCause::LiveCredentialsAbsent);
    for fx in [MountFixture::new(&[]), half_paired()] {
        assert_eq!(BybitVenueMount.resolve(&fx.inputs(AccountTier::Demo)), no_credentials);
        assert_eq!(BybitVenueMount.resolve(&fx.inputs(AccountTier::Live)), no_live_credentials);
    }
    assert_eq!(
        BybitVenueMount.resolve(&paired().inputs(AccountTier::Demo)),
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    );
    assert_eq!(
        BybitVenueMount.resolve(&paired().inputs(AccountTier::Live)),
        Resolution::Armed { tier: AccountTier::Live, held_below_live: None }
    );
}

/// Review Focus 2 at this venue: a complete pair under a tier below `live` never arms live —
/// `Paper` reads as `Demo`.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    for below in [AccountTier::Demo, AccountTier::Paper] {
        assert_eq!(
            BybitVenueMount.resolve(&paired().inputs(below)),
            Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
            "tier={below}"
        );
    }
}

/// The account 2×2 — bybit is the venue `vike-mount`'s `bybit_store_with_alt` fixture was written
/// for: a sub-account is selected BY THE KEY, so ALT's keys are ALT's book. The LABEL picks no key
/// here: the mount reads the credentials it is handed — the mounted account's own, which
/// `vike-mount` resolved from the account row — so a labelled account arms on its pair and stays
/// paper without one, exactly as the default account does.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_empty = MountFixture::new(&[]);
    alt_empty.account = alt();
    assert_eq!(
        BybitVenueMount.resolve(&alt_empty.inputs(AccountTier::Demo)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT without keys of its own is paper"
    );
    let mut alt_paired = paired();
    alt_paired.account = alt();
    assert_eq!(
        BybitVenueMount.resolve(&alt_paired.inputs(AccountTier::Demo)),
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
        "ALT arms on its own keys"
    );
}

/// No complete key pair: paper, nothing to reconcile, nothing recorded — and OFFLINE, which the
/// body shows when its three outward steps are doubles that panic: it returns before the grid
/// pre-fetch, the reconcile factory and the exec spawn.
///
/// ⚠ The doubled body runs FIRST, and the order is the point: were the credential gate ever to
/// regress, the row fails there, on an `unreachable!`, before the real `mount` below could run its
/// `instruments-info` pre-fetch and spawn an exec thread against a live host from a unit test.
#[test]
fn without_a_key_set_for_the_ceilings_tier_the_mount_is_paper_and_offline() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let empty = MountFixture::new(&[]);
    let half = half_paired();
    for (fx, ceiling_live) in [
        (&empty, AccountTier::Demo),
        (&empty, AccountTier::Live),
        (&half, AccountTier::Demo),
        (&half, AccountTier::Live),
    ] {
        let body = mount_with(
            fx.request(ceiling_live, "BTCUSDT", &tx),
            |_, _, _| unreachable!("no grid pre-fetch without a key set"),
            |_, _, _| unreachable!("no reconcile client without a key set"),
            |_| unreachable!("no exec spawn without a key set"),
        );
        assert!(matches!(body.exec, ExecOutcome::Paper), "ceiling_live={ceiling_live}");
        assert!(body.recon.is_none() && body.identity.is_none());
        let out = BybitVenueMount.mount(fx.request(ceiling_live, "BTCUSDT", &tx));
        assert!(matches!(out.exec, ExecOutcome::Paper), "ceiling_live={ceiling_live}");
        assert!(out.recon.is_none() && out.identity.is_none());
    }
    assert!(BybitVenueMount.credential_probe(&empty.inputs(AccountTier::Live)).is_none());
}

/// Was `vike-mount`'s `credentialed_bybit_yields_a_recon_client` and the bybit line of its
/// `an_armed_mainnet_verdict_still_yields_a_recon_client`. Those asked the reconcile FACTORY alone,
/// which answers `Some` by construction, so no edit to the mount could turn them red. This drives
/// the mount's own body at both tiers: the reconcile client is built on the network the grid
/// pre-fetch and the exec spawn are handed — the one the account's tier names — and comes back
/// beside the exec client, which is bound at that tier and signs with the account's pair.
#[test]
fn the_mount_reconciles_on_the_network_it_trades_on() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let fx = paired();
    for ceiling_live in [AccountTier::Demo, AccountTier::Live] {
        let (mut grid_on, mut recon_on, mut spawned_on) = (None, None, None);
        let mut signed_with = None;
        let out = mount_with(
            fx.request(ceiling_live, "BTCUSDT", &tx),
            |_, _, mainnet| {
                grid_on = Some(mainnet);
                None
            },
            |c, symbol, mainnet| {
                recon_on = Some(mainnet);
                crate::recon_client::recon_client(c, symbol, mainnet)
            },
            |spawn| {
                spawned_on = Some(spawn.mainnet);
                signed_with = Some(spawn.creds.api_key.into_inner());
                Box::new(Unspawned)
            },
        );
        let ExecOutcome::Live(exec) = out.exec else {
            panic!("the account's key pair mounts live (ceiling_live={ceiling_live})");
        };
        let tier = ceiling_live;
        assert_eq!(exec.bound_tier, tier, "the tier the mount binds");
        let on = Some(ceiling_live == AccountTier::Live);
        assert_eq!((grid_on, recon_on, spawned_on), (on, on, on), "one network for all three");
        assert_eq!(
            signed_with.as_deref(),
            Some("k"),
            "the exec client signs with the account's pair"
        );
        assert!(out.recon.is_some(), "the reconcile client comes back beside the exec client");
        assert!(out.identity.is_none());
    }
}

/// The exec client is spawned with what the legacy arm read for it: the FD-broker code out of the
/// deployment's named values, the `execution.fast` hint out of the venue's settings, the leverage
/// out of the operator's `[risk]` budget, and the recon trigger the resync supervisor fires —
/// each ABSENT one reading as the arm's default (no `X-Referer` header, the historical 2x, the fast
/// stream off, no trigger).
#[test]
fn the_exec_client_is_spawned_with_what_the_arm_read() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let bare = paired();
    assert_eq!(
        spawned_with(bare.request(AccountTier::Demo, "BTCUSDT", &tx)),
        (None, false, 2.0, false)
    );

    let mut set = paired();
    // The broker-code NAME — a tier-less DEPLOYMENT name, so it rides `MountInputs::secrets`, not
    // the account's fields — is asked of the table module rather than spelled out (the literal
    // harvest of `crates/vike-ops/tests/settings_secrets/settings_registry.rs` reads a name spelled
    // in a `src/` file as a variable this crate names); for bybit's `X-Referer` header mechanic it
    // is the `_BROKER_CODE` spelling.
    let broker_code = attribution_var_for(VENUE).expect("bybit stamps an attribution code");
    set.vars.insert(broker_code, "VK42".to_string());
    // `venue.bybit.fast_exec` — a `venue_setting` row, which the mount reads from
    // `MountInputs::settings` (decision 0095, Task 7).
    set.settings = vike_secrets::venue_setting::VenueSettings::from_rows(
        VENUE,
        &[vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: None,
            field: "FAST_EXEC".to_string(),
            value: "1".to_string(),
        }],
    );
    let profile = vike_model::ProfileRisk { max_leverage: Some(5.0), ..Default::default() };
    let (trigger, _poked) = mpsc::channel();
    let mut req = set.request(AccountTier::Demo, "BTCUSDT", &tx);
    req.risk_profile = Some(&profile);
    req.recon_trigger = Some(trigger);
    assert_eq!(spawned_with(req), (Some("VK42".to_string()), true, 5.0, true));
}

/// THE TIER at the one leg that SIGNS at startup, before any mount: the probe signs with the
/// account's pair, on the HOST of the account's tier, and reports that tier — so a `demo` account
/// never sends a signed read to the real-money network. With no complete pair there is no probe at
/// all. Pure — the reconcile client is a signer plus a transport.
#[test]
fn a_demo_ceiling_never_signs_against_the_real_money_tier() {
    let account = paired();
    for ceiling_live in [AccountTier::Demo, AccountTier::Live] {
        let tier = ceiling_live;
        let mut signed = None;
        let probe = credential_probe_with(&account.inputs(ceiling_live), |c, symbol, mainnet| {
            signed = Some((c.api_key.expose().clone(), symbol.to_string(), mainnet));
            crate::recon_client::recon_client(c, symbol, mainnet)
        });
        let Some(CredentialProbe::RecordsIdentity { bound_tier: reported, .. }) = probe else {
            panic!("the account's pair yields an identity-recording probe");
        };
        assert_eq!(reported, tier, "the tier the probe reports (ceiling_live={ceiling_live})");
        assert_eq!(
            signed,
            Some(("k".to_string(), PROBE_SYMBOL.to_string(), ceiling_live == AccountTier::Live)),
            "the key the probe signs with is the account's, and the host is its tier's"
        );
    }
    for ceiling_live in [AccountTier::Demo, AccountTier::Live] {
        assert!(BybitVenueMount.credential_probe(&half_paired().inputs(ceiling_live)).is_none());
    }
}

// ---- the clock PARSE, against the real captured body (moved from vike-mount) -------------------

#[test]
fn the_parser_reads_the_real_captured_body() {
    assert_eq!(parse_server_time(&clock_body()), Ok(1_786_242_369_911));
}

/// THE UNIT TRAP: `result.timeNano` is NANOSECONDS as a string and `result.timeSecond` SECONDS —
/// reading either is a millionfold or thousandfold error that still looks like a plausible integer.
#[test]
fn the_parser_does_not_read_a_neighbouring_field_in_the_wrong_unit() {
    let body = clock_body();
    let ns = body["result"]["timeNano"].as_str().expect("the capture carries timeNano");
    let secs = body["result"]["timeSecond"].as_str().expect("…and timeSecond");
    let read = parse_server_time(&body).expect("the ms number parses");
    assert_ne!(read.to_string(), ns, "timeNano is nanoseconds — a millionfold error");
    assert_ne!(read.to_string(), secs, "timeSecond is seconds — a thousandfold error");
    assert_eq!(read / 1_000, secs.parse::<i64>().unwrap(), "…and it agrees with them");
    assert_eq!(read, ns.parse::<i64>().unwrap() / 1_000_000);
}

#[test]
fn a_body_without_the_stamp_names_the_missing_field() {
    assert_eq!(parse_server_time(&serde_json::json!({})), Err(missing_time_field("time")));
}
