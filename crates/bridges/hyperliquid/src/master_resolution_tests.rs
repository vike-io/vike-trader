use super::*;
use crate::user_role::UserRole;

const SIGNER: &str = "0x1111111111111111111111111111111111111111";
const MASTER: &str = "0x85ecf584f25db6f146718b86d493e33c5af72052";

/// ⚠ **THE BRANCH THE PROBE EXISTS FOR.** An agent key mounts on the MASTER, not on itself, and
/// says nothing alarming — the venue answered, so there is no guess left to warn about.
#[test]
fn an_agent_key_mounts_on_the_master_the_venue_named() {
    let role = UserRole::Agent { master: MASTER.into() };
    let d = hl_master_from_role(Ok(&role), SIGNER);
    assert_eq!(d.address, MASTER);
    assert_eq!(d.warning, None, "a confirmed answer is not a warning");
}

/// The venue CONFIRMS the key is the account. Byte-identical to the old behaviour — and the
/// point is that it is now confirmed rather than assumed, which is why it carries no warning
/// while the fallbacks below do.
#[test]
fn a_plain_key_mounts_on_itself_and_is_not_warned_about() {
    let d = hl_master_from_role(Ok(&UserRole::User), SIGNER);
    assert_eq!(d.address, SIGNER);
    assert_eq!(d.warning, None);
}

/// ⚠ **THE PROBE MAY NEVER TURN A WORKING MOUNT INTO A PAPER ONE.** Every failure arm returns
/// the address the old code returned, so the worst this change can do is warn. Asserted over all
/// three failure shapes at once, because a single arm getting it right proves nothing about the
/// others.
#[test]
fn every_failure_falls_back_to_todays_behaviour_and_says_so() {
    let network = hl_master_from_role(Err("connection reset".to_string()), SIGNER);
    assert_eq!(network.address, SIGNER, "a failed probe must not move the mount");
    assert!(network.warning.as_deref().unwrap().contains("connection reset"), "{network:?}");

    let missing = hl_master_from_role(Ok(&UserRole::Missing), SIGNER);
    assert_eq!(missing.address, SIGNER);
    assert!(missing.warning.as_deref().unwrap().contains("EXPIRED"), "{missing:?}");

    let unknown = hl_master_from_role(Ok(&UserRole::Unknown("multiSigUser".into())), SIGNER);
    assert_eq!(unknown.address, SIGNER);
    assert!(unknown.warning.is_some());
}

/// A role that NAMES a parent is believed even when this build does not expect it there — a
/// sub-account address is still a better book than the signing key, and the warning says the
/// classification was unexpected rather than swallowing it.
#[test]
fn a_named_parent_is_used_even_from_an_unexpected_role() {
    let role = UserRole::SubAccount { master: MASTER.into() };
    let d = hl_master_from_role(Ok(&role), SIGNER);
    assert_eq!(d.address, MASTER);
    assert!(d.warning.is_some(), "…but the shape was not what a signing key should answer");
}

/// The cure an operator pastes is the LABELLED key for a labelled account — telling them to
/// write the unlabelled one would send them to configure a different account.
#[test]
fn the_cure_names_the_labelled_key_for_a_labelled_account() {
    assert!(hl_address_cure(&AccountLabel::Default).contains("ACCOUNT_ADDRESS`"));
    let alt = AccountLabel::parse("ALT").unwrap();
    assert!(hl_address_cure(&alt).contains("ACCOUNT_ADDRESS__ALT"), "{}", hl_address_cure(&alt));
}

// ── AUDITING AN ADDRESS THE OPERATOR WROTE ───────────────────────────────────────────────
//
// `hl_audit_configured` is the pure half of the arm that used to trust a configured
// `HYPERLIQUID_{TIER}_ACCOUNT_ADDRESS` without asking anybody. Every arm is driven here, because
// the emission around it needs a live `userRole` round trip and is reachable by no test.

