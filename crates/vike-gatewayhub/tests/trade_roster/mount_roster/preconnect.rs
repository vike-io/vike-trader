//! The pre-connect live-intent probe and the account tier, driven over the REAL registry with
//! real credential shapes. They lived in `crates/vike-tradehub-core/src/budget_tests.rs`, which keeps
//! the one test that names no venue row, until the venue-adapter contract finished
//! (docs/decisions/0096): each venue's probe is its bridge's `VenueAdapter::resolve` now, which only
//! the crate holding the registry can reach.
//!
//! ⚠ **A credential belongs to its ACCOUNT ROW** (decision 0156 item 6): every fixture below is an
//! account's FIELDS, planted on the row they belong to (`crate::account_book`), or handed to a
//! policy-less probe as that one account's credentials. No fixture names a tier — the tier is the
//! account row's, and nothing else.

use std::collections::HashMap;

use vike_gatewayhub::registry::trade::REGISTRY;
use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;

use crate::account_book::{book, creds, keyed_policy};
use crate::support::vars;

#[cfg(test)]
#[path = "preconnect/arming_ceiling.rs"]
mod arming_ceiling;
#[cfg(test)]
#[path = "preconnect/arming_projection.rs"]
mod arming_projection;

/// The `account.id` a policy-less probe's credentials are filed under — the one row
/// [`keyed_policy`] plants.
const ACCOUNT: i64 = 1;

/// Roster-wide inert default (the capability-map playbook's completeness direction): with NO
/// credentials of any shape, EVERY roster venue probes paper — a start with an empty vars map and
/// an account holding nothing must never be refused over a risk budget it cannot need.
#[test]
fn empty_vars_probe_false_for_every_roster_venue() {
    for v in vike_model::VENUES {
        assert!(
            !vike_gatewayhub::trade::adapters::arming::would_go_live(
                REGISTRY,
                v,
                &HashMap::new(),
                AccountCredentials::none()
            ),
            "{v} must probe paper with no creds"
        );
    }
}

/// One [`live_arming_cases`] row: the venue, the account tier its fields arm AT, and those fields.
type ArmingCase<'a> = (&'a str, AccountTier, &'a [(&'a str, &'a str)]);

/// One `(venue, account tier, the credential FIELDS that arm its arm AT that tier)` row per venue
/// with a live arm in a DEFAULT build — the field words are the arms' own loaders' (the venue's row
/// of `vike_venues::credential_schema::VENUE_KEY_SCHEMAS`, fixture-pinned in each bridge crate).
///
/// Hoisted out of `probe_recognizes_each_live_arms_cred_shape` because the account-tier suite
/// below needs exactly the same sets for the opposite claim: those tests assert that a `paper`
/// (or absent, or inactive) account refuses a start these fields WOULD have armed, and a table of
/// their own would let the two drift until the tier suite was silently testing unarmed venues.
/// `every_roster_venue_is_a_live_arm_row_or_a_declared_not_applicable` is the completeness half.
///
/// ⚠ **The TIER column is load-bearing (decision 0119 and the owner's no-downgrade rule).** An
/// account trades at exactly its own tier, and its fields carry none: the SAME set arms whatever
/// tier its row states, where the arm has that tier. binance/bybit/okx/hyperliquid are written at
/// `Live` (decision 0095: their network IS the tier); every other row is written at `Demo`, the one
/// tier its demo-only arm binds (aster's network is its tier too — it is simply exercised at demo
/// here).
fn live_arming_cases() -> &'static [ArmingCase<'static>] {
    &[
        ("binance", AccountTier::Live, &[("API_KEY", "k"), ("API_SECRET", "s")]),
        ("bybit", AccountTier::Live, &[("API_KEY", "k"), ("API_SECRET", "s")]),
        (
            "okx",
            AccountTier::Live,
            &[("API_KEY", "k"), ("API_SECRET", "s"), ("API_PASSPHRASE", "p")],
        ),
        ("deribit", AccountTier::Demo, &[("API_KEY", "k"), ("API_SECRET", "s")]),
        ("aster", AccountTier::Demo, &[("USER", "0xUser"), ("PRIVATE_KEY", "0xkey")]),
        ("hyperliquid", AccountTier::Live, &[("PRIVATE_KEY", "0xkey")]),
        // cTrader's OAuth application pair is the VENUE's (filed against the venue, every account
        // shares it); the grant is the account's.
        (
            "ctrader",
            AccountTier::Demo,
            &[
                ("CLIENT_ID", "id"),
                ("CLIENT_SECRET", "sec"),
                ("ACCESS_TOKEN", "at"),
                ("REFRESH_TOKEN", "rt"),
            ],
        ),
        (
            "alpaca",
            AccountTier::Demo,
            &[("CLIENT_ID", "id"), ("CLIENT_SECRET", "sec"), ("ACCOUNT_ID", "acct")],
        ),
        ("ig", AccountTier::Demo, &[("API_KEY", "k"), ("IDENTIFIER", "u"), ("PASSWORD", "p")]),
        ("oanda", AccountTier::Demo, &[("API_KEY", "k"), ("ACCOUNT_ID", "a")]),
        // ⚠ A BESPOKE SHAPE, not the `API_KEY`/`API_SECRET` grid: JForex authenticates with the
        // same LOGIN and PASSWORD as the desktop platform, so there is no API key to name. No
        // server here — `load_dukascopy_config_from` gates on the login alone, and the server is
        // not a credential at all since decision 0095's Task 7 (it is the
        // `venue.dukascopy.demo.server` setting).
        //
        // This row is what removing dukascopy from `NO_LIVE_ARM` obliges: the venue HAS a live
        // arm, so the account-tier suite must actually try to refuse it. That is the half that
        // matters — a venue with no row is a venue a `paper` account is never tested against.
        ("dukascopy", AccountTier::Demo, &[("LOGIN", "u"), ("PASSWORD", "p")]),
    ]
}

