//! THE PERMISSIONING ORACLE for the two USER-SIGNED Hyperliquid actions — asked of the venue
//! itself, on **TESTNET**, with a signer the venue KNOWS:
//!
//!     cargo test -p vike-hyperliquid --test hyperliquid_permissioning_smoke \
//!         -- --ignored --nocapture --test-threads=1
//!
//! # Why this file exists — and why the smoke beside it could not do this
//!
//! `crates/bridges/hyperliquid/tests/hyperliquid_user_signed_smoke.rs` certified the SIGNING of
//! `usdClassTransfer` and `approveBuilderFee`: the venue rebuilds the EIP-712 typed data from the
//! bytes this crate sends and recovers OUR address, so its digest is byte-identical to ours. It
//! signs with an **ephemeral key minted in-process**, which is what makes that oracle deterministic
//! — and what puts permissioning out of its reach, since an address the venue has never seen is
//! answered with `Must deposit before performing actions. User: 0x<RECOVERED>`. So two claims about
//! WHO may sign went on standing unverified:
//!
//! * `crates/bridges/hyperliquid/src/transfer.rs` asserted an **agent (API) wallet MAY sign
//!   `usdClassTransfer`** for its master — the whole reason that action looked actionable in this
//!   app's credential model, which holds an agent key and a master ADDRESS, never a master key;
//! * `crates/bridges/hyperliquid/src/builder_fee.rs` asserts `approveBuilderFee` is
//!   **MASTER WALLET ONLY**.
//!
//! # THE RESULT (MEASURED 2026-09-14, testnet `/exchange`, the demo agent key)
//!
//! **The first claim is FALSE and the second holds.** Both actions, signed by an address the venue
//! itself reports as an APPROVED AGENT of a funded master, came back:
//!
//! ```text
//! {"status":"err","response":"Must deposit before performing actions. User: 0x<THE AGENT>"}
//! ```
//!
//! — and the address the venue named is **the agent's own**, never the master it is an approved
//! agent of. (The literal addresses are deliberately not written down here: they are account state,
//! they belong in the run output, and a doc that names them goes stale the day the account does.)
//!
//! ⚠ **That the message mentions deposits is not what carries the verdict, and reading it as a
//! balance complaint is the trap this file was written to avoid.** What carries it is WHICH ACCOUNT
//! the venue named. A user-signed action has no sender field, so the address in that reply is the
//! principal the venue resolved the signature to. It named the SIGNER. The venue therefore applies
//! a user-signed action to the recovered address's OWN account and never to the master it is an
//! approved agent of — whatever the check that then failed happened to be about. An agent wallet
//! holds nothing by construction, so this is not a state the account can be funded out of.
//!
//! The discriminator is re-run inside each test rather than argued: each posts the SAME action a
//! second time under a FRESHLY MINTED key the venue has never seen — `/info userRole` must answer
//! `{"role":"missing"}` for it, asserted, so the two signers genuinely sit at opposite ends of what
//! the venue knows — and asserts the two replies are the same verdict, each naming its own signer.
//! A venue-approved agent and a total stranger are treated identically, so **agent approval is not
//! consulted for a user-signed action at all**. That is the positive form of the finding, and it is
//! what makes the negative readable.
//!
//! ⚠ **And the master is a FUNDED account, which is what rules out the last alternative reading.**
//! Given a reply that says "Must deposit", somebody will eventually ask whether the venue resolved
//! the agent to its master after all and the MASTER is the one that never deposited. It is not:
//! [`demo_agent`] asserts the master's `/info userNonFundingLedgerUpdates` is non-empty before
//! anything is signed. So the account the deposit check failed on cannot have been the master's,
//! and the venue named the agent's.
//!
//! # The response taxonomy this file discriminates on
//!
//! The first three rows are `hyperliquid_user_signed_smoke.rs`'s, MEASURED 2026-09-14. They are
//! NON-ANSWERS, and each is asserted against explicitly: a 422 is a protocol break in the action
//! SHAPE, `Invalid nonce` short-circuits at the venue BEFORE signature recovery, and a reply naming
//! NO address reaches no principal at all. A run landing on one fails loudly rather than reading as
//! a verdict.
//!
//! | what was sent | HTTP | body |
//! |---|---|---|
//! | action whose JSON no longer deserializes | **422** | `Failed to deserialize the JSON body into the target type` |
//! | nonce outside the accepted window / already seen | 200 | `{"status":"err","response":"Invalid nonce: …"}` |
//! | well-formed, recovered, signer UNKNOWN to the venue | 200 | `{"status":"err","response":"Must deposit before performing actions. User: 0x<RECOVERED>"}` |
//! | **`usdClassTransfer` signed by an APPROVED AGENT of a funded master** | 200 | `{"status":"err","response":"Must deposit before performing actions. User: 0x<THE AGENT>"}` |
//! | **`approveBuilderFee` signed by that same APPROVED AGENT** | 200 | *identical to the row above* — and `/info maxBuilderFee(master, builder)` stays `0` |
//!
//! The last two rows are this file's, MEASURED 2026-09-14 against
//! `https://api.hyperliquid-testnet.xyz/exchange`. Nothing on the account moved: the venue refused
//! both, so no transfer executed and no grant was made.
//!
//! # ⚠ SAFETY — testnet, demo tier, and the LIVE credentials structurally out of reach
//!
//! 1. **The LIVE keys are never in the map the loader sees.** [`demo_only_vars`] reads the store and
//!    then RETAINS only keys under the `HYPERLIQUID_DEMO` prefix, so `HYPERLIQUID_LIVE_PRIVATE_KEY`
//!    / `_ACCOUNT_ADDRESS` are discarded before `vike_hyperliquid::config::load` is called. Even a
//!    hypothetical caller asking this file's loader for the live tier would be handed a map that
//!    cannot answer, and `Env::Live` is never written, constructed or reachable from any input here.
//!    ⚠ This matters BECAUSE the store is shared: the sanctioned way to run this is
//!    `VIKE_SETTINGS_DIR` pointing at a real settings directory, which is the one that also holds
//!    the live keys. The filter, not the choice of directory, is what keeps them out.
//! 2. **`Env::Demo` ⇒ testnet, and that is asserted rather than assumed.** Every network call site
//!    takes the literal [`Network::Testnet`]; [`the_demo_tier_is_testnet_and_only_testnet`] pins the
//!    tier-to-network mapping and the URLs that follow from it, and runs in the ordinary suite with
//!    no network.
//! 3. **Nothing moved, and nothing was granted — measured, not intended.** The venue refused both
//!    actions. [`an_agent_wallet_cannot_sign_usd_class_transfer_for_its_master`] nonetheless still
//!    carries a full transfer-and-restore path for the branch where the venue ACCEPTS: that would
//!    mean the rule changed under us, and the test restores the account BEFORE failing on it.
//!    `usdClassTransfer` is an INTERNAL spot↔perp rebalance that cannot leave an account in either
//!    direction, which is why that one branch may execute rather than merely be refused.
//!    [`approve_builder_fee_refuses_a_signer_that_is_not_the_master`] has the mirror-image path: on
//!    an unexpected acceptance it re-approves at `0%` to revoke, then fails. The builder it names is
//!    [`BUILDER`], the placeholder the crate's unit tests and frozen fixtures already use — an
//!    address nobody operates, so even an unrevoked grant to it authorizes a fee no order can carry.
//!
//! # Why `--test-threads=1`
//!
//! Every network test signs with the SAME demo key, and each action takes its nonce from the wall
//! clock in milliseconds (`transfer::usd_class_transfer` and `builder_fee::approve_builder_fee` both
//! call `vike_model::clock::now_ms_u64`). Two actions from one signer inside one millisecond collide
//! on `Invalid nonce: duplicate nonce` — a non-answer this file refuses to read as a verdict, so it
//! would go red for a reason with nothing to do with permissioning. Serialise them. (The ephemeral
//! control inside each test is a different signer, and nonce dedup is per recovered user, so it does
//! not contend with the agent's.)
//!
//! # What this file does NOT prove
//!
//! **That the MASTER succeeds.** Every refusal here is of a NON-master signer, and this credential
//! store holds no master key — by design, it is the agent-wallet model. So "master-only" is
//! established in its NEGATIVE half (an approved agent is refused, and the grant does not appear)
//! and not in its positive one. The same gap applies to `usdClassTransfer`: what is proven is that
//! the agent cannot do it for the master, not that the master can.
//!
//! Mainnet, by construction. And the non-master signer this account can offer is an approved AGENT;
//! a sub-account and a vault are other principals the venue distinguishes, and neither is reachable
//! from here.
//!
//! # ⚠ THE PREMISE EXPIRES, and that is the venue's doing rather than a bug here
//!
//! An `approveAgent` grant is TIME-BOXED — `/info extraAgents` carries a `validUntil`, and the one
//! these tests were measured against was about thirty days out. When the store's agent lapses,
//! `userRole` stops answering `agent`, [`demo_agent`] panics naming exactly that, and every test
//! here goes red without reaching the venue's `/exchange` at all.
//!
//! That is the design: a premise that has decayed must not read as a pass, and these tests are
//! worth nothing the moment their signer is an ordinary stranger. The fix is to re-approve an agent
//! for the account, or to point the store at one — never to relax the assertion. Nothing in this
//! file can do it: `approveAgent` is not implemented in this crate, and making the grant would
//! itself be a permission grant, which these tests deliberately never make.

