//! The outside-world seam and its ONE production implementation: the `ureq` long-poll agent, this
//! surface's own [`ControlLimits`] bucket, the core's [`CommandSink`], and the published snapshot
//! cell.
//!
//! Every test in this workstream injects a scripted [`TelegramDeps`] instead, which is why none of
//! them touches the network. The bot token lives here (embedded in `api_base`), so
//! [`ProdTelegramDeps`] deliberately has NO `Debug`.

use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use rand::Rng; // rand 0.10 core trait — provides `fill_bytes` (the same idiom `server::fresh_nonce` uses)
use vike_core::{CommandSink, CoreSnapshot};
use vike_tradehub_client::wire::WireCommand;

use crate::server::{accept_command, ControlLimits, ControlLimitsConfig};

use super::{
    parse_updates, render_read, PollError, ReadVerb, TelegramConfig, TgUpdate, AGENT_TIMEOUT,
    LONG_POLL_SECS,
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
    /// [`crate::server::accept_command`] path. `Ok(coid)` on acceptance.
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
}

impl ProdTelegramDeps {
    /// Build the production deps for `cfg`. `limits` is the SAME [`ControlLimitsConfig`] the daemon
    /// resolved once for the TCP server — one config, one policy, a bucket per surface.
    pub fn new(
        cfg: &TelegramConfig,
        limits: ControlLimitsConfig,
        sink: CommandSink,
        snapshot: Arc<ArcSwap<CoreSnapshot>>,
    ) -> Self {
        ProdTelegramDeps {
            agent: vike_bridge_core::http::blocking_agent_with_timeout(AGENT_TIMEOUT),
            api_base: format!("https://api.telegram.org/bot{}", cfg.bot_token),
            limits: Mutex::new(ControlLimits::new(limits)),
            sink,
            snapshot,
        }
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
        let url = format!("{}/sendMessage", self.api_base);
        let body = serde_json::json!({ "chat_id": chat_id, "text": clamped }).to_string();
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
        self.limits.lock().expect("telegram control limits poisoned").preview_vet(cmd)
    }

    fn accept(&self, cmd: WireCommand, reason: &str) -> Result<String, String> {
        let mut limits = self.limits.lock().expect("telegram control limits poisoned");
        // THE shared path — the identical function the TCP `Request::Command` arm calls. `peer` is
        // `None` because this surface has no socket; the origin rides in `reason` instead. The
        // settings source is `None` too: this channel's parser mints no `SetSetting` (its verb
        // table is the /submit-family), and a `None` here means even a hand-built one would be
        // refused inside `accept_command` rather than writing a file from a chat message. The
        // `key_id` is `None` for the same reason as `peer`: this channel authenticates a Telegram
        // chat id, not a node key, so there is no key to fingerprint — and an absent id is
        // recorded as an absent field rather than as a borrowed one.
        accept_command(cmd, Some(reason), &mut limits, &self.sink, None, None, None)
            .map(crate::server::Accepted::into_coid)
            .map_err(|e| e.message())
    }

    fn read(&self, verb: ReadVerb) -> String {
        // The LOSSY arc-swap publication — never the core fold (the p99 gate's contract).
        let snap = self.snapshot.load_full();
        render_read(verb, &snap)
    }

    fn mint_coid(&self) -> String {
        format!("tg-{}", mint_hex())
    }

    fn mint_token(&self) -> String {
        mint_hex()
    }

    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// 8 CSPRNG bytes as lowercase hex — the confirmation token and the minted coid suffix. `fill_bytes`
/// is the same rand-0.10 entry point `server::fresh_nonce` uses for the handshake nonce; 64 bits is
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
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
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
}
