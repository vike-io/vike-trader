//! Dukascopy execution — drives the JForex Java sidecar over JSON-lines stdio.
//!
//! Dukascopy's only trading API is JForex (Java), so unlike every HMAC-REST venue this
//! client owns a CHILD PROCESS (`java -jar jforex-bridge.jar`) instead of a socket or
//! FFI handle: commands go to child stdin, canonical [`Event`] envelopes come back on
//! stdout and are pumped into the core ingest (the same inbound shape as the WS
//! user-data pumps). See docs/superpowers/specs/2026-07-04-dukascopy-jforex-bridge-design.md.
//!
//! LIVE GATE: jar missing, `java` missing, login failure, or handshake timeout →
//! [`DukascopyError::Unavailable`] → the caller stays paper. A child that dies later
//! turns NEW submits into synthetic OrderRejected("bridge unavailable") on the failed
//! write, and the reader thread's EOF drain rejects every already-in-flight order
//! ("bridge died") — either way every order still reaches a terminal state.
//!
//! # ⚠ The DUAL-PUBLISH fill contract (break this and position/PnL silently desync)
//!
//! A fill arrives on the wire as TWO envelopes carrying the SAME fill, and both must reach the
//! core, in order: the BARE `Event::Fill` first — which the core `Account` folds into position and
//! realized PnL independently of any order — and then the wrapping `Event::OrderFilled` the order
//! FSM applies. Dropping either one leaves half the system right and the other half permanently
//! wrong, and neither half complains.
//!
//! The netting shadow (`ShadowBook::fold_fill`, called from the reader thread) folds the **bare
//! lane ONLY**, for exactly the reason the `Account` does: the wrap carries the same fill, so
//! folding both double-counts. If you add a fill-consuming side effect to the reader loop, decide
//! which lane it belongs on before you write it — "both" is never the answer.
//!
//! # ⚠ `DUKASCOPY_*_SERVER` is usually NOT a JNLP URL
//!
//! That variable commonly holds the WEB-PLATFORM login URL. The sidecar would hand it to
//! `IClient.connect` as a JNLP and HANG there — no error, no timeout distinguishable from a slow
//! login. [`DukascopyExecutionClient::spawn_with_program`] self-defends: a `server` value that does
//! not end in `.jnlp` is warned about and replaced by the built-in demo JNLP. So a mis-set variable
//! costs a warning line, not a stall — do not "fix" that branch by trusting the value.
//!
//! # The two runtime paths are RESOLVED BY THE CALLER — this module decides nothing
//!
//! [`DukascopyExecutionClient::spawn`] takes a [`DukascopyTools`], which the composition root
//! produces with `vike_dukascopy::resolve_dukascopy_tools` from its own environment sweep and
//! `vike_model::state_path::project_bin_dir_from`. Nothing here reads the environment, walks for a
//! project, or knows where a jar lives.
//!
//! ⚠ **It used to, and that is worth keeping written down**: the deleted `bridge_jar` and
//! `java_program` built their fallbacks from `concat!(env!("CARGO_MANIFEST_DIR"), …)` — the tree
//! this crate was COMPILED in, not the project the binary is later run from. A binary built
//! elsewhere and deployed into a `<project>` folder found neither jar nor JVM and degraded to
//! paper, with nothing in the log but "bridge jar not found" — the same defect class the
//! credential-store ratchet spent a release retiring for settings. The escape hatches were
//! asymmetric too: `JFOREX_BRIDGE_JAR` and `JAVA_HOME` named the two artifacts outright, but the
//! `vendor/tools/` sweep beneath them had **no override at all**, so a deployment had to set both
//! explicitly or get nothing. Both variables were `Layer::Library` rows in
//! `crates/vike-ops/src/settings.rs`; they are `Layer::Injected` rows now, read from the map the
//! caller supplies, and `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` ratchet shrank
//! by two. The ladders themselves live with the parser, on
//! `crates/bridges/dukascopy/src/config.rs`'s `resolve_dukascopy_tools`.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::{Event, OrderRejected, OrderSubmitted};
use vike_model::OrderRequest;

