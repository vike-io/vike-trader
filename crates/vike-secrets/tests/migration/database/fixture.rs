//! The live-shaped store every proof here starts from, and the helpers more than one file shares.

use super::*;
use crate::support::{self, Rule};

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// Every key NAME in `<project>/settings/secrets.env` on the live box, read 2026-09-14 by
/// `grep -oE '^[A-Za-z_][A-Za-z0-9_]*=' | sort` over a read-only ssh probe. **67 names.**
///
/// Not a sample. The whole point of this array is that it is the real distribution: ten of these
/// names are reachable from `vike_model::credential_keys`' generated grid and fifty-seven are not,
/// and a migration is only interesting on the fifty-seven.
// vike:new-venue:note `{venue}` does NOT get a row here. This array is a SNAPSHOT of one box's store, read off the live machine on a dated day, and `classify` below mirrors it — a venue joins either one only when somebody re-reads that box. Adding `{venue}` by hand would make the fixture assert a store nobody has: crates/vike-secrets/tests/migration/database/fixture.rs's `LIVE_CREDENTIAL_KEYS`
pub(super) const LIVE_CREDENTIAL_KEYS: [&str; 67] = [
    "ALPACA_SANDBOX_ACCOUNT_ID",
    "ALPACA_SANDBOX_CLIENT_ID",
    "ALPACA_SANDBOX_CLIENT_SECRET",
    "ASTER_LIVE_PRIVATE_KEY",
    "ASTER_LIVE_SIGNER",
    "ASTER_LIVE_USER",
    "BINANCE_DEMO_API_KEY",
    "BINANCE_DEMO_API_SECRET",
    "BYBIT_DEMO_API_KEY",
    "BYBIT_DEMO_API_SECRET",
    "CLOUDFLARE_API_TOKEN",
    "CLOUDFLARE_ZONE_ID",
    "CTRADER_CLIENT_ID",
    "CTRADER_CLIENT_SECRET",
    "CTRADER_DEMO_ACCESS_TOKEN",
    "CTRADER_DEMO_ACCOUNT_ID",
    "CTRADER_DEMO_REFRESH_TOKEN",
    "DATAFORSEO_LOGIN",
    "DATAFORSEO_PASSWORD",
    "DERIBIT_DEMO_API_KEY",
    "DERIBIT_DEMO_API_SECRET",
    "DUKASCOPY_DEMO1_LOGIN",
    "DUKASCOPY_DEMO1_PASSWORD",
    "DUKASCOPY_DEMO1_SERVER",
    "DUKASCOPY_DEMO2_LOGIN",
    "DUKASCOPY_DEMO2_PASSWORD",
    "DUKASCOPY_DEMO2_SERVER",
    "FINNHUB_API_KEY",
    "FMP_API_KEY",
    "FXCM_DEMO_CONNECTION",
    "FXCM_DEMO_PASSWORD",
    "FXCM_DEMO_URL",
    "FXCM_DEMO_USER",
    "HYPERLIQUID_DEMO_ACCOUNT_ADDRESS",
    "HYPERLIQUID_DEMO_PRIVATE_KEY",
    "HYPERLIQUID_LIVE_ACCOUNT_ADDRESS",
    "HYPERLIQUID_LIVE_PRIVATE_KEY",
    "IBKR_DEMO_ACCOUNT",
    "IBKR_DEMO_BACKEND",
    "IBKR_DEMO_CLIENT_ID",
    "IBKR_DEMO_HOST",
    "IBKR_DEMO_PASSWORD",
    "IBKR_DEMO_PORT",
    "IBKR_DEMO_USERNAME",
    "IG_DEMO_API_KEY",
    "IG_DEMO_IDENTIFIER",
    "IG_DEMO_PASSWORD",
    "OANDA_DEMO_ACCOUNT_ID",
    "OANDA_DEMO_API_KEY",
    "OKX_DEMO_API_KEY",
    "OKX_DEMO_API_PASSPHRASE",
    "OKX_DEMO_API_SECRET",
    "PMDATA_API_KEY",
    "POLYDATA_API_KEY",
    "POLY_BUILDER_CODE",
    "POLY_FUNDER",
    "POLY_PRIVATE_KEY",
    "POLY_PROXY_ENABLED",
    "POLY_PROXY_HOST",
    "POLY_PROXY_PORT",
    "POLY_RELAYER_API_KEY",
    "POLY_RELAYER_API_KEY_ADDRESS",
    "POLY_SIGNATURE_TYPE",
    "VIKE_API_KEY",
    "VIKE_ARCHIVE_API_KEY",
    "VIKE_TELEGRAM_ALLOWED_CHAT_IDS",
    "VIKE_TELEGRAM_BOT_TOKEN",
];

