//! The outside-world seam and its ONE production implementation: the `ureq` long-poll agent, and
//! the node, reached over the control wire as every other remote surface reaches it.
//!
//! Every test in this crate injects a scripted [`TelegramDependencies`] instead, which is why none of the
//! pure ones touches the network. The bot token lives here (embedded in `api_base`) and so do the
//! two hub keys, so [`ProdTelegramDependencies`] and [`NodeLink`] deliberately have NO `Debug`.
//!
//! # The node, through the one door
//!
//! This process holds no core, no snapshot cell and no door of its own. It asks the tradehub the
//! way `vike-cli` and `vike-mcp` do, through `vike-tradehub-client`:
//!
//! - a read verb is ONE `vike_tradehub_client::snapshot_once` (observe key), rendered by
//!   `crate::render_read`;
//! - a preview is ONE `vike_tradehub_client::preview_command` (control key): the node's own dry run
//!   of its door, `Request::Preview`, the verdict the CLI's one-shot verbs and the MCP preview print;
//! - a confirmed command rides ONE long-lived `vike_tradehub_client::TradehubControlHandle`, so the
//!   node's per-connection `ControlLimits` bucket persists across commands while the link stays up,
//!   redialled when it drops. The node records the chat's rationale beside the command, with this
//!   connection's peer and key.
//!
//! What the node answers is printed as it answers it. What only a client can know — the node did not
//! answer in time, the link dropped mid-command, the link was already closed — is said in this
//! file's words, and never as SENT or REFUSED when the outcome is not known ([`Outcome::Unknown`]).

use std::sync::Mutex;
use std::time::Duration;

use rand::Rng; // rand 0.10 core trait — provides `fill_bytes` (the same idiom `vike_hub_protocol::auth::fresh_nonce` uses)
use vike_alerting::{telegram_bot_url, telegram_send_message_body, telegram_send_message_url};
use vike_client::verbs::OrderClientIdMinter;
use vike_model::orders::client_id::OrderClientIdSource;
use vike_tradehub_client::wire::CommandWire;
use vike_tradehub_client::{CommandOutcome, ControlRejected, TradehubControlHandle};

use super::{AGENT_TIMEOUT, LONG_POLL_SECS, PollError, ReadVerb, TelegramConfig, TelegramUpdate};

/// What the node did with a CONFIRMED command, as this process can know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The node accepted it: the order client id it minted or echoed, and its notes.
    Sent { order_client_id: String, notes: Vec<String> },
    /// Nothing executed: the node refused it, or it provably never reached the node.
    Refused(String),
    /// It was written to the node and its answer never came: it MAY have executed.
    Unknown(String),
}

/// Everything [`poll_once`](super::poll_once) needs from the outside world — the
/// `ResolveDependencies`/`RedeemDependencies` shape. [`ProdTelegramDependencies`] is the real wiring; the pure tests inject
/// a scripted stub, so **none of them touches the network**.
pub trait TelegramDependencies {
    /// Long-poll for updates at or after `offset`.
    ///
    /// The error is a [`PollError`], not a string, because the caller has to decide whether asking
    /// again could ever help — and only THIS layer knows the HTTP status. Classifying here rather
    /// than sniffing a message downstream is the whole point; see
    /// `crates/vike-telegram/src/failure.rs`'s module doc.
    fn get_updates(&self, offset: i64) -> Result<Vec<TelegramUpdate>, PollError>;

    /// Best-effort reply. Fire-and-forget: a failed reply must never wedge the poller.
    fn send_message(&self, chat_id: i64, text: &str);

    /// The node's DRY-RUN verdict on a command: `Ok(notes)` would pass (the notes are what the node
    /// says of it, today the quantity an order goes out with on the venue's step), `Err(reason)`
    /// would refuse, or the node could not be asked. Nothing is sent either way.
    fn preview(&self, cmd: &CommandWire) -> Result<Vec<String>, String>;

    /// Send a CONFIRMED command to the node with `reason`, the rationale its audit records.
    fn accept(&self, cmd: CommandWire, reason: &str) -> Outcome;