use std::collections::HashMap;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_bridge_core::transport::VenueApiError;
use vike_hyperliquid::config::{self, Env, HlCredentials, Network};
use vike_hyperliquid::consts::{MAINNET_EXCHANGE, TESTNET_EXCHANGE, TESTNET_INFO};
use vike_hyperliquid::signing::{Signer, eip712};
use vike_hyperliquid::transport::HyperliquidTransport;
use vike_hyperliquid::{builder_fee, transfer};

/// The ONLY credential-key prefix this file will read. Everything else in the store — the LIVE
/// hyperliquid pair above all — is dropped by [`demo_only_vars`] before the loader sees it.
const DEMO_PREFIX: &str = "HYPERLIQUID_DEMO";

/// How much USDC the transfer test would move spot → perp and straight back, in the branch where
/// the venue accepts (it does not — see the module doc). Small enough to be immaterial against the
/// demo account's balance, large enough to be unambiguous in HL's own six-decimal spot reporting.
const TRANSFER_USDC: f64 = 1.0;

/// The placeholder builder address the crate's unit tests and `fixtures/hyperliquid_signed/`
/// already use. Chosen deliberately for a NEGATIVE test: nobody operates this address, so in the
/// branch where the venue unexpectedly ACCEPTS, what was granted is permission to charge a fee that
/// no order can ever carry.
const BUILDER: &str = "0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e";

