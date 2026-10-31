//! A scripted venue whose EURUSD CHANGES its trading state while the session runs, for `tests/symbol_state.rs`.
//!
//! Everything is answered by `serve::script_reply` like any other fake, except that the symbol list states
//! EURUSD's `enabled` and the full-symbol answer states its `tradingMode` as the test last set them
//! ([`SymbolStateControl::change`], [`SymbolStateControl::set_quietly`]); `change` also pushes the unsolicited
//! `ProtoOASymbolChangedEvent` naming the symbol, which is what the real broker is documented to send, and
//! [`SymbolStateControl::drop_connection`] ends the session so the client reconnects (the listener accepts one
//! connection after another). The serve loop reads with a short timeout so it can push between two requests
//! without a second writer on the socket.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use prost::Message;

use vike_ctrader::framing::{self, FrameReader};
use vike_ctrader::proto::{
    ProtoOaSymbolByIdRes, ProtoOaSymbolChangedEvent, ProtoOaSymbolsListRes, ProtoOaTradingMode, pt,
};

use super::fake::AccountsScript;
use super::serve::{EofStream, script_reply};

/// What the control asks of the serve loop between two requests.
enum Signal {
    /// Push the symbol-changed event for EURUSD.
    Changed,
    /// End the session (the socket closes).
    Drop,
}

/// The test's hand on the venue: what it states for EURUSD (symbol id 1) from now on.
#[derive(Clone)]
pub struct SymbolStateControl {
    trading_mode: Arc<AtomicI32>,
    enabled: Arc<AtomicBool>,
    signals: Sender<Signal>,
}

/// Accept connections on `listener`, one after another, and serve each as the module doc says; the returned control
/// changes EURUSD.
pub(super) fn spawn_symbol_state(
    listener: TcpListener,
    seen: Arc<Mutex<Vec<u32>>>,
) -> SymbolStateControl {
    let (signals, receiver) = mpsc::channel();
    let receiver = Arc::new(Mutex::new(receiver));
    let trading_mode = Arc::new(AtomicI32::new(ProtoOaTradingMode::Enabled as i32));
    let enabled = Arc::new(AtomicBool::new(true));
    let control = SymbolStateControl {
        trading_mode: trading_mode.clone(),
        enabled: enabled.clone(),
        signals,
    };
    thread::Builder::new()
        .name("fake-ctrader-symbol-state".into())
        .spawn(move || {
            while let Ok((stream, _peer)) = listener.accept() {
                let (seen, trading_mode, enabled, receiver) =
                    (seen.clone(), trading_mode.clone(), enabled.clone(), receiver.clone());
                serve_symbol_state(stream, seen, trading_mode, enabled, receiver);
            }
        })
        .expect("spawn fake ctrader-symbol-state");
    control
}

impl SymbolStateControl {
    /// From now on the venue states `trading_mode` (a `ProtoOaTradingMode` as `i32`) and `enabled` for EURUSD, and
    /// pushes a `ProtoOASymbolChangedEvent` naming it.
    pub fn change(&self, trading_mode: i32, enabled: bool) {
        self.set_quietly(trading_mode, enabled);
        let _ = self.signals.send(Signal::Changed);
    }

    /// The same, with NO event: a broker whose event never arrived (or was never sent).
    pub fn set_quietly(&self, trading_mode: i32, enabled: bool) {
        self.trading_mode.store(trading_mode, Ordering::SeqCst);
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    /// End the current session: the client sees the socket close and reconnects.
    pub fn drop_connection(&self) {
        let _ = self.signals.send(Signal::Drop);
    }
}

/// Serve one connection until the client closes (or the control drops it): record each request type, answer it from
/// the shared script with EURUSD in the state the control last set, and push a symbol-changed event for every `change`.
fn serve_symbol_state(
    stream: TcpStream,
    seen: Arc<Mutex<Vec<u32>>>,
    trading_mode: Arc<AtomicI32>,
    enabled: Arc<AtomicBool>,
    signals: Arc<Mutex<Receiver<Signal>>>,
) {
    stream.set_read_timeout(Some(Duration::from_millis(50))).ok();
    let mut writer = stream.try_clone().expect("clone fake stream");
    let (eof_stream, eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);
    let mut write = |payload_type: u32, body: &[u8]| -> bool {
        let frame = framing::encode(payload_type, body, "srv");
        writer.write_all(&frame).is_ok() && writer.flush().is_ok()
    };
    loop {
        loop {
            let signal = signals.lock().expect("signals lock").try_recv();
            match signal {
                Ok(Signal::Changed) => {
                    let event = ProtoOaSymbolChangedEvent {
                        ctid_trader_account_id: 99,
                        symbol_id: vec![1],
                        ..Default::default()
                    };
                    if !write(pt::SYMBOL_CHANGED_EVENT, &event.encode_to_vec()) {
                        return;
                    }
                }
                Ok(Signal::Drop) => return,
                Err(_) => break,
            }
        }
        match reader.next_frame() {
            Ok(Some(msg)) => {
                seen.lock().expect("seen lock").push(msg.payload_type);
                for (reply_type, body) in script_reply(&msg, AccountsScript::OneDemo, false) {
                    let body = match reply_type {
                        pt::SYMBOLS_LIST_RES => {
                            let mut res =
                                ProtoOaSymbolsListRes::decode(body.as_slice()).expect("list");
                            for symbol in &mut res.symbol {
                                symbol.enabled = Some(enabled.load(Ordering::SeqCst));
                            }
                            res.encode_to_vec()
                        }
                        pt::SYMBOL_BY_ID_RES => {
                            let mut res =
                                ProtoOaSymbolByIdRes::decode(body.as_slice()).expect("by id");
                            for symbol in &mut res.symbol {
                                symbol.trading_mode = Some(trading_mode.load(Ordering::SeqCst));
                            }
                            res.encode_to_vec()
                        }
                        _ => body,
                    };
                    if !write(reply_type, &body) {
                        return;
                    }
                }
            }
            Ok(None) => {
                if eof.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}