use super::config::{DukascopyConfig, DukascopyTools};
use super::netting::{Reanchor, ShadowBook};
use super::proto::{encode_line, parse_envelope, Command, Envelope};
use super::recon_client::{DukascopyReconClient, PositionSnapshot, VenuePosition};

/// Shared tracing target for this module's diagnostics.
const TARGET: &str = "vike_dukascopy::exec";

/// Verified live demo JNLP (spec: Build & gating). `DukascopyConfig.server` overrides.
const DEFAULT_DEMO_JNLP: &str = "https://www.dukascopy.com/client/demo/jclient/jforex.jnlp";
/// JForex JNLP login is slow — first login downloads platform config (observed > 60s);
/// the sidecar polls the session for up to 240s, so bound the handshake just above that.
const READY_TIMEOUT: Duration = Duration::from_secs(300);
/// Grace between `shutdown` and kill.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Errors from the Dukascopy venue layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DukascopyError {
    /// No jar / no java / login failed / handshake timed out — stay paper.
    Unavailable,
}

impl std::fmt::Display for DukascopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DukascopyError::Unavailable => {
                write!(f, "Dukascopy JForex bridge unavailable (jar/java missing or login failed)")
            }
        }
    }
}

impl std::error::Error for DukascopyError {}

/// client_order_ids awaiting a terminal event, shared between `submit` (insert before
/// write) and the reader thread (remove on terminal event; drain into synthetic
/// rejects at EOF so a dead bridge never strands an order non-terminal).
type Inflight = Arc<Mutex<HashSet<String>>>;

/// The client_order_id when `event` is TERMINAL for an order (in-flight tracking).
/// Accepted/PartiallyFilled/Triggered are non-terminal — the order is still live.
fn terminal_coid(event: &Event) -> Option<&str> {
    match event {
        Event::OrderRejected(e) => Some(&e.client_order_id),
        Event::OrderCanceled(e) => Some(&e.client_order_id),
        Event::OrderFilled(e) => Some(&e.client_order_id),
        Event::OrderExpired(e) => Some(&e.client_order_id),
        Event::OrderLiquidated(e) => Some(&e.client_order_id),
        _ => None,
    }
}

/// Live Dukascopy exec client — owns the sidecar child process.
pub struct DukascopyExecutionClient {
    child: Child,
    /// `None` after a failed write or shutdown — the dead-bridge marker.
    stdin: Option<ChildStdin>,
    events: EventSender,
    reader: Option<std::thread::JoinHandle<()>>,
    inflight: Inflight,
    /// The sidecar's last authoritative `position` line per symbol, filled by the reader thread
    /// (raw venue truth, captured before the netting re-anchor). The reconcile seam
    /// ([`DukascopyReconClient`], built via [`Self::recon_client`]) reads it; see `recon_client.rs`.
    venue_positions: PositionSnapshot,
}

impl DukascopyExecutionClient {
    /// Spawn the real sidecar with the CALLER'S resolved tool paths, pass creds via child env
    /// (never argv), and wait for the ready/fatal handshake.
    ///
    /// `tools` comes from [`crate::resolve_dukascopy_tools`], called by the composition root that
    /// owns the environment sweep — see this module's doc for why this function resolves nothing
    /// itself. A jar that is not a file is the live gate: warn with the path that was actually
    /// tried (so an operator can see WHICH project was resolved) and stay paper.
    pub fn spawn(
        config: DukascopyConfig,
        tools: &DukascopyTools,
        events: EventSender,
    ) -> Result<Self, DukascopyError> {
        if !tools.bridge_jar.is_file() {
            tracing::warn!(
                target: TARGET,
                jar = %tools.bridge_jar.display(),
                java = %tools.java,
                "bridge jar not found — staying paper (install it at <project>/bin/jforex/, or name \
                 it with JFOREX_BRIDGE_JAR)"
            );
            return Err(DukascopyError::Unavailable);
        }
        Self::spawn_with_program(
            &tools.java,
            &["-jar".into(), tools.bridge_jar.display().to_string()],
            &config,
            events,
        )
    }