    /// Render a read verb off the node's current frame.
    fn read(&self, verb: ReadVerb) -> String;

    /// Fill a `/submit` preview's empty order client id; every other command unchanged.
    fn fill_order_client_id(&self, cmd: CommandWire) -> CommandWire;

    /// A fresh single-use confirmation token.
    fn mint_token(&self) -> String;

    /// Wall clock, ms since epoch — injected so the confirm window is testable without sleeping.
    fn now_ms(&self) -> i64;
}

/// Where the node is and what this process dials it with: the tradehub's address, its hub key pair
/// (the CLI's: `vike_client::hubkeys::HubKeyring`, from the `hub_key` table), the control link's
/// reply window (`preferences.control_reply_timeout_ms`) and how long a confirm waits for the node's
/// verdict (`config.node_ack_wait_ms`), the rows `vike-cli trade` reads.
///
/// **No `Debug`, by design**: it holds two key values.
pub struct NodeLink {
    pub addr: String,
    pub observe_key: String,
    pub control_key: String,
    pub reply_timeout: Duration,
    pub ack_wait: Duration,
}

/// The real wiring: a dedicated `ureq` agent for the long-poll, and the node.
///
/// **No `Debug`, by design** — `api_base` embeds the bot token. Nothing in this impl interpolates
/// the URL or a `ureq` error (whose `Display` can echo the request target) into a returned string
/// or a log line.
pub struct ProdTelegramDependencies {
    agent: ureq::Agent,
    api_base: String,
    node: NodeLink,
    /// The one control connection, dialled on the first confirm and again whenever it is found dead.
    control: Mutex<Option<TradehubControlHandle>>,
    /// The CLI's order-client-id mint (`vike_client::verbs::order_client_id_minter`), one per process, its ids opening `T`.
    order_client_ids: Mutex<OrderClientIdMinter>,
}

impl ProdTelegramDependencies {
    /// Build the production deps for `config` over `node`. Dials nothing: the first read, preview or
    /// confirm does.
    pub fn new(config: &TelegramConfig, node: NodeLink) -> Self {
        ProdTelegramDependencies {
            // The long-poll agent, `vike_alerting::no_redirect_agent`: a 4xx/5xx is a RESPONSE
            // (`failure.rs` classifies Telegram's 401/403/404/429 by status), the timeout is
            // [`AGENT_TIMEOUT`] (longer than one long-poll), and no redirect is followed: the bot
            // token is in the URL path, so a followed 3xx hands it to another host; a 3xx reads as a
            // transient `http 30x`.
            agent: vike_alerting::no_redirect_agent(AGENT_TIMEOUT),
            api_base: telegram_bot_url(&config.bot_token),
            node,
            control: Mutex::new(None),
            order_client_ids: Mutex::new(vike_client::verbs::order_client_id_minter(
                OrderClientIdSource::Telegram,
            )),
        }
    }
}

impl TelegramDependencies for ProdTelegramDependencies {
    fn get_updates(&self, offset: i64) -> Result<Vec<TelegramUpdate>, PollError> {
        // `allowed_updates=["message"]`, percent-encoded: an edited message must never be replayed
        // as a fresh instruction, so it is filtered at the SOURCE as well as in `parse_updates`.
        let url = format!(
            "{}/getUpdates?offset={offset}&timeout={LONG_POLL_SECS}&allowed_updates=%5B%22message%22%5D",
            self.api_base
        );
        // ⚠ Every failure below is classified HERE, where the status is known — `PollError::permanent`
        // STOPS the channel, so nothing downstream is allowed to re-derive it from a message string.
        // A socket that died, a body that would not read and JSON that would not parse are all
        // conditions a healthy token recovers from, so all three are transient; only the status line
        // can be a rejected credential.
        let mut response = self
            .agent
            .get(&url)
            .call()
            // Deliberately opaque: a ureq error's Display can echo the token-bearing URL.
            .map_err(|_| PollError::transient("getUpdates: transport error"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|_| PollError::transient("getUpdates: read error"))?;
        if !(200..300).contains(&status) {
            return Err(PollError::from_status(status));
        }
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| PollError::transient(format!("getUpdates: bad JSON: {e}")))?;
        Ok(super::parse_updates(&value))
    }

