use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_model::account_keys::{AccountLabel, account_key};

// `ReconClient`, `ExecutionClient`, `Environment`, `PolymarketMount`, `TickRegime` and the contract
// types arrive through `super::*`.
use crate::exec_plane::recon_client::POLY_RECONCILE_ENV;

/// A syntactically valid secp256k1 key that is NOT a real account — the one `vike-mount`'s
/// polymarket tests used. Every test here that holds it returns before anything is signed with it.
const KEY: &str = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The LIVE tier's private-key name, from this crate's own naming table rather than spelled by
/// hand: a hand-spelled literal in a `src/` file is a sighting for the settings registry's sweep,
/// which would then demand a registry row resting on this fixture alone.
fn live_private_key() -> String {
    let [private_key, ..] = crate::config::poly_env_var_names(Environment::Live);
    private_key
}

/// The exec flag (deployment-wide, so unlabelled) plus a LIVE key under `label`'s key name,
/// asking for `label`.
fn keyed(label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[(POLY_EXEC_ENV, "1")]);
    fx.vars.insert(account_key(&live_private_key(), label), KEY.to_string());
    fx.account = label.clone();
    fx
}

/// A reconcile client that answers nothing — enough to see which outcome carries it.
struct Stub;

impl ReconClient for Stub {
    fn fetch_order_status_reports(
        &self,
        _since: i64,
    ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
        Ok(vec![])
    }
    fn fetch_position_status_reports(
        &self,
    ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
        Ok(vec![])
    }
}

fn stub() -> Option<Box<dyn ReconClient>> {
    Some(Box::new(Stub))
}

/// An exec client that does nothing — enough to see which outcome carries it.
struct NullExec;

impl ExecutionClient for NullExec {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// The rows `vike-mount`'s tables carried for polymarket, pinned as the capability-map playbook
/// pins a matrix: a change to any of them is a deliberate edit here too. The clock row's two
/// sentences are spelled out in full because the preflight prints them word for word.
#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = PolymarketVenueMount.declaration();
    assert_eq!(PolymarketVenueMount.venue(), "polymarket");
    assert!(d.addresses_accounts);
    assert!(d.process_exclusive.is_none());
    assert!(!d.takes_recon_trigger, "interval-only reconcile: no reconnect poke");
    assert_eq!(d.grid_source, DeclaredGridSource::NoGrid);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named {
            prefix: "POLY",
            demo_tiers: &[],
            live_tiers: &["LIVE", "MAINNET"],
            name_suffixes: &["ADDRESS"],
            evm_key_suffixes: &["PRIVATE_KEY"],
        }
    );
    assert_eq!(
        d.clock,
        ClockDecl::NotWired {
            reason: concat!(
                "its CLOB is reachable only through the SOCKS egress proxy that the live mount ",
                "builds after this step, so there is nothing this preflight can read yet"
            ),
            unmeasured_risk: Some(concat!(
                "polymarket signs POLY_TIMESTAMP into every authenticated CLOB request, so a ",
                "drifted host clock is on the ORDER path here and this leg does not measure it"
            )),
        }
    );
}

