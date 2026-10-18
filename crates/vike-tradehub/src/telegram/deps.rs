//! The outside-world seam and its ONE production implementation: the `ureq` long-poll agent, this
//! surface's own [`ControlLimits`] bucket, the core's [`CommandSink`], and the published snapshot
//! cell.
//!
//! Every test in this workstream injects a scripted [`TelegramDeps`] instead, which is why none of
//! them touches the network. The bot token lives here (embedded in `api_base`), so
//! [`ProdTelegramDeps`] deliberately has NO `Debug`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;
use rand::Rng; // rand 0.10 core trait — provides `fill_bytes` (the same idiom `vike_hub_proto::auth::fresh_nonce` uses)
use vike_alerting::{telegram_bot_url, telegram_send_message_body, telegram_send_message_url};
use vike_core::CommandSink;
use vike_exec::CoreSnapshot;
use vike_tradehub_client::wire::WireCommand;

use crate::server::control::{ControlLimits, ControlLimitsConfig, accept_command};

use super::{
    AGENT_TIMEOUT, LONG_POLL_SECS, PollError, ReadVerb, TelegramConfig, TgUpdate, parse_updates,
    render::key_changed_lines, render_read,
};

/// Everything [`poll_once`](super::poll_once) needs from the outside world — the
/// `ResolveDeps`/`RedeemDeps` shape. [`ProdTelegramDeps`] is the real wiring; every test injects a
/// scripted stub, so **no test in this workstream touches the network**.
pub trait TelegramDeps {
    /// Long-poll for updates at or after `offset`.
    ///
    /// The error is a [`PollError`], not a string, because the caller has to decide whether asking
    /// again could ever help — and only THIS layer knows the HTTP status. Classifying here rather
    /// than sniffing a message downstream is the whole point; see
    /// `crates/vike-tradehub/src/telegram/failure.rs`'s module doc.
    fn get_updates(&self, offset: i64) -> Result<Vec<TgUpdate>, PollError>;

    /// Best-effort reply. Fire-and-forget: a failed reply must never wedge the poller.
    fn send_message(&self, chat_id: i64, text: &str);

    /// The server-authoritative DRY-RUN verdict for a command — `Some(reason)` would refuse,
    /// `None` would pass. Consumes no rate token (`ControlLimits::preview_vet`).
    fn preview(&self, cmd: &WireCommand) -> Option<String>;

    /// Hand a CONFIRMED command to the core through the shared
    /// [`crate::server::control::accept_command`] path. `Ok(coid)` on acceptance.
    fn accept(&self, cmd: WireCommand, reason: &str) -> Result<String, String>;

    /// Render a read verb off the published snapshot.
    fn read(&self, verb: ReadVerb) -> String;

    /// A fresh client-order-id for a `/submit` preview.
    fn mint_coid(&self) -> String;

    /// A fresh single-use confirmation token.
    fn mint_token(&self) -> String;

    /// Wall clock, ms since epoch — injected so the confirm window is testable without sleeping.
    fn now_ms(&self) -> i64;
}

/// The long-poll agent: `vike_bridge_core::http::blocking_agent_with_timeout`'s three settings, restated
/// (wave 2, W15: this daemon links no `vike-bridge-core`). A 4xx/5xx answer is a RESPONSE, not an error —
/// `failure.rs` classifies Telegram's 401/403/404/429 by status — the global timeout is the caller's
/// ([`AGENT_TIMEOUT`], longer than one long-poll) and the user agent is the workspace's.
fn long_poll_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        // No redirect: the bot token is in the URL path, so a followed 3xx hands it to another host; a 3xx reads as a transient `http 30x`.
        .max_redirects(0)
        .timeout_global(Some(timeout))
        .user_agent("vike-trader-rust")
        .build()
        .new_agent()
}

/// The real wiring: a dedicated `ureq` agent for the long-poll, this surface's OWN
/// [`ControlLimits`] bucket, the core's [`CommandSink`], and the published snapshot cell.
///
/// **No `Debug`, by design** — `api_base` embeds the bot token. Nothing in this impl interpolates
/// the URL or a `ureq` error (whose `Display` can echo the request target) into a returned string
/// or a log line.
pub struct ProdTelegramDeps {
    agent: ureq::Agent,
    api_base: String,
    limits: Mutex<ControlLimits>,
    sink: CommandSink,
    snapshot: Arc<ArcSwap<CoreSnapshot>>,
    /// The node's accounts that do NOT run, each with why (decision 0156) — the list the TCP edge
    /// reads from its publisher (`crate::publish::PublisherHandle::not_running`). The list the node booted
    /// with; the core publishes none of it into the cell this surface reads, so once the account watcher
    /// keeps the list current ([`Self::with_live_not_running`]) this one is only the fallback.
    not_running: Vec<vike_exec::NotRunningAccount>,
    /// The publisher whose list the account watcher keeps current (decision 0169): read per command, so a
    /// chat order to an account stopped or started after the boot is judged on today's list. `None` leaves
    /// the boot list.
    live_not_running: Option<crate::publish::PublisherHandle>,
    /// Where a new symbol's facts come from (`crate::server::symbol_admission`), the TCP edge's
    /// `PublisherHandle::symbol_facts`; `None` leaves a foreign symbol refused by name.
    symbol_facts: Option<Arc<dyn crate::server::symbol_admission::SymbolFacts>>,
}