/// The rate asked for in the negative test — the smallest expressible, for the same reason.
const MAX_FEE_RATE: &str = "0.001%";

/// The rate a revocation asks for, used only in the branch where the venue accepted a grant this
/// file did not want (see the SAFETY section).
const REVOKE_FEE_RATE: &str = "0%";

/// How long a balance poll waits for the venue's books to reflect an accepted transfer.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The credential store, **filtered to the demo tier**, as the loader will see it.
///
/// The `retain` is the safety property this whole file rests on, not tidiness: a real settings
/// directory also holds `HYPERLIQUID_LIVE_PRIVATE_KEY` / `_ACCOUNT_ADDRESS`, and a filtered map
/// cannot answer for a tier whose keys are not in it. `VIKE_SETTINGS_DIR` is honoured exactly as
/// `crates/bridges/hyperliquid/tests/hyperliquid_reconcile_smoke.rs`'s `load_demo_creds` honours it.
fn demo_only_vars() -> HashMap<String, String> {
    let mut vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    vars.retain(|k, _| k.starts_with(DEMO_PREFIX));
    vars
}

/// The double-gate every live smoke uses: `None` (after a `tracing::warn!`) when the demo key is
/// absent, so the caller self-skips with `let Some(..) = .. else { return };`.
fn demo_creds() -> Option<HlCredentials> {
    let vars = demo_only_vars();
    // Belt-and-braces on the filter above: assert on what is actually about to be handed over.
    assert!(
        vars.keys().all(|k| k.starts_with(DEMO_PREFIX)),
        "the demo-only filter let a non-demo key through: {:?}",
        vars.keys().collect::<Vec<_>>()
    );
    let creds = config::load(Env::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(
            target: "vike_hyperliquid",
            "SKIP: {DEMO_PREFIX}_PRIVATE_KEY absent from the credential store (point \
             VIKE_SETTINGS_DIR at the settings directory holding it)"
        );
    }
    creds
}

/// What every network test needs, with the premise already PROVEN rather than assumed: a testnet
/// transport, the demo signer, and the master address the venue itself agrees that signer is an
/// approved agent of.
struct DemoAgent {
    transport: HyperliquidTransport,
    signer: Signer,
    /// The master account, lowercased — the account an agent-signed action would have to reach.
    master: String,
}

/// Load the demo credentials, build the testnet transport, and ESTABLISH the premise with a keyless
/// `/info` read before anything is signed. `None` only when the store has no demo key (self-skip);
/// every other failure panics, because a test whose premise is void must not read as a pass.
fn demo_agent() -> Option<DemoAgent> {
    let creds = demo_creds()?;
    // `Env::Demo => Network::Testnet` (config.rs). Asserted, not trusted — this is the one place
    // where a wrong answer would put a signed action on mainnet.
    assert!(
        matches!(creds.network, Network::Testnet),
        "the DEMO tier must resolve to testnet; refusing to sign anything otherwise"
    );
    let transport = HyperliquidTransport::new(Network::Testnet);

    let signer = Signer::from_private_key(&creds.private_key, Network::Testnet)
        .expect("HYPERLIQUID_DEMO_PRIVATE_KEY must be a valid secp256k1 key");
    let signer_addr = signer.address().to_ascii_lowercase();

    // ⚠ THE MASTER COMES FROM THE VENUE, not from the store, and the direction matters. The store's
    // `_ACCOUNT_ADDRESS` is OPTIONAL (`config.rs`: absent means the key IS the account), so a store
    // carrying only the private key would otherwise make this file unrunnable — and, worse, the
    // store is OUR claim about the key while `/info userRole` is the venue's. The venue's answer is
    // the premise; a declared address is checked AGAINST it below rather than believed.
    let role = user_role(&transport, &signer_addr);
    assert_eq!(
        role.get("role").and_then(Value::as_str),
        Some("agent"),
        "the venue does not know the demo signer as an AGENT, so the premise of every test in this \
         file is void: a refusal below could then be nothing more than the answer any unknown \
         address gets. /info userRole({signer_addr}) said: {role}"
    );
    let master = role
        .get("data")
        .and_then(|d| d.get("user"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| {
            panic!("the venue called {signer_addr} an agent but named no master: {role}")
        });

    tracing::info!(
        target: "vike_hyperliquid",
        "demo signer {signer_addr} · master (per the venue) {master}"
    );

    assert_ne!(
        signer_addr, master,
        "the demo private key derives the MASTER address, so it is not a non-master signer and \
         neither question in this file can be asked of it"
    );
    if let Some(declared) = creds.account_address.as_deref() {
        assert_eq!(
            declared.to_ascii_lowercase(),
            master,
            "HYPERLIQUID_DEMO_ACCOUNT_ADDRESS names a different master than the venue does — the \
             store would have this key trade an account it is not an agent of"
        );
    }

    let master_role = user_role(&transport, &master);
    assert_eq!(
        master_role.get("role").and_then(Value::as_str),
        Some("user"),
        "the master the venue named is not itself a plain user account. /info userRole({master}) \
         said: {master_role}"
    );

    // ⚠ THE LOOPHOLE THIS CLOSES, and it is the last one in the argument. Both actions come back
    // `Must deposit before performing actions`, so somebody will eventually ask: what if the venue
    // DID resolve the agent to its master, and the MASTER is the one that has never deposited? The
    // master's ledger answers it — a non-empty `userNonFundingLedgerUpdates` means this account has
    // been funded, so "must deposit" cannot be a statement about it. Combined with the venue naming
    // the AGENT rather than the master, the check was applied to the signer's own account.
    let ledger = transport
        .info(&json!({ "type": "userNonFundingLedgerUpdates", "user": master, "startTime": 0 }))
        .expect("testnet /info userNonFundingLedgerUpdates is reachable");
    let funding_events = ledger.as_array().map(Vec::len).unwrap_or(0);
    assert!(
        funding_events > 0,
        "the master has NO funding history, so `Must deposit` below could legitimately be about IT \
         rather than about the signer, and this file's whole conclusion would be unsupported. \
         /info userNonFundingLedgerUpdates({master}) said: {ledger}"
    );
    tracing::info!(
        target: "vike_hyperliquid",
        "master {master} has {funding_events} non-funding ledger events — it is a funded account, \
         so `Must deposit` cannot be about it"
    );

    Some(DemoAgent { transport, signer, master })
}

/// A THROWAWAY secp256k1 key, minted for one use — the CONTROL each test runs its action under a
/// second time. A fresh address is one the venue has never seen, holds nothing and is approved for
/// nothing; if it draws the same verdict as an address the venue reports as an approved agent, then
/// agent approval played no part in that verdict.
///
/// Seeded from the wall clock, the pid and a caller-supplied tag, stretched through the crate's own
/// `keccak256` — no RNG dependency, and different on every call, so a reused address cannot collide
/// with its own earlier nonces. Lifted from `hyperliquid_user_signed_smoke.rs`'s `ephemeral_signer`,
/// where the same construction is the safety property that file rests on.
fn ephemeral_signer(tag: &str) -> Signer {
    let seed = format!(
        "vike-hl-permissioning-smoke:{tag}:{}:{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the unix epoch")
            .as_nanos(),
        std::process::id()
    );
    let mut material = eip712::keccak256(seed.as_bytes());
    loop {
        match Signer::from_private_key(&hex::encode(material), Network::Testnet) {
            Ok(signer) => return signer,
            Err(_) => material = eip712::keccak256(&material),
        }
    }
}

