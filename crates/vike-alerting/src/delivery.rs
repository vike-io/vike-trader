//! The delivery seam: where a [`FiredAlert`] goes. An [`AlertSink`] is a fire-and-forget consumer;
//! `AlertEngine` (feature `core`) broadcasts each fired alert to every registered sink, and each
//! sink SELF-FILTERS on the alert's `AlertTargets` (so routing lives in the alert, not the
//! dispatcher).
//!
//! Two sinks ship here:
//! - [`InProcessSink`] — an in-memory buffer the (future) GUI drains for toasts / OS notifications
//!   (the `vike-app` window — compile-checked but untested in CI — is the explicit follow-up).
//! - [`WebhookSink`] — an optional Telegram / generic-webhook POST over the existing `ureq` + rustls
//!   stack (no second HTTP/TLS crate). The blocking POST is done by an injected [`WebhookTransport`]
//!   so the seam is testable with a fake sender and never touches the network in CI.
//!
//! …and one DECORATOR, which is where this seam's only BOUND lives:
//! - [`QueuedSink`] — a bounded queue plus one delivery thread in front of a slow sink, so
//!   `deliver` becomes an enqueue and the caller's loop never waits on the wire. Register a
//!   wire-touching sink through it, not raw: without it the [`AlertSink`] contract's "must NEVER
//!   block the caller for long" is a request rather than a property, and a venue outage parked a
//!   whole daemon inside `AlertEngine::dispatch` for half an hour. The measurement, the overflow
//!   policy and what this deliberately does NOT do are on [`QueuedSink`]'s own doc.
//!
//! **Secrets** (`WebhookKind`'s bot token, a Discord/Slack webhook URL) never reach `Debug` (manual
//! redacting impl) and never reach a log: a failed [`WebhookSink`] delivery logs the target NAME and
//! a coarse error CATEGORY only — never the endpoint URL (a Telegram URL embeds the token) nor the
//! raw transport error (which can echo the request target). Built from a CALLER-SUPPLIED credential
//! map via [`webhook_configs_from_env`], the same absent-credentials-is-the-gate idiom the venues
//! use — this crate never opens the credential store itself, so nothing here can load a Telegram
//! token its caller did not hand it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::rule::AlertTargets;

/// One fired alert — the immutable output of the evaluator, handed to every [`AlertSink`]. Carries
/// the rule's delivery `targets` so the dispatcher needs no back-reference to the rule.
///
/// It lives HERE, not in the (feature-`core`) evaluator that produces it, because it is the one
/// type the crate's two halves share: the sinks below take it, so a vike-free consumer — the
/// standalone recorder watchdog this crate was split out for — must be able to build and deliver
/// one without `vike-core` in its graph.
#[derive(Debug, Clone, PartialEq)]
pub struct FiredAlert {
    /// the rule that fired (correlation id).
    pub rule_id: String,
    /// the rule's label (its `name`, or `id` when unnamed) — the message title.
    pub title: String,
    /// the specifics that fired it (numbers/context) — the message body.
    pub body: String,
    /// wall-clock ms of the fire (the `now_ms` the caller passed).
    pub ts_ms: i64,
    /// delivery targets, copied from the rule.
    pub targets: AlertTargets,
}

/// A fire-and-forget alert consumer. `deliver` must NEVER block the caller for long and must NEVER
/// panic (an alert delivery failure is logged, not propagated) — the engine calls it inline while
/// draining rules off the hot fold.
///
/// ⚠ **"NEVER block the caller for long" is a CONTRACT this trait cannot enforce, and
/// [`WebhookSink`] does not keep it.** That sink performs a BLOCKING HTTP POST whose only bound is
/// [`UreqTransport`]'s 10 s global timeout, `AlertEngine::dispatch` calls every sink inline and
/// serially for every fired alert, and nothing between the two batches or defers. Put a
/// wire-touching sink behind [`QueuedSink`] and the contract becomes a property of the type rather
/// than a hope about the endpoint.
pub trait AlertSink: Send + Sync {
    fn deliver(&self, alert: &FiredAlert);

    /// Would this sink do anything at all with `alert`? The ROUTING predicate, hoisted out of
    /// `deliver` so a decorator can ask before spending a resource on an alert the sink will
    /// discard — [`QueuedSink`] checks it before taking a queue slot, so a target that is not
    /// routed cannot crowd out one that is.
    ///
    /// The default is "yes", which is right for a sink that consumes everything (a log sink, a
    /// test tap). An implementation that filters MUST answer here and have `deliver` consult the
    /// same predicate rather than re-spelling it: two copies of a routing rule is exactly the
    /// duplication that lets a decorator and its inner sink disagree.
    fn accepts(&self, _alert: &FiredAlert) -> bool {
        true
    }
}

/// An in-memory buffer of fired alerts — the future toast / OS-notification seam. Cheap to clone
/// (an `Arc` handle to the shared inbox); the GUI holds one clone and [`drain`](Self::drain)s it on
/// repaint, the engine holds another as a boxed sink.
#[derive(Clone, Default)]
pub struct InProcessSink {
    inbox: Arc<Mutex<Vec<FiredAlert>>>,
}

impl InProcessSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// A shared handle onto the same inbox — hand this to the GUI so it drains what the engine's
    /// boxed sink writes.
    pub fn handle(&self) -> Arc<Mutex<Vec<FiredAlert>>> {
        self.inbox.clone()
    }

    /// Take + clear every buffered alert (the GUI's per-repaint drain).
    pub fn drain(&self) -> Vec<FiredAlert> {
        std::mem::take(&mut *self.inbox.lock().unwrap())
    }

    /// How many alerts are currently buffered (undelivered).
    pub fn len(&self) -> usize {
        self.inbox.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl AlertSink for InProcessSink {
    fn deliver(&self, alert: &FiredAlert) {
        if self.accepts(alert) {
            self.inbox.lock().unwrap().push(alert.clone());
        }
    }

    /// The routing rule, spelled ONCE: `deliver` above consults it, and so does a [`QueuedSink`]
    /// wrapping this sink.
    fn accepts(&self, alert: &FiredAlert) -> bool {
        alert.targets.in_process
    }
}

/// The blocking HTTP POST a [`WebhookSink`] performs, abstracted so the delivery seam is testable
/// with a fake sender. Implementations MUST keep any secret embedded in `url` (a Telegram token, a
/// Discord/Slack path secret) out of their return value and their logs.
pub trait WebhookTransport: Send + Sync {
    /// POST `body` (a JSON string) to `url`. Return `Ok(())` on a 2xx, `Err(category)` otherwise —
    /// where `category` is a NON-secret coarse label (never the url, never the raw request).
    fn post_json(&self, url: &str, body: &str) -> Result<(), String>;
}

/// The real transport: a blocking `ureq` agent (rustls, no OpenSSL — the one workspace HTTP stack).
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .user_agent("vike-trader-rust")
            .build()
            .new_agent();
        UreqTransport { agent }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookTransport for UreqTransport {
    fn post_json(&self, url: &str, body: &str) -> Result<(), String> {
        match self.agent.post(url).header("content-type", "application/json").send(body.as_bytes())
        {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let _ = resp.body_mut().read_to_string(); // drain so the connection can be reused
                if (200..300).contains(&status) {
                    Ok(())
                } else {
                    // a bare HTTP status carries no secret — safe to surface.
                    Err(format!("http {status}"))
                }
            }
            // Deliberately opaque: a transport error's Display can echo the request target (the
            // token-bearing url), so we NEVER interpolate it or the url here.
            Err(_) => Err("transport error".to_string()),
        }
    }
}

