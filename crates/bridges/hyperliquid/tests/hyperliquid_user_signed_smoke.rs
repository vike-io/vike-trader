//! THE CORRECTNESS ORACLE for the two USER-SIGNED Hyperliquid actions — `usdClassTransfer` and
//! `approveBuilderFee` — asked of the venue itself, on **TESTNET**:
//!
//!     cargo test -p vike-hyperliquid --test hyperliquid_user_signed_smoke -- --ignored --nocapture
//!
//! # Why this file exists
//!
//! `crates/bridges/hyperliquid/tests/signed_payload_fixtures.rs` freezes the bytes these two
//! actions put on the wire, and says in its own module doc that it buys **stability, not
//! correctness**: neither official Hyperliquid SDK publishes a vector for either action, so the
//! frozen bytes were recorded from this implementation and would pin an error just as faithfully
//! as a truth. `crates/bridges/hyperliquid/src/transfer.rs` and
//! `crates/bridges/hyperliquid/src/builder_fee.rs` each carried an `# UNVERIFIED live` section
//! saying the same thing — that a real testnet round-trip was owed before either is trusted. Both
//! now carry a `# LIVE-VERIFIED on testnet` section instead, and this file is what earned it.
//!
//! # ⚠ The proof used to EVAPORATE every run. That is what changed on 2026-09-14.
//!
//! Until then this file minted an EPHEMERAL key per run (clock+pid seeded). The venue's
//! certification was real each time and then unrecoverable: a different key meant a different
//! signature, so nothing could be frozen — and because the network tests are `#[ignore]`d, **no CI
//! lane ever ran any of it**. In CI the two payloads were guarded by the stability fixture alone,
//! which proves the bytes did not change and never that they were right.
//!
//! The key is now **FIXED** — a funds-less throwaway carried in
//! `fixtures/hyperliquid_signed/venue_certified_user_signed.json` — so a certified run produces a
//! `(key, nonce, action) -> signature` triple that can be written down. The pipeline is:
//!
//!   1. [`certify_a_fresh_vector_for_the_offline_gate`] signs with that key at a CURRENT nonce,
//!      posts, asserts the venue names that key's own address, and prints a paste-ready block
//!      carrying the nonce, the exact payload, the digest, the recovered address and the venue's
//!      verbatim reply;
//!   2. the block is pasted into that fixture's `cases`;
//!   3. `crates/bridges/hyperliquid/tests/hyperliquid_venue_certified_vectors.rs` — **OFFLINE, not
//!      `#[ignore]`d, run by every CI lane that builds this crate** — re-signs the same action with
//!      the same key and the same nonce and requires the same signature.
//!
//! A network proof therefore became a permanent gate. ⚠ The NONCE is why step 1 cannot simply be
//! re-run against a frozen value: Hyperliquid refuses a nonce outside a window around now (see the
//! table below), so a re-run certifies a NEW triple rather than re-confirming an old one. That is
//! the whole reason the fixture forbids refreshing a value from this build's own output — the only
//! thing that can re-certify a vector is another testnet run.
//!
//! This file does not replace the stability fixtures and cannot: it needs the network. The fixture
//! answers *did the bytes change*; this answers *were they ever right*; the offline gate carries
//! the second answer into CI. All three stay.
//!
//! # The oracle: Hyperliquid echoes the address it RECOVERED
//!
//! A user-signed HL action carries no sender field. The signer's identity **is** the ECDSA
//! recovery result: the venue rebuilds the EIP-712 typed data from the action's named fields, takes
//! the `\x19\x01`-prefixed digest over the domain separator and the struct hash, and recovers an
//! address from `{r,s,v}` over it. There is therefore no such thing as an "invalid signature" error
//! here — recovery always yields *some* address — and an unknown one comes back **named in the
//! error text**:
//!
//! ```text
//! {"status":"err","response":"Must deposit before performing actions. User: 0x<RECOVERED>"}
//! ```
//!
//! That makes the discriminator exact rather than a reading of prose. **If the address the venue
//! names is the address our own key derives, the venue's digest is byte-identical to ours** — and
//! the digest is a keccak over the whole typed-data construction, so that equality certifies every
//! part of it at once: the `HyperliquidTransaction:…` primaryType string character for character,
//! every field name (`toPerp`, `maxFeeRate`, `hyperliquidChain`, `builder`, `amount`, `nonce`),
//! every field TYPE, the field ORDER inside the type string, the `HyperliquidSignTransaction`
//! domain, and `signatureChainId` `0x66eee` / chainId `421614`. Change any one character and the
//! venue hashes something else, recovers a pseudorandom address instead, and these tests fail.
//! Matching by accident is a ~2^-160 event.
//!
//! # The response table this file discriminates on
//!
//! MEASURED against `https://api.hyperliquid-testnet.xyz/exchange`, 2026-09-14:
//!
//! | what was sent | HTTP | body |
//! |---|---|---|
//! | action whose JSON no longer deserializes (a key renamed, an unknown `type`) | **422** | `Failed to deserialize the JSON body into the target type` |
//! | nonce outside the accepted window | 200 | `{"status":"err","response":"Invalid nonce: nonce too low <n> < <now>"}` |
//! | a nonce already seen for that recovered user | 200 | `{"status":"err","response":"Invalid nonce: duplicate nonce <n>"}` |
//! | **well-formed, signature recovered, signer unknown to the venue** | 200 | `{"status":"err","response":"Must deposit before performing actions. User: 0x<RECOVERED>"}` |
//!
//! The last row is the one these tests land on, and it is a **business** rejection: the venue has
//! already parsed the action, rebuilt the typed data and recovered the signer, and only then looked
//! the account up and found nothing deposited. The first row is the protocol-break negative — it is
//! asserted against explicitly, with its own message, because a 422 means this crate's action shape
//! stopped parsing at the venue.
//!
//! That the echoed address is genuinely RECOVERED (rather than, say, parroted out of the request)
//! was established by measurement and is re-established every run by
//! [`the_oracle_discriminates_a_digest_the_venue_did_not_compute`], the negative control: holding
//! `{r,s}` fixed and flipping only `v` between 27 and 28 makes the venue name two different
//! addresses, as does moving the action's nonce out from under a signature already made.
//!
//! # ⚠ SAFETY — why a FIXED key is no weaker than the ephemeral one it replaced
//!
//! The ephemeral key bought exactly two properties, and a key that HOLDS NOTHING keeps both:
//!
//! 1. **The rejection is deterministic, and it moves nothing.** The oracle IS the `Must deposit`
//!    branch, which the venue takes for any address it has never seen funded. A throwaway that
//!    holds nothing takes that branch every run, and taking it means the venue rejected the action
//!    *before* executing anything. Nothing can move because nothing is there to move — and even
//!    had something been, `usdClassTransfer` is an INTERNAL spot↔perp rebalance that cannot leave
//!    an account and an `approveBuilderFee` grant binds only the signer that made it.
//! 2. **`Env::Live` stays STRUCTURALLY unreachable.** `vike_hyperliquid::config::Env` is never
//!    constructed, never imported and not reachable from any input this file takes, so
//!    `HYPERLIQUID_LIVE_PRIVATE_KEY` / `_ACCOUNT_ADDRESS` and mainnet are unavailable rather than
//!    merely unused. The network is the literal [`Network::Testnet`] at every call site, and
//!    [`only_testnet_endpoints_are_reachable_from_this_file`] pins the URLs that follow from it.
//!    **Do not add a code path that can select an `Env`, a credential or a network from anywhere.**
//!
//! **No credential is read by any test here**, which is why this runs on any box with network
//! egress, including a the CI box lane, and why the demo account is untouched. The key lives in the
//! fixture rather than in this file so that the key the venue certifies and the key the offline
//! gate re-signs with are ONE spelling: two copies could drift apart and both files would stay
//! green, since each is internally self-consistent.
//!
//! The one thing a fixed key gives up is that the address now has a history at the venue. If it is
//! ever FUNDED by a third party the `Must deposit` branch disappears; that shows up as a LOUD
//! failure here (no address named, or a different rejection), never as a false green — and the cure
//! is a new throwaway in the fixture plus a fresh certification run.
//!
//! The four network tests are `#[ignore]`d, so the ordinary suite compiles them and runs none.
//!
//! # What this does NOT prove
//!
//! That an AGENT (API) wallet may sign `usdClassTransfer` for its master — the credential-model
//! verdict in `crates/bridges/hyperliquid/src/transfer.rs`'s module doc — is a venue
//! AUTHORIZATION question, untouched here: an unknown signer is rejected before any
//! agent-approval lookup happens. Same for
//! `approveBuilderFee`'s master-only rule. This file certifies the SIGNING, not the permissioning.