/// **THE ONE THIS EXISTS FOR: the venue names a different account than the operator wrote.**
///
/// Until 2026-09-20 nothing in this tree compared those two values, so a stale or mistyped
/// address meant every read for this account answered about somebody else's book — and
/// Hyperliquid answers a wrong address with `accountValue: "0.0"` rather than an error, so it
/// read as FLAT and HEALTHY rather than broken.
#[test]
fn a_configured_address_the_venue_contradicts_is_a_disagreement() {
    let role = UserRole::Agent { master: MASTER.into() };
    let decided = hl_master_from_role(Ok(&role), SIGNER);
    let stranger = "0x2222222222222222222222222222222222222222";
    assert_eq!(
        hl_audit_configured(&decided, stranger),
        ConfiguredAudit::Disagrees { venue_said: MASTER.to_string() },
        "the venue's answer is carried so the report can name both sides"
    );
}

/// …and the agreeing case is `Confirms`, which is what records the handshake and moves the
/// account off `NEVER VERIFIED`. Without this the test above would pass against a rule that
/// called every configured address wrong.
#[test]
fn a_configured_address_the_venue_agrees_with_confirms() {
    let role = UserRole::Agent { master: MASTER.into() };
    let decided = hl_master_from_role(Ok(&role), SIGNER);
    assert_eq!(hl_audit_configured(&decided, MASTER), ConfiguredAudit::Confirms);
}

/// ⚠ **CASE IS NOT A DISAGREEMENT**, and this is the test that keeps the alarm worth reading.
///
/// An EVM address is the same account in any case: the operator types it however their wallet
/// printed it (checksummed, mixed case) and Hyperliquid answers lowercase. A raw comparison
/// would raise the alarm on EVERY BOOT of a perfectly correct box, which is exactly how an
/// operator learns to ignore a whole family of messages.
#[test]
fn a_differently_cased_address_is_the_same_account() {
    let role = UserRole::Agent { master: MASTER.to_ascii_uppercase() };
    let decided = hl_master_from_role(Ok(&role), SIGNER);
    assert_eq!(
        hl_audit_configured(&decided, MASTER),
        ConfiguredAudit::Confirms,
        "0xABC and 0xabc are one account"
    );
    // …and whitespace an operator pasted in is trimmed on the same rule.
    assert_eq!(hl_audit_configured(&decided, &format!("  {MASTER}  ")), ConfiguredAudit::Confirms);
}

/// **A PLAIN MASTER KEY CONFIRMS ITS OWN ADDRESS.** `UserRole::User` means *the key IS the
/// account*, so `hl_master_from_role` answers the signer — and an operator who wrote that same
/// address has written something correct, if redundant.
#[test]
fn a_plain_key_confirms_a_configured_address_equal_to_its_own() {
    let decided = hl_master_from_role(Ok(&UserRole::User), SIGNER);
    assert_eq!(hl_audit_configured(&decided, SIGNER), ConfiguredAudit::Confirms);
}

/// ⚠ **…and a plain key configured to a DIFFERENT address is a real finding, not a shrug.**
/// The venue says this key acts only for itself, so an address pointing elsewhere is one the
/// key cannot act for — the mount would read a book it can place no order against.
#[test]
fn a_plain_key_configured_elsewhere_disagrees() {
    let decided = hl_master_from_role(Ok(&UserRole::User), SIGNER);
    assert_eq!(
        hl_audit_configured(&decided, MASTER),
        ConfiguredAudit::Disagrees { venue_said: SIGNER.to_string() }
    );
}

/// **AN UNANSWERED PROBE SAYS NOTHING**, on every outcome that carries a warning: a network
/// failure, a lapsed approval (`missing`), a role this build does not classify.
///
/// ⚠ It must NOT read as a disagreement. On those arms `hl_master_from_role` falls back to the
/// signer's own address, which is today's behaviour and NOT an observation — comparing a
/// configured address against a fallback would raise the alarm every time the venue was
/// briefly unreachable, on a box where nothing is wrong.
#[test]
fn an_unanswered_probe_is_not_evidence_against_a_configured_address() {
    let stranger = "0x2222222222222222222222222222222222222222";
    for decided in [
        hl_master_from_role(Err("connection reset".to_string()), SIGNER),
        hl_master_from_role(Ok(&UserRole::Missing), SIGNER),
        hl_master_from_role(Ok(&UserRole::Unknown("multiSigUser".into())), SIGNER),
        hl_master_from_role(Ok(&UserRole::Vault), SIGNER),
    ] {
        assert!(decided.warning.is_some(), "premise: this outcome warns");
        assert_eq!(
            hl_audit_configured(&decided, stranger),
            ConfiguredAudit::Unanswered,
            "a fallback is not an observation: {decided:?}"
        );
    }
}