    fn send_message(&self, chat_id: i64, text: &str) {
        // Telegram caps a message at 4096 chars; clamp by CHARS so a multi-byte sequence is never
        // split (the `sanitize_reason` discipline).
        let clamped: String = text.chars().take(3_500).collect();
        let url = telegram_send_message_url(&self.api_base);
        let body = telegram_send_message_body(chat_id, &clamped);
        match self.agent.post(&url).header("content-type", "application/json").send(body.as_bytes())
        {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let _ = response.body_mut().read_to_string(); // drain so the connection is reusable
                if !(200..300).contains(&status) {
                    // The STATUS only — never the url (it carries the bot token) and never the body.
                    tracing::warn!(chat_id, %status, "telegram control: sendMessage rejected");
                }
            }
            Err(_) => tracing::warn!(chat_id, "telegram control: sendMessage transport error"),
        }
    }

    fn preview(&self, cmd: &CommandWire) -> Result<Vec<String>, String> {
        match vike_tradehub_client::preview_command(
            self.node.addr.as_str(),
            self.node.control_key.as_bytes(),
            cmd,
        ) {
            Ok((true, _, notes)) => Ok(notes),
            Ok((false, reason, _)) => {
                Err(reason.unwrap_or_else(|| "the node refused it and gave no reason".to_string()))
            }
            Err(e) => Err(format!("the node could not be asked for its verdict: {e}")),
        }
    }

    fn accept(&self, cmd: CommandWire, reason: &str) -> Outcome {
        let mut link = self.control.lock().expect("telegram control link poisoned");
        // Two attempts, and only ever for a command the node provably never got: a link the worker
        // found closed answers NEVER SENT (or `Gone` before the queue), which licenses one redial.
        for _ in 0..2 {
            if !link.as_ref().is_some_and(TradehubControlHandle::is_connected) {
                match TradehubControlHandle::connect_with_reply_timeout(
                    self.node.addr.as_str(),
                    self.node.control_key.as_bytes(),
                    self.node.reply_timeout,
                ) {
                    Ok(handle) => *link = Some(handle),
                    Err(e) => {
                        return Outcome::Refused(format!(
                            "not sent: the node's control port could not be opened: {e}"
                        ));
                    }
                }
            }
            let Some(control) = link.as_ref() else { break };
            let ticket = match control.try_command_with_reason(cmd.clone(), Some(reason.into())) {
                Ok(ticket) => ticket,
                Err(ControlRejected::Gone) => {
                    *link = None;
                    continue;
                }
                Err(ControlRejected::Busy) => {
                    return Outcome::Refused(
                        "not sent: the control link is busy; issue it again".to_string(),
                    );
                }
                Err(ControlRejected::UnsupportedByHub) => {
                    return Outcome::Refused(
                        "not sent: this node does not advertise the capability this command \
                         requires (an older vike-tradehub)"
                            .to_string(),
                    );
                }
            };
            return match control.await_outcome(ticket, self.node.ack_wait) {
                Some(CommandOutcome::Accepted { order_client_id, notes }) => {
                    Outcome::Sent { order_client_id, notes }
                }
                Some(CommandOutcome::Refused(msg)) => Outcome::Refused(msg),
                Some(CommandOutcome::NeverSent) => {
                    *link = None;
                    continue;
                }
                Some(CommandOutcome::Disconnected) => Outcome::Unknown(
                    "the control connection dropped before the node answered: it may or may not \
                     have executed. Check /orders and /positions before retrying."
                        .to_string(),
                ),
                None => Outcome::Unknown(format!(
                    "sent, but the node has not answered within {}: the outcome is not yet known. \
                     Check /orders and /positions before retrying.",
                    vike_client::verbs::wait_seconds_text(self.node.ack_wait)
                )),
            };
        }
        Outcome::Refused(
            "not sent: the control connection to the node closed twice, so nothing reached it; \
             issue it again"
                .to_string(),
        )
    }

    fn read(&self, verb: ReadVerb) -> String {
        match vike_tradehub_client::snapshot_once(
            self.node.addr.as_str(),
            self.node.observe_key.as_bytes(),
        ) {
            Ok(snap) => super::render_read(verb, &snap),
            Err(e) => format!("the node could not be read: {e}"),
        }
    }

    fn fill_order_client_id(&self, cmd: CommandWire) -> CommandWire {
        // The node's frame gives the target account's id shape, code and position. Read only for a command
        // the filler mints for, so a cancel or a flatten costs no second round trip. An unreadable node mints
        // the default shape and account `00`, labels that decide nothing.
        let mints = matches!(
            cmd,
            CommandWire::Submit(_)
                | CommandWire::OrderList(_)
                | CommandWire::Combo(_)
                | CommandWire::Bracket(_)
        );
        let frame = mints
            .then(|| {
                vike_tradehub_client::snapshot_once(
                    self.node.addr.as_str(),
                    self.node.observe_key.as_bytes(),
                )
                .ok()
            })
            .flatten();
        let mut order_client_ids =
            self.order_client_ids.lock().expect("telegram order client id mint poisoned");
        vike_client::verbs::fill_order_client_id(cmd, &mut order_client_ids, frame.as_ref(), |c| {
            self.control.lock().is_ok_and(|link| link.as_ref().is_some_and(|h| h.takes(c)))
        })
    }

    fn mint_token(&self) -> String {
        mint_hex()
    }

    fn now_ms(&self) -> i64 {
        vike_model::now_ms()
    }
}

