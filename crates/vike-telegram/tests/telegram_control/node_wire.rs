//! **The production deps against a REAL node**: a paper `vike-tradehub` (`vike_tradehub::{server,
//! publish}` over `vike_tradehub_core::test_support::paper_maker`, the harness `vike-cli`'s node tests
//! use) served on loopback with control on, and a scripted Telegram API in front. What moved out of
//! the tradehub is proven here: a chat command reaches the node's ONE door over the control wire,
//! the node previews it and sends it, its audit records the chat's literal words beside this
//! connection, and a read is the node's frame. No request reaches Telegram.

use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use vike_hub_protocol::auth::HubKeys;
use vike_log::poll::wait_until;
use vike_telegram::{NodeLink, ProdTelegramDependencies};
use vike_tradehub::{publish, server};
use vike_tradehub_core::StrategyCorePaper;

use super::*;

const TOKEN: &str = "TELEGRAM_NODE_WIRE_TOKEN";
/// Far-future resolution so the A-S horizon is positive.
const RESOLUTION_TIMESTAMP: i64 = 3_000_000_000;
const OBSERVE_KEY: &str = "observe-secret-key-for-telegram-tests";
const CONTROL_KEY: &str = "control-secret-key-for-telegram-tests";

/// The target `vike_tradehub::audit`'s one `tracing` event per accepted command carries. The event
/// is emitted on the server's session thread, so the capture is the process-global one, installed
/// by its first call: [`capture_audit`] runs before anything is sent.
const AUDIT_TARGET: &str = "vike_tradehub::audit";

fn capture_audit() {
    vike_log::capture::global(AUDIT_TARGET);
}