use std::path::PathBuf;

use vike_bridge_core::http::blocking_agent;
use vike_bridge_core::transport::{VenueApiError, read_body_ambiguous};
use vike_hyperliquid::config::Network;
use vike_hyperliquid::consts::{
    MAINNET_EXCHANGE, TESTNET_EXCHANGE, TESTNET_INFO, USER_SIGNED_CHAIN_ID,
};
use vike_hyperliquid::signing::{Signer, eip712};
use vike_hyperliquid::transport::HyperliquidTransport;
use vike_hyperliquid::{builder_fee, transfer};

/// The venue-certified vector file — the home of the throwaway KEY this file signs with, and the
/// destination of what [`certify_a_fresh_vector_for_the_offline_gate`] prints.
/// `crates/bridges/hyperliquid/tests/hyperliquid_venue_certified_vectors.rs` is its offline reader
/// and carries the full provenance rules.
const VECTORS: &str = "fixtures/hyperliquid_signed/venue_certified_user_signed.json";

/// The placeholder builder address the crate's own unit tests and `fixtures/hyperliquid_signed/`
/// already use. Its value is immaterial: a funds-less signer is rejected as unknown long before the
/// venue looks at who is being approved (MEASURED — the probe that produced the table in this
/// module's doc used exactly this address and still got the address echo).
const BUILDER: &str = "0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e";