/// Keyless `POST /info {"type":"userRole","user":…}`.
fn user_role(transport: &HyperliquidTransport, user: &str) -> Value {
    transport
        .info(&json!({ "type": "userRole", "user": user }))
        .expect("testnet /info userRole is reachable")
}

/// The CONTROL's other half: the venue must genuinely not know this address.
///
/// The comparison each test ends on — approved agent and stranger draw the same verdict — is only
/// evidence if the two signers actually differ in what the venue knows about them. `userRole`
/// answers `{"role":"missing"}` for an address it has never seen and `{"role":"agent", …}` for the
/// demo signer, so this asserts the control is the far end of that scale rather than, say, a
/// freshly-minted key that happened to collide with something.
fn assert_venue_does_not_know(transport: &HyperliquidTransport, signer: &Signer) {
    let addr = signer.address().to_ascii_lowercase();
    let role = user_role(transport, &addr);
    assert_eq!(
        role.get("role").and_then(Value::as_str),
        Some("missing"),
        "the CONTROL signer {addr} is not unknown to the venue, so comparing its verdict against \
         the approved agent's proves nothing about whether agent approval was consulted. /info \
         userRole said: {role}"
    );
}

/// The account's spot USDC total, from `spotClearinghouseState`.
fn spot_usdc(transport: &HyperliquidTransport, user: &str) -> f64 {
    let v = transport
        .info(&json!({ "type": "spotClearinghouseState", "user": user }))
        .expect("testnet /info spotClearinghouseState is reachable");
    v.get("balances")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter().find(|r| r.get("coin").and_then(Value::as_str) == Some("USDC"))
        })
        .and_then(|r| r.get("total"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("no parseable spot USDC row for {user}: {v}"))
}