/// The venues whose arm resolves the account's ROW out of the `account` table
/// (`crates/bridges/dukascopy/src/adapter.rs`'s `resolve_in`: the row is WHICH broker an order
/// reaches) — so a policy-less probe, which carries no table, answers them `AccountNotInStore`
/// whatever credentials it is handed, and only the probe a start asks (over the policy holding the
/// row) can say whether their fields arm them.
const ROW_RESOLVED: &[&str] = &["dukascopy"];

/// Whether `kv` arms `venue` at `tier` through the probe a START asks
/// (`vike_tradehub_core::would_go_live_under_policy`), over a policy whose one row is `venue`'s DEFAULT
/// account at `tier` holding `kv`.
fn arms(venue: &str, tier: AccountTier, kv: &[(&str, &str)]) -> bool {
    crate::local::would_go_live_under_policy(
        REGISTRY,
        venue,
        &HashMap::new(),
        Some(&keyed_policy(venue, tier, kv)),
    )
}

/// The POLICY-LESS probe (`vike_tradehub_core::would_go_live_under`) over `kv` as the DEFAULT account's
/// credentials, at `tier`.
fn probe(venue: &str, tier: AccountTier, kv: &[(&str, &str)]) -> bool {
    let held = book(venue, ACCOUNT, kv);
    vike_gatewayhub::trade::adapters::arming::would_go_live_under(
        REGISTRY,
        venue,
        &HashMap::new(),
        creds(&held, venue, ACCOUNT),
        tier,
    )
}

/// The policy-less PROJECTION (`vike_tradehub_core::venue_arming_under`) over `kv` as the DEFAULT
/// account's credentials, at `tier`.
fn projected(
    venue: &str,
    tier: AccountTier,
    kv: &[(&str, &str)],
) -> (Option<AccountTier>, vike_config::ArmingBlock) {
    let held = book(venue, ACCOUNT, kv);
    vike_gatewayhub::trade::adapters::arming::venue_arming_under(
        REGISTRY,
        venue,
        &HashMap::new(),
        creds(&held, venue, ACCOUNT),
        tier,
    )
}

/// Each live arm's credential shape flips its probe row to `true` at the row's own tier — so a
/// loader renaming its fields, or this probe drifting off its arm's loader, fails by name. Asked
/// through the probe a start asks, and — for every venue that reads nothing but the account's
/// credentials and its tier — through the policy-less probe handed the same fields, which must
/// agree.
#[test]
fn probe_recognizes_each_live_arms_cred_shape() {
    for (venue, tier, kv) in live_arming_cases() {
        assert!(
            arms(venue, *tier, kv),
            "{venue} @ {tier}: its live arm's cred shape must probe live"
        );
        if ROW_RESOLVED.contains(venue) {
            // The exception, pinned rather than skipped: with no table to resolve a row from, the
            // policy-less probe answers the store's absence — never a credential complaint.
            assert_eq!(
                projected(venue, *tier, kv),
                (None, vike_config::ArmingBlock::AccountNotInStore),
                "{venue}: a policy-less probe has no `account` row to resolve"
            );
        } else {
            assert!(
                probe(venue, *tier, kv),
                "{venue} @ {tier}: the policy-less probe handed the same fields must agree"
            );
        }
    }
}

