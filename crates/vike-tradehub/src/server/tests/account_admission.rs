use super::accounts::account_admission;
use vike_tradehub_client::proto::Scope;

/// Every scope the wire can authenticate under. Spelled as a literal list rather than derived,
/// so that ADDING a variant breaks this line and forces a decision about it here.
const EVERY_SCOPE: [Scope; 3] = [Scope::Read, Scope::Write, Scope::Account];

#[test]
fn only_the_admin_scope_is_admitted_and_only_when_the_capability_exists() {
    for scope in EVERY_SCOPE {
        let verdict = account_admission(true, scope);
        if scope == Scope::Account {
            assert!(
                verdict.is_ok(),
                "the ADMIN scope is the one this surface is for, and it was refused"
            );
        } else {
            let refusal = verdict.expect_err(
                "a non-Admin scope must be REFUSED on an armed box — this is the half that \
                     keeps the key a desktop carries to place orders from being the key that \
                     writes key material",
            );
            assert!(
                refusal.contains("requires the ADMIN scope"),
                "an armed box must refuse by AUTHORIZATION, naming the scope; got: {refusal}"
            );
        }
    }
}

#[test]
fn an_unarmed_node_refuses_every_scope_including_admin() {
    for scope in EVERY_SCOPE {
        let refusal = account_admission(false, scope).expect_err(
            "a node that declared no barrier holds no writer, so there is nothing to refuse \
                 WITH — and that must hold for Admin too, or the declaration is not the gate",
        );
        assert!(
            refusal.contains("is not armed on this node"),
            "an unarmed box must refuse by ABSENCE, not by authorization; got: {refusal}"
        );
    }
}

/// The two refusals must not be confusable. An operator who reads one and goes looking for the
/// other's cause has been sent to the wrong place, which is the whole reason they are distinct
/// strings rather than one message.
#[test]
fn the_absence_refusal_and_the_authorization_refusal_are_different_facts() {
    let unarmed = account_admission(false, Scope::Write).expect_err("unarmed refuses");
    let unauthorized = account_admission(true, Scope::Write).expect_err("wrong scope refuses");
    assert_ne!(unarmed, unauthorized);
    assert!(
        !unarmed.contains("requires the ADMIN scope"),
        "an unarmed box must not tell the operator to go find an admin key — the key is not \
             what is missing"
    );
    assert!(
        !unauthorized.contains("is not armed on this node"),
        "an armed box must not report itself unarmed — the operator would go declare a \
             barrier that is already declared"
    );
}