/// What a webhook target IS + where it points. Holds the secret (Telegram bot token / webhook url),
/// so its [`Debug`] is MANUAL and redacting (see the impl below) and it is NEVER serialized — it is
/// built at runtime from the `.env`, not persisted alongside the rules.
#[derive(Clone)]
pub enum WebhookKind {
    /// Telegram Bot API: `POST https://api.telegram.org/bot<token>/sendMessage` with
    /// `{chat_id, text}`. `token` is the secret; `chat_id` is a plain conversation id.
    Telegram { token: String, chat_id: String },
    /// A generic JSON webhook (Discord / Slack / custom): `POST {title, text, rule}` to `url`.
    /// The URL itself is treated as a SECRET (Discord/Slack webhook URLs embed a token in the path).
    Generic { url: String },
}

impl std::fmt::Debug for WebhookKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // token redacted; chat_id is not a credential (kept for diagnostics).
            WebhookKind::Telegram { chat_id, .. } => f
                .debug_struct("Telegram")
                .field("token", &"<redacted>")
                .field("chat_id", chat_id)
                .finish(),
            // the whole URL is redacted (it can carry a path secret).
            WebhookKind::Generic { .. } => {
                f.debug_struct("Generic").field("url", &"<redacted>").finish()
            }
        }
    }
}

/// One named webhook target a rule references by [`name`](Self::name) in
/// `AlertTargets::webhooks`. `Debug` is derived but redacts through [`WebhookKind`]'s manual impl.
#[derive(Clone, Debug)]
pub struct WebhookConfig {
    /// the target name rules deliver to (e.g. `"telegram"`, `"webhook"`).
    pub name: String,
    pub kind: WebhookKind,
}

/// A webhook [`AlertSink`]: POSTs a fired alert to its configured target when (and only when) the
/// alert's `AlertTargets::webhooks` name it. Generic over the [`WebhookTransport`] so tests inject
/// a fake sender.
pub struct WebhookSink<T: WebhookTransport> {
    cfg: WebhookConfig,
    transport: T,
}

impl<T: WebhookTransport> WebhookSink<T> {
    pub fn new(cfg: WebhookConfig, transport: T) -> Self {
        WebhookSink { cfg, transport }
    }

    /// The `(url, json_body)` this alert would POST — the token-bearing endpoint. PRIVATE and used
    /// only inside [`deliver`](Self::deliver); never logged.
    fn endpoint_and_body(&self, alert: &FiredAlert) -> (String, String) {
        match &self.cfg.kind {
            WebhookKind::Telegram { token, chat_id } => {
                let url = format!("https://api.telegram.org/bot{token}/sendMessage");
                let text = format!("{}\n{}", alert.title, alert.body);
                let body = serde_json::json!({ "chat_id": chat_id, "text": text }).to_string();
                (url, body)
            }
            WebhookKind::Generic { url } => {
                let body = serde_json::json!({
                    "title": alert.title,
                    "text": alert.body,
                    "rule": alert.rule_id,
                })
                .to_string();
                (url.clone(), body)
            }
        }
    }
}

impl<T: WebhookTransport> AlertSink for WebhookSink<T> {
    fn deliver(&self, alert: &FiredAlert) {
        if !self.accepts(alert) {
            return; // this alert is not routed to this webhook target
        }
        let (url, body) = self.endpoint_and_body(alert);
        if let Err(category) = self.transport.post_json(&url, &body) {
            // Log the target NAME + coarse category only — NEVER the url (Telegram token) or the
            // raw transport error.
            tracing::warn!(
                target_name = %self.cfg.name,
                rule = %alert.rule_id,
                error = %category,
                "alert webhook delivery failed"
            );
        }
    }

    /// Which alerts this target is named by — the routing rule, spelled ONCE (see
    /// [`AlertSink::accepts`]). A [`QueuedSink`] in front of this sink asks it before taking a
    /// queue slot, so a Telegram outage cannot consume the Discord target's queue.
    fn accepts(&self, alert: &FiredAlert) -> bool {
        alert.targets.webhooks.iter().any(|w| w == &self.cfg.name)
    }
}

/// Build the available webhook targets from a caller-supplied credential map — the ONLY entry
/// point, and `Layer::Injected` by construction (the same `&HashMap<String, String>` shape
/// `vike_ops::reconcile_config` and every venue `config.rs` loader take). A target is built ONLY
/// when its credentials are present, so an operator who configures nothing gets an empty list and
/// every webhook-targeted rule delivers nowhere — byte-identical to the alerting engine being off.
///
/// There is deliberately NO convenience twin that opens the credential store itself. One existed
/// (`webhook_configs_from_workspace_env`, behind a `workspace-env` feature) and was the whole
/// reason this crate carried an optional `vike-bridge-core` edge; its single caller was
/// `vike-tradehub`'s `main.rs`, which already sweeps the process env and resolves the full
/// credential chain for the live mount. Deleting it moved the read to that binary — where the rule
/// puts it — and removed the crate's last non-`core` vike-\* dependency.
///
/// Keys (the workspace credential store):
/// - `VIKE_ALERT_TELEGRAM_TOKEN` + `VIKE_ALERT_TELEGRAM_CHAT_ID` ⇒ a `"telegram"` target.
/// - `VIKE_ALERT_WEBHOOK_URL` ⇒ a `"webhook"` (generic) target.
pub fn webhook_configs_from_env(env: &HashMap<String, String>) -> Vec<WebhookConfig> {
    let get = |k: &str| -> Option<String> {
        let v = env.get(k)?.trim();
        (!v.is_empty()).then(|| v.to_string())
    };
    let mut out = Vec::new();
    if let (Some(token), Some(chat_id)) =
        (get("VIKE_ALERT_TELEGRAM_TOKEN"), get("VIKE_ALERT_TELEGRAM_CHAT_ID"))
    {
        out.push(WebhookConfig {
            name: "telegram".to_string(),
            kind: WebhookKind::Telegram { token, chat_id },
        });
    }
    if let Some(url) = get("VIKE_ALERT_WEBHOOK_URL") {
        out.push(WebhookConfig { name: "webhook".to_string(), kind: WebhookKind::Generic { url } });
    }
    out
}

