//! `SpreadMaker::from_params` — the registry/harness TOML reader: every feature family's
//! keys parse into the maker, and a KEYLESS table is byte-identical to today's construction.

use super::*;

/// `spread_model = "gueant"` (+ `base_intensity_a`) flows through `from_params` into the maker's
/// `AsParams`, so the SAME `spread_maker` registry arm backtests GLFT with zero new registry
/// surface. Absent ⇒ A-S default (byte-identical). This is the backtest-wiring proof.
#[test]
fn from_params_reads_spread_model_and_base_intensity() {
    let gueant: Value =
        toml::from_str("spread_model = \"gueant\"\nbase_intensity_a = 3.0\ngamma = 0.2").unwrap();
    let as_p = SpreadMaker::from_params(&gueant).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.spread_model, SpreadModel::Gueant, "spread_model = gueant is read");
    assert_eq!(as_p.base_intensity_a, 3.0, "base_intensity_a is read");
    assert_eq!(as_p.gamma, 0.2, "sibling A-S knobs still read");

    // Absent ⇒ A-S default (byte-identical to the pre-GLFT path).
    let plain: Value = toml::from_str("gamma = 0.2").unwrap();
    let as_d = SpreadMaker::from_params(&plain).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_d.spread_model, SpreadModel::AvellanedaStoikov, "absent ⇒ A-S");
    assert_eq!(as_d.base_intensity_a, AsParams::default().base_intensity_a, "absent ⇒ default A");
}

/// `with_spread_model` forces the model on an already-built maker — the mechanism the
/// `gueant_maker` registry alias uses to price the `spread_maker` knobs with GLFT.
#[test]
fn with_spread_model_forces_the_model() {
    let plain: Value = toml::from_str("gamma = 0.2").unwrap();
    let m = SpreadMaker::from_params(&plain).unwrap().with_spread_model(SpreadModel::Gueant);
    assert_eq!(
        m.params().avellaneda_stoikov.unwrap().spread_model,
        SpreadModel::Gueant,
        "the alias forces Gueant regardless of the (absent) spread_model param"
    );
    // No A-S state (bare maker) ⇒ no-op, no panic.
    let _ = SpreadMaker::new(0.001, 0.01).with_spread_model(SpreadModel::Gueant);
}

// --- from_params reachability (audit F1–F4/F8): every feature family has a key ---------------

/// F1 (skew): the three `with_skew` knobs parse from the flat table into the maker verbatim.
#[test]
fn from_params_reads_the_skew_keys() {
    let p: Value =
        toml::from_str("target_inventory = 5.0\nmax_inventory = 40.0\nskew = 0.6").unwrap();
    let m = SpreadMaker::from_params(&p).unwrap();
    assert_eq!(m.cfg.target_inventory, 5.0);
    assert_eq!(m.cfg.max_inventory, 40.0);
    assert_eq!(m.cfg.skew, 0.6);
}

/// F2 (breaker): the three `with_fill_breaker` knobs parse and ENGAGE the breaker
/// (`breaker_enabled` needs all three positive), plus the `AsParams` pull-accel pair that makes
/// the #798 accelerated pull reachable from the same table.
#[test]
fn from_params_reads_the_breaker_and_pull_accel_keys() {
    let p: Value = toml::from_str(
        "fill_window_ms = 5000\nnet_fill_threshold = 60.0\nsuppress_cooldown_ms = 10000\n\
         pull_accel_ramp_ms = 60000\npull_accel_max = 4.0",
    )
    .unwrap();
    let m = SpreadMaker::from_params(&p).unwrap();
    assert_eq!(m.cfg.fill_window_ms, 5_000);
    assert_eq!(m.cfg.net_fill_threshold, 60.0);
    assert_eq!(m.cfg.suppress_cooldown_ms, 10_000);
    assert!(m.breaker_enabled(), "all three positive ⇒ the breaker engages");
    let as_p = m.params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.pull_accel_ramp_ms, 60_000);
    assert_eq!(as_p.pull_accel_max, 4.0);
}