/// The `usdClassTransfer` inputs every test here signs. `to_perp = true` (spot → perp) and a token
/// amount: a funds-less key holds nothing, so the direction and the size only ever reach the
/// venue's typed-data hash, never its books.
const AMOUNT: f64 = 1.0;
const TO_PERP: bool = true;

/// The `approveBuilderFee` rate every test here signs.
const MAX_FEE_RATE: &str = "0.01%";

/// What a captured case's `primary_type` says until a human fills it in. It is a LABEL for the
/// hashed type string, copied from the source rather than certified — this file deliberately does
/// not carry a second spelling of a string whose every character is hashed into the digest, since a
/// copy here could disagree with `signing/eip712.rs` and nothing would notice. Left unedited, it
/// reddens `hyperliquid_venue_certified_vectors.rs`'s completeness test, which is the intended
/// behaviour: an uncopied label is an uncovered scheme.
const PRIMARY_TYPE_PLACEHOLDER: &str =
    "REPLACE WITH THE primaryType LITERAL FROM crates/bridges/hyperliquid/src/signing/eip712.rs";

/// Repo-root-relative read. `CARGO_MANIFEST_DIR` (never CWD), the idiom every fixture suite in
/// this workspace uses. This crate sits at `crates/bridges/hyperliquid`, so the root is three up.
fn read_repo(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{rel} could not be read ({e}); restore it from git"))
}

