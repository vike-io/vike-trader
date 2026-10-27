use super::handshake::scope_admission;
use vike_hub_proto::auth::{HubKeys, Scope};

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

/// **A node holding no admin key refuses the ADMIN claim — without consulting a key.** That is
/// the closed-gate property: an absent key is not an empty key to compare a MAC against, it is
/// a scope that cannot be reached.
#[test]
fn a_node_with_no_admin_key_refuses_the_admin_claim() {
    let reason = scope_admission(Scope::Account, &keys(true, false))
        .expect_err("a node holding no admin key must refuse the claim");
    assert!(reason.contains("account administration is not armed"), "{reason}");
}

/// ...and the same for CONTROL, which is the older half of the same gate.
#[test]
fn a_node_with_no_control_key_refuses_the_control_claim() {
    let reason = scope_admission(Scope::Write, &keys(false, false))
        .expect_err("a node with control disabled must refuse the claim");
    assert!(reason.contains("control disabled"), "{reason}");
}

/// ⚠ **The complement, and it is the half that stops this gate being a capability regression:**
/// an ARMED node admits every scope it holds a key for. Without this, both tests above are
/// satisfied by a function that refuses everything.
#[test]
fn an_armed_node_admits_every_scope_it_holds_a_key_for() {
    let armed = keys(true, true);
    for scope in EVERY_SCOPE {
        assert!(
            scope_admission(scope, &armed).is_ok(),
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
    assert!(scope_admission(Scope::Read, &keys(false, false)).is_ok());
}

/// The two refusals are DIFFERENT facts and must read differently — a post-incident review
/// searches the log by text, and `crates/vike-tradehub/src/server/handshake.rs`'s two `warn!` sentences
/// are what it finds. Same shape as `server::tests::account_admission`'s own distinctness assertion.
#[test]
fn the_admin_and_control_refusals_are_different_facts() {
    let admin = scope_admission(Scope::Account, &keys(true, false)).expect_err("unarmed admin");
    let control = scope_admission(Scope::Write, &keys(false, true)).expect_err("unarmed control");
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
    assert!(scope_admission(Scope::Trade, &held).is_ok(), "the closed gate is the MAC, below");
    let nonce = [7u8; 32];
    for key in [&b""[..], b"observe-key", b"control-key", b"admin-key", b"trade-key"] {
        let mac = vike_hub_proto::auth::sign(DOMAIN, key, &nonce, NODE_PROTO_VERSION, Scope::Trade);
        assert!(
            !held.verify(DOMAIN, Scope::Trade, &nonce, NODE_PROTO_VERSION, &mac),
            "a Trade claim signed with {key:?} must be refused"
        );
    }
}