/// The venues [`live_arming_cases`] deliberately has NO row for, each with the reason — the
/// not-applicable column of the same table, which is what makes the partition below a
/// completeness gate rather than a length check.
///
/// Every one of them is a venue no DEFAULT build can start live from an account's credentials
/// alone, so there is no set on which `would_go_live` is true and nothing for the account-tier
/// suite to refuse. `venues_without_a_live_arm_probe_false_even_with_creds` and
/// `fxcm_probes_false_with_the_feature_off_even_with_creds` drive the individual cases with their
/// real field shapes. fxcm's feature-on half runs in `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs`,
/// ibkr's in `crates/vike-gatewayhub/tests/trade_roster/ibkr_adapter.rs` and polymarket's in
/// `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs`, since their features moved from vike-tradehub-core to
/// vike-tradehub.
const NO_LIVE_ARM: &[(&str, &str)] = &[
    (
        "fxcm",
        "the default build's registry carries it FeatureAbsent: its mount is the fxcm bridge's, \
         registered only under vike-tradehub's `fxcm` feature, and it needs a loadable \
         ForexConnect shim",
    ),
    (
        "ibkr",
        "the default build's registry carries it FeatureAbsent: its mount is the ibkr bridge's, \
         registered only under vike-tradehub's `ibkr` feature",
    ),
    (
        "polymarket",
        "the default build's registry carries it FeatureAbsent: its mount is the polymarket \
         bridge's, registered only under vike-tradehub's `polymarket` feature, and it needs \
         flags.poly_exec and an active live account",
    ),
];

/// **The COMPLETENESS gate over [`live_arming_cases`]**, in the shape every per-venue
/// capability table uses: the armed rows and the declared not-applicable rows must partition
/// `vike_model::VENUES` exactly — no venue in both, no venue in neither.
///
/// A thirteenth bridge therefore reddens this until its author says which column it is in, and
/// a venue that GAINS a live arm cannot stay silently uncovered by the account-tier suite below
/// (the failure that matters: that suite proves a `paper` account refuses an arming, and a venue
/// with no row is a venue it never tries to refuse).
#[test]
fn every_roster_venue_is_a_live_arm_row_or_a_declared_not_applicable() {
    let mut rows: Vec<&str> = live_arming_cases().iter().map(|(v, _, _)| *v).collect();
    let excused: Vec<&str> = NO_LIVE_ARM.iter().map(|(v, _)| *v).collect();
    for (venue, why) in NO_LIVE_ARM {
        assert!(why.len() > 20, "{venue}'s exemption must carry a REASON, got {why:?}");
        assert!(!rows.contains(venue), "{venue} is in BOTH columns");
    }
    rows.extend(&excused);
    rows.sort_unstable();
    let mut roster = vike_model::VENUES.to_vec();
    roster.sort_unstable();
    assert_eq!(
        rows, roster,
        "`live_arming_cases` + NO_LIVE_ARM must be exactly the roster: a new venue needs its \
             live arm's credential shape in the first, or a written reason in the second"
    );
    // vike:new-venue:note add "{venue}" to `NO_LIVE_ARM` with the reason its scaffolded start cannot arm (its `resolve` answers `NoLiveArm`), and move it into `live_arming_cases` with its real credential shape in the PR that makes that start arm — this partition is exact over the roster, so it stays red until the venue is in one of the two: crates/vike-gatewayhub/tests/trade_roster/mount_roster/preconnect.rs's `every_roster_venue_is_a_live_arm_row_or_a_declared_not_applicable`
}

