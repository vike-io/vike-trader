//! A node that answers NOT NOW (its core busy, still starting) is read off its refusal's TYPE on both write paths
//! (the one-shot writes' `send_and_classify`, the REPL's and `halt`/`resume`'s `send_and_report`): the retry rung,
//! never the far side's final no, whether the node sent the refusal frame or, being older, the words alone.

use std::collections::HashMap;
use std::time::Duration;

use vike_client::hubkeys;
use vike_tradehub_client::TradehubControlHandle;
use vike_tradehub_client::proto::{BUSY_ERROR, Response};
use vike_tradehub_client::refusals::TradehubRefusal;
use vike_tradehub_client::test_support::strategy_node::{
    KEY, scripted_node_answering, todays_node_features,
};
use vike_tradehub_client::wire::CommandWire;

use crate::cmd::trade::oneshot::{WriteContext, send_and_classify};
use crate::cmd::trade::write::send_and_report;
use crate::exit::Exit;

const WAIT: Duration = Duration::from_secs(5);

/// A control link to a node that answers a busy core's frame, then an older node's busy words, then a final no.
fn busy_then_refusing_node() -> TradehubControlHandle {
    let (addr, _report) = scripted_node_answering(todays_node_features(), |n| match n {
        0 => Response::Refused(TradehubRefusal::Busy.into()),
        1 => Response::Error(BUSY_ERROR.into()),
        _ => Response::Error("over the cap".into()),
    });
    TradehubControlHandle::connect(addr, KEY).expect("handshake")
}

fn cancel() -> CommandWire {
    CommandWire::Cancel("c-1".into())
}

/// The one-shot writes: NOT NOW is `Exit::Connect` (send it again shortly), a final no `Exit::Venue`.
#[test]
fn a_busy_node_is_the_retry_rung_of_a_one_shot_write() {
    let control = busy_then_refusing_node();
    let keys = hubkeys::resolve(&HashMap::new(), None);
    let ctx = WriteContext {
        node: String::new(),
        keys: &keys,
        policy_max_notional: None,
        ack_wait: WAIT,
        yes: true,
        json: false,
        no_wait: true,
        reason: None,
    };
    let exits: Vec<Exit> = (0..3)
        .map(|_| send_and_classify(&control, cancel(), &ctx).map_or_else(|e| e.exit, |()| Exit::Ok))
        .collect();
    assert_eq!(exits, [Exit::Connect, Exit::Connect, Exit::Venue]);
}

/// `halt`/`resume` and the REPL: NOT NOW is `Exit::Connect`, a final no `Exit::Failed`.
#[test]
fn a_busy_node_is_the_retry_rung_of_halt_and_resume() {
    let control = busy_then_refusing_node();
    let exits: Vec<Exit> =
        (0..3).map(|_| send_and_report(&control, cancel(), None, WAIT)).collect();
    assert_eq!(exits, [Exit::Connect, Exit::Connect, Exit::Failed]);
}
