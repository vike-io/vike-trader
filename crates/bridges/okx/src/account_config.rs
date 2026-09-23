//! **`GET /api/v5/account/config` — which account does this key trade, and whose sub-account is it?**
//!
//! The okx half of the blind-CEX identity work. `vike_mount::book_identity`'s okx row is
//! `BookIdentity::Undeterminable` — the credential store holds a key/secret/passphrase trio and
//! nothing that NAMES an account — so two keys minted against ONE sub-account are one book and the
//! mount cannot tell. Per-engine risk ceilings then apply twice to one venue ledger.
//!
//! # ⚠ MEASURED, not read off the venue's documentation
//!
//! On 2026-09-20 a read-only probe (`crates/bridges/okx/tests/okx_identity_probe.rs`) asked the
//! live demo endpoint. The sanitizer's audit trail named BOTH fields this module parses, and the
//! body also carried `perm` — the one thing the single prior mention of this path in the whole
//! workspace (`vike_bridge_core::key_permissions`' module doc) had ever claimed about it.
//!
//! That distinction is the point rather than pedantry: this tree carries an incident from writing a
//! venue parser out of public docs, recorded in that same module as *the `canWithdraw` trap*.
//!
//! # ⚠ TWO fields, and the second is the one that answers the question
//!
//! * `uid` — **this** account.
//! * `mainUid` — its **parent**. Equal to `uid` on a master account; different on a sub-account.
//!
//! [`AccountIdentity::is_sub_account`] is that comparison, and it is the only thing in this tree
//! that can distinguish a sub-account from its master on okx. `uid` alone cannot: two keys of the
//! same master and two keys of two subs look identical through it.
//!
//! # What this costs
//!
//! ONE signed GET per okx mount. `crates/bridges/okx/src/ratelimit.rs`' module doc is the authority
//! on the metering — *OKX meters request COUNT (no Binance-style weight)* — and `rest_rate_gate`
//! admits 50 per window, so the probe draws one of those, once, at startup and never in a loop.
//!
//! ⚠ It is NOT free the way binance's is. There the uid rides a body the live path already fetches
//! for its fee schedule; here nothing calls this path at all, so the request is genuinely new.
//!
//! # ⚠ Demo and mainnet share one host
//!
//! The environment is the `x-simulated-trading` HEADER, not the URL — see
//! [`crate::transport::UreqOkxTransport`]. So a caller passes the environment it has ALREADY
//! resolved; this module derives nothing, because a probe that re-read the flag could confirm a
//! MAINNET account id onto a DEMO row, and `vike_model::account_confirmation::verdict` cannot tell
//! that apart from a wrong-broker finding.

use serde_json::Value;

use crate::transport::{OkxTransport, unwrap_okx};
use vike_bridge_core::VenueApiError;
use vike_bridge_core::signer::OkxV5Signer;

/// The endpoint. Public so a test can name it without a second spelling.
pub const PATH_ACCOUNT_CONFIG: &str = "/api/v5/account/config";

/// **Who this key is at okx** — the pair the venue answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountIdentity {
    /// `uid` — the account this key trades. This is the value that belongs in
    /// `vike_secrets::Account::venue_account_id`.
    pub uid: String,
    /// `mainUid` — the parent. `None` when the venue omitted it.
    ///
    /// ⚠ **Equal to [`Self::uid`] on a MASTER account**, which is the venue's own way of saying
    /// *this account has no parent*; it is not a duplicate and must not be cleaned up into one.
    pub main_uid: Option<String>,
}

impl AccountIdentity {
    /// **Is this a SUB-account?** — `main_uid` present and different from `uid`.
    ///
    /// ⚠ `None` for an ABSENT `mainUid`, never `false`: a venue that did not answer is not a venue
    /// that answered *master*, and collapsing the two would state a fact nobody established. That is
    /// the same rule `vike_secrets::Accounts` draws between *this store holds no accounts* and
    /// *this store cannot be asked*.
    #[must_use]
    pub fn is_sub_account(&self) -> Option<bool> {
        self.main_uid.as_ref().map(|m| m != &self.uid)
    }
}

/// Parse the identity out of an already-unwrapped `data[0]` object. PURE — the half a test drives
/// over the shape the live endpoint actually returned.
///
/// `None` when `uid` is absent or blank. Fail-soft: a missing id must degrade to *not yet known*,
/// which is the state every migrated `account` row already carries, rather than fail a mount over a
/// bookkeeping field.
#[must_use]
pub fn parse_account_identity(row: &Value) -> Option<AccountIdentity> {
    let text = |k: &str| {
        row.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(AccountIdentity { uid: text("uid")?, main_uid: text("mainUid") })
}

/// Ask okx who this key is.
///
/// `simulated` is the environment the CALLER already resolved — see the ⚠ in the module doc for why
/// this function derives nothing.
pub fn account_identity(
    transport: &impl OkxTransport,
    signer: &OkxV5Signer,
    base_url: &str,
) -> Result<Option<AccountIdentity>, VenueApiError> {
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

    /// **THE MEASURED SHAPE**, trimmed to the fields this module reads. The live demo endpoint
    /// answered both on 2026-09-20; the values here are invented, because the real ones are the
    /// operator's account and this file ships to the public mirror.
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

    /// A MASTER account answers its own uid as the parent — the venue's way of saying *no parent*.
    #[test]
    fn a_master_account_is_its_own_parent() {
        let id = parse_account_identity(&row("111", Some("111"))).expect("uid present");
        assert_eq!(id.uid, "111");
        assert_eq!(id.is_sub_account(), Some(false));
    }

    /// ⚠ **THE ONE THIS MODULE EXISTS FOR.** A different parent is what makes this a SUB-account,
    /// and nothing else in this tree can tell okx sub-accounts apart from their master.
    #[test]
    fn a_different_parent_makes_it_a_sub_account() {
        let id = parse_account_identity(&row("222", Some("111"))).expect("uid present");
        assert_eq!(id.uid, "222", "the BOOK is this account, not its parent");
        assert_eq!(id.is_sub_account(), Some(true));
    }

    /// ⚠ **An ABSENT parent is UNKNOWN, never `false`.** A venue that did not answer is not a venue
    /// that answered *master*; collapsing them would state a fact nobody established.
    #[test]
    fn an_absent_parent_is_not_an_answer_of_master() {
        let id = parse_account_identity(&row("333", None)).expect("uid present");
        assert_eq!(id.main_uid, None);
        assert_eq!(id.is_sub_account(), None, "unknown, not `false`");
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
    fn edge_whitespace_is_trimmed_on_both_fields() {
        let id = parse_account_identity(&row("  222  ", Some("  111  "))).expect("uid present");
        assert_eq!(id.uid, "222");
        assert_eq!(id.main_uid.as_deref(), Some("111"));
        assert_eq!(id.is_sub_account(), Some(true));
    }
}