    /// Test seam: spawn an arbitrary program speaking the bridge protocol.
    pub fn spawn_with_program(
        program: &str,
        args: &[String],
        config: &DukascopyConfig,
        events: EventSender,
    ) -> Result<Self, DukascopyError> {
        let server = config.server.trim();
        let jnlp = if server.is_empty() {
            DEFAULT_DEMO_JNLP
        } else if !server.ends_with(".jnlp") {
            // The DUKASCOPY_*_SERVER .env var commonly holds the web-platform LOGIN
            // URL, which the sidecar would feed to IClient.connect as a JNLP and hang.
            tracing::warn!(
                target: TARGET,
                server = ?server,
                "DUKASCOPY_*_SERVER is not a JNLP URL; using the default demo JNLP"
            );
            DEFAULT_DEMO_JNLP
        } else {
            server
        };
        let mut child = std::process::Command::new(program)
            .args(args)
            .env("DUKASCOPY_LOGIN", &config.login)
            .env("DUKASCOPY_PASSWORD", &config.password)
            .env("DUKASCOPY_JNLP", jnlp)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                tracing::error!(target: TARGET, error = %e, "failed to spawn bridge");
                DukascopyError::Unavailable
            })?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        // Handshake channel: the reader thread forwards the first ready/fatal here,
        // then pumps every subsequent `event` envelope into the core ingest.
        let (hs_tx, hs_rx) = std_mpsc::sync_channel::<Result<String, String>>(1);
        let pump = events.clone();
        let inflight: Inflight = Arc::new(Mutex::new(HashSet::new()));
        let inflight_reader = Arc::clone(&inflight);
        let venue_positions: PositionSnapshot = Arc::new(Mutex::new(HashMap::new()));
        let venue_positions_reader = Arc::clone(&venue_positions);
        let reader = std::thread::Builder::new()
            .name("dukascopy-bridge-reader".into())
            .spawn(move || {
                let mut handshaken = false;
                // Netting-truth shadow (law A7, see `netting.rs`): folds every bare fill this
                // thread pumps, so the sidecar's authoritative `position` lines can be checked
                // against exactly what the core Account will hold.
                let mut shadow = ShadowBook::default();
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    match parse_envelope(&line) {
                        Some(Envelope::Ready { account, balance }) if !handshaken => {
                            handshaken = true;
                            // connectivity sanity check (spec: `balance` has no other consumer)
                            tracing::info!(target: TARGET, %account, balance, "bridge ready");
                            let _ = hs_tx.send(Ok(account));
                        }
                        Some(Envelope::Ready { .. }) => {} // duplicate ready: ignore
                        Some(Envelope::Fatal { reason }) => {
                            tracing::error!(target: TARGET, %reason, "bridge fatal");
                            if !handshaken {
                                let _ = hs_tx.send(Err(reason));
                            }
                            // post-ready fatal precedes sidecar exit → EOF ends the loop
                        }
                        Some(Envelope::Event { event }) => {
                            if !handshaken {
                                // Protocol: no event may precede the ready handshake —
                                // a ghost event here would reach the core for an order
                                // it never submitted (and pre-ready blocking_send could
                                // wedge the not-yet-live ingest lane).
                                tracing::debug!(target: TARGET, ?event, "dropped pre-ready event");
                                continue;
                            }
                            if let Some(coid) = terminal_coid(&event) {
                                inflight_reader.lock().unwrap().remove(coid);
                            }
                            // Shadow-fold the bare fill lane ONLY (the OrderFilled wrap carries
                            // the same fill and must not double-fold — same law as the Account).
                            if let Event::Fill(fill) = event.as_ref() {
                                shadow.fold_fill(fill);
                            }
                            if pump.blocking_send(*event).is_err() {
                                break; // core gone — stop pumping
                            }
                        }
                        Some(Envelope::Position { symbol, size, avg_px, ts }) => {
                            if !handshaken {
                                tracing::debug!(target: TARGET, %symbol, "dropped pre-ready position line");
                                continue;
                            }
                            // Capture the RAW venue truth for the reconcile seam BEFORE the
                            // re-anchor consumes it — so the snapshot reflects the venue even in
                            // the `SizeMismatch` case the re-anchor refuses to auto-heal.
                            venue_positions_reader
                                .lock()
                                .unwrap()
                                .insert(symbol.clone(), VenuePosition { size, avg_px, ts });
                            match shadow.reanchor(&symbol, size, avg_px, ts) {
                                Reanchor::Corrected(legs) => {
                                    // Netted-close attribution drift: fold the venue truth in via
                                    // ordinary synthesized fills (see netting.rs module doc).
                                    tracing::info!(
                                        target: TARGET,
                                        %symbol, venue_size = size, venue_avg = avg_px,
                                        "netting re-anchor: folding venue basis via synthesized close+reopen legs"
                                    );
                                    if legs.into_iter().any(|leg| pump.blocking_send(leg).is_err())
                                    {
                                        break; // core gone — stop pumping
                                    }
                                }
                                Reanchor::SizeMismatch { local_size, venue_size } => {
                                    // Never auto-healed here — a missed/spurious fill is real
                                    // recon territory; keep it LOUD until fills explain it.
                                    tracing::warn!(
                                        target: TARGET,
                                        %symbol, local_size, venue_size,
                                        "sidecar position line disagrees with folded fills — NOT auto-healed"
                                    );
                                }
                                Reanchor::Baseline => {
                                    tracing::info!(
                                        target: TARGET,
                                        %symbol, venue_size = size, venue_avg = avg_px,
                                        "adopted venue position line as shadow baseline (no folded fills yet)"
                                    );
                                }
                                Reanchor::InSync => {}
                            }
                        }
                        None => {
                            let snippet: String = line.chars().take(160).collect();
                            tracing::debug!(target: TARGET, %snippet, "skipped unparseable stdout line");
                        }
                    }
                }
                // EOF: child exited (clean shutdown or death). Any order still awaiting
                // its terminal event would otherwise wedge non-terminal (a later failed
                // stdin write only covers NEW submits) — synthesize its rejection now.
                let orphans: Vec<String> = {
                    let mut set = inflight_reader.lock().unwrap();
                    set.drain().collect()
                };
                for coid in orphans {
                    tracing::warn!(target: TARGET, %coid, "bridge died with in-flight order — synthesizing OrderRejected");
                    let _ = pump.blocking_send(Event::OrderRejected(OrderRejected {
                        client_order_id: coid,
                        reason: "bridge died".into(),
                        ts: 0,
                    }));
                }
            })
            .expect("spawn reader thread");

        // Ok(ready) | Err(fatal) | RecvTimeout (hang) | Disconnected (silent death).
        match hs_rx.recv_timeout(READY_TIMEOUT) {
            Ok(Ok(_account)) => Ok(Self {
                child,
                stdin: Some(stdin),
                events,
                reader: Some(reader),
                inflight,
                venue_positions,
            }),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                Err(DukascopyError::Unavailable)
            }
        }
    }

    /// Build a [`DukascopyReconClient`] sharing this client's live venue-position snapshot (the
    /// recon-breadth seam). Unlike the REST venues' `recon_client(config, symbol)` factories, the
    /// Dukascopy reconcile client is DERIVED from the running exec client: there is no REST endpoint
    /// to connect to, so the venue's position truth arrives only through the sidecar stdio this
    /// client owns. See `recon_client.rs` for what it covers (positions) and what it defers
    /// (orders/fills — the Java/jar-gated query slice).
    pub fn recon_client(&self) -> DukascopyReconClient {
        DukascopyReconClient::new(Arc::clone(&self.venue_positions))
    }

    /// Write one command line; `false` marks the bridge dead (stdin dropped).
    fn write_line(&mut self, line: &str) -> bool {
        let Some(stdin) = self.stdin.as_mut() else { return false };
        let ok = writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_ok();
        if !ok {
            self.stdin = None;
        }
        ok
    }

    /// `shutdown` → ≤5s grace → kill → reap child + reader (idempotent).
    fn shutdown_child(&mut self) {
        let _ = self.write_line(&encode_line(&Command::Shutdown));
        self.stdin = None; // close the pipe: stdin EOF is the sidecar's fallback exit signal
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        // Bounded: the reader normally exits at pipe EOF, but it can be parked in
        // blocking_send on a stalled ingest channel — killing the child doesn't
        // unblock that. Wait briefly, then detach; the thread dies on its own when
        // the channel unblocks/closes (bounded teardown beats a guaranteed join).
        if let Some(handle) = self.reader.take() {
            let deadline = Instant::now() + SHUTDOWN_GRACE;
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

impl ExecutionClient for DukascopyExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        // Rust-side synchronous half of the lifecycle.
        let _ = self.events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        // Track BEFORE writing: if the child dies with this order in flight, the
        // reader's EOF drain synthesizes the terminal rejection.
        self.inflight.lock().unwrap().insert(request.client_order_id.clone());
        let line = encode_line(&Command::Submit { order: Box::new(request.clone()) });
        if !self.write_line(&line) {
            // Dead bridge: the order must still reach a terminal state (spec). Only
            // reject if the reader's EOF drain (or a real terminal event) hasn't
            // already taken this coid — belt and braces without double-rejecting.
            let still_inflight = self.inflight.lock().unwrap().remove(&request.client_order_id);
            if still_inflight {
                let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: "bridge unavailable".into(),
                    ts: request.ts,
                }));
            }
        }
    }

    fn cancel(&mut self, client_order_id: &str) {
        let line = encode_line(&Command::Cancel { client_order_id: client_order_id.into() });
        if !self.write_line(&line) {
            // Cancels are best-effort (spec): log, no synthetic event.
            tracing::warn!(target: TARGET, client_order_id, "cancel dropped — bridge unavailable");
        }
    }

    fn detach(&mut self) {
        self.shutdown_child();
    }
}

impl Drop for DukascopyExecutionClient {
    fn drop(&mut self) {
        self.shutdown_child(); // idempotent: try_wait returns Ok(Some) once reaped
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// The live gate reports the path it actually TRIED, which is the whole operator-facing point
    /// of resolving in the composition root: `spawn` no longer has an opinion about where a jar
    /// lives, so a wrong answer has to be visible as a wrong PATH rather than as a venue that went
    /// quiet. (`vendored_java_picks_highest_jdk_or_none` stood here until the sweep moved to
    /// `config.rs` beside the ladder it belongs to; `the_jre_sweep_picks_the_highest_image_or_falls_through`
    /// is its successor.)
    #[test]
    fn a_missing_jar_is_unavailable_rather_than_a_spawn() {
        let (tx, _rx) = vike_exec::event_channel(4);
        let tools = DukascopyTools {
            java: "java".into(),
            bridge_jar: PathBuf::from("Z:/no-such-project/bin/jforex/jforex-bridge.jar"),
        };
        let config =
            DukascopyConfig { login: "l".into(), password: "p".into(), server: String::new() };
        assert_eq!(
            DukascopyExecutionClient::spawn(config, &tools, tx).err(),
            Some(DukascopyError::Unavailable)
        );
    }
}