/// F3 (the #790 κ-fit family): `kappa_mode` selects the fit and the clamp/window/sample-floor
/// keys parse. ⚠ The line that used to close this test — `kappa_mode = "nonsense"` ⇒ `Fixed`,
/// "the safe default" — is GONE: a mode that names no variant now fails the whole read (see
/// `unrecognized_kappa_mode_is_an_error`), because "safe" was only ever true if the operator had
/// not meant one of the real fits.
#[test]
fn from_params_reads_the_kappa_mode_family() {
    let p: Value = toml::from_str(
        "kappa_mode = \"live_fit\"\nkappa_default = 25.0\nkappa_min = 2.0\n\
         kappa_max = 500.0\nn_min = 8\ntrade_window_ms = 30000",
    )
    .unwrap();
    let as_p = SpreadMaker::from_params(&p).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.kappa_mode, KappaMode::LiveFit);
    assert_eq!(as_p.kappa_default, 25.0);
    assert_eq!(as_p.kappa_min, 2.0);
    assert_eq!(as_p.kappa_max, 500.0);
    assert_eq!(as_p.n_min, 8);
    assert_eq!(as_p.trade_window_ms, 30_000);
    let own: Value = toml::from_str("kappa_mode = \"own_fill_fit\"").unwrap();
    let as_own = SpreadMaker::from_params(&own).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_own.kappa_mode, KappaMode::OwnFillFit);
}

/// F4 (settlement): `resolution_ts` — the anchor every settlement feature keys off — plus the
/// tenor/blackout/terminal/underlying/ATM knobs all parse into `AsParams`.
#[test]
fn from_params_reads_the_settlement_and_underlying_keys() {
    let p: Value = toml::from_str(
        "resolution_ts = 1700000000000\ntau_hold_ms = 600000\nresolution_blackout_ms = 30000\n\
         terminal_penalty_gamma = 0.5\nterminal_ramp_ms = 120000\natm_blackout_scale = 2.0\n\
         underlying_weight = 0.7\nunderlying_beta = 1.5\nwindow_secs = 300.0",
    )
    .unwrap();
    let as_p = SpreadMaker::from_params(&p).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.resolution_ts, Some(1_700_000_000_000));
    assert_eq!(as_p.tau_hold_ms, 600_000);
    assert_eq!(as_p.resolution_blackout_ms, 30_000);
    assert_eq!(as_p.terminal_penalty_gamma, 0.5);
    assert_eq!(as_p.terminal_ramp_ms, 120_000);
    assert_eq!(as_p.atm_blackout_scale, 2.0);
    assert_eq!(as_p.underlying_weight, 0.7);
    assert_eq!(as_p.underlying_beta, 1.5);
    assert_eq!(as_p.window_secs, 300.0);
    // absent ⇒ no resolution known (the default None), not a zero.
    let none: Value = toml::from_str("qty = 1.0").unwrap();
    let as_none = SpreadMaker::from_params(&none).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_none.resolution_ts, None, "absent resolution_ts stays None");
}

/// F1 (toxicity): a present `toxicity_*` key builds the [`ToxicityParams`] guard (either key
/// alone works, the other axis defaulting `0.0`) and `ofi_toxicity_scale` reaches `AsParams`,
/// so the #798 OFI synthesis can actually fire from a registry table. Absent ⇒ NO bag at all.
#[test]
fn from_params_reads_the_toxicity_keys() {
    let p: Value = toml::from_str(
        "toxicity_widen = 1.5\ntoxicity_size_cut = 0.8\nofi_toxicity_scale = 2.0\n\
         alpha_lambda_ofi = 0.1",
    )
    .unwrap();
    let m = SpreadMaker::from_params(&p).unwrap();
    assert_eq!(m.cfg.toxicity, Some(ToxicityParams { widen: 1.5, size_cut: 0.8 }));
    let as_p = m.params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.ofi_toxicity_scale, 2.0);
    // one key alone still builds the bag (the other axis stays 0.0 — off).
    let one: Value = toml::from_str("toxicity_widen = 1.0").unwrap();
    assert_eq!(
        SpreadMaker::from_params(&one).unwrap().cfg.toxicity,
        Some(ToxicityParams { widen: 1.0, size_cut: 0.0 })
    );
    // absent ⇒ no bag at all (not an all-zero Some), keeping `params()` byte-identical.
    let none: Value = toml::from_str("qty = 1.0").unwrap();
    assert!(SpreadMaker::from_params(&none).unwrap().cfg.toxicity.is_none());
}

