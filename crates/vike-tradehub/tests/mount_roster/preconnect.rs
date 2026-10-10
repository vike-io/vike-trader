//! The pre-connect live-intent probe and the arming ceiling, driven over the REAL registry with
//! real credential shapes. They lived in `crates/vike-mount/src/preconnect_tests.rs`, which keeps
//! the one test that names no venue row, until the venue mount contract finished
//! (docs/decisions/0096): each venue's probe is its bridge's `VenueMount::resolve` now, which only
//! the crate holding the registry can reach.

use std::collections::HashMap;

use vike_config::{VenueMode, VenuePolicy};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::registry::REGISTRY;

use crate::support::vars;

#[cfg(test)]
#[path = "preconnect/arming_ceiling.rs"]
mod arming_ceiling;
#[cfg(test)]
#[path = "preconnect/arming_projection.rs"]
mod arming_projection;

/// A bybit store holding the DEFAULT account's demo key set and a LABELLED `ALT` one beside it.
///
/// ⚠ The labelled key NAMES are rendered by `vike_model::accounts::account_keys::account_key` rather than
/// written out, and that is not style. `crates/vike-ops/tests/settings_secrets/settings_registry.rs` harvests
/// credential-key LITERALS out of every `.rs` file under `crates/*/src/` and demands a
/// `vike_ops::settings::SETTINGS` row for each; a `__ALT` spelling has no row and can never have
/// one, because the label is a name the OPERATOR chooses at runtime. Building the key through
/// the grammar's own renderer keeps the literal out of the source AND makes this fixture wrong
/// the day the separator changes.
fn bybit_store_with_alt() -> HashMap<String, String> {
    use vike_model::accounts::account_keys::account_key;
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let mut out = HashMap::new();
    for (base, value) in [("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")] {
        out.insert(base.to_string(), value.to_string());
        out.insert(account_key(base, &alt), format!("{value}2"));
    }
    out
}

/// Roster-wide inert default (the capability-map playbook's completeness direction): with NO
/// credentials of any shape, EVERY roster venue probes paper — a mount with an empty vars map
/// must never be refused over a risk budget it cannot need.
#[test]
fn empty_vars_probe_false_for_every_roster_venue() {
    for v in vike_model::VENUES {
        assert!(
            !vike_mount::would_mount_live(REGISTRY, v, &HashMap::new()),
            "{v} must probe paper with no creds"
        );
    }
}

