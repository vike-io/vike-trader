//! A local `ws://127.0.0.1:<port>` stand-in for a venue's PRIVATE stream, for the `SubmitFence`
//! scenario: the venue's REAL user-data pump dials it, so a venue's pump wiring (its sink, its replay
//! fetch) is what the scenario exercises, not a model of it.
//!
//! It speaks no venue protocol. The venue's handshake (bybit: `auth` then `subscribe`; okx: `login`
//! then `subscribe`) is a fixed list of REPLY frames: the n-th text frame the pump sends is answered
//! with the n-th reply, and once the last one is out the pump counts as subscribed
//! (`FakePrivateWs::wait_ready`). From then on `FakePrivateWs::push` sends a frame to the pump as the
//! venue would. One connection only: a dial after that is never accepted, so a pump that reconnects
//! mid-test is a failure and not a silent second stream.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tungstenite::Message;

/// Server-side read timeout, so the fake's thread polls its stop flag and its push queue.
const TICK: Duration = Duration::from_millis(20);

pub(super) struct FakePrivateWs {
    url: String,
    push: Sender<String>,
    ready: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakePrivateWs {
    pub(super) fn spawn(handshake_replies: &'static [&'static str]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
        listener.set_nonblocking(true).expect("nonblocking listener");
        let url = format!("ws://{}", listener.local_addr().expect("local addr"));
        let (push, queue) = channel::<String>();
        let ready = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_t, stop_t) = (Arc::clone(&ready), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        if stop_t.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(TICK);
                    }
                    Err(_) => return,
                }
            };
            drop(listener); // one connection: any re-dial is refused
            stream.set_nonblocking(false).expect("blocking accepted stream");
            let Ok(mut ws) = tungstenite::accept(stream) else { return };
            let _ = ws.get_ref().set_read_timeout(Some(TICK));
            let mut replies = handshake_replies.iter();
            if handshake_replies.is_empty() {
                ready_t.store(true, Ordering::Release);
            }
            while !stop_t.load(Ordering::Relaxed) {
                match ws.read() {
                    Ok(Message::Text(_)) => {
                        if let Some(reply) = replies.next() {
                            if ws.send(Message::Text((*reply).into())).is_err() {
                                return;
                            }
                            if replies.len() == 0 {
                                ready_t.store(true, Ordering::Release);
                            }
                        }
                    }
                    Ok(Message::Close(_)) => return,
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(ref e))
                        if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(_) => return,
                }
                while let Ok(frame) = queue.try_recv() {
                    if ws.send(Message::Text(frame.into())).is_err() {
                        return;
                    }
                }
            }
        });
        Self { url, push, ready, stop, thread: Some(thread) }
    }

    pub(super) fn url(&self) -> String {
        self.url.clone()
    }

    /// Wait (bounded) for the pump to finish its handshake: only then can a pushed frame be read.
    pub(super) fn wait_ready(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.ready.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err(
                    "the venue's pump never finished its handshake with the fake socket".into()
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    /// Send `frame` to the pump as a text frame, as the venue would.
    pub(super) fn push(&self, frame: &str) {
        let _ = self.push.send(frame.to_string());
    }
}

impl Drop for FakePrivateWs {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