/// The FIXED, funds-less THROWAWAY signer — the safety property this whole file rests on (see the
/// SAFETY section above). Read from [`VECTORS`] rather than spelled here so that the key the venue
/// certifies is, by construction, the key the offline gate re-signs with.
fn throwaway_signer() -> Signer {
    let doc: serde_json::Value = serde_json::from_str(&read_repo(VECTORS))
        .unwrap_or_else(|e| panic!("{VECTORS} is not valid JSON: {e}"));
    let key = doc["signer"]["private_key"]
        .as_str()
        .unwrap_or_else(|| panic!("{VECTORS} has no string `signer.private_key`"))
        .to_string();
    Signer::from_private_key(&key, Network::Testnet).unwrap_or_else(|e| {
        panic!(
            "{VECTORS}'s `signer.private_key` is not a valid secp256k1 key ({e}). It is a \
             throwaway whose ONLY required property is that it holds nothing — restore it from \
             git rather than substituting one, since every frozen signature in that file was made \
             with it."
        )
    })
}

/// Every `0x`-prefixed 20-byte address appearing in `text`, lowercased — the venue names the
/// recovered signer inline in a prose error, so the assertion reads the addresses OUT of it rather
/// than pattern-matching the sentence around them (the sentence is the venue's to reword; the
/// address is the evidence).
fn addresses_in(text: &str) -> Vec<String> {
    // Scanned over BYTES throughout (never `&text[a..b]`): the venue's reply is prose and may carry
    // multi-byte characters, which would make a str slice at a fixed offset a panic rather than a
    // non-match.
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0usize;
    while i + 42 <= bytes.len() {
        let body = &bytes[i + 2..i + 42];
        if bytes[i] == b'0'
            && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X')
            && body.iter().all(u8::is_ascii_hexdigit)
        {
            let hex: String =
                body.iter().map(|b| char::from(b.to_ascii_lowercase())).collect::<String>();
            found.push(format!("0x{hex}"));
            i += 42;
            continue;
        }
        i += 1;
    }
    found
}

/// THE ORACLE, over the venue's raw reply text: the reply must name the throwaway signer's OWN
/// address and no other. Returns that address, so a certification run writes down what the venue
/// actually said rather than what this file assumed it would say.
///
/// Panics with the venue's verbatim reply on every failure path, so a red run carries the evidence
/// rather than a boolean.
fn assert_venue_recovered_our_signer_text(
    action: &str,
    signer: &Signer,
    status: u16,
    body: &str,
) -> String {
    assert_ne!(
        status, 422,
        "PROTOCOL BREAK on {action}: the venue could not DESERIALIZE the action this crate sent \
         (HTTP 422: {body}). A key in the action JSON no longer matches what Hyperliquid parses — \
         this is a wire break, not a business rejection. Do NOT adjust \
         fixtures/hyperliquid_signed/signed_payloads.json to match; fix the source."
    );

    let named = addresses_in(body);
    let ours = signer.address().to_ascii_lowercase();

    // `Invalid nonce: …` short-circuits BEFORE recovery (see the table in the module doc), so it
    // names no address and must not be read as either answer.
    assert!(
        !body.contains("Invalid nonce"),
        "{action}: the venue rejected the NONCE before it ever recovered a signer, so this run \
         reached no oracle: {body}"
    );
    assert!(
        !named.is_empty(),
        "{action}: the venue named no address at all, so the oracle this test depends on is \
         gone — re-derive the discriminator before trusting (or distrusting) the payload. If the \
         throwaway key in {VECTORS} has been FUNDED by somebody, the `Must deposit` branch is no \
         longer reachable and that fixture needs a fresh throwaway plus a fresh certification \
         run. Reply: {body}"
    );
    assert!(
        named.iter().all(|a| a == &ours),
        "{action}: SIGNATURE IS WRONG. The venue rebuilt the EIP-712 typed data from the action we \
         sent and recovered {named:?}, but this key derives {ours}. A different recovered address \
         means the venue hashed something other than what we signed — a primaryType string, a \
         field name, a field type, the field order, the domain or the chain id disagrees with \
         Hyperliquid. Reply: {body}"
    );
    ours
}