impl ProdTelegramDeps {
    /// Build the production deps for `cfg`. `limits` is the SAME [`ControlLimitsConfig`] the daemon
    /// resolved once for the TCP server — one config, one policy, a bucket per surface;
    /// `not_running` the same accounts-that-do-not-run list the TCP edge refuses with.
    pub fn new(
        cfg: &TelegramConfig,
        limits: ControlLimitsConfig,
        sink: CommandSink,
        snapshot: Arc<ArcSwap<CoreSnapshot>>,
        not_running: Vec<vike_exec::NotRunningAccount>,
    ) -> Self {
        ProdTelegramDeps {
            agent: long_poll_agent(AGENT_TIMEOUT),
            api_base: telegram_bot_url(&cfg.bot_token),
            limits: Mutex::new(ControlLimits::new(limits)),
            sink,
            snapshot,
            not_running,
            live_not_running: None,
            symbol_facts: None,
        }
    }

    /// Read the accounts that do NOT run from the observe server's publisher, which the account watcher keeps
    /// current as accounts stop and start in the running node (decision 0169), instead of the list the node
    /// booted with. Without it a chat order to an account stopped after the boot would reach its tombstone, and
    /// one to an account started after it would be refused as not running.
    #[must_use]
    pub fn with_live_not_running(
        mut self,
        publisher: Option<crate::publish::PublisherHandle>,
    ) -> Self {
        self.live_not_running = publisher;
        self
    }

    /// The accounts that do not run NOW.
    fn not_running(&self) -> Arc<Vec<vike_exec::NotRunningAccount>> {
        match &self.live_not_running {
            Some(publisher) => publisher.not_running(),
            None => Arc::new(self.not_running.clone()),
        }
    }

    /// Give this surface the source the TCP edge has, so a chat order on a symbol the engine was not
    /// started on is admitted the same way (decision 0173). Without it such an order is refused.
    #[must_use]
    pub fn with_symbol_facts(
        mut self,
        facts: Option<Arc<dyn crate::server::symbol_admission::SymbolFacts>>,
    ) -> Self {
        // ALWAYS fail-fast here: the poller runs the whole acceptance inline, so a price wait would
        // hold every chat command (a /panic included) behind one slow order.
        self.symbol_facts = facts.map(|inner| {
            Arc::new(crate::server::symbol_admission::FailFast(inner))
                as Arc<dyn crate::server::symbol_admission::SymbolFacts>
        });
        self
    }

    /// The venues this node runs an ENGINE for, off the published snapshot cell — this surface's
    /// twin of `crate::publish::PublisherHandle::engine_venues`, which reads the SAME cell from the
    /// TCP side. Both must exist because the two surfaces hold the cell through different types
    /// (this one directly, that one behind a publisher), and both must answer identically because
    /// `crate::server::control::accept_command` is the one acceptance path — a roster that differed by
    /// surface would be a routing rule that differed by surface.
    ///
    /// EMPTY means the core has not published yet, never "no engines" — `venue_refusal` treats it
    /// as UNKNOWN and refuses nothing; the argument is at that function.
    fn engine_venues(&self) -> Vec<String> {
        self.snapshot.load().portfolio.venues.iter().map(|v| v.venue.clone()).collect()
    }

    /// The ROUTING KEYS this node runs an engine under, off the same published cell — this
    /// surface's twin of `crate::publish::PublisherHandle::engine_route_keys`, and here for exactly
    /// the reason [`Self::engine_venues`] is: `crate::server::control::accept_command` is the one acceptance
    /// path, so a roster that differed by surface would be a routing rule that differed by surface.
    /// A chat message naming an account this node does not run must be refused in the same words a
    /// TCP peer gets.
    ///
    /// EMPTY means the core has not published yet, never "no engines" — `account_refusal` inherits
    /// `venue_refusal`'s treatment of that exactly, and the two rosters are empty together because
    /// both project one `portfolio.venues`.
    fn engine_route_keys(&self) -> Vec<String> {
        self.snapshot.load().portfolio.venues.iter().map(|v| v.route_key.clone()).collect()
    }