/// F8 (pricing selector, the #803 inert-`style` finding): `pricing = "style"` SKIPS the A-S
/// layer so the QuoteStyle placement actually prices (under `"as"`, `requote` bypasses the
/// style whenever A-S state is `Some`); absent/`"as"`/unrecognized keeps today's A-S chaining.
#[test]
fn from_params_pricing_style_skips_the_as_layer() {
    let style: Value =
        toml::from_str("pricing = \"style\"\nstyle = \"top\"\ntick_size = 0.01").unwrap();
    let m = SpreadMaker::from_params(&style).unwrap();
    assert!(m.params().avellaneda_stoikov.is_none(), "style pricing mounts NO A-S state");
    assert_eq!(m.cfg.style, QuoteStyle::Top, "the style key now genuinely selects placement");
    for tbl in ["pricing = \"as\"", "pricing = \"nonsense\"", "qty = 1.0"] {
        let v: Value = toml::from_str(tbl).unwrap();
        assert!(
            SpreadMaker::from_params(&v).unwrap().params().avellaneda_stoikov.is_some(),
            "absent/as/unrecognized ⇒ the A-S layer chains as before ({tbl})"
        );
    }
}

/// The absent-key guarantee, whole-surface: a keyless table builds a maker whose ENTIRE
/// observable config (`params()`) equals today's construction — the exact pre-audit chain
/// (`new → with_quote_style → with_avellaneda_stoikov → with_refresh_tolerance`) — so every key
/// the reachability audit added is proven additive in one assert.
#[test]
fn from_params_keyless_table_is_byte_identical_to_todays_construction() {
    let empty = Value::Table(Default::default());
    let got = SpreadMaker::from_params(&empty).unwrap().params();
    let want = SpreadMaker::new(1.0, 0.01)
        .with_quote_style(QuoteStyle::Mid, 1, 0.0)
        .with_avellaneda_stoikov(AsParams::default())
        .with_refresh_tolerance(0.0, 0.0)
        .params();
    assert_eq!(got, want, "a keyless params table must equal today's maker exactly");
}

// --- present-but-unrecognized enum values are ERRORS, not fallbacks ---------------------------
//
// One test per string→enum reader. `None` used to mean BOTH "absent" and "present and
// unrecognized", and `unwrap_or(default)` could not tell them apart — so a one-character typo ran
// a DIFFERENT model than the profile named, silently, and a startup line reported the fallback as
// if it had been chosen. The absent-key half of the contract is unchanged and proven above by
// `from_params_keyless_table_is_byte_identical_to_todays_construction`.

/// The shared assertion: `key = "<bad>"` fails the whole read, and the failure hands the operator
/// the three things they need to fix their profile — the KEY, their OWN spelling back verbatim,
/// and the ACCEPTED spellings (`a_spelling` is one of them, spot-checked).
fn assert_unrecognized(key: &str, bad: &str, a_spelling: &str) {
    let v: Value = toml::from_str(&format!("{key} = \"{bad}\"")).unwrap();
    // `let Err(..) else`, not `expect_err`: the Ok side is a `SpreadMaker`, which is not `Debug`.
    let Err(err) = SpreadMaker::from_params(&v) else {
        panic!("{key} = {bad:?} must not resolve to the default");
    };
    let ParamError::Unrecognized { key: got_key, value, accepted } = &err else {
        panic!("expected ParamError::Unrecognized, got {err:?}");
    };
    assert_eq!(*got_key, key, "the error names the offending key");
    assert_eq!(value, bad, "the error quotes the operator's own value back");
    assert!(accepted.contains(a_spelling), "accepted spellings list {a_spelling}: {accepted}");
    let msg = err.to_string();
    for needle in [key, bad, a_spelling] {
        assert!(msg.contains(needle), "the message must name {needle:?}, got: {msg}");
    }
}