/// The account's perp `marginSummary.accountValue`, from `clearinghouseState`.
fn perp_account_value(transport: &HyperliquidTransport, user: &str) -> f64 {
    let v = transport
        .info(&json!({ "type": "clearinghouseState", "user": user }))
        .expect("testnet /info clearinghouseState is reachable");
    v.get("marginSummary")
        .and_then(|m| m.get("accountValue"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("no parseable perp accountValue for {user}: {v}"))
}

/// Keyless `POST /info {"type":"maxBuilderFee","user":…,"builder":…}` — the venue's record of what
/// this account has approved this builder to charge, kept as a raw JSON value so a shape change is
/// visible rather than coerced. This is the OUTCOME check behind the builder-fee test: an error
/// reply is the venue's word, an unchanged `maxBuilderFee` is its books.
fn max_builder_fee(transport: &HyperliquidTransport, user: &str, builder: &str) -> Value {
    transport
        .info(&json!({ "type": "maxBuilderFee", "user": user, "builder": builder }))
        .expect("testnet /info maxBuilderFee is reachable")
}

/// Poll until `probe` reports a value within `tol` of `want`, or [`SETTLE_TIMEOUT`] elapses.
/// Returns the last value seen either way — the caller decides whether a timeout is fatal, because
/// on a restore path it is a stranded balance to report rather than merely a failed assertion.
fn settle_to(label: &str, want: f64, tol: f64, mut probe: impl FnMut() -> f64) -> f64 {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    let mut last = probe();
    while (last - want).abs() > tol {
        if Instant::now() >= deadline {
            break;
        }
        sleep(Duration::from_millis(500));
        last = probe();
    }
    tracing::info!(target: "vike_hyperliquid", "{label}: want {want}, settled at {last}");
    last
}

/// Reduce a `/exchange` reply to the venue's verdict, panicking on every outcome that is NOT a
/// verdict — so a run that reached no oracle cannot read as either answer.
///
/// HTTP 422 is a protocol break in the action SHAPE (the venue stopped parsing what this crate
/// sends); any other transport error means no verdict was obtained at all; and `Invalid nonce`
/// short-circuits at the venue BEFORE signature recovery, so it names no principal.
fn venue_verdict(action: &str, reply: Result<Value, VenueApiError>) -> Value {
    match reply {
        Ok(v) => {
            let text = v.to_string();
            assert!(
                !text.contains("Invalid nonce"),
                "{action}: the venue rejected the NONCE before it recovered a signer, so this run \
                 reached no permissioning verdict at all. Re-run serially (`--test-threads=1`). \
                 Reply: {text}"
            );
            v
        }
        Err(e) if e.code == 422 => panic!(
            "PROTOCOL BREAK on {action}: the venue could not DESERIALIZE the action this crate \
             sent (HTTP 422: {}). That is a wire break in the action SHAPE, not a permissioning \
             answer — fix the source, and do not touch the frozen fixtures to match.",
            e.msg
        ),
        Err(e) => panic!(
            "{action}: transport failure before any venue verdict was obtained (code {}, {}); this \
             run proves nothing either way",
            e.code, e.msg
        ),
    }
}

/// `{"status":"ok", …}` — the venue accepted and applied the action.
fn accepted(verdict: &Value) -> bool {
    verdict.get("status").and_then(Value::as_str) == Some("ok")
}

/// Every `0x`-prefixed 20-byte address in `text`, lowercased. The venue names the principal it
/// resolved inline in a prose error, so the assertions read the addresses OUT of it rather than
/// pattern-matching the sentence around them: the sentence is the venue's to reword, the address is
/// the evidence. Scanned over BYTES throughout (never `&text[a..b]`) — the reply is prose and may
/// carry multi-byte characters, which would make a `str` slice at a fixed offset a panic rather
/// than a non-match.
fn addresses_in(text: &str) -> Vec<String> {
    address_spans(text).into_iter().map(|(s, e)| text[s..e].to_ascii_lowercase()).collect()
}

/// The BYTE SPANS of every address in `text` — the one scanner [`addresses_in`] and
/// [`redact_addresses`] both build on.
///
/// ⚠ **It exists because factoring it out was the fix for a real defect.** `redact_addresses` used
/// to take the LOWERCASED strings from `addresses_in` and `str::replace` them out of the ORIGINAL
/// text, so a checksummed (mixed-case, EIP-55) address matched nothing and was left in place. That
/// fails safe rather than silently — the control comparison then sees two un-redacted replies naming
/// different signers and goes red — but it goes red for the wrong reason, which is the shape of
/// failure this whole file exists to avoid. Every match is pure ASCII, so both ends of a span are
/// char boundaries and slicing `text` with them cannot panic.
fn address_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0usize;
    while i + 42 <= bytes.len() {
        let body = &bytes[i + 2..i + 42];
        if bytes[i] == b'0'
            && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X')
            && body.iter().all(u8::is_ascii_hexdigit)
        {
            found.push((i, i + 42));
            i += 42;
            continue;
        }
        i += 1;
    }
    found
}

/// ⚠ **THE ASSERTION THE WHOLE FILE TURNS ON.** A user-signed HL action carries no sender field, so
/// the address the venue names back is the principal it resolved the signature to. Requiring that
/// address to be the SIGNER — and, explicitly, never the `master` it is an approved agent of — is
/// what converts a prose rejection into a statement about authorization.
///
/// Read it in that order: it is not "the venue complained about a deposit". It is "the venue
/// applied this action to the SIGNER's own account". The deposit is merely what that account then
/// failed; an agent wallet holds nothing by construction, so it is not a state it can be funded out
/// of, and no amount of balance on the master changes the account the action landed on.
fn assert_principal_was_the_signer(action: &str, signer: &Signer, master: &str, verdict: &Value) {
    let body = verdict.to_string();
    let named = addresses_in(&body);
    let ours = signer.address().to_ascii_lowercase();

    assert!(
        !named.is_empty(),
        "{action}: the venue named no address at all, so it reached no principal and this run is \
         not a verdict. Reply: {body}"
    );
    assert!(
        named.iter().all(|a| a == &ours),
        "{action}: the venue named {named:?}, which is neither this signer ({ours}) nor nothing — \
         the oracle this file depends on has changed shape and must be re-derived before any \
         verdict here is trusted. Reply: {body}"
    );
    assert!(
        !named.iter().any(|a| a == master),
        "{action}: the venue named the MASTER ({master}). That is the opposite of the measured \
         behaviour and would mean a user-signed action DOES resolve an agent to its master — \
         re-derive this file's conclusion and correct transfer.rs/builder_fee.rs accordingly. \
         Reply: {body}"
    );
}