/// The rationale the node's audit recorded for `order_client_id`.
fn audit_reason_for(order_client_id: &str) -> Option<String> {
    for _ in 0..200 {
        let events = vike_log::capture::global(AUDIT_TARGET);
        if let Some(e) = events.iter().find(|e| e.field("order_client_id") == Some(order_client_id))
        {
            return e.field("reason").map(str::to_string);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// A PAPER node on `TOKEN`, served on an ephemeral loopback port with control ENABLED under
/// `limits`. Hold the returned mount for the test's lifetime.
fn spawn_node(limits: server::control::ControlLimitsConfig) -> (StrategyCorePaper, SocketAddr) {
    let mount = vike_tradehub_core::test_support::paper_maker::paper_maker_core(
        vike_strategies_check::makers::spread_maker::SpreadMaker::from_params,
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TIMESTAMP),
    );
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let commands = Some(mount.handle.command_sink());
    let keys = HubKeys::new(OBSERVE_KEY.as_bytes().to_vec(), CONTROL_KEY.as_bytes().to_vec());
    std::thread::spawn(move || {
        let _ = server::serve(listener, publisher, keys, commands, limits, None, None, None);
    });
    (mount, addr)
}

/// The PRODUCTION deps for the node, with Telegram's two calls scripted in front: the updates a test
/// queues, and the replies it reads back. Everything else is `ProdTelegramDependencies`' own.
struct NodeDependencies {
    node: ProdTelegramDependencies,
    batches: Mutex<VecDeque<Vec<TelegramUpdate>>>,
    replies: Mutex<Vec<String>>,
}

impl NodeDependencies {
    fn new(addr: SocketAddr) -> Self {
        let link = NodeLink {
            addr: addr.to_string(),
            observe_key: OBSERVE_KEY.to_string(),
            control_key: CONTROL_KEY.to_string(),
            reply_timeout: Duration::from_secs(5),
            ack_wait: Duration::from_secs(5),
        };
        NodeDependencies {
            node: ProdTelegramDependencies::new(&config(), link),
            batches: Mutex::new(VecDeque::new()),
            replies: Mutex::new(Vec::new()),
        }
    }

    /// One chat line through one [`poll_once`], and the reply it got.
    fn say(
        &self,
        led: &UpdateLedger,
        pending: &mut PendingConfirms,
        id: i64,
        text: &str,
    ) -> String {
        self.batches.lock().unwrap().push_back(vec![update(id, CHAT, text)]);
        poll_once(self, &config(), led, pending);
        self.replies.lock().unwrap().last().cloned().expect("a reply")
    }
}

impl TelegramDependencies for NodeDependencies {
    fn get_updates(&self, _offset: i64) -> Result<Vec<TelegramUpdate>, PollError> {
        Ok(self.batches.lock().unwrap().pop_front().unwrap_or_default())
    }
    fn send_message(&self, _chat_id: i64, text: &str) {
        self.replies.lock().unwrap().push(text.to_string());
    }
    fn preview(&self, cmd: &CommandWire) -> Result<Vec<String>, String> {
        self.node.preview(cmd)
    }
    fn accept(&self, cmd: CommandWire, reason: &str) -> Outcome {
        self.node.accept(cmd, reason)
    }
    fn read(&self, verb: ReadVerb) -> String {
        self.node.read(verb)
    }
    fn fill_order_client_id(&self, cmd: CommandWire) -> CommandWire {
        self.node.fill_order_client_id(cmd)
    }
    fn mint_token(&self) -> String {
        self.node.mint_token()
    }
    fn now_ms(&self) -> i64 {
        self.node.now_ms()
    }
}

/// The token a preview reply ends with.
fn token_of(preview: &str) -> String {
    preview.rsplit("/confirm ").next().expect("a preview names its token").trim().to_string()
}

/// The `order_client_id=` a preview line carries.
fn order_client_id_of_preview(preview: &str) -> String {
    let at = preview.find("order_client_id=").expect("a submit preview names its order client id")
        + "order_client_id=".len();
    preview[at..].split_whitespace().next().expect("an order client id").to_string()
}

/// **A chat order goes to the node's door over the wire, previewed by the node and sent on confirm,
/// with the CLI's order client id and the chat's words in the node's audit.** The preview is the node's
/// `Request::Preview`, the send its `Request::Command` on one control connection, and `/orders` the
/// node's frame: the order the chat placed is listed there, as the CLI would see it.
#[test]
fn a_chat_order_reaches_the_nodes_door_over_the_control_wire() {
    capture_audit();
    let (_mount, addr) = spawn_node(server::control::ControlLimitsConfig::default());
    let dependencies = NodeDependencies::new(addr);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    let preview = dependencies.say(
        &led,
        &mut pending,
        1,
        &format!("/submit polymarket {TOKEN} buy 2 0.4\nflat before the print"),
    );
    assert!(preview.starts_with("PREVIEW — nothing has been sent."), "{preview}");
    assert!(preview.contains("guardrail: OK"), "the node passed it: {preview}");
    let order_client_id = order_client_id_of_preview(&preview);
    assert!(
        vike_model::is_valid_crypto_order_client_id(&order_client_id),
        "the CLI's mint, not `tg-…`: {order_client_id}"
    );
    let parsed = vike_model::orders::client_id::parse_order_client_id(&order_client_id);
    assert!(
        matches!(parsed, Some(vike_model::orders::client_id::OrderClientIdReading::V2(ref p))
            if p.src == vike_model::orders::client_id::OrderClientIdSource::Telegram),
        "a v2 id with Telegram's letter: {order_client_id}"
    );
    assert!(
        order_client_id.starts_with("T0000"),
        "Telegram's letter, no strategy: {order_client_id}"
    );

    let sent = dependencies.say(&led, &mut pending, 2, &format!("/confirm {}", token_of(&preview)));
    assert!(sent.starts_with("SENT\n"), "{sent}");
    assert!(
        sent.contains(&format!("order_client_id={order_client_id}")),
        "the node echoes the previewed id: {sent}"
    );

    let reason =
        audit_reason_for(&order_client_id).expect("the node audited the command with a rationale");
    assert!(reason.contains(&format!("telegram chat {CHAT} user {USER}")), "the origin: {reason}");
    assert!(reason.contains("flat before the print"), "the operator's literal words: {reason}");
    assert!(reason.contains(&format!("[/confirm by user {USER}")), "who authorized: {reason}");
    assert!(!reason.contains('\n'), "the node sanitized it: {reason:?}");

    assert!(
        wait_until(Duration::from_secs(5), || dependencies
            .read(ReadVerb::Orders)
            .contains(&order_client_id)),
        "/orders lists the order the chat placed, off the node's frame: {}",
        dependencies.read(ReadVerb::Orders)
    );
    assert!(
        dependencies.read(ReadVerb::Status).starts_with("seq="),
        "{}",
        dependencies.read(ReadVerb::Status)
    );
}

/// **The node's ceiling refuses a chat order at the PREVIEW, in its own words, and nothing is sent.**
/// No token is minted, so there is nothing to confirm.
#[test]
fn the_nodes_ceiling_refuses_a_chat_order_at_the_preview() {
    let (_mount, addr) = spawn_node(server::control::ControlLimitsConfig {
        max_notional: Some(1.0),
        ..Default::default()
    });
    let dependencies = NodeDependencies::new(addr);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    let reply =
        dependencies.say(&led, &mut pending, 1, &format!("/submit polymarket {TOKEN} buy 10 0.4"));
    assert!(reply.starts_with("REFUSED (nothing sent)\n"), "{reply}");
    assert!(pending.is_empty(), "no token for a refused command");
    let direct = vike_tradehub_client::preview_command(
        addr,
        CONTROL_KEY.as_bytes(),
        &CommandWire::Submit(vike_tradehub_client::wire::OrderRequestWire {
            order_client_id: "probe".into(),
            venue: "polymarket".into(),
            symbol: TOKEN.into(),
            side: 1,
            qty: 10.0,
            order_type: "limit".into(),
            price: Some(0.4),
            trigger_price: None,
            reduce_only: false,
            account: None,
            time_in_force: None,
            gtd_expiry: None,
            display_quantity: None,
            release: None,
            trail: None,
            limit_offset: None,
            peg: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
        }),
    )
    .expect("the node answers");
    let words = direct.1.expect("the node says why");
    assert!(reply.ends_with(&words), "the chat reads the node's own words: {reply} / {words}");
}

/// **A node that cannot be reached is said, and nothing is sent**: a preview names the failure and
/// mints no token, and a read says it could not read.
#[test]
fn an_unreachable_node_is_refused_at_the_preview_and_said_on_a_read() {
    let dead = TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("addr");
    let dependencies = NodeDependencies::new(dead); // the listener is dropped: nothing answers
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    let reply = dependencies.say(&led, &mut pending, 1, "/cancel ORDER-A");
    assert!(reply.starts_with("REFUSED (nothing sent)\ncancel ORDER-A\n"), "{reply}");
    assert!(reply.contains("the node could not be asked for its verdict"), "{reply}");
    assert!(pending.is_empty());
    assert!(dependencies.read(ReadVerb::Status).starts_with("the node could not be read: "));
}

/// The address this program dials when nothing names one is the tradehub's own default bind.
#[test]
fn the_default_node_is_the_tradehubs_own_default_bind() {
    assert_eq!(
        vike_telegram::telegram_cli::DEFAULT_NODE_ADDRESS,
        vike_tradehub::server::DEFAULT_ADDRESS
    );
}