    /// The engine BLOCKS, off the same published cell — this surface's twin of
    /// `crate::publish::PublisherHandle::engine_blocks`, for the one gate that needs more than a
    /// key: a bracket's (`crate::server::refusal::bracket_refusal`), which judges a bracket by what the one
    /// engine it reaches trades. Here for [`Self::engine_route_keys`]' reason: a chat surface and the
    /// TCP one must refuse a bracket in the same words.
    fn engine_blocks(&self) -> Vec<vike_exec::VenueBlock> {
        self.snapshot.load().portfolio.venues.clone()
    }

    /// The OPEN order a `Modify` names, off the same published cell — this surface's twin of
    /// `crate::publish::PublisherHandle::open_orders_named_by`, through the ONE
    /// `crate::publish::named_open_orders` they share, for the one gate that needs it: the notional
    /// ceiling, which sizes a `Modify` with the contract multiplier of the instrument that order
    /// rests on. Here for [`Self::engine_route_keys`]' reason: a chat surface and the TCP one must
    /// size one frame to one verdict.
    fn open_orders_named_by(&self, cmd: &WireCommand) -> Vec<vike_exec::OrderView> {
        crate::publish::named_open_orders(cmd, &self.snapshot.load().orders)
    }
}

impl TelegramDeps for ProdTelegramDeps {
    fn get_updates(&self, offset: i64) -> Result<Vec<TgUpdate>, PollError> {
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
        let mut resp = self
            .agent
            .get(&url)
            .call()
            // Deliberately opaque: a ureq error's Display can echo the token-bearing URL.
            .map_err(|_| PollError::transient("getUpdates: transport error"))?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|_| PollError::transient("getUpdates: read error"))?;
        if !(200..300).contains(&status) {
            return Err(PollError::from_status(status));
        }
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| PollError::transient(format!("getUpdates: bad JSON: {e}")))?;
        Ok(parse_updates(&value))
    }

    fn send_message(&self, chat_id: i64, text: &str) {
        // Telegram caps a message at 4096 chars; clamp by CHARS so a multi-byte sequence is never
        // split (the `sanitize_reason` discipline).
        let clamped: String = text.chars().take(3_500).collect();
        let url = telegram_send_message_url(&self.api_base);
        let body = telegram_send_message_body(chat_id, &clamped);
        match self.agent.post(&url).header("content-type", "application/json").send(body.as_bytes())
        {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let _ = resp.body_mut().read_to_string(); // drain so the connection is reusable
                if !(200..300).contains(&status) {
                    // The STATUS only — never the url (it carries the bot token) and never the body.
                    tracing::warn!(chat_id, %status, "telegram control: sendMessage rejected");
                }
            }
            Err(_) => tracing::warn!(chat_id, "telegram control: sendMessage transport error"),
        }
    }

    fn preview(&self, cmd: &WireCommand) -> Option<String> {
        // The same four-step verdict the TCP preview arm gives, in the same order: the edge gate,
        // then the ROUTING gate, then the ACCOUNT within it, then a BRACKET's own verdict (its
        // values, and what the one engine it reaches can hold — the call `accept_command` makes at
        // its step 1d). A chat surface previewing a command whose venue this node does not run must
        // say so here — this channel's whole affordance is that a human types a venue name into a
        // phone, and a phone is exactly where `binance/ALT` gets typed as `binance/ALt`. (This
        // channel's parser mints no bracket today; the fourth step is here so a preview here can
        // never answer a bracket differently from the TCP one.) The blocks are read once, ahead of
        // the chain, for the TCP preview's reason: the edge gate sizes with the addressed engine's
        // contract multiplier and the bracket verdict reads what it trades. The order a `Modify`
        // names is read beside them, for the same gate: it is how a bare coid reaches its engine.
        let blocks = self.engine_blocks();
        let orders = self.open_orders_named_by(cmd);
        let limits = self.limits.lock().expect("telegram control limits poisoned");
        let foreign = limits.has_ceiling().then(|| {
            crate::server::symbol_admission::foreign_contract_value(
                cmd,
                &blocks,
                self.symbol_facts.as_deref(),
            )
        });
        limits
            .preview_vet_foreign(cmd, &blocks, &orders, foreign.flatten())
            .or_else(|| crate::server::refusal::not_running_refusal(cmd, &self.not_running()))
            .or_else(|| crate::server::refusal::venue_refusal(cmd, &self.engine_venues()))
            .or_else(|| crate::server::refusal::account_refusal(cmd, &self.engine_route_keys()))
            .or_else(|| crate::server::refusal::bracket_refusal(cmd, &blocks))
            .or_else(|| {
                crate::server::symbol_admission::submit_symbol_refusal(
                    cmd,
                    &blocks,
                    self.symbol_facts.as_deref(),
                )
            })
    }

    fn accept(&self, cmd: WireCommand, reason: &str) -> Result<String, String> {
        let engines = self.engine_venues();
        let route_keys = self.engine_route_keys();
        let blocks = self.engine_blocks();
        let orders = self.open_orders_named_by(&cmd);
        let not_running = self.not_running();
        let mut limits = self.limits.lock().expect("telegram control limits poisoned");
        // THE shared path — the identical function the TCP `Request::Command` arm calls. `peer` is
        // `None` because this surface has no socket; the origin rides in `reason` instead. The
        // settings source is `None` too: this channel's parser mints no `SetSetting` (its verb
        // table is the /submit-family), and a `None` here means even a hand-built one would be
        // refused inside `accept_command` rather than writing a file from a chat message. The
        // `key_id` is `None` for the same reason as `peer`: this channel authenticates a Telegram
        // chat id, not a hub key, so there is no key to fingerprint — and an absent id is
        // recorded as an absent field rather than as a borrowed one.
        //
        // ⚠ The ENGINE ROSTERS are NOT `None`-shaped here, unlike the two above: this surface holds
        // the same published snapshot cell the TCP server's publisher reads, so it can and must
        // answer the routing question. A chat message naming a venue this node does not run is
        // refused with the node's own roster rather than applied to its primary engine, and one
        // naming an ACCOUNT it does not run is refused with the accounts it does hold rather than
        // applied to another book of the same exchange.
        accept_command(
            cmd,
            Some(reason),
            &mut limits,
            &self.sink,
            None,
            &engines,
            &route_keys,
            &not_running,
            &blocks,
            &orders,
            self.symbol_facts.as_deref(),
            None,
            None,
        )
        .map(crate::server::control::Accepted::into_coid)
        .map_err(|e| e.message())
    }

    fn read(&self, verb: ReadVerb) -> String {
        // The LOSSY arc-swap publication — never the core fold (the p99 gate's contract).
        let snap = self.snapshot.load_full();
        let mut out = render_read(verb, &snap);
        // `/status` also names a running account whose key changed in the database (a warning, not a
        // stop); the snapshot above does not carry it, the publisher the watcher keeps current does.
        if let (ReadVerb::Status, Some(publisher)) = (verb, &self.live_not_running) {
            out.push_str(&key_changed_lines(&publisher.key_changed()));
        }
        out
    }

    fn mint_coid(&self) -> String {
        format!("tg-{}", mint_hex())
    }

    fn mint_token(&self) -> String {
        mint_hex()
    }

    fn now_ms(&self) -> i64 {
        vike_model::now_ms()
    }
}