/// ═══ THE PREMISE ═══ — the demo credential is an APPROVED AGENT of a funded master, per the venue.
///
/// Read-only and keyless: three `/info` calls, no signature, nothing moved. It is broken out as its
/// own test because it is the fact the others rest on, and a failure here should be read as "the
/// account changed shape", never as "hyperliquid changed its permissioning".
#[test]
#[ignore = "network (Hyperliquid TESTNET) + demo creds — run manually; see the module doc"]
fn the_demo_signer_is_an_agent_wallet_the_venue_knows() {
    vike_log::test_init();
    let Some(agent) = demo_agent() else { return };

    // `demo_agent` has asserted every part of the premise already; this test exists so that fact
    // has a NAME in the run output. Report the account state it was established against.
    let spot = spot_usdc(&agent.transport, &agent.master);
    let perp = perp_account_value(&agent.transport, &agent.master);
    tracing::info!(
        target: "vike_hyperliquid",
        "PREMISE HELD: signer {} is an approved agent of master {} (spot USDC {spot}, perp \
         accountValue {perp})",
        agent.signer.address(),
        agent.master
    );
    assert!(
        spot > TRANSFER_USDC,
        "the master must hold more spot USDC than the transfer test would move: {spot}"
    );
}

/// ═══ A ═══ — `crates/bridges/hyperliquid/src/transfer.rs`'s credential-model verdict, asked of the
/// venue and **FALSIFIED**: an agent (API) wallet may NOT sign `usdClassTransfer` for its master.
///
/// Signs with the demo AGENT key — an address the venue resolves to the funded master, proven in
/// `demo_agent` — and posts through the PRODUCTION `transfer::usd_class_transfer`, so the nonce, the
/// rate-gate charge, the `/exchange` POST and the response handling are the shipped ones.
///
/// MEASURED 2026-09-14: refused, with the venue naming **the agent's own address**. The control
/// below is what makes that readable rather than arguable — the same action under a key the venue
/// has never seen draws the same verdict naming ITS address, so the approval the master granted
/// this agent bought it nothing at all.
///
/// ⚠ The ACCEPTED branch is kept live and restores the account before failing. It is not dead code:
/// it is what must happen if Hyperliquid ever starts resolving agents here, and a test that had
/// quietly dropped the restore would then move funds and leave them moved.
#[test]
#[ignore = "network (Hyperliquid TESTNET) + demo creds — run manually; see the module doc"]
fn an_agent_wallet_cannot_sign_usd_class_transfer_for_its_master() {
    vike_log::test_init();
    let Some(agent) = demo_agent() else { return };
    let t = &agent.transport;
    let master = agent.master.clone();

    let before_spot = spot_usdc(t, &master);
    let before_perp = perp_account_value(t, &master);
    tracing::info!(
        target: "vike_hyperliquid",
        "BEFORE: spot USDC {before_spot}, perp accountValue {before_perp}"
    );

    // spot → perp, signed by the AGENT. If the module's claim were right, this would move the
    // MASTER's balances.
    let out = venue_verdict(
        "usdClassTransfer(spot→perp) signed by the agent",
        transfer::usd_class_transfer(t, &agent.signer, TRANSFER_USDC, true, Network::Testnet),
    );
    tracing::info!(target: "vike_hyperliquid", "usdClassTransfer spot→perp reply: {out}");

    if accepted(&out) {
        // The venue's rule changed. RESTORE FIRST — retried — then fail on it.
        let mid_perp =
            settle_to("perp after spot→perp", before_perp + TRANSFER_USDC, 1e-6, || {
                perp_account_value(t, &master)
            });
        let mut restored = false;
        for attempt in 1..=3u32 {
            let back = transfer::usd_class_transfer(
                t,
                &agent.signer,
                TRANSFER_USDC,
                false,
                Network::Testnet,
            );
            tracing::info!(
                target: "vike_hyperliquid",
                "usdClassTransfer perp→spot (attempt {attempt}) reply: {back:?}"
            );
            if let Ok(v) = &back
                && accepted(v)
            {
                restored = true;
                break;
            }
            sleep(Duration::from_secs(2));
        }
        let after_spot =
            settle_to("spot after perp→spot", before_spot, 1e-6, || spot_usdc(t, &master));
        let after_perp = settle_to("perp after perp→spot", before_perp, 1e-6, || {
            perp_account_value(t, &master)
        });
        assert!(
            restored
                && (after_spot - before_spot).abs() < 1e-6
                && (after_perp - before_perp).abs() < 1e-6,
            "⚠ THE ACCOUNT WAS NOT RESTORED: {TRANSFER_USDC} USDC moved spot→perp and the reverse \
             leg did not complete. spot {before_spot} → {after_spot}, perp {before_perp} → \
             {after_perp} (perp read {mid_perp} mid-test). Move it back by hand before re-running."
        );
        panic!(
            "THE VENUE NOW ACCEPTS an agent-signed usdClassTransfer for its master. That is the \
             OPPOSITE of the behaviour measured on 2026-09-14, and it reverses this file's \
             conclusion and transfer.rs's corrected module doc. The account was restored. Reply: \
             {out}"
        );
    }

    // The measured branch. The venue refused — and named the SIGNER, not the master.
    assert_principal_was_the_signer(
        "usdClassTransfer signed by the agent",
        &agent.signer,
        &master,
        &out,
    );

    // ── THE CONTROL. The same action under a key the venue has never seen. If agent approval were
    // consulted at all, these two could not draw the same verdict.
    let stranger = ephemeral_signer("usd-class-transfer-control");
    assert_venue_does_not_know(t, &stranger);
    let control = venue_verdict(
        "usdClassTransfer signed by a stranger",
        transfer::usd_class_transfer(t, &stranger, TRANSFER_USDC, true, Network::Testnet),
    );
    tracing::info!(
        target: "vike_hyperliquid",
        "CONTROL — stranger {} usdClassTransfer reply: {control}",
        stranger.address()
    );
    assert!(!accepted(&control), "a key the venue has never seen was ACCEPTED: {control}");
    assert_principal_was_the_signer(
        "usdClassTransfer signed by a stranger",
        &stranger,
        &master,
        &control,
    );
    assert_eq!(
        redact_addresses(&out.to_string()),
        redact_addresses(&control.to_string()),
        "the approved AGENT and a total STRANGER drew DIFFERENT verdicts, so agent approval IS \
         consulted here and this file's reasoning needs re-deriving. agent: {out} · stranger: \
         {control}"
    );

    // ...and the master's books never moved.
    assert!(
        (spot_usdc(t, &master) - before_spot).abs() < 1e-6
            && (perp_account_value(t, &master) - before_perp).abs() < 1e-6,
        "the venue refused the transfer yet the master's balances moved"
    );
    tracing::info!(
        target: "vike_hyperliquid",
        "FALSIFIED: an approved agent of {master} cannot sign usdClassTransfer for it — the venue \
         applied the action to the AGENT's own account, and answered a stranger identically"
    );
}