/// Build boxed [`WebhookSink`]s (real `ureq` transport) from a set of [`WebhookConfig`]s — the
/// RAW registration of every configured webhook target on an engine.
///
/// ⚠ **These sinks BLOCK the caller for as long as the endpoint takes**, up to
/// [`UreqTransport`]'s 10 s global timeout, once per alert per target, inline in
/// `AlertEngine::dispatch`. The one caller left on this twin is vike-tradehub's summary thread
/// (`crates/vike-tradehub/src/alerts.rs`'s `maybe_mount`), and it survives here only because that
/// thread is DEDICATED — its dispatch parks nothing but itself — and its join is already a bounded,
/// COUNTED task inside the daemon's teardown deadline, so a stall there costs a share of a budget
/// rather than a stop flag going unread. It is NOT acceptable on a loop that also polls a stop
/// flag, which is what [`queued_ureq_webhook_sinks`] is for. Reach for that one unless you can
/// write down, as tradehub's call site does, why this one is safe where you are calling it.
pub fn ureq_webhook_sinks(configs: Vec<WebhookConfig>) -> Vec<Box<dyn AlertSink>> {
    configs
        .into_iter()
        .map(|cfg| Box::new(WebhookSink::new(cfg, UreqTransport::new())) as Box<dyn AlertSink>)
        .collect()
}

/// The BOUNDED twin of [`ureq_webhook_sinks`]: every configured webhook target as a
/// [`QueuedSink`], plus the stop handle its owner must shut down.
///
/// This is what a caller on a LOOP wants — a daemon tick, a repaint, anything that also has to
/// observe a stop flag — because the sinks it returns cannot park that loop on the wire. The plain
/// twin above stays for a caller that has already decided a blocking POST is acceptable where it
/// stands, and its own doc now says which is which.
///
/// The two `Vec`s are positionally paired by target and both preserve `configs`' order, so a
/// caller that logs an outcome can name the target from either.
pub fn queued_ureq_webhook_sinks(
    configs: Vec<WebhookConfig>,
    capacity: usize,
) -> (Vec<Box<dyn AlertSink>>, Vec<QueuedSinkStop>) {
    let mut sinks: Vec<Box<dyn AlertSink>> = Vec::with_capacity(configs.len());
    let mut stops = Vec::with_capacity(configs.len());
    for cfg in configs {
        let name = cfg.name.clone();
        let inner: Box<dyn AlertSink> = Box::new(WebhookSink::new(cfg, UreqTransport::new()));
        let (sink, stop) = QueuedSink::spawn(name, capacity, inner);
        sinks.push(Box::new(sink));
        stops.push(stop);
    }
    (sinks, stops)
}

/// How deep a [`QueuedSink`]'s queue is when the caller does not choose
/// ([`queued_ureq_webhook_sinks`] passes it).
///
/// **Sized against the worst episode the producer can raise in one tick, not against a guess.** The
/// recorder dispatches ONE alert per silent series per tick
/// (`crates/vike-recorder/src/alerts.rs`'s `RecorderAlerts::on_silence`), gated per series rather
/// than per rule, and its series set is one entry per (symbol, stream) — it scales with a profile's
/// family glob, so a venue-wide outage raises the whole set at once. The arithmetic that produced
/// this fix used a hundred series (100 x 2 targets x 10 s ≈ 33 minutes parked); the the CI box profile
/// that motivated the watchdog subscribes far fewer, and no larger episode has been MEASURED — this
/// number is sized against the arithmetic, not against a tape. 128 swallows a hundred-series first
/// tick whole and still bounds the memory at a few hundred KiB of `FiredAlert`.
///
/// It is a BOUND, not a promise: past it alerts are dropped, counted and logged — see
/// [`QueuedSink`]'s overflow policy.
pub const DEFAULT_QUEUE_CAPACITY: usize = 128;

/// How long a delivery worker waits for its next alert before re-reading its stop flag. It is the
/// CEILING on how long [`QueuedSinkStop::shutdown`] takes when the worker is IDLE — not
/// microseconds: an idle worker is parked inside `recv_timeout` and re-reads the flag only when
/// that wait returns, so a stop raised while it sleeps costs up to one full poll, and an owner that
/// stops several targets one after another pays up to one poll EACH. Small for that reason; an idle
/// worker costs one wakeup per tenth of a second, which is nothing beside the thread.
///
/// Public so an owner can size its stop budget from this constant rather than from a restated
/// number; `crates/vike-recorder/src/alerts.rs`'s `stop_delivery` is the one that does.
pub const STOP_POLL: Duration = Duration::from_millis(100);

/// How often [`QueuedSinkStop::shutdown`] re-asks whether the worker has finished. `JoinHandle`
/// has no join-with-timeout, so the wait is a poll — see that method for why it must be bounded.
const JOIN_POLL: Duration = Duration::from_millis(5);

/// **A bounded queue and one delivery thread in front of a slow sink** — the decorator that makes
/// the [`AlertSink`] contract's "must NEVER block the caller for long" true of a sink that POSTs.
///
/// ## The defect this exists to remove
///
/// `AlertEngine::dispatch` is a nested SERIAL loop — every fired alert, times every sink, inline on
/// the caller's thread — and [`WebhookSink`]'s `deliver` performs a blocking HTTP POST whose only
/// bound is [`UreqTransport`]'s 10 s global timeout. Nothing spawned a thread, nothing batched, and
/// there was no queue between the caller and the wire. On the recorder that caller is the single
/// `loop { rt.tick(..) … }` in `crates/vike-datahub/src/recorder.rs`'s `run` — the same loop
/// that polls the stop flag — and its producer raises one alert per SILENT SERIES, so the first
/// tick of a venue outage makes every silent series due at once: 100 series x 2 webhook targets x
/// 10 s is ~2000 s, i.e. **~33 minutes parked inside alert delivery, observing no stop flag**. A
/// `systemctl stop` in that window reaches its `TimeoutStopSec=` and SIGKILL lands on the writer.
///
/// ## Why a BOUND, and why not the other two fixes
///
/// The root cause is a missing bound, not missing parallelism, and the three candidates were
/// weighed in that light:
///
/// * **A cap on dispatches per tick** bounds the loop but not the STALL — one alert x one dead
///   endpoint is still 10 s on the caller's thread, and the deferred remainder needs a queue to be
///   deferred INTO, which is this type with worse ergonomics.
/// * **Coalescing N silent series into one incident message** is a real improvement for the human
///   on the pager, and it is NOT this crate's decision to make: the per-series repeat gate lives in
///   `crates/vike-recorder/src/liveness.rs`'s `SilenceWatch::alertable`, the prefix scope lives in
///   the RULE, and `crates/vike-recorder/src/alerts.rs`'s module doc argues at length that six
///   silent series must read as six rather than as one. It also does not bound anything — one alert
///   x two targets x 10 s is 20 s of a stop flag going unread, which is a smaller number, not a
///   bound.
/// * **This**: make `deliver` an enqueue. The caller's cost becomes a `try_send` (a lock, a move, a
///   wakeup) whatever the endpoint does, which is the property a stop flag needs. Coalescing
///   remains open ON TOP of it, upstream, where the incident is actually known.
///
/// ## The overflow policy — LOUD, and it drops the NEWEST
///
/// The queue is `sync_channel(capacity)` and `deliver` uses `try_send`, so a full queue drops the
/// arriving alert rather than blocking (blocking would hand the stall straight back). Every drop
/// increments [`dropped`](Self::dropped) and emits a `tracing::warn!` naming the target and the
/// rule — a dropped alert is a safety-relevant loss in a watchdog whose whole subject is a failure
/// that is silent by nature, so it may be dropped but must never be dropped QUIETLY.
///
/// Newest-first is the deliberate choice: within one incident the FIRST alerts are the ones that
/// page a human, and the queue fills only when the endpoint is already failing to keep up, i.e.
/// when the later alerts are same-incident repeats. Dropping the oldest instead would let a long
/// outage push the alert that STARTED it out of the queue unsent.
///
/// "Every drop is counted" is a property of the TYPE, not of the worker being polite about it: the
/// queue carries `Envelope`s (a private wrapper — the alert plus the drop counter it charges
/// itself to), and an envelope that is destroyed still holding its alert counts
/// itself. So the alerts a stopping worker leaves in the buffer are counted when the receiver is
/// dropped — including one a concurrent `deliver` slipped in AFTER the worker's last look at the
/// queue and BEFORE that drop, the window an explicit drain-then-drop would lose in silence. A
/// producer that arrives after the drop gets `Disconnected` and counts its own.
///
/// A [`QueuedSink`] never queues an alert its inner sink would discard: it consults
/// [`AlertSink::accepts`] first, so a Telegram target's queue is never filled with alerts routed
/// only to Discord.
///
/// ## What it deliberately does NOT do
///
/// * It does **not** retry. A failed POST is the inner sink's business and stays logged-not-retried;
///   a retry queue in front of a pager is how a five-minute outage becomes a thousand duplicate
///   pages an hour later.
/// * It does **not** deduplicate or coalesce. See above — that decision belongs upstream, where the
///   incident is known.
/// * It does **not** preserve ordering ACROSS sinks. Each wrapped sink has its own thread, on
///   purpose: a stalled Telegram must not delay a healthy Discord. Ordering WITHIN one sink is FIFO.
/// * It does **not** guarantee delivery of anything still queued at shutdown — see
///   [`QueuedSinkStop::shutdown`], which bounds the wait and counts what it abandons.
pub struct QueuedSink {
    /// the target name, for diagnostics only (drops, shutdown lines, the thread's name).
    name: String,
    /// kept beside the worker's own clone SOLELY to answer [`AlertSink::accepts`] on the caller's
    /// thread — this handle never delivers.
    inner: Arc<dyn AlertSink>,
    /// A bare `SyncSender`, no lock: `try_send` takes `&self` and the sender is `Sync`, so
    /// [`AlertSink::deliver`]'s `&self` sends directly. An earlier draft wrapped it in a `Mutex`
    /// "so `&self` can send", which was a false reason paid for on the caller's thread — the one
    /// thread this type exists to keep cheap.
    tx: SyncSender<Envelope>,
    dropped: Arc<AtomicU64>,
}