/// The same oracle over what a PRODUCTION call returned — `Ok` carries HL's `{status, response}`
/// body from a 2xx, `Err` a non-2xx, which for this endpoint means the 422 protocol break.
fn assert_venue_recovered_our_signer(
    action: &str,
    signer: &Signer,
    reply: Result<serde_json::Value, VenueApiError>,
) {
    let (status, body) = match reply {
        Ok(v) => (200u16, v.to_string()),
        Err(e) if e.code == 422 => (422u16, e.msg),
        Err(e) => panic!(
            "{action}: transport failure before any venue verdict was obtained (code {}, {}); \
             this test reached no oracle and proves nothing either way",
            e.code, e.msg
        ),
    };
    assert_venue_recovered_our_signer_text(action, signer, status, &body);
}

/// POST raw `/exchange` bytes to TESTNET and read the verdict back as (status, VERBATIM body).
///
/// A bare agent rather than `HyperliquidTransport::post` (which is `pub(crate)`, so a `tests/`
/// binary cannot call it, and which parses the body into a `Value`): a certification has to record
/// the venue's reply exactly as sent, and the negative control needs to post bytes the production
/// API deliberately offers no way to build. Same rustls agent and same body reader the real
/// transport uses, so only the bytes differ.
fn post_testnet_exchange(payload: &[u8]) -> (u16, String) {
    let agent = blocking_agent();
    let mut resp = agent
        .post(TESTNET_EXCHANGE)
        .header("Content-Type", "application/json")
        .send(payload)
        .expect("testnet /exchange is reachable");
    let status = resp.status().as_u16();
    let text = read_body_ambiguous(&mut resp).expect("response body reads");
    (status, text)
}

/// `usdClassTransfer` (the spot↔perp rebalance; `toPerp` is the DIRECTION field) — signed by
/// `crates/bridges/hyperliquid/src/transfer.rs`, posted by it, and confirmed against the venue.
///
/// This calls the PRODUCTION `transfer::usd_class_transfer`, not a re-spelling of it, so the nonce,
/// the rate-gate charge, the `/exchange` POST and the response handling are all the shipped ones.
/// It certifies the whole shipped entry point and freezes nothing — its nonce is generated inside
/// the call and never surfaces. [`certify_a_fresh_vector_for_the_offline_gate`] is the half that
/// produces a writable vector; the two are complements, and neither covers the other.
#[test]
#[ignore = "network (Hyperliquid TESTNET) — run manually; see the module doc"]
fn usd_class_transfer_signature_is_recovered_by_the_venue_as_ours() {
    vike_log::test_init();
    let signer = throwaway_signer();
    let transport = HyperliquidTransport::new(Network::Testnet);
    println!("throwaway signer {}", signer.address());

    let reply =
        transfer::usd_class_transfer(&transport, &signer, AMOUNT, TO_PERP, Network::Testnet);
    println!("usdClassTransfer reply: {reply:?}");
    assert_venue_recovered_our_signer("usdClassTransfer", &signer, reply);
}

/// `approveBuilderFee` (the standing builder-fee grant; `maxFeeRate` is the rate field) — signed
/// by `crates/bridges/hyperliquid/src/builder_fee.rs`, posted by it, and confirmed against the
/// venue. Twin of the test above.
#[test]
#[ignore = "network (Hyperliquid TESTNET) — run manually; see the module doc"]
fn approve_builder_fee_signature_is_recovered_by_the_venue_as_ours() {
    vike_log::test_init();
    let signer = throwaway_signer();
    let transport = HyperliquidTransport::new(Network::Testnet);
    println!("throwaway signer {}", signer.address());

    let reply = builder_fee::approve_builder_fee(
        &transport,
        &signer,
        BUILDER,
        MAX_FEE_RATE,
        Network::Testnet,
    );
    println!("approveBuilderFee reply: {reply:?}");
    assert_venue_recovered_our_signer("approveBuilderFee", &signer, reply);
}