/// The motivating case: `gueant_lehale` is ONE `l` short of the accepted `gueant_lehalle`, and it
/// used to run Avellaneda–Stoikov under a profile that named GLFT. Also pins the two halves that
/// must NOT change — an accepted spelling still resolves, case-insensitively.
#[test]
fn unrecognized_spread_model_is_an_error() {
    assert_unrecognized("spread_model", "gueant_lehale", "gueant_lehalle");
    let ok: Value = toml::from_str("spread_model = \"GLFT\"").unwrap();
    let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.spread_model, SpreadModel::Gueant, "accepted spellings stay case-insensitive");
    // Present but not a string at all — `Value::as_str` answered `None` for this too, so it rode
    // the very same silent fallback.
    let typed: Value = toml::from_str("spread_model = 3").unwrap();
    let Err(err) = SpreadMaker::from_params(&typed) else {
        panic!("a non-string value must not fall back to the default");
    };
    assert!(
        matches!(err, ParamError::NotAString { key: "spread_model", found: "integer", .. }),
        "got {err:?}"
    );
    assert!(err.to_string().contains("must be a string"), "{err}");
}

#[test]
fn unrecognized_variance_mode_is_an_error() {
    assert_unrecognized("variance_mode", "raw_locale", "raw_local");
    let ok: Value = toml::from_str("variance_mode = \"bernoulli\"").unwrap();
    let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.variance_mode, VarianceMode::PureBernoulli);
}

#[test]
fn unrecognized_horizon_mode_is_an_error() {
    assert_unrecognized("horizon_mode", "constant_taus", "constant_tau");
    let ok: Value = toml::from_str("horizon_mode = \"constant\"").unwrap();
    let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.horizon_mode, HorizonMode::ConstantTau);
}

/// `price_domain` is the one reader with a non-trivial arm (`band` also reads `band_lo`/`band_hi`),
/// so the accepted half checks that arm rather than a bare variant.
#[test]
fn unrecognized_price_domain_is_an_error() {
    assert_unrecognized("price_domain", "unbound", "unbounded");
    let ok: Value =
        toml::from_str("price_domain = \"band\"\nband_lo = 0.2\nband_hi = 0.9").unwrap();
    let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.price_domain, PriceDomain::Band { lo: 0.2, hi: 0.9 });
}

#[test]
fn unrecognized_kappa_mode_is_an_error() {
    assert_unrecognized("kappa_mode", "nonsense", "own_fill_fit");
    let ok: Value = toml::from_str("kappa_mode = \"live\"").unwrap();
    let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.kappa_mode, KappaMode::LiveFit);
}

#[test]
fn unrecognized_reservation_model_is_an_error() {
    assert_unrecognized("reservation_model", "lmsr_logistic", "logistic");
    let ok: Value = toml::from_str("reservation_model = \"lmsr\"").unwrap();
    let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.reservation_model, ReservationModel::Lmsr);
}

#[test]
fn unrecognized_spread_source_is_an_error() {
    assert_unrecognized("spread_source", "glosten_milgram", "glosten_milgrom");
    let ok: Value = toml::from_str("spread_source = \"ls_lmsr\"").unwrap();
    let as_p = SpreadMaker::from_params(&ok).unwrap().params().avellaneda_stoikov.unwrap();
    assert_eq!(as_p.spread_source, SpreadSource::LsLmsr);
}

/// ⚠ The KEY is `style`, not `quote_style` — and unlike its siblings this one lands on `cfg`, not
/// on `AsParams`, so it is read even when the A-S layer is skipped.
#[test]
fn unrecognized_quote_style_is_an_error() {
    assert_unrecognized("style", "topp", "top");
    let ok: Value = toml::from_str("pricing = \"style\"\nstyle = \"join\"").unwrap();
    assert_eq!(SpreadMaker::from_params(&ok).unwrap().cfg.style, QuoteStyle::Join);
}
