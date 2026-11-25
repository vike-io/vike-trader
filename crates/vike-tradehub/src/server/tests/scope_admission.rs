use super::handshake::scope_admission;
use vike_hub_protocol::access::AccessList;
use vike_hub_protocol::auth::{HubKeys, Scope};

/// Every scope the wire can authenticate under. A literal list, matching
/// `server::tests::account_admission`'s reason: ADDING a variant must break this line and force a
/// decision about it here rather than defaulting into the permissive arm.
const EVERY_SCOPE: [Scope; 4] = [Scope::Read, Scope::Write, Scope::Account, Scope::Trade];

fn keys(control: bool, admin: bool) -> HubKeys {
    let k = HubKeys::new(
        b"observe-key".to_vec(),
        if control { b"control-key".to_vec() } else { Vec::new() },
    );
    if admin { k.with_admin(b"admin-key".to_vec()) } else { k }
}

/// **A node whose account administration is not ARMED refuses the ADMIN claim — without consulting a
/// key, and whatever its key store holds.** That is the closed-gate property: an unarmed capability is
/// not an empty key to compare a MAC against, it is a scope that cannot be reached (decision 0288: for a
/// device's admin role too).
#[test]
fn a_node_not_armed_for_account_administration_refuses_the_admin_claim() {
    for held in [keys(true, false), keys(true, true)] {
        let reason = scope_admission(Scope::Account, &held, true, false)
            .expect_err("an unarmed node must refuse the claim");
        assert!(reason.contains("account administration is not armed"), "{reason}");
    }
}

/// ...and the same for CONTROL, which is the older half of the same gate: refused when the node holds
/// no control key (the daemon zeroes it when control is off) and no access list a device's role could
/// grant it by, whatever the sink. A node that HOLDS the key admits the claim with no sink, as it always
/// did: a preview needs the scope and no sink.
#[test]
fn a_node_with_nothing_to_grant_control_refuses_the_control_claim() {
    for control in [false, true] {
        let reason = scope_admission(Scope::Write, &keys(false, false), control, false)
            .expect_err("a node with control disabled must refuse the claim");
        assert!(reason.contains("control disabled"), "{reason}");
    }
    assert!(scope_admission(Scope::Write, &keys(true, false), false, false).is_ok());
}

/// **A node with control on and an ACCESS LIST admits the control claim with no control key**: a
/// device's role is what grants it then (decision 0288), and the list decides below.
#[test]
fn a_node_with_control_on_and_an_access_list_admits_the_control_claim_with_no_control_key() {
    let devices = keys(false, false).with_access(AccessList::new(|_| None));
    assert!(scope_admission(Scope::Write, &devices, true, false).is_ok());
    let reason = scope_admission(Scope::Write, &devices, false, false)
        .expect_err("control off refuses a device too");
    assert!(reason.contains("control disabled"), "{reason}");
}

/// ⚠ **The complement, and it is the half that stops this gate being a capability regression:**
/// an ARMED node admits every scope. Without this, the tests above are satisfied by a function that
/// refuses everything.
#[test]
fn an_armed_node_admits_every_scope() {
    let armed = keys(true, true);
    for scope in EVERY_SCOPE {
        assert!(
            scope_admission(scope, &armed, true, true).is_ok(),
            "an armed node must admit {scope:?} — the MAC below is what then decides"
        );
    }
}

/// **OBSERVE is never gated here**, on any node, and that is deliberate rather than an
/// oversight: the observe key is the one capability every node has, so there is no "unarmed"
/// state for it to be refused from. Pinned so a future arm cannot quietly start gating it and
/// take every read-only client down with it.
#[test]
fn the_observe_scope_is_admitted_even_by_a_node_armed_for_nothing_else() {
    assert!(scope_admission(Scope::Read, &keys(false, false), false, false).is_ok());
}

/// The two refusals are DIFFERENT facts and must read differently — a post-incident review
/// searches the log by text, and `crates/vike-tradehub/src/server/handshake.rs`'s two `warn!` sentences
/// are what it finds. Same shape as `server::tests::account_admission`'s own distinctness assertion.
#[test]
fn the_admin_and_control_refusals_are_different_facts() {
    let admin = scope_admission(Scope::Account, &keys(true, false), true, false)
        .expect_err("unarmed admin");
    let control = scope_admission(Scope::Write, &keys(false, true), false, true)
        .expect_err("unarmed control");
    assert_ne!(admin, control, "two different absences may not print one sentence");
}

/// **The tradehub serves no `Trade` scope, whatever a peer signs.** `Scope::Trade` is the key a
/// tradehub HOLDS as a client of the gatewayhub and the datahub; it verifies nothing with it. This
/// daemon\'s `HubKeys` never carry the key (`HubKeys::with_trade` is not called here), so the claim is
/// refused at the MAC, before any HMAC is built, for the empty key and for every key this node does
/// hold: `scope_admission` lets the claim through to that check (it gates only `Account` and `Write`).
#[test]
fn the_tradehub_serves_no_trade_scope_whatever_a_peer_signs() {
    use vike_tradehub_client::auth::DOMAIN;
    use vike_tradehub_client::proto::NODE_PROTO_VERSION;
    let held = keys(true, true);
    assert!(!held.has(Scope::Trade), "the tradehub\'s keys carry no trade key");
    assert!(
        scope_admission(Scope::Trade, &held, true, true).is_ok(),
        "the closed gate is the MAC, below"
    );
    let nonce = [7u8; 32];
    for key in [&b""[..], b"observe-key", b"control-key", b"admin-key", b"trade-key"] {
        let mac =
            vike_hub_protocol::auth::sign(DOMAIN, key, &nonce, NODE_PROTO_VERSION, Scope::Trade);
        assert!(
            !held.verify(DOMAIN, Scope::Trade, &nonce, NODE_PROTO_VERSION, &mac),
            "a Trade claim signed with {key:?} must be refused"
        );
    }
}