/// ═══ B ═══ — `crates/bridges/hyperliquid/src/builder_fee.rs`'s **MASTER WALLET ONLY** rule, asked
/// of the venue as the only informative form it has: the NEGATIVE.
///
/// The signer is the same approved agent test A uses, which is the design: it removes every
/// alternative explanation for a refusal except the one under test, and it lets the two tests be
/// read together — the two actions behave IDENTICALLY, which is itself the correction owed to
/// `builder_fee.rs`'s doc (it justified master-only by CONTRASTING this action with
/// `usdClassTransfer`, and there is no contrast).
///
/// Two independent outcomes are asserted: the venue's WORD (an error naming the signer, never the
/// master) and the venue's BOOKS (`/info maxBuilderFee(master, builder)` unchanged). The second is
/// what rules out a grant having been recorded against the master anyway.
///
/// ⚠ The ACCEPTED branch revokes at [`REVOKE_FEE_RATE`] and then fails: a `MASTER WALLET ONLY` rule
/// that turns out not to hold is a finding, not a pass.
#[test]
#[ignore = "network (Hyperliquid TESTNET) + demo creds — run manually; see the module doc"]
fn approve_builder_fee_refuses_a_signer_that_is_not_the_master() {
    vike_log::test_init();
    let Some(agent) = demo_agent() else { return };
    let t = &agent.transport;
    let master = agent.master.clone();

    let before = max_builder_fee(t, &master, BUILDER);
    tracing::info!(target: "vike_hyperliquid", "maxBuilderFee before: {before}");

    let verdict = venue_verdict(
        "approveBuilderFee signed by the agent",
        builder_fee::approve_builder_fee(t, &agent.signer, BUILDER, MAX_FEE_RATE, Network::Testnet),
    );
    tracing::info!(target: "vike_hyperliquid", "approveBuilderFee reply: {verdict}");

    if accepted(&verdict) {
        // The rule is falsified. Undo the grant, then fail — in that order.
        let revoke = builder_fee::approve_builder_fee(
            t,
            &agent.signer,
            BUILDER,
            REVOKE_FEE_RATE,
            Network::Testnet,
        );
        tracing::warn!(target: "vike_hyperliquid", "revocation attempt reply: {revoke:?}");
        let after = max_builder_fee(t, &master, BUILDER);
        panic!(
            "⚠ THE MASTER-ONLY RULE DOES NOT HOLD: Hyperliquid ACCEPTED an `approveBuilderFee` \
             signed by an APPROVED AGENT of {master}, not by the master key. builder_fee.rs's \
             `# ⚠ MASTER WALLET ONLY` section is wrong and must be corrected. A grant was made and \
             a revocation at {REVOKE_FEE_RATE} was attempted: maxBuilderFee({BUILDER}) was \
             {before}, is now {after}. Reply: {verdict}"
        );
    }

    // The venue's WORD: refused, naming the signer — so the grant it declined to make was the
    // AGENT's, and the master was never the principal.
    assert_principal_was_the_signer(
        "approveBuilderFee signed by the agent",
        &agent.signer,
        &master,
        &verdict,
    );

    // ── THE CONTROL, as in test A: a key the venue has never seen draws the same verdict.
    let stranger = ephemeral_signer("approve-builder-fee-control");
    assert_venue_does_not_know(t, &stranger);
    let control = venue_verdict(
        "approveBuilderFee signed by a stranger",
        builder_fee::approve_builder_fee(t, &stranger, BUILDER, MAX_FEE_RATE, Network::Testnet),
    );
    tracing::info!(
        target: "vike_hyperliquid",
        "CONTROL — stranger {} approveBuilderFee reply: {control}",
        stranger.address()
    );
    assert!(!accepted(&control), "a key the venue has never seen was ACCEPTED: {control}");
    assert_eq!(
        redact_addresses(&verdict.to_string()),
        redact_addresses(&control.to_string()),
        "the approved AGENT and a total STRANGER drew DIFFERENT verdicts, so agent approval IS \
         consulted here and this file's reasoning needs re-deriving. agent: {verdict} · stranger: \
         {control}"
    );

    // The venue's BOOKS: nothing was recorded against the master either way.
    let after = max_builder_fee(t, &master, BUILDER);
    assert_eq!(
        after, before,
        "the venue REFUSED the agent's approveBuilderFee yet maxBuilderFee({BUILDER}) moved from \
         {before} to {after}"
    );
    tracing::info!(
        target: "vike_hyperliquid",
        "MASTER-ONLY HELD (negative half): the venue refused an approveBuilderFee from an approved \
         agent of {master}, naming the agent as the principal, and maxBuilderFee is unchanged at \
         {after}"
    );
}