/// What actually travels through a [`QueuedSink`]'s channel: the alert plus the drop counter it
/// charges itself to if it is destroyed still holding the alert.
///
/// This is how "every drop is counted" survives the one path a worker cannot see: the receiver's
/// own `Drop` destroys whatever is buffered, and an alert can be buffered between the worker's
/// last `try_recv` and that drop (a concurrent `deliver` whose `try_send` succeeded into the
/// freed slot). A count taken by the worker would miss it; a count taken by the ENVELOPE cannot.
/// The worker calls [`Envelope::disarm`] after the inner sink returns — after, not before, so an
/// inner sink that breaks the never-panic contract drops its envelope during the unwind and the
/// alert it was holding is counted rather than vanishing with the thread.
struct Envelope {
    alert: Option<FiredAlert>,
    dropped: Arc<AtomicU64>,
}

impl Envelope {
    /// The alert, for delivery. `None` only after [`disarm`](Self::disarm), which the worker calls
    /// once and only once.
    fn alert(&self) -> Option<&FiredAlert> {
        self.alert.as_ref()
    }

    /// Delivered: this envelope no longer counts itself.
    fn disarm(&mut self) {
        self.alert = None;
    }
}

impl Drop for Envelope {
    fn drop(&mut self) {
        if self.alert.is_some() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl QueuedSink {
    /// Wrap `inner` in a bounded queue served by one named thread, returning the sink to register
    /// and the [`QueuedSinkStop`] its owner must shut down.
    ///
    /// `capacity` is clamped UP to 1: `sync_channel(0)` is a rendezvous channel, where `try_send`
    /// succeeds only if the worker is already parked in `recv` — that would drop almost every alert
    /// while looking like a queue.
    ///
    /// # Panics
    /// If the OS refuses a thread. That is a process-is-doomed condition at mount time, and the
    /// alternative — returning a `Result` the caller degrades through — would need this type to
    /// hold a synchronous fallback, i.e. to keep the very code path the queue exists to remove.
    pub fn spawn(
        name: impl Into<String>,
        capacity: usize,
        inner: Box<dyn AlertSink>,
    ) -> (Self, QueuedSinkStop) {
        let name = name.into();
        let inner: Arc<dyn AlertSink> = Arc::from(inner);
        let (tx, rx) = sync_channel::<Envelope>(capacity.max(1));
        let stopping = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));
        let delivered = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));

        let worker = std::thread::Builder::new()
            .name(format!("vike-alert-{name}"))
            .spawn({
                let (worker_name, worker_inner) = (name.clone(), inner.clone());
                let (stopping, busy, delivered, dropped) =
                    (stopping.clone(), busy.clone(), delivered.clone(), dropped.clone());
                move || {
                    drain(&worker_name, worker_inner, rx, &stopping, &busy, &delivered, &dropped);
                }
            })
            .expect("spawning an alert delivery thread");

        let sink = QueuedSink { name: name.clone(), inner, tx, dropped: dropped.clone() };
        let stop = QueuedSinkStop { name, stopping, busy, worker, delivered, dropped };
        (sink, stop)
    }

    /// How many alerts this sink has DROPPED rather than handed to its worker (a full queue, or a
    /// worker that died). Never resets — it is a session total.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// One drop, counted and said out loud. Never the alert BODY: the body is already in the log
    /// through the producer's own record, and a drop line is about the delivery, not the incident.
    fn record_drop(&self, alert: &FiredAlert, reason: &'static str) {
        let total = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::warn!(
            target_name = %self.name,
            rule = %alert.rule_id,
            reason,
            dropped_total = total,
            "alert DROPPED — the delivery queue could not take it, so this alert was never sent"
        );
    }
}

impl AlertSink for QueuedSink {
    /// Enqueue and return. The only work on the caller's thread is a routing check, a clone and a
    /// `try_send` — no wire, no wait, whatever the endpoint is doing.
    fn deliver(&self, alert: &FiredAlert) {
        if !self.accepts(alert) {
            return;
        }
        let envelope = Envelope { alert: Some(alert.clone()), dropped: self.dropped.clone() };
        // A refused send hands the envelope back inside the error. It is DISARMED before it goes
        // out of scope so the drop is counted exactly once, by `record_drop`, which also says it.
        let (mut envelope, reason) = match self.tx.try_send(envelope) {
            Ok(()) => return,
            Err(TrySendError::Full(e)) => (e, "queue full"),
            // The worker panicked (its inner sink broke the never-panic contract) or was already
            // joined. Either way this is not recoverable here and must not be silent.
            Err(TrySendError::Disconnected(e)) => (e, "delivery worker gone"),
        };
        envelope.disarm();
        self.record_drop(alert, reason);
    }

