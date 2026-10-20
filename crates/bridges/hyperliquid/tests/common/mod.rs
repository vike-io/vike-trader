//! The live smokes' one way to the DEMO credentials: the fields of the DEFAULT hyperliquid account
//! at tier `demo`, resolved through the credential store's account directory, so no other
//! account's key — the `live` account's above all — can reach a smoke that uses it. Shared by the
//! reconcile, restore, permissioning and L1-class-transfer-probe smokes; a test binary names it
//! with `mod common;` (Cargo does not build a `tests/` subfolder as a binary).

use vike_hyperliquid::base::config::{self, HlCredentials};
use vike_hyperliquid::base::consts::VENUE;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

/// The ONLY tier a smoke reads an account at. The account HAS one tier (decision 0156 item 6), so
/// asking the directory for the `demo` account is the safety property the smokes rest on: the
/// `live` account's fields are never in the view a smoke is handed, and a view cannot answer for an
/// account it does not hold. The sanctioned way to run a smoke is `VIKE_SETTINGS_DIR` pointed at
/// the real settings directory, which also holds the live account.
pub const SMOKE_TIER: AccountTier = AccountTier::Demo;

/// The double-gate every live smoke uses: `None` (after a `tracing::warn!`) when the demo account's
/// key is absent, so the caller self-skips with `let Some(..) = .. else { return };`.
pub fn demo_creds() -> Option<HlCredentials> {
    let dir = vike_secrets::smoke_directory();
    let creds =
        config::load(SMOKE_TIER, &dir.credentials_for(VENUE, &AccountLabel::Default, SMOKE_TIER));
    if creds.is_none() {
        tracing::warn!(
            target: "vike_hyperliquid",
            "SKIP: the demo hyperliquid account holds no PRIVATE_KEY in the credential store (point \
             VIKE_SETTINGS_DIR at the settings directory holding it)"
        );
    }
    creds
}
