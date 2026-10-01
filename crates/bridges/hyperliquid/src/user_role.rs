//! **`POST /info {"type":"userRole"}` — who does this address belong to?**
//!
//! The one Hyperliquid read that goes AGENT → MASTER. Every other account-shaped read goes the
//! other way (`extraAgents` and `subAccounts` both take the master and list what hangs off it), and
//! a client holding an agent key does not have the master to ask with — which is the whole reason
//! this endpoint is the answer and they are not.
//!
//! # ⚠ The pitfall this exists to escape, in the venue's own words
//!
//! Hyperliquid's API docs warn:
//!
//! > A master account can approve API wallets to sign on behalf of the master account or any of its
//! > sub-accounts. […] Note that API wallets are only used to sign. **To query the account data
//! > associated with a master or sub-account, you must pass in the actual address of that account. A
//! > common pitfall is to use the agent wallet which leads to an empty result.**
//!
//! An agent address through `clearinghouseState` answers `accountValue: "0.0"` — not an error, an
//! EMPTY ACCOUNT, which reads as "this book is flat" rather than "you asked the wrong question".
//! MEASURED on live mainnet: three distinct agent addresses each returned `0.0` there while
//! answering with the same master here.
//!
//! # What it costs, and why that decides where it may be called
//!
//! **`userRole` is the single most expensive `/info` request Hyperliquid has** — weight 60 against a
//! shared 1200-per-minute per-IP budget, i.e. twenty calls a minute, thirty times a
//! `clearinghouseState`. [`crate::transport`]'s `info_weight` already carries that number.
//!
//! So this is an identity probe run ONCE PER KEY AT MOUNT and remembered. It must never sit in a
//! poll, a reconcile pass or anything per-order; the budget it spends is shared with every other
//! REST call the box makes, including the instruments load beside it.
//!
//! # ⚠ It is UNAUTHENTICATED, and that cuts both ways
//!
//! No signature, no key, no scope — `/info` is public. So the probe works from a key with no
//! trading permission at all and needs nothing provisioned. It also means **agent → master is public
//! information**: anyone who learns an agent address can resolve the master. An agent address in a
//! log line or a committed fixture de-anonymises the account behind it.
//!
//! # What it does NOT answer
//!
//! **The SUB-ACCOUNT.** An approval is registered to the signing account, so [`UserRole::Agent`]
//! names the MASTER and never the sub-account a process is pointed at — on Hyperliquid the sub is
//! chosen PER ACTION (the `vaultAddress` field), not baked into the key. A key alone therefore
//! cannot say which sub it is about to trade, and nothing here pretends otherwise.

use serde_json::Value;

use crate::transport::HyperliquidTransport;
use vike_bridge_core::VenueApiError;

/// What an address IS, as Hyperliquid classifies it.
///
/// A closed set taken from the documented response union rather than from a string compare at the
/// call site, so a role nobody handled is [`UserRole::Unknown`] carrying the word — an unrecognised
/// role must not silently read as one of the known ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserRole {
    /// An ordinary account — it holds its own book. For a credential this means *the key IS the
    /// account*, which is the assumption a key with no configured account address otherwise rests
    /// on unverified.
    User,
    /// An API wallet approved to sign for `master`. **This is the answer the probe exists for.**
    Agent {
        /// The account that approved this agent — the docs' `data.user`.
        master: String,
    },
    /// A sub-account of `master` — the docs' `data.master`. Reached by asking about the
    /// SUB-ACCOUNT's address, not by asking about a key.
    SubAccount {
        /// The account the sub-account hangs off.
        master: String,
    },
    /// A vault address.
    Vault,
    /// ⚠ **Hyperliquid does not know this address**, and for an agent key the ordinary cause is an
    /// EXPIRED APPROVAL — an agent whose `validUntil` has passed answers `missing`, not `agent`.
    /// MEASURED on two live agents whose approvals had lapsed. So this is not "bad address"; it is
    /// most often "this key can no longer sign for anyone", which is worth saying out loud.
    Missing,
    /// A role this build does not know. Carried verbatim rather than collapsed, so a venue that adds
    /// one is visible instead of being read as [`UserRole::User`].
    Unknown(String),
}

impl UserRole {
    /// The account whose BOOK this address trades, when the role names one.
    ///
    /// `None` for [`UserRole::User`] — deliberately. That role means the address IS the account, so
    /// the caller already holds the answer and returning it here would invite a call site that could
    /// not tell "the venue told me" from "I passed this in myself".
    #[must_use]
    pub fn master(&self) -> Option<&str> {
        match self {
            UserRole::Agent { master } | UserRole::SubAccount { master } => Some(master.as_str()),
            _ => None,
        }
    }
}

/// The `/info` request body. Exposed so a test can pin the exact wire shape without a transport.
#[must_use]
pub fn user_role_body(address: &str) -> Value {
    serde_json::json!({ "type": "userRole", "user": address })
}