    /// Delegated, so the decorator and the sink it wraps can never disagree about routing.
    fn accepts(&self, alert: &FiredAlert) -> bool {
        self.inner.accepts(alert)
    }
}

/// The worker body: deliver until told to stop, then let the queue count what is left.
///
/// The stop flag is read BEFORE each `recv`, and `recv_timeout` bounds how long an idle worker can
/// sleep through one — so a shutdown costs at most [`STOP_POLL`] plus whatever a delivery already in
/// flight still needs. That in-flight delivery is the residual [`QueuedSinkStop::shutdown`] bounds,
/// and `busy` is how it tells the two apart when it gives up.
fn drain(
    name: &str,
    inner: Arc<dyn AlertSink>,
    rx: Receiver<Envelope>,
    stopping: &AtomicBool,
    busy: &AtomicBool,
    delivered: &AtomicU64,
    dropped: &AtomicU64,
) {
    while !stopping.load(Ordering::Relaxed) {
        match rx.recv_timeout(STOP_POLL) {
            Ok(mut envelope) => {
                if let Some(alert) = envelope.alert() {
                    busy.store(true, Ordering::Relaxed);
                    inner.deliver(alert);
                    busy.store(false, Ordering::Relaxed);
                }
                // AFTER the sink returned: a sink that panics drops an ARMED envelope on the way
                // out, so the alert it was holding is counted rather than lost with the thread.
                envelope.disarm();
                delivered.fetch_add(1, Ordering::Relaxed);
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Every sender is gone: the engine holding this sink was dropped without a shutdown.
            // Nothing can arrive any more, so exiting is the whole of it.
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
    // Stopping. Whatever is still queued will NOT be sent, and that is a loss the shutdown line
    // must state rather than let a reader infer it from a delivered count. The count is NOT taken
    // by draining the queue here — that leaves a window between the last look and the receiver's
    // drop in which a concurrent `deliver` can still land one, and it would be destroyed uncounted.
    // Dropping the receiver disconnects the channel and destroys every buffered [`Envelope`], each
    // of which counts itself, so the delta across the drop IS the abandoned set, late arrivals
    // included; a producer arriving after it gets `Disconnected` and counts its own. (Such a
    // producer's own count can land inside this delta and inflate THIS line by one; the session
    // total an outcome reports is exact either way, because both charge the same counter once.)
    let before = dropped.load(Ordering::Relaxed);
    drop(rx);
    let abandoned = dropped.load(Ordering::Relaxed) - before;
    if abandoned > 0 {
        tracing::warn!(
            target_name = %name,
            abandoned,
            "alert delivery stopped with alerts still queued — they were never sent"
        );
    }
}

/// What a [`QueuedSinkStop::shutdown`] did. Returned rather than logged so the CALLER decides the
/// wording and the level: this crate has no idea whether an abandoned pager matters more or less
/// than whatever else its owner is tearing down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueuedSinkOutcome {
    /// The worker finished and was joined. `dropped` is the session total, including anything still
    /// queued when the stop was raised.
    Joined { name: String, delivered: u64, dropped: u64 },
    /// The worker did not finish inside `budget` and the thread was left running; the process is
    /// expected to exit shortly. `dropped` is the total as of the give-up instant; the worker may
    /// still increment it afterwards.
    ///
    /// `mid_delivery` says WHAT was abandoned, because the two cases are different losses and the
    /// owner's disclosure must not conflate them: `true` means the worker was inside its inner
    /// sink's `deliver` at the give-up instant — an alert on the wire that may never arrive;
    /// `false` means it was IDLE, parked in its poll and merely not yet joined, because the budget
    /// it was handed was shorter than one [`STOP_POLL`] — nothing was in flight, and the thread
    /// exits on its own within one poll. An owner that stops several targets against one shared
    /// deadline hits the second case whenever an earlier target ate the deadline, and it must not
    /// then report a page that was never being sent as one that may not have arrived.
    Abandoned { name: String, delivered: u64, dropped: u64, budget: Duration, mid_delivery: bool },
}

impl QueuedSinkOutcome {
    /// The target this outcome is about.
    pub fn name(&self) -> &str {
        match self {
            QueuedSinkOutcome::Joined { name, .. } | QueuedSinkOutcome::Abandoned { name, .. } => {
                name
            }
        }
    }

    /// Alerts this target actually handed to its inner sink.
    pub fn delivered(&self) -> u64 {
        match self {
            QueuedSinkOutcome::Joined { delivered, .. }
            | QueuedSinkOutcome::Abandoned { delivered, .. } => *delivered,
        }
    }

    /// Alerts this target never sent — a full queue, a dead worker, or a queue abandoned at stop.
    pub fn dropped(&self) -> u64 {
        match self {
            QueuedSinkOutcome::Joined { dropped, .. }
            | QueuedSinkOutcome::Abandoned { dropped, .. } => *dropped,
        }
    }
}

/// The other half of [`QueuedSink::spawn`]: the handle that stops the worker. Held by whoever owns
/// the teardown, NOT by the engine — the engine owns `Box<dyn AlertSink>`es and has no shutdown of
/// its own to hang this on.
///
/// Dropping it without calling [`shutdown`](Self::shutdown) does not leak the thread forever: the
/// worker also exits when every sender is gone, i.e. when the engine holding the sink is dropped.
/// It does mean nothing is joined and nothing is REPORTED, which is why the owner should call it.
pub struct QueuedSinkStop {
    name: String,
    stopping: Arc<AtomicBool>,
    /// set by the worker around its inner `deliver`, read once at the give-up instant — see
    /// [`QueuedSinkOutcome::Abandoned`]'s `mid_delivery`.
    busy: Arc<AtomicBool>,
    worker: JoinHandle<()>,
    delivered: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

impl QueuedSinkStop {
    /// The target this handle stops.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Raise the stop flag and wait up to `budget` for the worker to finish, joining it if it does.
    ///
    /// ⚠ **The wait is BOUNDED and the fallback is to ABANDON the thread**, which is deliberate and
    /// is the one place this type trades a guarantee away. `JoinHandle` offers no join-with-timeout,
    /// and a worker caught mid-POST cannot be interrupted — a plain `join()` would therefore add up
    /// to [`UreqTransport`]'s 10 s to its owner's teardown, and on the recorder that teardown is
    /// already sized against a systemd `TimeoutStopSec=`: a graceful stop that outruns that budget
    /// is SIGKILLed halfway, which loses buffered rows. So the caller names a budget, and past it
    /// the outcome says the thread was left running.
    ///
    /// Abandoning costs at most one un-POSTed page and leaks nothing that survives the process: the
    /// worker holds no lock, no file and no store handle — only a socket the OS closes on exit.
    /// That is the correct thing to lose, and it is REPORTED rather than assumed — and reported
    /// PRECISELY: the outcome carries whether the worker was actually inside a delivery at the
    /// give-up instant, because a `budget` shorter than one [`STOP_POLL`] abandons an IDLE worker
    /// too (it cannot have woken to see the flag yet), and that is a thread left to exit on its
    /// own, not a page left on the wire.
    ///
    /// What the wait costs in the NORMAL case — nothing queued, nothing in flight — is up to one
    /// [`STOP_POLL`], not microseconds: the worker re-reads the flag only when its `recv_timeout`
    /// returns. A caller stopping several handles one after another pays up to one poll each.
    pub fn shutdown(self, budget: Duration) -> QueuedSinkOutcome {
        self.stopping.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + budget;
        while !self.worker.is_finished() {
            if Instant::now() >= deadline {
                return QueuedSinkOutcome::Abandoned {
                    name: self.name,
                    delivered: self.delivered.load(Ordering::Relaxed),
                    dropped: self.dropped.load(Ordering::Relaxed),
                    budget,
                    mid_delivery: self.busy.load(Ordering::Relaxed),
                };
            }
            std::thread::sleep(JOIN_POLL);
        }
        // `is_finished` is true, so this join returns at once. A panicked worker is swallowed: it
        // was already reported through the disconnected-sender drops, and a teardown may not unwind
        // on one.
        let _ = self.worker.join();
        QueuedSinkOutcome::Joined {
            name: self.name,
            delivered: self.delivered.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::AlertTargets;

    fn alert(webhooks: &[&str], in_process: bool) -> FiredAlert {
        FiredAlert {
            rule_id: "r1".into(),
            title: "BTC breakout".into(),
            body: "binance BTCUSDT crossed above 100000".into(),
            ts_ms: 42,
            targets: AlertTargets {
                in_process,
                webhooks: webhooks.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    /// A transport double that records every POST (never touches the network) and can be told to
    /// fail, to exercise the swallow-the-error path.
    #[derive(Clone, Default)]
    struct FakeTransport {
        calls: Arc<Mutex<Vec<(String, String)>>>,
        fail: bool,
    }

    impl WebhookTransport for FakeTransport {
        fn post_json(&self, url: &str, body: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push((url.to_string(), body.to_string()));
            if self.fail { Err("boom".to_string()) } else { Ok(()) }
        }
    }

    #[test]
    fn in_process_sink_buffers_only_in_process_targeted_alerts() {
        let sink = InProcessSink::new();
        sink.deliver(&alert(&[], true)); // in-process on → buffered
        sink.deliver(&alert(&["telegram"], false)); // in-process off → NOT buffered
        assert_eq!(sink.len(), 1);
        let drained = sink.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].title, "BTC breakout");
        assert!(sink.is_empty(), "drain clears the buffer");
    }

    #[test]
    fn in_process_handle_shares_the_same_inbox() {
        let sink = InProcessSink::new();
        let handle = sink.handle();
        sink.deliver(&alert(&[], true));
        assert_eq!(handle.lock().unwrap().len(), 1, "the GUI-side handle sees the engine's writes");
    }

    #[test]
    fn webhook_sink_posts_a_telegram_message_for_a_matching_target_only() {
        let fake = FakeTransport::default();
        let sink = WebhookSink::new(
            WebhookConfig {
                name: "telegram".into(),
                kind: WebhookKind::Telegram { token: "123:ABC".into(), chat_id: "9001".into() },
            },
            fake.clone(),
        );
        // routed to "telegram" → one POST.
        sink.deliver(&alert(&["telegram"], true));
        // NOT routed to "telegram" (different name) → no POST.
        sink.deliver(&alert(&["webhook"], true));
        // in-process-only alert → no POST.
        sink.deliver(&alert(&[], true));

        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "exactly the one matching-target alert POSTs");
        let (url, body) = &calls[0];
        assert_eq!(url, "https://api.telegram.org/bot123:ABC/sendMessage");
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["chat_id"], "9001");
        assert!(v["text"].as_str().unwrap().contains("BTC breakout"));
        assert!(v["text"].as_str().unwrap().contains("crossed above 100000"));
    }

    #[test]
    fn webhook_sink_posts_a_generic_json_body() {
        let fake = FakeTransport::default();
        let sink = WebhookSink::new(
            WebhookConfig {
                name: "webhook".into(),
                kind: WebhookKind::Generic { url: "https://example.test/hook/secret".into() },
            },
            fake.clone(),
        );
        sink.deliver(&alert(&["webhook"], false));
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (url, body) = &calls[0];
        assert_eq!(url, "https://example.test/hook/secret");
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["rule"], "r1");
        assert_eq!(v["title"], "BTC breakout");
    }

    #[test]
    fn webhook_delivery_failure_is_swallowed_not_propagated() {
        // A failing transport must not panic or propagate — deliver returns normally (the error is
        // logged with the target name + coarse category, never the url).
        let fake = FakeTransport { calls: Arc::new(Mutex::new(Vec::new())), fail: true };
        let sink = WebhookSink::new(
            WebhookConfig {
                name: "telegram".into(),
                kind: WebhookKind::Telegram { token: "t".into(), chat_id: "c".into() },
            },
            fake.clone(),
        );
        sink.deliver(&alert(&["telegram"], true)); // must not panic
        assert_eq!(fake.calls.lock().unwrap().len(), 1, "the POST was attempted");
    }

    #[test]
    fn webhook_config_debug_redacts_the_secret() {
        let tg = WebhookConfig {
            name: "telegram".into(),
            kind: WebhookKind::Telegram {
                token: "123456:SUPER-SECRET-TOKEN".into(),
                chat_id: "9001".into(),
            },
        };
        let dbg = format!("{tg:?}");
        assert!(!dbg.contains("SUPER-SECRET-TOKEN"), "token must never reach Debug: {dbg}");
        assert!(!dbg.contains("123456:SUPER-SECRET-TOKEN"));
        assert!(dbg.contains("<redacted>"));
        assert!(dbg.contains("9001"), "the non-secret chat_id may show for diagnostics");

        let generic = WebhookConfig {
            name: "webhook".into(),
            kind: WebhookKind::Generic {
                url: "https://discord.com/api/webhooks/111/AAA-secret-BBB".into(),
            },
        };
        let dbg = format!("{generic:?}");
        assert!(
            !dbg.contains("secret"),
            "a webhook URL is a secret and must not reach Debug: {dbg}"
        );
        assert!(!dbg.contains("discord.com"));
    }

    #[test]
    fn webhook_configs_from_env_builds_only_present_targets() {
        // empty env → no targets (byte-identical to alerting off).
        assert!(webhook_configs_from_env(&HashMap::new()).is_empty());

        // telegram needs BOTH token and chat_id — token alone yields nothing.
        let mut partial = HashMap::new();
        partial.insert("VIKE_ALERT_TELEGRAM_TOKEN".to_string(), "t".to_string());
        assert!(
            webhook_configs_from_env(&partial).is_empty(),
            "an incomplete telegram config builds no target"
        );

        let mut full = HashMap::new();
        full.insert("VIKE_ALERT_TELEGRAM_TOKEN".to_string(), "123:ABC".to_string());
        full.insert("VIKE_ALERT_TELEGRAM_CHAT_ID".to_string(), "9001".to_string());
        full.insert("VIKE_ALERT_WEBHOOK_URL".to_string(), "https://example.test/h".to_string());
        let cfgs = webhook_configs_from_env(&full);
        assert_eq!(cfgs.len(), 2);
        assert_eq!(cfgs[0].name, "telegram");
        assert!(matches!(cfgs[0].kind, WebhookKind::Telegram { .. }));
        assert_eq!(cfgs[1].name, "webhook");
        assert!(matches!(cfgs[1].kind, WebhookKind::Generic { .. }));
        // and even the built config never leaks its token through Debug.
        assert!(!format!("{:?}", cfgs[0]).contains("ABC"));
    }

    // ── [`QueuedSink`] — the BOUND on the delivery seam ────────────────────────────────────────
    //
    // These run in the `alerting-standalone` lane like everything else here: no vike crate, no
    // network, no clock beyond `Instant`. What they pin is the property the type exists for — the
    // CALLER's cost is bounded whatever the endpoint does — plus the two things that make a bound
    // honest: the overflow is counted, and the shutdown is bounded too.

    /// An endpoint that answers SLOWLY. The real one is bounded only by `UreqTransport`'s 10 s.
    struct SleepySink {
        delay: Duration,
        seen: Arc<AtomicU64>,
    }

    impl AlertSink for SleepySink {
        fn deliver(&self, _alert: &FiredAlert) {
            std::thread::sleep(self.delay);
            self.seen.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// An endpoint that does not answer at ALL until released — a black-holed webhook, the shape
    /// that parked the recorder's tick loop. The hard cap is so a failing test cannot leave a
    /// thread wedged for the life of the test binary.
    struct WedgedSink {
        entered: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }

    impl AlertSink for WedgedSink {
        fn deliver(&self, _alert: &FiredAlert) {
            self.entered.store(true, Ordering::Relaxed);
            let cap = Instant::now() + Duration::from_secs(30);
            while !self.release.load(Ordering::Relaxed) && Instant::now() < cap {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Poll `cond` until it holds or `budget` runs out. Returns whether it held — every caller
    /// asserts on that rather than sleeping a guessed interval, which is how a timing test earns
    /// the right to exist on a loaded CI box.
    fn wait_until(cond: impl Fn() -> bool, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        cond()
    }

    /// **The deliverable.** Six alerts into a sink that takes 500 ms each — 3 s of endpoint work —
    /// and the caller is back in single-digit milliseconds. Unwrapped, this is the loop the
    /// recorder's stop flag sat behind.
    ///
    /// The ceiling is ONE delivery (500 ms): ~50x what six `try_send`s cost on a quiet box (well
    /// under 10 ms), so a scheduler stall of a few hundred milliseconds on a loaded CI lane passes,
    /// while a caller parked for even a single delivery — let alone the six a synchronous
    /// `dispatch` would cost — fails. The count assertion below is the other half and is not
    /// timing-sensitive at all.
    #[test]
    fn a_slow_sink_never_parks_the_caller() {
        const DELAY: Duration = Duration::from_millis(500);
        let seen = Arc::new(AtomicU64::new(0));
        let (sink, stop) = QueuedSink::spawn(
            "slow",
            16,
            Box::new(SleepySink { delay: DELAY, seen: seen.clone() }),
        );

        let began = Instant::now();
        for _ in 0..6 {
            sink.deliver(&alert(&["slow"], true));
        }
        let dispatched = began.elapsed();
        assert!(
            dispatched < DELAY,
            "six dispatches took {dispatched:?} — the caller waited on the endpoint for at least \
             one whole delivery ({DELAY:?} each, 3 s in all), which is the whole defect"
        );

        // …and nothing VANISHED: everything is either delivered or counted as abandoned at stop.
        // The budget covers the 3 s drain with room for a loaded box: an `Abandoned` here would be
        // the box, not the type.
        let outcome = stop.shutdown(Duration::from_secs(10));
        assert!(matches!(outcome, QueuedSinkOutcome::Joined { .. }), "{outcome:?}");
        assert_eq!(
            outcome.delivered() + outcome.dropped(),
            6,
            "every alert is accounted for, delivered or dropped: {outcome:?}"
        );
    }

    /// A wedged endpoint fills the queue, and past it alerts are DROPPED — bounded memory, a
    /// caller that still returns, and a count an operator can read. The one thing that must never
    /// happen is a silent loss, so the count is the assertion.
    ///
    /// The wall-clock ceiling on the ten dispatches is 2 s — >100x their quiet-box cost — because
    /// the regression it guards is a `send` where a `try_send` belongs, and that parks the caller
    /// until the wedged sink's own 30 s cap: any ceiling under that catches it, so the ceiling is
    /// sized for a loaded lane rather than for precision.
    #[test]
    fn an_overflowing_queue_drops_and_counts_rather_than_blocking() {
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let (sink, stop) = QueuedSink::spawn(
            "wedged",
            2,
            Box::new(WedgedSink { entered: entered.clone(), release: release.clone() }),
        );

        let began = Instant::now();
        for _ in 0..10 {
            sink.deliver(&alert(&["wedged"], true));
        }
        let dispatched = began.elapsed();
        assert!(
            dispatched < Duration::from_secs(2),
            "the caller blocked ({dispatched:?}) — a full queue must DROP, never wait"
        );
        // At most 2 in the queue plus at most 1 in the worker's hand ⇒ 7 or 8 dropped, depending on
        // whether the worker had taken one yet. Both are correct; a range is the honest assertion.
        let dropped = sink.dropped();
        assert!(
            (7..=8).contains(&dropped),
            "expected the overflow past capacity 2 to be dropped and counted, got {dropped}"
        );

        release.store(true, Ordering::Relaxed);
        let outcome = stop.shutdown(Duration::from_secs(5));
        assert_eq!(
            outcome.delivered() + outcome.dropped(),
            10,
            "delivered + dropped must still account for every alert: {outcome:?}"
        );
    }

    /// Routing is consulted BEFORE a queue slot is spent, so a target that is not named cannot have
    /// its queue filled by alerts meant for another one — the failure this prevents is the ONE
    /// alert routed here being the one dropped.
    #[test]
    fn a_queue_slot_is_never_spent_on_an_alert_this_target_ignores() {
        let fake = FakeTransport::default();
        let hook = WebhookSink::new(
            WebhookConfig {
                name: "telegram".into(),
                kind: WebhookKind::Telegram { token: "t".into(), chat_id: "c".into() },
            },
            fake.clone(),
        );
        let (sink, stop) = QueuedSink::spawn("telegram", 1, Box::new(hook));

        for _ in 0..50 {
            sink.deliver(&alert(&["webhook"], false)); // routed to a DIFFERENT target
        }
        sink.deliver(&alert(&["telegram"], false)); // …and the one that is routed here

        assert!(
            wait_until(|| fake.calls.lock().unwrap().len() == 1, Duration::from_secs(5)),
            "the routed alert never reached the transport"
        );
        assert_eq!(sink.dropped(), 0, "50 unrouted alerts must cost no queue slot at all");
        let outcome = stop.shutdown(Duration::from_secs(5));
        assert_eq!(outcome.delivered(), 1, "{outcome:?}");
    }

    /// The shutdown is BOUNDED, and it says so when it gives up. A worker caught inside a delivery
    /// cannot be interrupted, so the alternative — a plain `join()` — would add the endpoint's
    /// whole timeout to its owner's teardown, which on the recorder is sized against a systemd
    /// `TimeoutStopSec=`.
    #[test]
    fn shutdown_abandons_a_worker_stuck_in_a_delivery_within_its_budget() {
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let (sink, stop) = QueuedSink::spawn(
            "stuck",
            4,
            Box::new(WedgedSink { entered: entered.clone(), release: release.clone() }),
        );
        sink.deliver(&alert(&["stuck"], true));
        assert!(
            wait_until(|| entered.load(Ordering::Relaxed), Duration::from_secs(5)),
            "the worker never entered the delivery, so this test would prove nothing"
        );

        let began = Instant::now();
        let outcome = stop.shutdown(Duration::from_millis(200));
        let waited = began.elapsed();
        assert!(
            matches!(outcome, QueuedSinkOutcome::Abandoned { mid_delivery: true, .. }),
            "a worker that cannot finish must be reported as abandoned MID-DELIVERY — the page \
             was on the wire — not silently joined and not reported as idle: {outcome:?}"
        );
        assert!(
            waited < Duration::from_secs(2),
            "the wait must be bounded by the budget it was given; it took {waited:?}"
        );
        // Let the abandoned thread end rather than leaving it wedged for the test binary's life.
        release.store(true, Ordering::Relaxed);
    }

    /// A budget shorter than one `STOP_POLL` abandons an IDLE worker too — it cannot have woken to
    /// read the flag — and that outcome must say the worker was idle, not that a page was on the
    /// wire. This is the shape an owner stopping several targets against ONE shared deadline
    /// produces for every target after the one that ate the deadline, so a wrong answer here is a
    /// false "may not have arrived" in a teardown log, which is the disclosure this type exists for.
    #[test]
    fn an_idle_worker_abandoned_under_a_zero_budget_is_reported_idle_not_mid_delivery() {
        let seen = Arc::new(AtomicU64::new(0));
        let (sink, stop) = QueuedSink::spawn(
            "idle",
            4,
            Box::new(SleepySink { delay: Duration::ZERO, seen: seen.clone() }),
        );
        // Queue empty, worker parked in its poll: the normal state at a stop. The sink stays ALIVE
        // — dropping it would disconnect the channel and wake the worker into an immediate exit,
        // which is the other path, not this one.
        let outcome = stop.shutdown(Duration::ZERO);
        drop(sink);
        match outcome {
            QueuedSinkOutcome::Abandoned { mid_delivery, delivered, dropped, .. } => {
                assert!(!mid_delivery, "an idle worker was reported as inside a delivery");
                assert_eq!((delivered, dropped), (0, 0));
            }
            // A zero budget can only join a worker that had ALREADY exited, which an idle one has
            // not — but a scheduler that ran it first is not a defect, so this arm is tolerated.
            QueuedSinkOutcome::Joined { delivered, dropped, .. } => {
                assert_eq!((delivered, dropped), (0, 0));
            }
        }
    }

    /// The envelope guarantee: an alert that lands in the queue AFTER the worker has been told to
    /// stop — the slot a drain-then-drop would free and then destroy uncounted — is still either
    /// delivered or counted as dropped. `delivered + dropped` reaches the number sent, with no
    /// budget for the worker and a producer still running the whole time.
    #[test]
    fn an_alert_landing_across_the_stop_is_delivered_or_counted_never_lost() {
        let seen = Arc::new(AtomicU64::new(0));
        let (sink, stop) = QueuedSink::spawn(
            "late",
            2,
            Box::new(SleepySink { delay: Duration::from_millis(1), seen: seen.clone() }),
        );
        // Raise the stop with NO budget, so the worker is left to notice on its own — that is the
        // window in which a producer can still land alerts in the buffer.
        let _ = stop.shutdown(Duration::ZERO);
        const SENT: u64 = 40;
        for _ in 0..SENT {
            sink.deliver(&alert(&["late"], true));
            std::thread::sleep(Duration::from_millis(3));
        }
        assert!(
            wait_until(
                || seen.load(Ordering::Relaxed) + sink.dropped() == SENT,
                Duration::from_secs(5)
            ),
            "delivered ({}) + dropped ({}) never reached {SENT}: an alert vanished uncounted",
            seen.load(Ordering::Relaxed),
            sink.dropped()
        );
    }

    /// Capacity 0 would be a RENDEZVOUS channel, where `try_send` succeeds only when the worker is
    /// already parked in `recv` — a "queue" that drops almost everything. It is clamped to 1.
    #[test]
    fn a_zero_capacity_queue_is_clamped_rather_than_becoming_a_rendezvous() {
        let seen = Arc::new(AtomicU64::new(0));
        let (sink, stop) = QueuedSink::spawn(
            "clamped",
            0,
            Box::new(SleepySink { delay: Duration::from_millis(50), seen: seen.clone() }),
        );
        sink.deliver(&alert(&["clamped"], true));
        assert!(
            wait_until(|| seen.load(Ordering::Relaxed) == 1, Duration::from_secs(5)),
            "a capacity-0 queue swallowed the alert instead of holding one"
        );
        let outcome = stop.shutdown(Duration::from_secs(5));
        assert_eq!(outcome.delivered(), 1, "{outcome:?}");
    }

    /// `queued_ureq_webhook_sinks` pairs its two `Vec`s positionally, and spawns nothing for an
    /// empty config list — the unconfigured recorder mounts no delivery thread at all.
    #[test]
    fn the_queued_constructor_pairs_its_outputs_and_spawns_nothing_when_unconfigured() {
        let (sinks, stops) = queued_ureq_webhook_sinks(Vec::new(), DEFAULT_QUEUE_CAPACITY);
        assert!(sinks.is_empty() && stops.is_empty(), "no targets ⇒ no sinks and no threads");

        let mut full = HashMap::new();
        full.insert("VIKE_ALERT_TELEGRAM_TOKEN".to_string(), "123:ABC".to_string());
        full.insert("VIKE_ALERT_TELEGRAM_CHAT_ID".to_string(), "9001".to_string());
        full.insert("VIKE_ALERT_WEBHOOK_URL".to_string(), "https://example.test/h".to_string());
        let (sinks, stops) =
            queued_ureq_webhook_sinks(webhook_configs_from_env(&full), DEFAULT_QUEUE_CAPACITY);
        assert_eq!(sinks.len(), 2);
        assert_eq!(stops.iter().map(|s| s.name()).collect::<Vec<_>>(), ["telegram", "webhook"]);
        // Nothing was delivered, so nothing touches the network — the threads are idle and join at
        // once.
        for stop in stops {
            let outcome = stop.shutdown(Duration::from_secs(5));
            assert!(matches!(outcome, QueuedSinkOutcome::Joined { .. }), "{outcome:?}");
            assert_eq!(outcome.delivered() + outcome.dropped(), 0);
        }
    }
}