/// ⚠ THE CERTIFICATION RUN — the only thing that can mint a value for
/// `crates/bridges/hyperliquid/tests/hyperliquid_venue_certified_vectors.rs` to gate on.
///
/// For each action it builds the payload through the PRODUCTION `*_payload` builder (the same
/// function `usd_class_transfer` / `approve_builder_fee` call on their second line) at an EXPLICIT
/// current nonce, posts those exact bytes, and asserts the oracle. The nonce has to be explicit for
/// the result to be writable at all: the production entry points generate one internally and return
/// only the venue's reply, so nothing they certify can be written down.
///
/// It then prints a paste-ready `cases` block. **Paste it; do not compute it.** A value in that
/// fixture regenerated from this build's own output is a stability pin wearing a correctness test's
/// clothes — the exact hole this pipeline closes. See that fixture's `_regeneration` field.
///
/// ⚠ A re-run mints a NEW triple (the nonce must be near `now`, or the venue refuses it before
/// recovery), so this can never re-confirm an already-frozen value. Replacing a case means
/// replacing its date, its recovered address and its venue reply together.
#[test]
#[ignore = "network (Hyperliquid TESTNET) — run manually; see the module doc"]
fn certify_a_fresh_vector_for_the_offline_gate() {
    vike_log::test_init();
    let signer = throwaway_signer();
    let chain_id = u128::from(USER_SIGNED_CHAIN_ID);
    let chain = Network::Testnet.hyperliquid_chain();
    println!("throwaway signer {}", signer.address());

    let mut cases = serde_json::Map::new();

    // ---- usdClassTransfer ----------------------------------------------------------------------
    let nonce = vike_model::clock::now_ms_u64();
    let payload =
        transfer::usd_class_transfer_payload(&signer, AMOUNT, TO_PERP, Network::Testnet, nonce)
            .expect("the production payload builder serializes");
    let (status, body) = post_testnet_exchange(&payload);
    println!("usdClassTransfer [nonce {nonce}] -> HTTP {status} {body}");
    let recovered =
        assert_venue_recovered_our_signer_text("usdClassTransfer", &signer, status, &body);
    let digest =
        eip712::usd_class_transfer_digest(chain, &format!("{AMOUNT}"), TO_PERP, nonce, chain_id);
    cases.insert(
        "usd_class_transfer_testnet_to_perp".to_string(),
        serde_json::json!({
            "action": "usdClassTransfer",
            "primary_type": PRIMARY_TYPE_PLACEHOLDER,
            "network": chain,
            "amount": AMOUNT,
            "to_perp": TO_PERP,
            "nonce": nonce,
            "digest": format!("0x{}", hex::encode(digest)),
            "payload": String::from_utf8(payload).expect("an /exchange body is UTF-8 JSON"),
            "certified_on": "REPLACE WITH THE RUN DATE (YYYY-MM-DD)",
            "recovered_address": recovered,
            "venue_response": body,
        }),
    );

    // ---- approveBuilderFee ---------------------------------------------------------------------
    let nonce = vike_model::clock::now_ms_u64();
    let payload = builder_fee::approve_builder_fee_payload(
        &signer,
        BUILDER,
        MAX_FEE_RATE,
        Network::Testnet,
        nonce,
    )
    .expect("the production payload builder serializes");
    let (status, body) = post_testnet_exchange(&payload);
    println!("approveBuilderFee [nonce {nonce}] -> HTTP {status} {body}");
    let recovered =
        assert_venue_recovered_our_signer_text("approveBuilderFee", &signer, status, &body);
    let digest = eip712::approve_builder_fee_digest(chain, MAX_FEE_RATE, BUILDER, nonce, chain_id);
    cases.insert(
        "approve_builder_fee_testnet".to_string(),
        serde_json::json!({
            "action": "approveBuilderFee",
            "primary_type": PRIMARY_TYPE_PLACEHOLDER,
            "network": chain,
            "builder": BUILDER,
            "max_fee_rate": MAX_FEE_RATE,
            "nonce": nonce,
            "digest": format!("0x{}", hex::encode(digest)),
            "payload": String::from_utf8(payload).expect("an /exchange body is UTF-8 JSON"),
            "certified_on": "REPLACE WITH THE RUN DATE (YYYY-MM-DD)",
            "recovered_address": recovered,
            "venue_response": body,
        }),
    );

    let block = serde_json::to_string_pretty(&serde_json::json!({
        "signer_address": signer.address(),
        "cases": cases,
    }))
    .expect("the capture block serializes");
    println!(
        "\n===== PASTE INTO {VECTORS} (`signer.address` and `cases`), THEN SET certified_on \
         =====\n{block}\n===== END OF CAPTURE =====\n"
    );
}