/// Replace every address in a venue reply with a fixed placeholder, so two replies that differ only
/// in WHICH signer they name compare equal. That is exactly the comparison the controls above want:
/// "same verdict, each naming its own principal".
fn redact_addresses(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for (start, end) in address_spans(text) {
        out.push_str(&text[last..start]);
        out.push_str("0x<SIGNER>");
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

/// The SAFETY pin, and the only line of this file the ordinary suite executes: the DEMO tier
/// resolves to testnet and to nothing else, and the endpoints that follow from it are the testnet
/// pair. No network, no credential, no signature.
#[test]
fn the_demo_tier_is_testnet_and_only_testnet() {
    assert!(matches!(Env::Demo.network(), Network::Testnet));
    assert_eq!(Env::Demo.prefix(), DEMO_PREFIX);

    let (info, exchange, _ws) = Network::Testnet.urls();
    assert_eq!(exchange, TESTNET_EXCHANGE);
    assert_eq!(info, TESTNET_INFO);
    assert_ne!(exchange, MAINNET_EXCHANGE);
    assert!(exchange.contains("testnet"), "the signed-action endpoint must be testnet: {exchange}");
    assert_eq!(Network::Testnet.hyperliquid_chain(), "Testnet");

    // The demo-only filter, exercised over a map carrying both tiers — the property the SAFETY
    // section claims, checked rather than described. (`demo_only_vars` reads the real store; this
    // proves the predicate it applies.)
    let planted: HashMap<String, String> = [
        ("HYPERLIQUID_DEMO_PRIVATE_KEY", "demo"),
        ("HYPERLIQUID_LIVE_PRIVATE_KEY", "live"),
        ("HYPERLIQUID_LIVE_ACCOUNT_ADDRESS", "live"),
        ("BINANCE_LIVE_API_KEY", "other"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let kept: Vec<&String> = planted.keys().filter(|k| k.starts_with(DEMO_PREFIX)).collect();
    assert_eq!(kept.len(), 1, "exactly one demo key survives the filter: {kept:?}");
    assert_eq!(kept[0], "HYPERLIQUID_DEMO_PRIVATE_KEY");

    // The address scanner the verdicts are read with, and the redaction the controls compare on.
    // A SYNTHETIC address, mixed-case on purpose: the venue's replies are prose, and the scanner
    // must lowercase what it finds because every address compared against it is lowercased.
    let reply =
        "Must deposit before performing actions. User: 0xAbCdEf0123456789aBcDeF0123456789AbCdEf01";
    assert_eq!(addresses_in(reply), vec!["0xabcdef0123456789abcdef0123456789abcdef01".to_string()]);
    assert_eq!(redact_addresses(reply), "Must deposit before performing actions. User: 0x<SIGNER>");
    assert_eq!(addresses_in("no addresses here"), Vec::<String>::new());
    assert_eq!(redact_addresses("no addresses here"), "no addresses here");

    // ⚠ The case this file's own control comparison rests on, and the one the first version of
    // `redact_addresses` got wrong: two replies that differ ONLY in the address they name must
    // redact to the same string — including when one is checksummed and the other is not.
    let lower = "Must deposit. User: 0xabcdef0123456789abcdef0123456789abcdef01 now";
    let mixed = "Must deposit. User: 0x0123456789AbCdEf0123456789aBcDeF01234567 now";
    assert_eq!(redact_addresses(lower), redact_addresses(mixed));
    assert_eq!(redact_addresses(lower), "Must deposit. User: 0x<SIGNER> now");

    // Two addresses in one reply, and text on both sides of each.
    let two = "a 0x1111111111111111111111111111111111111111 b \
               0x2222222222222222222222222222222222222222 c";
    assert_eq!(addresses_in(two).len(), 2);
    assert_eq!(redact_addresses(two), "a 0x<SIGNER> b 0x<SIGNER> c");

    // Multi-byte characters around a match — the reason the scanner walks BYTES and slices on
    // spans it produced itself rather than on fixed offsets.
    let unicode = "⚠ refused — User: 0xabcdef0123456789abcdef0123456789abcdef01 ⚠";
    assert_eq!(addresses_in(unicode).len(), 1);
    assert_eq!(redact_addresses(unicode), "⚠ refused — User: 0x<SIGNER> ⚠");
}