/// Parse the documented response union. PURE — the half a test can drive over the venue's own
/// examples, which is where every field name below came from.
#[must_use]
pub fn parse_user_role(v: &Value) -> UserRole {
    let Some(role) = v.get("role").and_then(Value::as_str) else {
        return UserRole::Unknown(String::new());
    };
    // ⚠ The parent field is named DIFFERENTLY for the two roles that have one — `data.user` for an
    // agent, `data.master` for a sub-account. Reading one for the other yields `None` and would
    // quietly demote a resolvable address to "no master", so both are spelled here.
    let field = |name: &str| {
        v.get("data").and_then(|d| d.get(name)).and_then(Value::as_str).map(str::to_string)
    };
    match role {
        "user" => UserRole::User,
        "vault" => UserRole::Vault,
        "missing" => UserRole::Missing,
        "agent" => match field("user") {
            Some(master) => UserRole::Agent { master },
            // A role that says `agent` and carries no master is a shape this code does not
            // understand — reported as such rather than downgraded to `User`, which would assert
            // the opposite of what the venue just said.
            None => UserRole::Unknown("agent".to_string()),
        },
        "subAccount" => match field("master") {
            Some(master) => UserRole::SubAccount { master },
            None => UserRole::Unknown("subAccount".to_string()),
        },
        other => UserRole::Unknown(other.to_string()),
    }
}

/// Ask Hyperliquid what `address` is.
///
/// See the module doc before calling: weight 60, once per key at mount, never in a loop.
pub fn user_role(
    transport: &HyperliquidTransport,
    address: &str,
) -> Result<UserRole, VenueApiError> {
    Ok(parse_user_role(&transport.info(&user_role_body(address))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire body, pinned: the venue keys this read on `user`, and a client that sent `address`
    /// would get a parse error from the venue rather than a wrong answer — but pinning it here is
    /// what keeps the rename from being discovered at mount time on a live box.
    #[test]
    fn the_request_names_the_address_under_user() {
        assert_eq!(
            user_role_body("0xabc"),
            serde_json::json!({ "type": "userRole", "user": "0xabc" })
        );
    }

    /// ⚠ **THE ONE THE PROBE EXISTS FOR.** The fixture is the shape measured on live mainnet, and
    /// the field is `data.user` — not `data.master`, which is the SUB-ACCOUNT's spelling.
    #[test]
    fn an_agent_resolves_the_master_it_signs_for() {
        let v = serde_json::json!({"role":"agent","data":{"user":"0x85ecf584f25db6f146718b86d493e33c5af72052"}});
        assert_eq!(
            parse_user_role(&v),
            UserRole::Agent { master: "0x85ecf584f25db6f146718b86d493e33c5af72052".into() }
        );
        assert_eq!(
            parse_user_role(&v).master(),
            Some("0x85ecf584f25db6f146718b86d493e33c5af72052")
        );
    }

    /// ⚠ **The two parent fields are NOT the same name**, and this is the test that keeps one from
    /// being read for the other. A sub-account carries `data.master`.
    #[test]
    fn a_sub_account_resolves_its_master_under_a_different_field() {
        let v = serde_json::json!({"role":"subAccount","data":{"master":"0x7b7f72a2"}});
        assert_eq!(parse_user_role(&v), UserRole::SubAccount { master: "0x7b7f72a2".into() });
        assert_eq!(parse_user_role(&v).master(), Some("0x7b7f72a2"));
    }

    /// A plain account names no master — and `master()` says `None` rather than echoing the address
    /// back, so a caller can never confuse "the venue told me" with "I passed this in".
    #[test]
    fn an_ordinary_account_names_no_master() {
        let v = serde_json::json!({"role":"user"});
        assert_eq!(parse_user_role(&v), UserRole::User);
        assert_eq!(parse_user_role(&v).master(), None);
    }

    /// ⚠ **An EXPIRED agent approval answers `missing`, not `agent`** — measured on two live agents
    /// whose `validUntil` had passed. So `missing` for an agent key means "this key can no longer
    /// sign for anyone", which is a different operator problem from a typo'd address.
    #[test]
    fn a_lapsed_approval_reads_as_missing_rather_than_as_an_agent() {
        assert_eq!(parse_user_role(&serde_json::json!({"role":"missing"})), UserRole::Missing);
        assert_eq!(parse_user_role(&serde_json::json!({"role":"vault"})), UserRole::Vault);
    }

    /// ⚠ An unknown role is carried VERBATIM, never collapsed into a known one. Collapsing to `User`
    /// would assert "the key is the account" about a role nobody has classified, which is the exact
    /// unverified assumption this whole probe exists to replace.
    #[test]
    fn an_unrecognised_role_is_reported_rather_than_read_as_an_ordinary_account() {
        let v = serde_json::json!({"role":"multiSigUser"});
        assert_eq!(parse_user_role(&v), UserRole::Unknown("multiSigUser".into()));
        assert_eq!(parse_user_role(&v).master(), None);
        // …and so is a role-shaped answer with the parent missing.
        assert_eq!(
            parse_user_role(&serde_json::json!({"role":"agent"})),
            UserRole::Unknown("agent".into())
        );
    }
}