/// The node file's four names on the live box, same probe, same day.
///
/// The four names MEASURED in the live node file, spelled here rather than imported from
/// `vike_model::credential_keys::PLATFORM_KEYS`. ⚠ The reason this used to give — *"which
/// `vike-secrets` cannot see — it declares no `vike-*` dependency"* — is false since
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
/// 2026-09-20). What this array is FOR survives the correction and is the better reason: it is a
/// measurement of one real box on one day, so importing the production table would make the fixture
/// track the table instead of the box and the test would stop being able to disagree with it.
/// (`PLATFORM_KEYS` holds five names today; this one holds the four that were on the box.)
pub(super) const LIVE_NODE_KEYS: [&str; 4] = [
    "VIKE_TRADEHUB_OBSERVE_KEY",
    "VIKE_TRADEHUB_CONTROL_KEY",
    "VIKE_DATAHUB_OBSERVE_KEY",
    "VIKE_DATAHUB_CONTROL_KEY",
];

/// The classification predicate the production callers pass
/// (`vike_model::credential_keys::is_platform_key`), spelled here over [`LIVE_NODE_KEYS`] so the
/// test answers about the MEASURED box rather than about the production table. ⚠ It used to say
/// "because this crate cannot link the crate that owns it", which
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` made false —
/// `vike-model` is a normal dependency now, and this predicate is a fixture by choice.
///
/// ⚠ **That choice was RULED ON, 2026-09-26, and this function is cited in the ruling** — see
/// `vike_secrets::migrate`'s doc, which keeps its predicate a parameter partly because this
/// fixture reaches BOTH it and `vike_secrets::resolve_node_keys` (whose own seam is not
/// collapsible: its correct value is per-SERVICE and a wide one is a measured `bad mac`). Read the
/// residual there too: over today's fixture this predicate and `is_platform_key` are
/// observationally IDENTICAL, because nothing here plants the fifth platform name, so the
/// independence being preserved is prospective rather than something that has caught a defect.
pub(super) fn is_node_key(key: &str) -> bool {
    support::node_keys_in(&LIVE_NODE_KEYS)(key)
}

/// The account classification the production callers pass
/// (`vike_bridge_core::credentials::classify_credential_name`), spelled here because this crate
/// genuinely cannot link the crate that owns it: `vike-bridge-core` is rank 25 where tier 15's rule
/// is *nothing above rank 10*, and it declares `vike-secrets` itself, so the edge is a cycle as
/// well as a band violation. ⚠ That
/// bound is untouched by `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md`
/// — unlike [`is_node_key`] above, whose stated reason 0072 did retire, so the two are no longer
/// "the same reason" even though the shape is still the same.
///
/// ⚠ **It is deliberately a SIMPLER rule than the production one, and that is what makes it a
/// test.** It knows nothing about `vike_model::VENUES`, the `secret = 0` set or §7's book keys; it
/// knows only the two things the store's own shape forces — *which prefix owns this name* and
/// *which of those prefixes are two accounts rather than one*. Every assertion in this file is
/// about what the MIGRATION does with a classification, never about the classification itself, so
/// mirroring the production tables here would be pinning a table against its own copy.
/// `crates/vike-bridge-core/src/credentials.rs`'s own `classify_credential_name` tests are where
/// the real rows are held.
pub(super) fn classify(name: &str) -> vike_secrets::Classification {
    support::classify_by(LIVE_RULES, name)
}

/// The rows behind [`classify`], in the order the hand-built function tried them — first match
/// wins. Every ACCOUNT row carries `.pending_moves()`: two rows of the PRODUCTION classifier's
/// pending-move table (`SERVER`, `ACCOUNT_ID`), spelled in `support::pending_move_of_field` only so
/// the report path they feed is exercised end to end. `crates/vike-bridge-core/src/credentials.rs`
/// is where the real table lives and is held.
const LIVE_RULES: &[Rule] = &[
    // The two accounts of ONE venue at ONE tier — the whole reason the account is a row. The
    // discriminator reaches no column; it is how the hand-map says *these are two* without the
    // index token becoming an identity again.
    // ⚠ The canonical-tier `DUKASCOPY_DEMO_` is deliberately NOT here: no hand-map row claims it,
    // so it falls through to the venue arm below and classifies as the AMBIGUOUS
    // `(dukascopy, demo, no label)` — which is what
    // `a_key_whose_account_has_two_answers_is_refused_by_name` needs to exist.
    Rule::prefix("DUKASCOPY_DEMO1_")
        .account("dukascopy", "demo")
        .discriminator("DEMO1")
        .pending_moves(),
    Rule::prefix("DUKASCOPY_DEMO2_")
        .account("dukascopy", "demo")
        .discriminator("DEMO2")
        .pending_moves(),
    Rule::prefix("ALPACA_SANDBOX_").account("alpaca", "demo").pending_moves(),
    // cTrader's OAuth APPLICATION pair — venue-scoped, no tier token, shared by every account.
    Rule::exact("CTRADER_CLIENT_ID").venue("ctrader").field("CLIENT_ID"),
    Rule::exact("CTRADER_CLIENT_SECRET").venue("ctrader").field("CLIENT_SECRET"),
    Rule::prefix("POLY_").account("polymarket", "live").pending_moves(),
    Rule::prefix("ASTER_LIVE_").account("aster", "live").pending_moves(),
    // ⚠ **The LEGACY tier spelling, normalized onto `live` exactly as the production
    // classifier normalizes it.** `vike_model::accounts::account_keys::AccountRef::tier` maps
    // `MAINNET` to `LIVE` ("a pre-rename store holding a MAINNET key set has one live account
    // and not a second one called MAINNET"), and `field_after_tier` strips whichever token the
    // NAME carries — so `ASTER_LIVE_API_KEY` and `ASTER_MAINNET_API_KEY` reach the migration as
    // ONE account and ONE `field`. That is the premise
    // `a_legacy_tier_spelling_is_filed_as_an_alias_and_both_names_still_answer` is about, and it
    // is a property of every classifier that normalizes rather than a quirk of the real one —
    // `crates/vike-bridge-core/tests/credential_classification.rs`'s
    // `the_two_tier_spellings_are_one_account_and_one_field` holds the production half.
    Rule::prefix("ASTER_MAINNET_").account("aster", "live").pending_moves(),
    Rule::prefix("BINANCE_DEMO_").account("binance", "demo").pending_moves(),
    Rule::prefix("BYBIT_DEMO_").account("bybit", "demo").pending_moves(),
    Rule::prefix("CTRADER_DEMO_").account("ctrader", "demo").pending_moves(),
    // ⚠ The CANONICAL-tier dukascopy spelling, which no hand-map row claims — so it resolves to
    // the AMBIGUOUS `(dukascopy, demo, no label)` once DEMO1 and DEMO2 are two rows. That is
    // the state `a_key_whose_account_has_two_answers_is_refused_by_name` needs to exist, and
    // the production classifier reaches it the same way.
    Rule::prefix("DUKASCOPY_DEMO_").account("dukascopy", "demo").pending_moves(),
    Rule::prefix("DERIBIT_DEMO_").account("deribit", "demo").pending_moves(),
    Rule::prefix("FXCM_DEMO_").account("fxcm", "demo").pending_moves(),
    Rule::prefix("HYPERLIQUID_DEMO_").account("hyperliquid", "demo").pending_moves(),
    Rule::prefix("HYPERLIQUID_LIVE_").account("hyperliquid", "live").pending_moves(),
    Rule::prefix("IBKR_DEMO_").account("ibkr", "demo").pending_moves(),
    Rule::prefix("IG_DEMO_").account("ig", "demo").pending_moves(),
    Rule::prefix("OANDA_DEMO_").account("oanda", "demo").pending_moves(),
    Rule::prefix("OKX_DEMO_").account("okx", "demo").pending_moves(),
];

impl Fixture {
    /// A settings directory shaped like the live box's: a credential file with all 67 names and a
    /// node file with the four, both written with comments and blank lines so a migration that
    /// "normalised" the file would show up as a byte difference.
    pub(super) fn live_shaped() -> Fixture {
        let fx = Fixture::empty();
        write_store(&fx.store(), &LIVE_CREDENTIAL_KEYS);
        write_store(&fx.node(), &LIVE_NODE_KEYS);
        fx
    }

    /// The migration of this directory, through the live-shaped predicate and classifier.
    pub(super) fn migrate(&self) -> vike_secrets::Migration {
        self.migrate_with(is_node_key, &classify)
    }
}

pub(super) fn write_store(path: &Path, keys: &[&str]) {
    let mut text = String::from(support::HAND_EDITED_HEADER);
    for (i, k) in keys.iter().enumerate() {
        if i == 3 {
            text.push_str("\n# a blank line and a comment in the middle\n");
        }
        text.push_str(&format!("{k}={}\n", fake_value(k)));
    }
    std::fs::write(path, text).expect("write fixture store");
}

/// A 64-bit FNV-1a over the file's bytes. Not a security hash and not pretending to be one — it is a
/// short, stable thing to PRINT in a failure message beside a byte comparison that is strictly
/// stronger. No dependency, because this crate deliberately carries almost none.
pub(super) fn digest(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}/{}B", bytes.len())
}

/// One value out of a `SecretMap`. `SecretMap` deliberately exposes `keys` and `into_map` and no
/// per-key `get` — reaching a plaintext value is supposed to be a visible act — so a test that wants
/// one says so here, once.
pub(super) fn value(map: &vike_secrets::SecretMap, key: &str) -> Option<String> {
    map.clone().into_map().remove(key)
}

pub(super) fn refusal(fx: &Fixture) -> Vec<vike_secrets::Ambiguity> {
    match vike_secrets::migrate(
        fx.arg(),
        is_node_key,
        &classify,
        vike_secrets::WhenNothingToCarry::CreateNothing,
    ) {
        Err(vike_secrets::MigrateError::Ambiguous(list)) => list,
        Ok(m) => panic!("expected a refusal, got: {m}"),
        Err(e) => panic!("expected an ambiguity refusal, got: {e}"),
    }
}
/// The preview, with the same predicate every production caller passes.
pub(super) fn preview(fx: &Fixture) -> vike_secrets::MigrationPlan {
    match vike_secrets::preview(
        fx.arg(),
        is_node_key,
        &classify,
        vike_secrets::WhenNothingToCarry::CreateNothing,
    ) {
        Ok(p) => p,
        Err(e) => panic!("preview refused: {e}"),
    }
}