/// The arming-probe row as it was, conjunct by conjunct: the ceiling first (no testnet, so nothing
/// below `live` arms), then the exec flag, then a key. The flag ALONE is not intent — with no key
/// nothing can ever mount live — and a key alone is not intent either.
///
/// The ORDER is part of the row: the first conjunct that fails is what the arming screen reports,
/// so an empty store under a `demo` ceiling reads `LiveOnlyArm`, never `ExecFlagUnset`.
#[test]
fn resolve_arms_live_only_with_a_live_ceiling_the_exec_flag_and_a_key() {
    let armed = keyed(&AccountLabel::Default);
    assert_eq!(
        PolymarketVenueMount.resolve(&armed.inputs(false)),
        Resolution::Paper(PaperCause::LiveOnlyArm)
    );
    assert_eq!(
        PolymarketVenueMount.resolve(&MountFixture::new(&[]).inputs(false)),
        Resolution::Paper(PaperCause::LiveOnlyArm),
        "the ceiling is asked BEFORE the exec flag"
    );
    let key_only = MountFixture::new(&[(live_private_key().as_str(), KEY)]);
    assert_eq!(
        PolymarketVenueMount.resolve(&key_only.inputs(true)),
        Resolution::Paper(PaperCause::ExecFlagUnset)
    );
    assert_eq!(
        PolymarketVenueMount.resolve(&MountFixture::new(&[(POLY_EXEC_ENV, "1")]).inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
    assert_eq!(
        PolymarketVenueMount.resolve(&armed.inputs(true)),
        Resolution::Armed { tier: Tier::Live, held_below_live: None }
    );
    // A PRESENT-but-unusable key IS intent (the arming row's stance, shared with hyperliquid's):
    // it arms here so the pre-connect budget refusal can fire; the mount then declines the key.
    let bad = MountFixture::new(&[(POLY_EXEC_ENV, "1"), ("POLY_PRIVATE_KEY", "not-a-key")]);
    assert!(matches!(
        PolymarketVenueMount.resolve(&bad.inputs(true)),
        Resolution::Armed { tier: Tier::Live, .. }
    ));
}

/// Review Focus 2 at this venue: below `live` this arm resolves nothing armed at all.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let fx = keyed(&AccountLabel::Default);
    assert!(!matches!(PolymarketVenueMount.resolve(&fx.inputs(false)), Resolution::Armed { .. }));
}

/// The account 2×2: a labelled account reads its OWN key and never the default wallet's — on a
/// venue with no testnet, the whole safety property.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        PolymarketVenueMount.resolve(&alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT must not arm off the default wallet's key"
    );
    assert!(matches!(
        PolymarketVenueMount.resolve(&keyed(&alt()).inputs(true)),
        Resolution::Armed { tier: Tier::Live, .. }
    ));
    let mut default_asks_alt_keys = keyed(&alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        PolymarketVenueMount.resolve(&default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// Absent key: paper, nothing to reconcile and no identity, under either ceiling and with both gates
/// on. It is also OFFLINE — `live_mount_for_account` and `recon_client_for_account` each return
/// through `?` on `load_polymarket_creds_for_account` before the geoblock pre-flight and the L2
/// round-trip — but that half rests on review, not on this test: a factory that dialled out before
/// checking the key would still return `None` and pass every assertion here.
#[test]
fn with_no_key_the_mount_is_paper_whatever_the_gates_say() {
    let fx = MountFixture::new(&[(POLY_EXEC_ENV, "1"), (POLY_RECONCILE_ENV, "1")]);
    let (tx, _rx) = vike_exec::event_channel(8);
    for live in [false, true] {
        let mut req = fx.request(live, "0", &tx);
        req.recon_enabled = true;
        let out = PolymarketVenueMount.mount(req);
        assert!(matches!(out.exec, ExecOutcome::Paper), "live_permitted = {live}");
        assert!(out.recon.is_none() && out.identity.is_none(), "live_permitted = {live}");
    }
}

/// THE CEILING AND `flags.poly_exec` ARE BOTH REQUIRED: below `live`, and with the flag off, the
/// real-money factory — key derivation, the geoblock pre-flight, the L2 round-trip — is never
/// CALLED. The property is the laziness, which no assertion on the returned `Option` can see, so
/// the factory here counts its calls; and the decision is driven through the mount's real inputs —
/// the flag read out of the credential map, the ceiling as `live_permitted` — so each conjunct has
/// a row that goes red without it: the flag's is row 2, the ceiling's is row 3.
#[test]
fn the_exec_factory_runs_only_when_the_flag_and_the_ceiling_both_permit_it() {
    let flag_on = MountFixture::new(&[(POLY_EXEC_ENV, "1")]);
    let flag_off = MountFixture::new(&[]);
    for (fx, live_permitted, expected, row) in [
        (&flag_off, false, 0, "row 1: neither permits it"),
        (&flag_off, true, 0, "row 2: a `live` ceiling alone must not arm exec — flag required"),
        (&flag_on, false, 0, "row 3: the flag alone must not arm exec below `live` — no testnet"),
        (&flag_on, true, 1, "row 4: both permit it, so the factory runs exactly once"),
    ] {
        let calls = AtomicUsize::new(0);
        let mounted = exec_if_permitted(&fx.inputs(live_permitted), || {
            calls.fetch_add(1, Ordering::SeqCst);
            None
        });
        assert!(mounted.is_none(), "{row}");
        assert_eq!(calls.load(Ordering::SeqCst), expected, "{row}");
    }
}

/// Review Focus 4 at this venue: with no live exec mount and reconcile wanted, the outcome is a
/// PAPER engine that carries the recon-only client; not wanted, the recon-only factory never runs.
#[test]
fn a_paper_outcome_keeps_the_recon_only_client() {
    let built = AtomicUsize::new(0);
    let out = outcome_of(None, true, || {
        built.fetch_add(1, Ordering::SeqCst);
        stub()
    });
    assert!(matches!(out.exec, ExecOutcome::Paper), "recon-only is a PAPER engine");
    assert!(out.recon.is_some(), "…WITH the reconcile client");
    assert!(out.identity.is_none());
    assert_eq!(built.load(Ordering::SeqCst), 1);
    let skipped = outcome_of(None, false, || {
        built.fetch_add(1, Ordering::SeqCst);
        stub()
    });
    assert!(matches!(skipped.exec, ExecOutcome::Paper) && skipped.recon.is_none());
    assert_eq!(built.load(Ordering::SeqCst), 1, "not wanted ⇒ the factory never runs");
}

/// The LIVE half of the same `match`, which no offline mount can reach: the factory's client and
/// its shared-registry reconcile client come out together, the recon-only factory never runs, and
/// the exec half is what the arm folded — the bound tier is `Live` and no grid, contract size,
/// margin mode or leg grid is set, so `vike-mount` keeps the permissive limits the arm left.
#[test]
fn a_live_outcome_carries_the_factorys_recon_and_no_grid() {
    let mounted = PolymarketMount {
        client: Box::new(NullExec),
        recon: stub(),
        tick_regime: TickRegime::new(),
    };
    let out = outcome_of(Some(mounted), true, || -> Option<Box<dyn ReconClient>> {
        panic!("a mounted exec must not call the recon-only factory")
    });
    let ExecOutcome::Live(live) = out.exec else {
        panic!("a mounted factory is a LIVE outcome");
    };
    assert_eq!(live.bound_tier, Tier::Live);
    assert!(live.grid.is_none() && live.contract_size.is_none() && live.margin_mode.is_none());
    assert!(live.leg_grids.is_empty());
    assert!(out.recon.is_some(), "the factory's shared-registry reconcile client rides along");
    assert!(out.identity.is_none());
}

/// POLYMARKET'S OWN COMPOSITION, all four rows — the venue that is gated TWICE, pinned as a
/// matrix so neither gate can be dropped without a red test. MOVED from `vike-mount`'s
/// reconcile-gate tests with the function (docs/decisions/0096).
///
/// The whole matrix is the assertion rather than the one `true` row: dropping the master gate
/// (the shipped spelling until 2026-09-06, `poly_reconcile` alone) leaves row 2 green and only
/// row 3 red, and dropping the venue gate leaves row 3 green and only row 2 red. Asserting the
/// `true` row alone would pass with either gate removed, which is the shape of test this
/// workspace treats as a bug.
///
/// It is a pure function precisely because the real mount cannot be exercised in either
/// direction: reaching it needs a real Polygon key and a reachable CLOB (the same reason
/// `vike_bridge_core::venue_mount::recon_if_enabled` is extracted). What CANNOT be asserted here
/// is that the MOUNT calls it — that rests on review, and on
/// `crates/vike-tradehub/tests/polymarket_mount.rs`'s
/// `polymarket_without_the_gates_is_inert_and_offline`, which runs the real mount with the master
/// gate ON and both venue gates off.
#[test]
fn polymarket_wants_a_recon_client_only_when_both_gates_are_on() {
    assert!(
        poly_recon_wanted(true, true),
        "master gate on + flags.poly_reconcile ⇒ the venue's reconcile client is built — since S2 \
             the master gate is on by DEFAULT for a live mount, so this is the row a box carrying \
             flags.poly_reconcile on and no VIKE_RECONCILE now takes"
    );
    assert!(
        !poly_recon_wanted(true, false),
        "the venue gate is still an act nobody else needs: no POLY_RECONCILE ⇒ Polymarket \
             reconciles nothing, whatever the master gate says"
    );
    assert!(
        !poly_recon_wanted(false, true),
        "THE REGRESSION ROW: a refused or paper mount (VIKE_RECONCILE_OFF=1, or nothing armed \
             live) must do NO authenticated Polymarket work — the arm used to build the client \
             here anyway and let the driver-less root drop it"
    );
    assert!(!poly_recon_wanted(false, false), "neither gate ⇒ nothing, as before");
}

/// The polymarket row's PREMISE, asserted rather than asserted-about: this venue really does sign
/// a timestamp into every authenticated request. It reads this crate's own header builder, so the
/// row cannot outlive the fact it claims. MOVED from `vike-mount`'s clock tests with the row
/// (docs/decisions/0096).
#[test]
fn the_polymarket_row_is_at_risk_because_that_venue_signs_a_timestamp() {
    let creds = crate::config::PolymarketCreds {
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string(),
        address: "0xabc".to_string(),
        api_key: "key-1".to_string(),
        passphrase: "pass-1".to_string(),
        ..Default::default()
    };
    let headers = crate::exec_plane::auth::l2_auth_headers(&creds, 1_700_000_000, "GET", "/x", "")
        .expect("the fixture secret is valid base64url");
    assert!(
        headers.iter().any(|(k, _)| k == "POLY_TIMESTAMP"),
        "polymarket's row claims its clock is on the order path — prove it"
    );
    let ClockDecl::NotWired { unmeasured_risk, .. } = PolymarketVenueMount.declaration().clock
    else {
        panic!("polymarket is declared, not wired");
    };
    assert!(unmeasured_risk.is_some(), "…so its row must declare the risk, not shrug");
}
