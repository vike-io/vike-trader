//! **`GET /api/v5/account/config` — which account does this key trade?**
//!
//! The okx half of the blind-CEX identity work. okx's book-identity row (the
//! `BookIdentityRule::Undeterminable` declaration in `crate::adapter`) says that the credential
//! store holds a key/secret/passphrase trio and nothing that NAMES an account — so two keys minted
//! against ONE sub-account are one book and the start cannot tell. Per-engine risk ceilings then
//! apply twice to one venue ledger.
//!
//! # ⚠ MEASURED, not read off the venue's documentation
//!
//! On 2026-09-20 a read-only probe (`crates/bridges/okx/tests/okx_smokes/okx_identity_probe.rs`) asked the
//! live demo endpoint. The sanitizer's audit trail named the `uid` this module parses (beside
//! `mainUid`), and the body also carried `perm` — the one thing the single prior mention of this path
//! in the whole workspace (`vike_secrets::key_permissions`' module doc) had ever claimed about it.
//!
//! That distinction is the point rather than pedantry: this tree carries an incident from writing a
//! venue parser out of public docs, recorded in that same module as *the `canWithdraw` trap*.
//!
//! # ONE field is read
//!
//! * `uid` — **this** account, the BOOK.
//!
//! The body also answers `mainUid`, the parent (equal to `uid` on a master account, different on a
//! sub-account). Nothing in this tree acts on the parentage, so it is not parsed.
//!
//! # What this costs
//!
//! ONE signed GET per okx start. `crates/bridges/okx/src/transport/ratelimit.rs`' module doc is the authority
//! on the metering — *OKX meters request COUNT (no Binance-style weight)* — and `rest_rate_gate`
//! admits 50 per window, so the probe draws one of those, once, at startup and never in a loop.
//!
//! ⚠ It is NOT free the way binance's is. There the uid rides a body the live path already fetches
//! for its fee schedule; here nothing calls this path at all, so the request is genuinely new.
//!
//! # ⚠ Demo and mainnet share one host
//!
//! The environment is the `x-simulated-trading` HEADER, not the URL — see
//! [`vike_bridge_core::transport::json::RestTransportJsonUreq`]. So a caller passes the environment it has ALREADY
//! resolved; this module derives nothing, because a probe that re-read the flag could confirm a
//! MAINNET account id onto a DEMO row, and `vike_secrets::account_confirmation::verdict` cannot tell
//! that apart from a wrong-broker finding.

use serde_json::Value;

use crate::signer::OkxV5Signer;
use crate::transport::client::unwrap_okx;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;

/// The endpoint. Public so a test can name it without a second spelling.
pub const PATH_ACCOUNT_CONFIG: &str = "/api/v5/account/config";

/// **Who this key is at okx.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookIdentityRead {
    /// `uid` — the account this key trades. This is the value that belongs in
    /// `vike_secrets::AccountRow::venue_account_id`.
    pub uid: String,
}

/// Parse the identity out of an already-unwrapped `data[0]` object. PURE — the half a test drives
/// over the shape the live endpoint actually returned.
///
/// `None` when `uid` is absent or blank. Fail-soft: a missing id must degrade to *not yet known*,
/// which is the state every migrated `account` row already carries, rather than fail a start over a
/// bookkeeping field.
#[must_use]
pub fn parse_account_identity(row: &Value) -> Option<BookIdentityRead> {
    let text = |k: &str| {
        row.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(BookIdentityRead { uid: text("uid")? })
}

/// Ask okx who this key is.
///
/// `simulated` is the environment the CALLER already resolved — see the ⚠ in the module doc for why
/// this function derives nothing.
pub fn account_identity(
    transport: &impl RestTransportJson,
    signer: &OkxV5Signer,
    base_url: &str,
) -> Result<Option<BookIdentityRead>, VenueApiError> {
    let body = transport.signed(base_url, PATH_ACCOUNT_CONFIG, "GET", &[], signer)?;
    // `data` is an ARRAY on every okx v5 response, and this endpoint answers with exactly one row.
    // An empty array is a real answer — *the venue told us nothing* — and reads as `None` rather
    // than as an error, for the same fail-soft reason the parser has.
    let data = unwrap_okx(body)?;
    Ok(data.get(0).and_then(parse_account_identity))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE MEASURED SHAPE**, trimmed to the fields the venue answered on 2026-09-20 (`mainUid`
    /// included, which this module does not read); the values here are invented, because the real
    /// ones are the operator's account and this file ships to the public mirror.
    fn row(uid: &str, main: Option<&str>) -> Value {
        let mut o = serde_json::Map::new();
        o.insert("uid".into(), Value::String(uid.into()));
        if let Some(m) = main {
            o.insert("mainUid".into(), Value::String(m.into()));
        }
        o.insert("perm".into(), Value::String("read_only,trade".into()));
        o.insert("posMode".into(), Value::String("net_mode".into()));
        Value::Object(o)
    }

    /// A MASTER account's book is its uid.
    #[test]
    fn a_master_account_reports_its_uid() {
        let id = parse_account_identity(&row("111", Some("111"))).expect("uid present");
        assert_eq!(id.uid, "111");
    }

    /// A SUB-account's book is ITS OWN uid, never its parent's.
    #[test]
    fn a_sub_account_reports_its_own_uid() {
        let id = parse_account_identity(&row("222", Some("111"))).expect("uid present");
        assert_eq!(id.uid, "222", "the BOOK is this account, not its parent");
    }

    /// A blank or absent `uid` degrades to *not yet known* rather than failing.
    #[test]
    fn a_blank_uid_is_not_yet_known() {
        assert_eq!(parse_account_identity(&row("", Some("111"))), None);
        assert_eq!(parse_account_identity(&row("   ", None)), None);
        assert_eq!(parse_account_identity(&serde_json::json!({"perm": "trade"})), None);
    }

    /// Whitespace an operator or a venue left on an edge is one id, not two.
    #[test]
    fn edge_whitespace_is_trimmed() {
        let id = parse_account_identity(&row("  222  ", Some("  111  "))).expect("uid present");
        assert_eq!(id.uid, "222");
    }
}