/// 8 CSPRNG bytes as lowercase hex — the confirmation token. `fill_bytes` is the same rand-0.10
/// entry point `vike_hub_protocol::auth::fresh_nonce` uses for the handshake nonce; 64 bits is far
/// beyond guessing inside a 60 s single-use window on an allowlisted chat.
///
/// ⚠ **This is the UNPREDICTABILITY half of the confirm-token contract** — `confirm.rs`'s module doc
/// owns the other five properties and tabulates this one. A token a third party could PREDICT lets
/// them confirm an order the operator only previewed, so "CSPRNG" here is a security property, not
/// an implementation detail: never a counter, never derived from the clock, never narrower than
/// these 8 bytes. Two gates hold it — the [`rand::CryptoRng`] bound just below, which fires at
/// COMPILE time, and `every_bit_of_a_minted_token_varies_the_anti_counter_gate` in this file's
/// tests, which fires on the minted output.
fn mint_hex() -> String {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    vike_utility::hex::hex_lower(&bytes)
}

/// Compile-time proof that [`mint_hex`]'s entropy source is a CRYPTOGRAPHIC generator.
///
/// `rand::rng()` is a `ThreadRng`, which implements [`rand::CryptoRng`]. Swapping it for a
/// non-cryptographic generator — `SmallRng`, an xorshift, anything whose future output is derivable
/// from observed output — stops COMPILING here, before a single test runs. The statistical gate in
/// the tests below catches the other shape of the same mistake (a counter or a timestamp, which are
/// not `RngCore` at all and would simply delete this call rather than fail its bound).
const _: fn() = || {
    fn require_crypto_rng<R: rand::CryptoRng>(_: &R) {}
    require_crypto_rng(&rand::rng());
};

#[cfg(test)]
mod tests {
    use super::mint_hex;
    use std::collections::BTreeSet;

    /// Sample size for the statistical gates. Every assertion below has a false-failure probability
    /// far under 1e-14 at this size (each one states its own), and 512 mints are instant.
    const SAMPLES: usize = 512;

    fn sample() -> Vec<String> {
        (0..SAMPLES).map(|_| mint_hex()).collect()
    }

    fn as_u64(token: &str) -> u64 {
        u64::from_str_radix(token, 16).expect("a minted token is 16 hex chars")
    }