/// 8 CSPRNG bytes as lowercase hex — the confirmation token and the minted coid suffix. `fill_bytes`
/// is the same rand-0.10 entry point `vike_hub_proto::auth::fresh_nonce` uses for the handshake nonce; 64 bits is
/// far beyond guessing inside a 60 s single-use window on an allowlisted chat.
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
    vike_util::hex::hex_lower(&bytes)
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

    /// A loopback origin answering every request with a 302 to a SECOND loopback listener, plus
    /// that listener's hit counter: the stand-in for the host a followed redirect would replay the
    /// request (its path carries the bot token) to.
    fn redirecting_origin() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering::SeqCst};
        use std::time::Duration;
        fn serve(reply: String) -> (u16, Arc<AtomicUsize>) {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let port = listener.local_addr().expect("local addr").port();
            let hits = Arc::new(AtomicUsize::new(0));
            let seen = hits.clone();
            std::thread::spawn(move || {
                for mut conn in listener.incoming().flatten() {
                    seen.fetch_add(1, SeqCst);
                    let _ = conn.set_read_timeout(Some(Duration::from_millis(300)));
                    let mut buf = [0u8; 8192];
                    for _ in 0..2 {
                        let _ = conn.read(&mut buf);
                    }
                    let _ = conn.write_all(reply.as_bytes());
                }
            });
            (port, hits)
        }
        let (target, target_hits) =
            serve("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".into());
        let (origin, _) = serve(format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{target}/stolen\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        ));
        (format!("http://127.0.0.1:{origin}"), target_hits)
    }

    /// **The long-poll agent never follows a redirect.** The bot token is in the URL PATH, so a
    /// followed redirect hands the operator's bot to whatever host answered the `Location`; the
    /// 3xx comes back as a response (`PollError::from_status` then reads it as transient).
    ///
    /// MUTATION PROOF: delete `.max_redirects(0)` from `long_poll_agent` — the 302 is followed.
    #[test]
    fn the_long_poll_agent_never_follows_a_redirect() {
        let (base, target_hits) = redirecting_origin();
        let resp = super::long_poll_agent(std::time::Duration::from_secs(5))
            .get(&format!("{base}/bot123:test/getUpdates"))
            .call();
        assert_eq!(target_hits.load(std::sync::atomic::Ordering::SeqCst), 0, "target dialled");
        assert_eq!(resp.expect("a 302 is a response").status().as_u16(), 302);
    }
}