/// One `(venue, its live arm's own credential shape)` row per venue with a live arm in a
/// DEFAULT build — the var names are the arms' own loaders' names (fixture-pinned in each
/// bridge crate).
///
/// Hoisted out of `probe_recognizes_each_live_arms_cred_shape` because the arming-ceiling
/// suite below needs exactly the same maps for the opposite claim: those tests assert that a
/// `paper` ceiling refuses a mount these maps WOULD have armed, and a table of their own would
/// let the two drift until the ceiling suite was silently testing unarmed venues.
/// `every_roster_venue_is_a_live_arm_row_or_has_no_live_arm` is the completeness half.
///
/// ⚠ **binance/bybit/okx/hyperliquid carry LIVE-tier keys, not DEMO-tier — decision 0095.**
/// `would_mount_live` probes at the `Live` ceiling, and those four venues now REQUIRE LIVE-tier
/// credentials to reach anything but Paper under it (a mainnet host is never signed with demo
/// keys), so a DEMO-shaped row here would probe false and this table would stop being "the shape
/// that arms this venue's live arm" for exactly the venues the migration changed. This table said
/// DEMO for all four until that regression was measured.
fn live_arming_cases() -> &'static [(&'static str, &'static [(&'static str, &'static str)])] {
    &[
        ("binance", &[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")]),
        ("bybit", &[("BYBIT_LIVE_API_KEY", "k"), ("BYBIT_LIVE_API_SECRET", "s")]),
        (
            "okx",
            &[
                ("OKX_LIVE_API_KEY", "k"),
                ("OKX_LIVE_API_SECRET", "s"),
                ("OKX_LIVE_API_PASSPHRASE", "p"),
            ],
        ),
        ("deribit", &[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")]),
        ("aster", &[("ASTER_TESTNET_USER", "0xUser"), ("ASTER_TESTNET_PRIVATE_KEY", "0xkey")]),
        ("hyperliquid", &[("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xkey")]),
        (
            "ctrader",
            &[
                ("CTRADER_CLIENT_ID", "id"),
                ("CTRADER_CLIENT_SECRET", "sec"),
                ("CTRADER_DEMO_ACCESS_TOKEN", "at"),
                ("CTRADER_DEMO_REFRESH_TOKEN", "rt"),
            ],
        ),
        (
            "alpaca",
            &[
                ("ALPACA_SANDBOX_CLIENT_ID", "id"),
                ("ALPACA_SANDBOX_CLIENT_SECRET", "sec"),
                ("ALPACA_SANDBOX_ACCOUNT_ID", "acct"),
            ],
        ),
        ("ig", &[("IG_DEMO_API_KEY", "k"), ("IG_DEMO_IDENTIFIER", "u"), ("IG_DEMO_PASSWORD", "p")]),
        ("oanda", &[("OANDA_DEMO_API_KEY", "k"), ("OANDA_DEMO_ACCOUNT_ID", "a")]),
        // ⚠ A BESPOKE SHAPE, not the `{VENUE}_{TIER}_API_KEY/_SECRET` grid: JForex authenticates
        // with the same LOGIN and PASSWORD as the desktop platform, so there is no API key to
        // name. `SERVER` is absent here — `load_dukascopy_config_from` gates on login and
        // password alone, and the server is not a credential at all since decision 0095's Task 7
        // (it is the `venue.dukascopy.demo.server` setting).
        //
        // This row is what removing dukascopy from `NO_LIVE_ARM` obliges: the venue now HAS a
        // live arm, so the arming-ceiling suite must actually try to refuse it. That is the
        // half that matters — a venue with no row is a venue the `paper` ceiling is never
        // tested against.
        ("dukascopy", &[("DUKASCOPY_DEMO1_LOGIN", "u"), ("DUKASCOPY_DEMO1_PASSWORD", "p")]),
    ]
}

/// Each live arm's credential shape flips its probe row to `true` — so a loader renaming its
/// keys, or this probe drifting off its arm's loader, fails by name.
#[test]
fn probe_recognizes_each_live_arms_cred_shape() {
    for (venue, kv) in live_arming_cases() {
        assert!(
            vike_mount::would_mount_live(REGISTRY, venue, &vars(kv)),
            "{venue}: its live arm's cred shape must probe live"
        );
    }
}

/// The venues [`live_arming_cases`] deliberately has NO row for, each with the reason — the
/// not-applicable column of the same table, which is what makes the partition below a
/// completeness gate rather than a length check.
///
/// Every one of them is a venue no DEFAULT build can mount live from a credential map alone, so
/// there is no map on which `would_mount_live` is true and nothing for the arming-ceiling suite
/// to refuse. `venues_without_a_live_arm_probe_false_even_with_creds` and
/// `fxcm_probes_false_with_the_feature_off_even_with_creds` drive the individual cases with their
/// real key shapes. fxcm's feature-on half runs in `crates/vike-tradehub/tests/fxcm_mount.rs`,
/// ibkr's in `crates/vike-tradehub/tests/ibkr_mount.rs` and polymarket's in
/// `crates/vike-tradehub/tests/polymarket_mount.rs`, since their features moved from vike-mount to
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
         flags.poly_exec and a live ceiling",
    ),
];

/// **The COMPLETENESS gate over [`live_arming_cases`]**, in the shape every per-venue
/// capability table uses: the armed rows and the declared not-applicable rows must partition
/// `vike_model::VENUES` exactly — no venue in both, no venue in neither.
///
/// A thirteenth bridge therefore reddens this until its author says which column it is in, and
/// a venue that GAINS a live arm cannot stay silently uncovered by the arming-ceiling suite
/// below (the failure that matters: that suite proves a `paper` ceiling refuses an arming, and
/// a venue with no row is a venue it never tries to refuse).
#[test]
fn every_roster_venue_is_a_live_arm_row_or_a_declared_not_applicable() {
    let mut rows: Vec<&str> = live_arming_cases().iter().map(|(v, _)| *v).collect();
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
    // vike:new-venue:note add "{venue}" to `NO_LIVE_ARM` with the reason its scaffolded mount cannot arm (its `resolve` answers `NoLiveArm`), and move it into `live_arming_cases` with its real credential shape in the PR that makes that mount arm — this partition is exact over the roster, so it stays red until the venue is in one of the two: crates/vike-tradehub/tests/mount_roster/preconnect.rs's `every_roster_venue_is_a_live_arm_row_or_a_declared_not_applicable`
}

/// **THE HALF-CREDENTIAL GATE, at the mount.** OKX's signer sends `OK-ACCESS-PASSPHRASE` on
/// every request (`vike_bridge_core::venue_passphrase`'s `venue_passphrase` row), so a store
/// holding key + secret and NO `OKX_{TIER}_API_PASSPHRASE` is UNUSABLE — and until this gate it
/// LOADED: `("okx", Some(c))` matched, the venue was marked live, an exec actor was spawned,
/// and every signed request came back rejected by the venue. Half credentials must reach the
/// same verdict as absent ones, because absent credentials ARE the live gate.
///
/// Asserted at BOTH gates that decide it, since either alone could regress independently: the
/// pure pre-connect probe, and a REAL `make_engine` mount (which stays offline precisely
/// because the gate holds — a regression makes this test dial OKX).
#[test]
fn okx_key_and_secret_without_a_passphrase_do_not_mount_live() {
    // Decision 0095: LIVE-tier, not DEMO-tier — `would_mount_live` probes the `Live` ceiling, and
    // okx now requires LIVE-tier keys specifically to reach anything but Paper under it. Using
    // DEMO-tier keys here would make the tier itself the gate this test measures, not the
    // passphrase.
    let half = vars(&[("OKX_LIVE_API_KEY", "k"), ("OKX_LIVE_API_SECRET", "s")]);
    assert!(
        !vike_mount::would_mount_live(REGISTRY, "okx", &half),
        "okx with no passphrase must probe PAPER — its key+secret cannot sign anything"
    );

    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    // ⚠ okx is ARMED by the ceiling: the gate under test is the missing PASSPHRASE, and a
    // `paper` ceiling would produce the same paper mount one step earlier — making every
    // assertion below true for the wrong reason. `Live`, not `Demo` (decision 0095): with
    // LIVE-tier `half`, only a `live` ceiling is the meaningful "armed but for the passphrase"
    // case.
    let policy = crate::support::armed_policy("okx", vike_config::VenueMode::Live);
    let mut env = vike_mount::MountEnv::new(REGISTRY, &half, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let (engine, recon) = vike_mount::make_engine(&mut env, "okx", "BTC-USDT-SWAP")
        .expect("a paper mount must never refuse to start");
    assert!(live.is_empty(), "half credentials must NOT mark okx live");
    assert!(recon.is_none(), "a paper venue never reconciles");
    assert_eq!(
        engine.fee_schedule,
        Some(vike_model::fee_schedule_for("okx")),
        "the paper mount is tagged with okx's static fee schedule"
    );

    // The CONTROL: the same map plus the passphrase flips the probe live, so this test pins the
    // PASSPHRASE as the gate and not some unrelated okx breakage. (Only the pure probe is
    // exercised on that side — `make_engine` with complete credentials would dial the venue.)
    let mut full = half.clone();
    full.insert("OKX_LIVE_API_PASSPHRASE".to_string(), "p".to_string());
    assert!(
        vike_mount::would_mount_live(REGISTRY, "okx", &full),
        "with all three credentials okx probes live — the gate is the passphrase, nothing else"
    );
}

/// **THE OANDA LIVE-TIER REFUSAL, at the mount.** `vike_oanda::oanda_hosts` implements and
/// tests the fxTrade tier and no caller in this workspace ever asks for it, so a store holding
/// `OANDA_LIVE_*` used to mount PAPER in silence — and a store holding BOTH tiers used to place
/// REAL orders on the practice account while the operator believed their live keys were in
/// force. The arm now refuses to select ANY tier from a live-armed store, the practice fallback
/// included, and says so at `error!`.
///
/// Asserted at BOTH gates that decide it, the same shape as the OKX passphrase gate above: the
/// pure pre-connect probe, and a REAL `make_engine` mount — which stays offline precisely
/// because the refusal holds. A regression makes this test dial OANDA's practice account with
/// the demo token below, which is the behaviour being pinned out.
#[test]
fn an_oanda_live_key_set_refuses_the_mount_instead_of_trading_the_practice_account() {
    let armed = vars(&[
        ("OANDA_LIVE_API_KEY", "live-tok"),
        ("OANDA_LIVE_ACCOUNT_ID", "001-001-0000001-001"),
        ("OANDA_DEMO_API_KEY", "demo-tok"),
        ("OANDA_DEMO_ACCOUNT_ID", "101-004-1234567-001"),
    ]);
    assert!(
        !vike_mount::would_mount_live(REGISTRY, "oanda", &armed),
        "a live-armed oanda store must probe PAPER — the arm refuses it, so live INTENT here \
             would raise a budget refusal over a venue that can only be paper"
    );

    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    // ⚠ oanda is ARMED by the ceiling: the gate under test is the arm's own refusal of a
    // LIVE-named key set, and a `paper` ceiling would reach the same paper mount one step
    // earlier — making every assertion below true for the wrong reason.
    let permitted = crate::support::armed_policy("oanda", vike_config::VenueMode::Demo);
    let mut env = vike_mount::MountEnv::new(REGISTRY, &armed, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&permitted);
    let (engine, recon) = vike_mount::make_engine(&mut env, "oanda", "EUR_USD")
        .expect("a paper mount must never refuse to start");
    assert!(live.is_empty(), "a live-armed oanda store must NOT mount live on practice");
    assert!(recon.is_none(), "a paper venue never reconciles");
    assert_eq!(
        engine.fee_schedule,
        Some(vike_model::fee_schedule_for("oanda")),
        "the paper mount is tagged with oanda's static fee schedule"
    );

    // The CONTROL: the SAME map with the live pair removed probes live again, so this test
    // pins the live-named key set as the gate and not some unrelated oanda breakage. (Only the
    // pure probe is exercised on that side — `make_engine` with practice credentials would
    // dial the venue.)
    let practice = vars(&[
        ("OANDA_DEMO_API_KEY", "demo-tok"),
        ("OANDA_DEMO_ACCOUNT_ID", "101-004-1234567-001"),
    ]);
    assert!(
        vike_mount::would_mount_live(REGISTRY, "oanda", &practice),
        "practice credentials alone still probe live — the gate is the live-named set, \
             nothing else"
    );
}

/// The MIRROR of the gate above, and the reason it is a per-venue TABLE rather than a blanket
/// rule: binance/bybit/deribit sign with key + secret alone, so requiring a passphrase
/// everywhere would silently strand every working mount on paper — the same defect wearing the
/// opposite sign. Their probes must stay live with exactly two credentials.
#[test]
fn passphrase_free_venues_still_probe_live_without_one() {
    for (venue, kv) in [
        // Decision 0095: binance/bybit need LIVE-tier keys to probe live under `would_mount_live`
        // (the `Live`-ceiling probe) — deribit's network is never the ceiling, so it stays DEMO.
        ("binance", &[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")]),
        ("bybit", &[("BYBIT_LIVE_API_KEY", "k"), ("BYBIT_LIVE_API_SECRET", "s")]),
        ("deribit", &[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")]),
    ] {
        assert!(
            vike_mount::would_mount_live(REGISTRY, venue, &vars(kv)),
            "{venue} takes no passphrase — key+secret alone must still probe live"
        );
    }
}

/// FXCM credentials for the probe test below — the `load_fxcm_config_from(Demo, …)` shape the fxcm
/// mount gates on.
fn fxcm_creds() -> HashMap<String, String> {
    vars(&[("FXCM_DEMO_USER", "D251112911"), ("FXCM_DEMO_PASSWORD", "p")])
}

/// This crate's registry carries fxcm `FeatureAbsent` in every build since its bridge left with the
/// `fxcm` feature, so it probes false with credentials present — the answer a build without the
/// feature has always given. The feature-on half is `crates/vike-tradehub/tests/fxcm_mount.rs`'s.
#[test]
fn fxcm_probes_false_with_the_feature_off_even_with_creds() {
    assert!(
        !vike_mount::would_mount_live(REGISTRY, "fxcm", &fxcm_creds()),
        "no feature ⇒ no arm ⇒ paper, whatever the store says"
    );
}

/// Venues whose registry row cannot arm live probe paper even when their own credential shapes
/// are present, so a budget refusal over one would block a mount that can only ever be paper.
///
/// ⚠ This doc named dukascopy — *"ships an exec factory, but nothing in `make_engine` mounts
/// it"* — and that has been false since 2026-09-09; the test body already said so and the
/// summary line above it did not. What remains here is polymarket as the default build's registry
/// carries it — `FeatureAbsent` — whose feature-on half is
/// `crates/vike-tradehub/tests/polymarket_mount.rs`'s
/// `the_exec_flag_alone_is_not_intent_and_the_flag_with_a_key_is`.
///
/// ⚠ fxcm USED TO BE IN THIS TEST and is not any more: it grew an arm. Its replacement is
/// `fxcm_probes_false_with_the_feature_off_even_with_creds` above, which asserts the same `false`
/// in the default build this module runs in, with the two-fact conjunction asserted in
/// `crates/vike-tradehub/tests/fxcm_mount.rs` and in the bridge's own
/// `crates/bridges/fxcm/src/mount_tests.rs`.
#[test]
fn venues_without_a_live_arm_probe_false_even_with_creds() {
    // ⚠ dukascopy LEFT this test on 2026-09-09, for the same reason fxcm did: it grew an arm.
    // It used to be the first assertion here, and `crates/vike-mount/src/lib.rs`'s own
    // `NO_LIVE_ARM` table carried its reason — "ships an exec factory, and `make_engine` has no
    // arm that mounts it". Both are gone; the venue now has a row in `live_arming_cases` and a
    // probe arm that self-gates on `load_dukascopy_config_from(Demo1, …)`, so a store carrying
    // its credentials probes TRUE and the arming-ceiling suite exercises the refusal.
    // polymarket is `FeatureAbsent` in the default build's registry, so neither map arms it; under
    // vike-tradehub's `polymarket` feature its mount needs BOTH gates: the explicit
    // flags.poly_exec flag AND key material. The flag ALONE can never mount live (no key ⇒ the
    // factory returns before any network) and must stay paper-probed — the contract
    // `crates/vike-tradehub/tests/polymarket_mount.rs`'s
    // `polymarket_with_the_exec_gate_but_no_creds_stays_paper_and_offline` pins.
    assert!(!vike_mount::would_mount_live(REGISTRY, "polymarket", &vars(&[("POLY_EXEC", "1")])));
    assert!(!vike_mount::would_mount_live(
        REGISTRY,
        "polymarket",
        &vars(&[("POLY_EXEC", "1"), ("POLY_PRIVATE_KEY", "0xkey")])
    ));
}

/// A `MountPolicy` declaring exactly one venue's ceiling — every other venue keeps `paper`.
fn ceiling(venue: &str, mode: VenueMode) -> vike_mount::MountPolicy {
    vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare(venue, mode),
        ..vike_mount::MountPolicy::default()
    }
}

/// Mount `venue` for real, with no risk profile and no recorder, and report both halves: the
/// result and the live set the mount wrote into.
fn mount(
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&vike_mount::MountPolicy>,
) -> (Result<vike_mount::EngineAndRecon, vike_mount::MountError>, std::collections::HashSet<String>)
{
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    let mut env = vike_mount::MountEnv::new(REGISTRY, vars, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = policy;
    let out = vike_mount::make_engine_with_legs(&mut env, venue, "BTCUSDT", &[]);
    (out, live)
}
