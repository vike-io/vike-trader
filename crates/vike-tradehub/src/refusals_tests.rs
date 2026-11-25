//! The carrier rule at the tradehub's one bridge-facing refusal: a refused symbol shows ONE id.

use vike_gatewayhub_client::proto::{SymbolPropertiesRefusal, SymbolPropertiesRefusalCode};
use vike_gatewayhub_client::trade::SymbolPropertiesRequestError;
use vike_hub_protocol::refusal::HubRefusal;
use vike_tradehub_client::refusals::TradehubRefusalId;

use super::SymbolRefusal;

/// The gatewayhub's answer for `XYZ`, with the bridge's id (`W1`) or as free text.
fn daemon(refusal_id: Option<&str>) -> SymbolPropertiesRequestError {
    SymbolPropertiesRequestError::Daemon(SymbolPropertiesRefusal {
        code: SymbolPropertiesRefusalCode::Verdict,
        reason: "ibkr: no contract for `XYZ`".into(),
        refusal_id: refusal_id.map(str::to_owned),
    })
}

fn facts_refused(why: &SymbolPropertiesRequestError) -> SymbolRefusal {
    SymbolRefusal::FactsRefused { engine: "E".into(), symbol: "XYZ".into(), why: why.carried() }
}

/// A bridge's refusal of a symbol reaches the person under the BRIDGE's id, once, in the frame and in its
/// rendered line; one without an id shows the node's own. A carried id is shown, never decided on: no client
/// reads it as one of its own ids.
#[test]
fn a_facts_refusal_shows_the_bridges_id_once_and_its_own_without_one() {
    let text = "submit: E: cannot admit `XYZ`: ibkr: no contract for `XYZ`. The order was REFUSED";
    let bridge = facts_refused(&daemon(Some("ibkr.symbol.no_contract")));
    assert_eq!(bridge.rendered(), format!("error [ibkr.symbol.no_contract]: {text}"));
    let frame = HubRefusal::from(bridge);
    assert_eq!(frame, HubRefusal::new("ibkr.symbol.no_contract", text));
    assert_eq!(TradehubRefusalId::from_word(&frame.id), None);
    let free = HubRefusal::from(facts_refused(&daemon(None)));
    assert_eq!(free, HubRefusal::new("tradehub.symbol.facts_refused", text));
}
