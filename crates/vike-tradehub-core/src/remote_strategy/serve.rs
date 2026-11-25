//! The CHILD's half of the pipe: serve one mount's hooks to the tradehub that spawned the process.
//!
//! `crates/vike-strategy-sandbox`'s `live` verb jails the process, loads the strategy file and calls
//! [`serve`] with a builder over the loaded file; this crate's tests call it with a strategy written in
//! Rust over a pair of pipes, which is how both halves of the protocol are tested together without a
//! process. The serve loop is generic over the strategy, so the two are the same code.

use std::io::{self, Read, Write};
use std::time::Instant;

use vike_hub_protocol::child_pipe;
use vike_model::Strategy;
use vike_utility::errors::{CarriedRefusal, Declared};

use super::protocol::{FromChild, Hook, Open, Opened, STATE_REFRESH, ToChild, ViewBroker};

/// Serve one mount over `input` / `output` until the tradehub closes the pipe (`Ok`).
///
/// `identity` is this process's build, the pipe's first frame, which the tradehub's must match
/// ([`child_pipe::check_build`]): the binary's `vike_buildinfo::summary`, handed in by the composition root.
/// `build` makes the strategy from the `Open` (its params text and mount facts), with the file's
/// effective-params line when it has one (`Opened::echo`); its `Err` is answered as
/// [`FromChild::Refused`] in its words, its id kept. The strategy's `save_state` is the bytes it encodes
/// (`vike_model::StrategyState`; a strategy file's own, which its host never decodes), sent and compared
/// as they are. A hook that panics unwinds out of this function: the process dies, which the tradehub
/// reads as a crash and restarts.
///
/// # Errors
/// The pipe failed, the tradehub is another build ([`child_pipe::check_build`]), or it sent what this
/// protocol does not allow (a frame that is not a message, a second `Open`, a view that does not fit what
/// this side holds).
pub fn serve<S, R, W>(
    mut input: R,
    mut output: W,
    identity: &str,
    build: impl FnOnce(&Open) -> Result<(S, Option<Vec<(String, String)>>), CarriedRefusal>,
) -> io::Result<()>
where
    S: Strategy<ViewBroker>,
    R: Read,
    W: Write,
{
    child_pipe::send_build(&mut output, identity)?;
    child_pipe::check_build(&mut input, identity, "the tradehub")?;
    let open = match child_pipe::read_message::<_, ToChild>(&mut input)? {
        ToChild::Open(open) => *open,
        ToChild::Event(_) => return Err(refuse("an event before the open")),
    };
    let (mut strategy, echo) = match build(&open) {
        Ok(built) => built,
        Err(refusal) => {
            let id = refusal.declared_id().map(str::to_owned);
            let refused = FromChild::Refused { reason: refusal.to_string(), id };
            return child_pipe::write_message(&mut output, &refused);
        }
    };
    if let Some(state) = &open.state {
        strategy.load_state(state);
    }
    let mut said = (strategy.save_state(), strategy.params());
    let opened = Opened {
        symbols: strategy.symbols(),
        inputs: strategy.inputs(),
        warmup: strategy.warmup(),
        state: said.0.clone(),
        params: said.1.clone().map(Box::new),
        echo,
    };
    child_pipe::write_message(&mut output, &FromChild::Opened(opened))?;
    let mut broker = ViewBroker::default();
    let mut said_at = Instant::now();
    loop {
        let event = match child_pipe::read_message::<_, ToChild>(&mut input) {
            Ok(ToChild::Event(event)) => *event,
            Ok(ToChild::Open(_)) => return Err(refuse("a second open")),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        if let Some(view) = event.view {
            broker.absorb(view).map_err(|why| refuse(&why))?;
        }
        // An order-cadence hook (a fill, an order's end, a re-tune, a state) is when a state moves.
        let retuned = event.hook.is_lossless();
        run_hook(&mut strategy, &mut broker, event.hook);
        let calls = std::mem::take(&mut broker.calls);
        child_pipe::write_message(
            &mut output,
            &FromChild::Reply { sequence: event.sequence, calls },
        )?;
        // AFTER the reply, so a state never stands between a hook and its orders.
        if retuned || said_at.elapsed() >= STATE_REFRESH {
            said_at = Instant::now();
            // The state's bytes are compared: an unchanged state costs one copy and no decode.
            let now = (strategy.save_state(), strategy.params());
            if now != said {
                said = now;
                let (state, params) = (said.0.clone(), said.1.clone().map(Box::new));
                child_pipe::write_message(&mut output, &FromChild::State { state, params })?;
            }
        }
    }
}

fn refuse(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("the tradehub sent {why}"))
}

/// Make the one hook call `hook` names.
fn run_hook<S: Strategy<ViewBroker>>(s: &mut S, b: &mut ViewBroker, hook: Hook) {
    match hook {
        Hook::Start => s.on_start(b),
        Hook::Stop => s.on_stop(b),
        Hook::Bar(bar) => s.on_bar(b, &bar),
        Hook::TickQuote(q) => s.on_quote_tick(b, &q),
        Hook::TickTrade(t) => s.on_trade_tick(b, &t),
        Hook::OrderBook(book) => s.on_order_book(b, &book),
        Hook::Schedule(tag) => s.on_schedule(b, &tag),
        Hook::Fill(fill) => s.on_fill(b, &fill),
        Hook::FeedStatus(status) => s.on_feed_status(b, status),
        Hook::Mark(mark) => s.on_mark(b, &mark),
        Hook::ReferenceQuote { venue, quote } => s.on_reference_quote(b, &venue, &quote),
        Hook::Flow(flow) => s.on_flow(b, flow),
        Hook::OrderEvent(event) => s.on_order_event(b, &event),
        Hook::ParamsUpdated(params) => s.on_params_updated(b, &params),
        Hook::LoadState(state) => s.load_state(&state),
    }
}