/// **THE HALF-CREDENTIAL GATE, at the start.** OKX's signer sends `OK-ACCESS-PASSPHRASE` on
/// every request (`vike_secrets::venue_passphrase`'s `venue_passphrase` row), so an account
/// holding `API_KEY` + `API_SECRET` and NO `API_PASSPHRASE` is UNUSABLE — and until this gate it
/// LOADED: `("okx", Some(c))` matched, the venue was marked live, an exec actor was spawned,
/// and every signed request came back rejected by the venue. Half credentials must reach the
/// same verdict as absent ones, because absent credentials ARE the live gate.
///
/// Asserted at BOTH gates that decide it, since either alone could regress independently: the
/// pure pre-connect probe, and a REAL `make_engine` start (which stays offline precisely
/// because the gate holds — a regression makes this test dial OKX).
#[test]
fn okx_key_and_secret_without_a_passphrase_do_not_start_live() {
    // A `live` account (decision 0095): `would_go_live` probes `live`, and the account's own
    // tier is what reaches anything but Paper there — so the passphrase is the only gate this test
    // can be measuring.
    let half: &[(&str, &str)] = &[("API_KEY", "k"), ("API_SECRET", "s")];
    let held = book("okx", ACCOUNT, half);
    assert!(
        !vike_gatewayhub::trade::adapters::arming::would_go_live(
            REGISTRY,
            "okx",
            &HashMap::new(),
            creds(&held, "okx", ACCOUNT)
        ),
        "okx with no passphrase must probe PAPER — its key+secret cannot sign anything"
    );

    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    // ⚠ okx is ARMED by its account row: the gate under test is the missing PASSPHRASE, and a
    // `paper` account would produce the same paper start one step earlier — making every
    // assertion below true for the wrong reason. `Live`, not `Demo` (decision 0095): the
    // meaningful "armed but for the passphrase" case is the live account holding the half set.
    let policy = keyed_policy("okx", AccountTier::Live, half);
    let no_vars = HashMap::new();
    let rig = crate::local::Rig::new(REGISTRY, &no_vars, Some(&policy));
    let mut env = rig.env(&tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let stopped =
        not_running("okx", vike_tradehub_core::make_engine(&mut env, "okx", "BTC-USDT-SWAP"));
    assert_eq!(
        stopped.block,
        vike_config::ArmingBlock::LiveCredentialsAbsent,
        "half credentials: the `live` account does not run, naming the missing live set"
    );
    assert!(live.is_empty(), "half credentials must NOT mark okx live");

    // The CONTROL: the same fields plus the passphrase flips the probe live, so this test pins the
    // PASSPHRASE as the gate and not some unrelated okx breakage. (Only the pure probe is
    // exercised on that side — `make_engine` with complete credentials would dial the venue.)
    let full =
        book("okx", ACCOUNT, &[("API_KEY", "k"), ("API_SECRET", "s"), ("API_PASSPHRASE", "p")]);
    assert!(
        vike_gatewayhub::trade::adapters::arming::would_go_live(
            REGISTRY,
            "okx",
            &HashMap::new(),
            creds(&full, "okx", ACCOUNT)
        ),
        "with all three credentials okx probes live — the gate is the passphrase, nothing else"
    );
}

/// **THE OANDA LIVE-ACCOUNT REFUSAL, at the start.** `vike_oanda::oanda_hosts` implements and
/// tests the fxTrade tier and no caller in this workspace ever asks for it, so a `live` oanda
/// account used to start PAPER in silence — and, when a store could hold two tiers' keys, one
/// holding BOTH used to place REAL orders on the practice account while the operator believed their
/// live keys were in force. The arm now refuses a `live` account holding its pair outright, the
/// practice tier included, and says so at `error!` naming the ACCOUNT
/// (`vike_oanda::UnreachableLiveTier`).
///
/// Asserted at BOTH gates that decide it, the same shape as the OKX passphrase gate above: the
/// pure pre-connect probe, and a REAL `make_engine` start — which stays offline precisely
/// because the refusal holds. A regression makes this test dial OANDA's practice account with
/// the token below, which is the behaviour being pinned out.
#[test]
fn an_oanda_live_key_set_refuses_the_start_instead_of_trading_the_practice_account() {
    let pair: &[(&str, &str)] = &[("API_KEY", "live-tok"), ("ACCOUNT_ID", "001-001-0000001-001")];
    assert_eq!(
        projected("oanda", AccountTier::Live, pair),
        (None, vike_config::ArmingBlock::LiveTierNotWired),
        "a `live` oanda account holding its pair is refused by NAME — never a credential complaint"
    );
    assert!(
        !probe("oanda", AccountTier::Live, pair),
        "a `live` oanda account must probe PAPER — the arm refuses it, so live INTENT here would \
         raise a budget refusal over a venue that can only be paper"
    );

    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    // ⚠ oanda is ARMED by its `live` account row holding the pair: the gate under test is the
    // arm's own refusal of a LIVE account, and a `paper` account would reach the same paper start
    // one step earlier — making every assertion below true for the wrong reason.
    let refused = keyed_policy("oanda", AccountTier::Live, pair);
    let no_vars = HashMap::new();
    let rig = crate::local::Rig::new(REGISTRY, &no_vars, Some(&refused));
    let mut env = rig.env(&tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&refused);
    let stopped =
        not_running("oanda", vike_tradehub_core::make_engine(&mut env, "oanda", "EUR_USD"));
    assert_eq!(
        stopped.block,
        vike_config::ArmingBlock::LiveTierNotWired,
        "a `live` oanda account holding its pair: the account does not run, naming the live tier"
    );
    assert!(live.is_empty(), "a `live` oanda account must NOT mount live on practice");

    // The CONTROL: the SAME pair held by a `demo` account probes live (the practice tier), so this
    // test pins the account's LIVE tier as the gate and not some unrelated oanda breakage. (Only
    // the pure probe is exercised on that side — `make_engine` with practice credentials would
    // dial the venue.)
    assert!(
        probe("oanda", AccountTier::Demo, pair),
        "the same pair under a `demo` account still probes live — the gate is the live tier, \
             nothing else"
    );
}

/// The MIRROR of the gate above, and the reason it is a per-venue TABLE rather than a blanket
/// rule: binance/bybit/deribit sign with key + secret alone, so requiring a passphrase
/// everywhere would silently strand every working start on paper — the same defect wearing the
/// opposite sign. Their probes must stay live with exactly two credentials.
#[test]
fn passphrase_free_venues_still_probe_live_without_one() {
    let cases: [ArmingCase<'_>; 3] = [
        // Decision 0095: binance/bybit probed as the `live` accounts their fields arm; deribit as
        // the `demo` account its demo-only arm binds (a `live` deribit account is refused).
        ("binance", AccountTier::Live, &[("API_KEY", "k"), ("API_SECRET", "s")]),
        ("bybit", AccountTier::Live, &[("API_KEY", "k"), ("API_SECRET", "s")]),
        ("deribit", AccountTier::Demo, &[("API_KEY", "k"), ("API_SECRET", "s")]),
    ];
    for (venue, tier, kv) in cases {
        assert!(
            probe(venue, tier, kv),
            "{venue} takes no passphrase — key+secret alone must still probe live"
        );
    }
}

/// FXCM's login for the probe test below — the `load_fxcm_config_from(Demo, …)` shape the fxcm
/// start gates on.
const FXCM_LOGIN: &[(&str, &str)] = &[("USER", "D251112911"), ("PASSWORD", "p")];

/// This crate's registry carries fxcm `FeatureAbsent` in every build since its bridge left with the
/// `fxcm` feature, so it probes false with credentials present — the answer a build without the
/// feature has always given. The feature-on half is `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs`'s.
#[test]
fn fxcm_probes_false_with_the_feature_off_even_with_creds() {
    let held = book("fxcm", ACCOUNT, FXCM_LOGIN);
    assert!(
        !vike_gatewayhub::trade::adapters::arming::would_go_live(
            REGISTRY,
            "fxcm",
            &HashMap::new(),
            creds(&held, "fxcm", ACCOUNT)
        ),
        "no feature ⇒ no arm ⇒ paper, whatever the store says"
    );
}

/// Venues whose registry row cannot arm live probe paper even when their own credential shapes
/// are present, so a budget refusal over one would block a start that can only ever be paper.
///
/// ⚠ This doc named dukascopy — *"ships an exec factory, but nothing in `make_engine` starts
/// it"* — and that has been false since 2026-09-09; the test body already said so and the
/// summary line above it did not. What remains here is polymarket as the default build's registry
/// carries it — `FeatureAbsent` — whose feature-on half is
/// `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs`'s
/// `the_exec_flag_alone_is_not_intent_and_the_flag_with_a_key_is`.
///
/// ⚠ fxcm USED TO BE IN THIS TEST and is not any more: it grew an arm. Its replacement is
/// `fxcm_probes_false_with_the_feature_off_even_with_creds` above, which asserts the same `false`
/// in the default build this module runs in, with the two-fact conjunction asserted in
/// `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs` and in the bridge's own
/// `crates/bridges/fxcm/src/adapter_tests.rs`.
#[test]
fn venues_without_a_live_arm_probe_false_even_with_creds() {
    // ⚠ dukascopy LEFT this test on 2026-09-09, for the same reason fxcm did: it grew an arm.
    // It used to be the first assertion here, and `crates/vike-tradehub-core/src/lib.rs`'s own
    // `NO_LIVE_ARM` table carried its reason — "ships an exec factory, and `make_engine` has no
    // arm that starts it". Both are gone; the venue now has a row in `live_arming_cases` and an
    // arm that self-gates on its account row's login, so the account-tier suite exercises the
    // refusal.
    // polymarket is `FeatureAbsent` in the default build's registry, so neither arms it; under
    // vike-tradehub's `polymarket` feature its start needs BOTH gates: the explicit
    // flags.poly_exec flag (a DEPLOYMENT name, folded into the vars map) AND the account's key.
    // The flag ALONE can never start live (no key ⇒ the factory returns before any network) and
    // must do not run-probed — the contract `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs`'s
    // `polymarket_with_the_exec_gate_but_no_creds_does_not_run_and_stays_offline` pins.
    let flag = vars(&[("POLY_EXEC", "1")]);
    assert!(!vike_gatewayhub::trade::adapters::arming::would_go_live(
        REGISTRY,
        "polymarket",
        &flag,
        AccountCredentials::none()
    ));
    let key = book("polymarket", ACCOUNT, &[("PRIVATE_KEY", "0xkey")]);
    assert!(!vike_gatewayhub::trade::adapters::arming::would_go_live(
        REGISTRY,
        "polymarket",
        &flag,
        creds(&key, "polymarket", ACCOUNT)
    ));
}

/// A `AdapterPolicy` whose `account` table holds exactly ONE active row: `venue`'s DEFAULT account
/// at `tier`, holding nothing. Every other venue has no row, so it does not run (`NoAccountRow`).
fn account_at(venue: &str, tier: AccountTier) -> vike_tradehub_core::AdapterPolicy {
    crate::support::armed_policy(venue, tier)
}

/// One `account` row for `venue`'s DEFAULT account (label NULL) at `tier`, `active` as given, no
/// book and no exposure figure, id 1 — for the rows [`account_at`] cannot state (an inactive row,
/// a `max_exposure` figure).
fn account_row(venue: &str, tier: AccountTier, active: bool) -> vike_secrets::Account {
    vike_secrets::Account {
        id: ACCOUNT,
        venue: venue.to_string(),
        tier,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
    }
}

/// The account a [`start_adapter`] reported NOT RUNNING (decision 0156: an account runs at its row's tier
/// or not at all), or a panic naming what came back instead — an engine, or another refusal.
fn not_running(
    venue: &str,
    out: Result<vike_tradehub_core::EngineAndRecon, vike_tradehub_core::EngineError>,
) -> vike_tradehub_core::NotRunning {
    match out {
        Err(vike_tradehub_core::EngineError::NotRunning(stopped)) => stopped,
        Err(other) => panic!("{venue}: expected NOT RUNNING, got the refusal: {other}"),
        Ok((engine, _)) => {
            panic!("{venue}: expected NOT RUNNING, got an engine at {}", engine.tier)
        }
    }
}

/// Start `venue`'s DEFAULT account for real under `policy` — its credentials are the ones `policy`
/// files against its row, and the deployment map is empty — with no risk profile and no recorder,
/// and report both halves: the result and the live set the start wrote into.
fn start_adapter(
    venue: &str,
    policy: Option<&vike_tradehub_core::AdapterPolicy>,
) -> (
    Result<vike_tradehub_core::EngineAndRecon, vike_tradehub_core::EngineError>,
    std::collections::HashSet<String>,
) {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    let no_vars = HashMap::new();
    let rig = crate::local::Rig::new(REGISTRY, &no_vars, policy);
    let mut env = rig.env(&tx, &mut live);
    env.recon_enabled = true;
    env.policy = policy;
    let out = vike_tradehub_core::make_engine_with_legs(&mut env, venue, "BTCUSDT", &[]);
    (out, live)
}