    /// SHAPE: 16 lowercase hex characters — the full 8 bytes the doc claims. A minter that
    /// truncated, re-encoded, or prefixed its output changes this.
    #[test]
    fn a_minted_token_is_sixteen_lowercase_hex_chars() {
        for token in sample() {
            assert_eq!(token.len(), 16, "8 bytes as hex: {token:?}");
            assert!(
                token.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "lowercase hex only: {token:?}"
            );
        }
    }

    /// ⚠ **THE ANTI-COUNTER / ANTI-CLOCK GATE.** Across the sample, every one of the 64 bit
    /// positions must be observed BOTH set and clear.
    ///
    /// A counter — however it is seeded — only ever varies its LOW bits: 512 increments move at most
    /// nine of them and leave the other 55 constant. A clock-derived token has the same signature;
    /// the high bits of a millisecond (or even nanosecond) timestamp do not change inside a test's
    /// runtime. A constant fails on every bit. Only a real CSPRNG varies all 64.
    ///
    /// False-failure probability per bit position: 2 * 2^-512.
    #[test]
    fn every_bit_of_a_minted_token_varies_the_anti_counter_gate() {
        let (mut ever_set, mut ever_clear) = (0u64, 0u64);
        for token in sample() {
            let v = as_u64(&token);
            ever_set |= v;
            ever_clear |= !v;
        }
        assert_eq!(
            ever_set,
            u64::MAX,
            "bits {:#018x} were NEVER set across {SAMPLES} tokens — the minter is not a CSPRNG \
             (a counter or a clock leaves its high bits constant exactly like this)",
            !ever_set
        );
        assert_eq!(
            ever_clear,
            u64::MAX,
            "bits {:#018x} were NEVER clear across {SAMPLES} tokens — same finding",
            !ever_clear
        );
    }

    /// A token is never REUSED. Two live previews sharing a token would make one confirm ambiguous;
    /// a repeat across the process's life narrows the guessing window the 60 s expiry assumes.
    ///
    /// Birthday-collision probability at 512 draws from 2^64: about 7e-15.
    #[test]
    fn minted_tokens_do_not_repeat() {
        let tokens = sample();
        let distinct: BTreeSet<&String> = tokens.iter().collect();
        assert_eq!(distinct.len(), tokens.len(), "a minted token repeated within one sample");
    }

    /// …and successive tokens are not a SEQUENCE. Stated separately from the bit gate because it is
    /// the property an operator actually cares about: seeing one token must not reveal the next.
    ///
    /// Two random 64-bit values differ by exactly one with probability 2^-63, so the observed count
    /// is 0; the threshold is loose only so the gate can never flake.
    #[test]
    fn minted_tokens_are_not_a_sequence() {
        let values: Vec<u64> = sample().iter().map(|t| as_u64(t)).collect();
        let adjacent = values
            .windows(2)
            .filter(|w| w[1].wrapping_sub(w[0]) == 1 || w[0].wrapping_sub(w[1]) == 1)
            .count();
        assert!(adjacent < 4, "{adjacent} successive tokens differed by exactly 1 — a counter");
        assert!(
            !values.windows(2).all(|w| w[0] < w[1]),
            "every token was larger than the last — the minter is monotonic, not random"
        );
    }

    /// **The long-poll agent never follows a redirect.** The bot token is in the URL PATH, so a
    /// followed redirect hands the operator's bot to whatever host answered the `Location`; the
    /// 3xx comes back as a response (`PollError::from_status` then reads it as transient).
    ///
    /// MUTATION PROOF: delete `.max_redirects(0)` from `vike_alerting::no_redirect_agent` (this agent) — the
    /// 302 is followed.
    #[test]
    fn the_long_poll_agent_never_follows_a_redirect() {
        let (base, target_hits) = vike_log::loopback::redirecting_origin();
        let response = vike_alerting::no_redirect_agent(std::time::Duration::from_secs(5))
            .get(&format!("{base}/bot123:test/getUpdates"))
            .call();
        assert_eq!(target_hits.load(std::sync::atomic::Ordering::SeqCst), 0, "target dialled");
        assert_eq!(response.expect("a 302 is a response").status().as_u16(), 302);
    }
}
