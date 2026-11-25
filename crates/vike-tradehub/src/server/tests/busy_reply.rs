//! A busy core's answer to a command: the refusal FRAME by its id to a client that reads one, and the bytes every
//! node has sent to one that does not.

use vike_hub_protocol::refusal::HubRefusal;
use vike_tradehub_client::proto::{BUSY_ERROR, Response};
use vike_tradehub_client::refusals::{TradehubRefusal, frame_outcome};
use vike_utility::errors::FailureOutcome;

use crate::refusals::CommandRefusal;
use crate::server::control::ControlAcceptError;

/// **A busy core is NOT NOW by its id**: the frame carries `tradehub.busy`, an older client is answered the
/// `Error` it always read, and every other refusal is the frame of its own id, its words the older `Error`.
#[test]
fn a_busy_core_answers_the_frame_and_an_older_client_the_same_bytes() {
    let busy = ControlAcceptError::Busy.reply();
    let Response::Refused(frame) = &busy else { panic!("a busy core answers the frame: {busy:?}") };
    assert_eq!(*frame, HubRefusal::from(TradehubRefusal::Busy));
    assert_eq!(frame_outcome(frame), FailureOutcome::NotNow);
    assert_eq!(busy.older_form(true), None);
    assert_eq!(busy.older_form(false), Some(Response::Error(BUSY_ERROR.to_string())));
    let over = HubRefusal::new("tradehub.test.over_the_cap", "over the cap");
    let refused = ControlAcceptError::Refused(over.clone()).reply();
    assert_eq!(refused, Response::Refused(over));
    assert_eq!(refused.older_form(false), Some(Response::Error("over the cap".into())));
    let gone = ControlAcceptError::Gone.reply();
    assert_eq!(gone, Response::Refused(CommandRefusal::ShuttingDown.into()));
    assert_eq!(gone.older_form(false), Some(Response::Error("core is shutting down".into())));
    assert_eq!(frame_outcome(&ControlAcceptError::Gone.refusal()), FailureOutcome::Refused);
}
