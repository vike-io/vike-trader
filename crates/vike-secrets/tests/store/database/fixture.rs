//! The live-shaped store every proof here starts from, and the helpers more than one file shares.

use super::*;
use crate::support;

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// Every credential key NAME on the live box, read 2026-09-14 over a read-only ssh probe. **67
/// names.**
///
/// Not a sample. The whole point of this array is that it is the real distribution: ten of these
/// names were reachable from the generated `VENUE × TIER × SUFFIX` grid and fifty-seven were not,
/// and a store is only interesting on the fifty-seven.
// vike:new-venue:note `{venue}` does NOT get a row here. This array is a SNAPSHOT of one box's store, read off the live machine on a dated day — a venue joins it only when somebody re-reads that box. Adding `{venue}` by hand would make the fixture assert a store nobody has: crates/vike-secrets/tests/store/database/fixture.rs's `LIVE_CREDENTIAL_KEYS`
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

/// The four node-key names on the live box, same probe, same day.
///
/// The four names MEASURED on the live box, spelled here rather than imported from
/// `vike_venues::credential_keys::PLATFORM_KEYS`. ⚠ The reason this used to give — *"which
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

/// The node-key predicate over [`LIVE_NODE_KEYS`] — the table a seeded name lands in, and the
/// family `vike_secrets::resolve_node_keys` is asked for — spelled here so the test answers about
/// the MEASURED box rather than about the production table
/// (`vike_venues::credential_keys::is_platform_key`). Over today's fixture the two are
/// observationally IDENTICAL, because nothing here plants the fifth platform name, so the
/// independence being preserved is prospective rather than something that has caught a defect.
pub(super) fn is_node_key(key: &str) -> bool {
    support::node_keys_in(&LIVE_NODE_KEYS)(key)
}

impl Fixture {
    /// A store holding the live box's NAMES: all 67 credential names and the four node keys,
    /// written through the name-addressed writer.
    ///
    /// ⚠ Shaped by its NAMES, not by its accounts: a name-addressed write files every new
    /// credential name as a DEPLOYMENT row (no account, no venue) and mints no account (decision
    /// 0156 item 6), so this store holds no `account` row at all — the live box's own rows were
    /// filed against accounts by the retired schema-2 fill. Every proof that uses it is about the
    /// name-addressed writer and the name-keyed reads, which answer the same over either.
    pub(super) fn live_shaped() -> Fixture {
        Fixture::seeded_with(
            support::fake_rows(LIVE_CREDENTIAL_KEYS.iter().chain(&LIVE_NODE_KEYS)),
            is_node_key,
        )
    }

    /// `vike_secrets::create_store` over this directory.
    pub(super) fn create(&self) -> vike_secrets::StoreCreation {
        match vike_secrets::create_store(self.arg()) {
            Ok(m) => m,
            Err(e) => panic!("create_store refused: {e}"),
        }
    }
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

/// The dry run of [`Fixture::create`] — `vike-cli secrets init --dry-run`.
pub(super) fn preview(fx: &Fixture) -> vike_secrets::StoreCreationPlan {
    match vike_secrets::preview_create_store(fx.arg()) {
        Ok(p) => p,
        Err(e) => panic!("preview refused: {e}"),
    }
}