/// THE NEGATIVE CONTROL — without it the tests above are a green nobody can size.
///
/// It proves the echoed address is RECOVERED from the signature over the venue's OWN digest, and is
/// therefore capable of disagreeing with us: a payload is built through the production
/// `transfer::usd_class_transfer_payload` at one nonce and then has the action's nonce moved out
/// from under the signature, which is exactly what a wrong field name or a wrong type string would
/// do — make the venue hash something other than what was signed. The venue must then name an
/// address that is NOT ours.
#[test]
#[ignore = "network (Hyperliquid TESTNET) — run manually; see the module doc"]
fn the_oracle_discriminates_a_digest_the_venue_did_not_compute() {
    vike_log::test_init();
    let signer = throwaway_signer();
    let ours = signer.address().to_ascii_lowercase();

    // A nonce inside the venue's accepted window, so the run reaches recovery rather than the
    // `Invalid nonce` short-circuit.
    let signed_nonce = vike_model::clock::now_ms_u64();
    let payload = transfer::usd_class_transfer_payload(
        &signer,
        AMOUNT,
        TO_PERP,
        Network::Testnet,
        signed_nonce,
    )
    .expect("the production payload builder serializes");

    // Move the action's nonce by one AFTER signing. The signature is untouched and still perfectly
    // valid — over a digest the venue will not compute.
    let mut body: serde_json::Value =
        serde_json::from_slice(&payload).expect("the production payload is JSON");
    let sent_nonce = signed_nonce + 1;
    body["action"]["nonce"] = serde_json::json!(sent_nonce);
    body["nonce"] = serde_json::json!(sent_nonce);
    let mangled = serde_json::to_vec(&body).expect("re-serializes");

    let (_status, text) = post_testnet_exchange(&mangled);
    println!("mangled-nonce reply: {text}");

    let named = addresses_in(&text);
    assert!(
        !text.contains("Invalid nonce") && !named.is_empty(),
        "the negative control reached no recovery, so it proved nothing: {text}"
    );
    assert!(
        named.iter().all(|a| a != &ours),
        "THE ORACLE HAS NO TEETH: the venue named our own address {ours} for a signature made over \
         a DIFFERENT digest ({named:?}). The tests above cannot be trusted until this is \
         understood — the echoed address is evidently not a recovery result. Reply: {text}"
    );
}

/// A belt-and-braces pin on the SAFETY property this file's doc claims: the only Hyperliquid hosts
/// reachable from here are the testnet pair. No `Env` is constructed anywhere in this file, so
/// `Env::Live` cannot be selected by any input; this asserts the consequence — that the network
/// literal used at every call site above resolves to testnet and to nothing else.
///
/// Not `#[ignore]`d and touches no network: it is a compile-time-shaped fact and costs nothing to
/// run in the ordinary suite.
#[test]
fn only_testnet_endpoints_are_reachable_from_this_file() {
    let (info, exchange, _ws) = Network::Testnet.urls();
    assert_eq!(exchange, TESTNET_EXCHANGE);
    assert_eq!(info, TESTNET_INFO);
    assert_ne!(exchange, MAINNET_EXCHANGE);
    assert!(exchange.contains("testnet"), "the signed-action endpoint must be testnet: {exchange}");
    assert_eq!(Network::Testnet.hyperliquid_chain(), "Testnet");
}
