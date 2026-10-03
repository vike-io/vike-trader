//! THE SIGNED-PAYLOAD WIRE GUARD: the exact `/exchange` bytes this build signs and posts, frozen
//! on disk **outside `crates/`**.
//!
//! # What this proves, and — read this first — what it does NOT
//!
//! ⚠ **This is a STABILITY witness, not a correctness oracle** — with exactly one exception, named
//! below. The bytes it holds were RECORDED from this implementation. They prove the payload *did
//! not change*; they do not prove it was ever right. That the keys are right rests on the SDK
//! cross-read in the next section and on real transfers being accepted by the exchange — not on
//! this file.
//!
//! ⚠ **That second half is now a TEST rather than a hope, and it changes nothing here.**
//! `crates/bridges/hyperliquid/tests/hyperliquid_user_signed_smoke.rs` (added 2026-09-14) posts the
//! `usdClassTransfer` and `approveBuilderFee` payloads to the TESTNET `/exchange` and asserts the
//! address Hyperliquid names back is the one the signing key derives — which, for a user-signed
//! action whose signer identity IS an ECDSA recovery over the venue's own EIP-712 digest, means the
//! venue hashed byte-for-byte what we signed. Both actions passed. That is the CORRECTNESS oracle
//! this file's header says it is not, and the two are complements, never substitutes: the smoke
//! needs the network and credentials-free internet egress, so no CI lane runs it, and **this file
//! is what guards these bytes on every PR**. Do not relax an expectation here because the smoke
//! exists, and do not let a green smoke justify regenerating a fixture.
//!
//! ⚠ **A THIRD file joined them later the same day, and it does not change this one either.** The
//! smoke's key used to be minted per run, so its certification evaporated the moment it finished;
//! it is now a FIXED funds-less throwaway, and one certified
//! `(key, nonce, action) -> signature` triple per action is frozen in
//! `fixtures/hyperliquid_signed/venue_certified_user_signed.json` and re-signed OFFLINE, in CI, by
//! `crates/bridges/hyperliquid/tests/hyperliquid_venue_certified_vectors.rs`. That is a
//! CORRECTNESS gate running on every PR, which this file's header rightly says it is not — and it
//! covers two testnet cases at one nonce with one key. The six cases here are the SDK test key, a
//! fixed nonce, BOTH chains, BOTH `toPerp` directions and the two L1 `vaultAddress` shapes, none of
//! which the venue has ever been asked about. Neither file shrinks because the other exists.
//!
//! The exception is [`the_frozen_l1_payload_carries_the_official_sdk_signature`]: the no-vault L1
//! case reproduces the official Rust SDK's `test_limit_order_action_hashing` inputs exactly, so
//! the signature frozen in it IS that SDK's published vector and is checked against it. One case
//! of six. It is here because a file made only of stability pins can drift as a WHOLE — regenerate
//! once and every expectation moves together, with no test left standing to object — and an
//! externally-provenanced value in the middle of it is what makes that visible.
//!
//! That distinction is deliberate, and it is the opposite of its neighbour's.
//! `tests/signing_vectors.rs` is a CORRECTNESS gate: every value in it is transcribed verbatim
//! from the official SDKs' own hardcoded test vectors, and its module doc states the rule this
//! file obeys by NOT pretending to be one — *"Nothing here is invented: if the fixture is wrong
//! the test is meaningless."*
//!
//! **The official SDKs publish no vector for either action covered here.** Measured 2026-09-14
//! against `hyperliquid-dex/hyperliquid-python-sdk@master` and
//! `hyperliquid-dex/hyperliquid-rust-sdk@master`:
//!
//! * `tests/signing_test.py` declares thirteen tests — phantom-agent, five L1 action vectors,
//!   `float_to_int_for_hashing`, `usdSend`, `withdrawFromBridge`, a multi-sig payload shape,
//!   `createSubAccount`, `subAccountTransfer`, `scheduleCancel`. It contains **zero** occurrences
//!   of `class_transfer`, `classTransfer`, `builder`, `toPerp` or `maxFeeRate`.
//! * The Rust SDK's `src/exchange/exchange_client.rs` carries only the three order/cancel hashing
//!   tests this repo already ported into `signing_vectors.rs`.
//!
//! So porting a vector was not available, and MINTING one — recording our own output and labelling
//! it golden — would be a characterization test wearing a correctness test's clothes: if a key were
//! already wrong it would pin the error and stay green forever. This file says what it is instead.
//!
//! # What IS cross-validated against the SDK (by reading, not by signature)
//!
//! The hashed field names and types were read out of the SDK source and match this crate verbatim:
//!
//! * `signing.py`'s `USD_CLASS_TRANSFER_SIGN_TYPES` = `hyperliquidChain:string, amount:string,
//!   toPerp:bool, nonce:uint64` under primaryType `HyperliquidTransaction:UsdClassTransfer`;
//! * `signing.py`'s `sign_approve_builder_fee` types = `hyperliquidChain:string,
//!   maxFeeRate:string, builder:address, nonce:uint64` under
//!   `HyperliquidTransaction:ApproveBuilderFee`;
//! * `signing.py`'s `sign_user_signed_action` sets `signatureChainId = "0x66eee"` and
//!   `hyperliquidChain = "Mainnet"|"Testnet"` on every user-signed action.
//!
//! # The gap this closes
//!
//! `src/transfer.rs`, `src/builder_fee.rs` and `src/transport.rs` pin five wire keys as
//! `#[serde(rename = "…")]` arguments — `toPerp`, `signatureChainId`, `hyperliquidChain`,
//! `maxFeeRate`, `vaultAddress` — and `src/signing/eip712.rs` pins two EIP-712 type strings whose
//! every character is hashed into the digest. Until this file, the only thing checking any of them
//! was a literal in the SAME FILE's `mod tests`. One editing pass rewrites a pin and its expected
//! literal together, and the suite stays green over a silent protocol break — not hypothetical:
//! that is exactly what happened in this repo on 2026-09-13 in
//! `crates/vike-datahub-client/src/proto.rs`, where a corrupted pin survived 1,097 tests and a
//! 22-gate `verify-branch`. The expectations here are therefore not source code: they live in
//! [`FIXTURE`], under `fixtures/`, which a pass over `crates/` cannot reach.
//!
//! The two layers are independent and this file covers both in one comparison, because the frozen
//! payload carries the signature:
//!
//! 1. **Wire spelling** — the serde renames. Not hashed, but the venue re-derives the EIP-712
//!    message from those keys, so a changed one is a rejected (or misrouted) request. A change
//!    moves the payload's JSON key.
//! 2. **Hashed spelling** — the `HyperliquidTransaction:…(…)` type strings in `eip712.rs`, which
//!    feed `typeHash`. A change there produces a perfectly valid signature over a DIFFERENT
//!    message, and moves the payload's `signature`. ⚠ This layer had **no** coverage at all before
//!    this file: `eip712.rs` carries no unit tests by design, `signing_vectors.rs` covers only the
//!    L1 phantom-agent scheme, and `transfer.rs`/`builder_fee.rs`'s own
//!    `signature_recovers_the_signers_address` tests recompute the digest with the SAME function
//!    they are testing, so they are self-consistent and blind to it.
//!
//! # Why `assert_l1` could not be reused
//!
//! `signing_vectors.rs`'s helper signs an `Action` through `Signer::sign_l1_action` — msgpack
//! `action_hash` → phantom `Agent` under the **Exchange** domain (chainId 1337). `usdClassTransfer`
//! and `approveBuilderFee` are user-signed EIP-712: a different domain
//! (`HyperliquidSignTransaction`, chainId `0x66eee`), a different primaryType, no msgpack and no
//! `Action` value to hand it. So this file drives the user-signed entry points directly
//! ([`eip712::sign_usd_class_transfer`] and friends, reached through the crate's public
//! `*_payload` seams) and keeps a separate assertion path. The two L1 cases below DO ride
//! `sign_l1_action`; they are here for `vaultAddress`, which nothing else observes, and — signed
//! at the SDK's own nonce — to carry the correctness anchor described above.
//!
//! # Declared scope
//!
//! The completeness scan covers [`PINNED_SRC`]: `transfer.rs`, `builder_fee.rs`, `transport.rs`.
//! `src/signing/action.rs`'s renames (`a`/`b`/`p`/`s`/`r`/`t`/`c`/`f`/`o`) are deliberately NOT in
//! it: those are msgpack field names that ARE the L1 signature, so `signing_vectors.rs`'s SDK
//! vectors already redden on any change to one — they have an external witness of the stronger,
//! correctness kind. The day another file grows a user-signed `rename`, it joins [`PINNED_SRC`]
//! rather than being assumed covered here.
//!
//! # ⚠ Two of the six cases are compared MODULO JSON KEY ORDER, and the reason is not this crate
//!
//! The L1 pair does not reach the wire as a serialized STRUCT. `src/transport.rs`'s
//! `exchange_body` returns a `serde_json::Value` (`serde_json::to_value(body)`), so the emitted
//! key order is whatever `serde_json::Map` happens to be in THIS build — and that is not a
//! property of this crate's source:
//!
//! * `serde_json`'s `preserve_order` feature swaps `Map`'s backing store from `BTreeMap`
//!   (**sorted** keys) to `IndexMap` (**insertion**, i.e. declaration, order);
//! * `datafusion-physical-plan` declares `serde_json` with `features = ["preserve_order"]` as a
//!   NORMAL dependency (measured against the vendored 55.0.0 manifest, 2026-09-14), and cargo's
//!   resolver-2 unifies features across everything in one invocation's graph;
//! * so `cargo test -p vike-hyperliquid` ALONE emits sorted keys, while the roster lane — which
//!   builds this crate beside the DataFusion consumers — emits declaration order. Same source,
//!   same signature, different bytes.
//!
//! That is exactly how this file went red on `main` after #1796: the fixture was recorded in the
//! narrow configuration and CI ran the wide one. `grouping,orders,type` became `type,orders,
//! grouping` and `a,b,p,r,s,t` became `a,b,p,s,r,t`, with `r`/`s`/`v` **byte-identical in every
//! case** — which is the whole diagnosis, because it says the signed preimage never moved.
//!
//! ⚠ **The SIGNED BYTES do not pass through this `Value` and cannot.** The L1 signature is
//! produced by `Signer::sign_l1_action` from the `Action` STRUCT via `rmp_serde` msgpack —
//! computed BEFORE `to_value` is ever called, and msgpack field order is struct declaration order
//! in every feature configuration. The user-signed pair's digest is an EIP-712 hash over typed
//! fields, with no JSON anywhere. So this is a rendering difference in the request envelope, not
//! a signing difference, and the venue re-derives the action from the parsed JSON by field NAME.
//!
//! The cure keeps the gate: the comparison is byte-for-byte FIRST, and only the two cases named
//! in [`ORDER_UNSTABLE_CASES`] may fall back to a canonical (key-sorted) re-rendering. A changed
//! `#[serde(rename)]` argument changes the key SET, a changed signature changes a VALUE, and
//! neither survives canonicalization — both still redden here. Nothing else is relaxed, and the
//! four other cases stay byte-exact because `transfer.rs` and `builder_fee.rs` serialize their
//! bodies from the struct (each says so at the call, naming this very hazard).
//!
//! # There is no regenerator, deliberately
//!
//! Nothing in this file writes the fixture. A regeneration step is the door a corrupted pin walks
//! in through — regenerate, and the broken bytes become the new expectation. A mismatch prints
//! every case's actual value so a human can see WHAT moved and decide; restoring the source is
//! almost always the right answer. ⚠ Regenerating to chase the key-order diff above would have
//! been the worst available answer: the next build that resolves features the other way reddens
//! it again, in the opposite direction, with the fixture now recorded from whichever lane
//! happened to run last.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;
use vike_hyperliquid::config::Network;
use vike_hyperliquid::consts::USER_SIGNED_CHAIN_ID;
use vike_hyperliquid::signing::action::{Action, LimitParams, OrderAction, OrderKind, OrderWire};
use vike_hyperliquid::signing::{Signer, eip712};
use vike_hyperliquid::{builder_fee, transfer, transport};

/// The frozen payloads.
///
/// ⚠ Spelled as a REPO-RELATIVE path in a module-level `const` and read at RUNTIME rather than
/// through `include_str!` — the same shape `crates/vike-datahub-client/tests/wire_tag_fixtures.rs`
/// uses, and for the same two reasons: it puts the path inside
/// `crates/vike-ops/tests/path_key_gate.rs`'s scan (which excludes `include_*!` arguments by
/// construction), so a moved or deleted fixture reddens a second independent gate; and it keeps
/// the bytes out of any compilation unit a rename pass is editing.
const FIXTURE: &str = "fixtures/hyperliquid_signed/signed_payloads.json";

/// The files whose `#[serde(rename = "…")]` arguments these payloads hold still. Read as TEXT by
/// [`every_pinned_wire_key_appears_in_the_frozen_payloads`] — never parsed, never compiled against.
/// See the module doc's *Declared scope* for what is out and why.
const PINNED_SRC: &[&str] = &[
    "crates/bridges/hyperliquid/src/transfer.rs",
    "crates/bridges/hyperliquid/src/builder_fee.rs",
    "crates/bridges/hyperliquid/src/transport.rs",
];

/// The file whose EIP-712 primaryType strings these digests hold still.
const EIP712_SRC: &str = "crates/bridges/hyperliquid/src/signing/eip712.rs";

/// ⚠ The ONLY cases whose JSON KEY ORDER is not this repo's to pin — see the module doc's
/// *compared MODULO JSON KEY ORDER* section for the mechanism and the measurement.
///
/// Both are built by `src/transport.rs`'s `exchange_body`, which returns a `serde_json::Value`;
/// `serde_json::Map` is a `BTreeMap` or an `IndexMap` depending on whether anything else in the
/// build graph turns on `serde_json/preserve_order`, and `datafusion-physical-plan` does. A case
/// listed here is still compared byte-for-byte first and only falls back to a canonical
/// (key-sorted) rendering if that fails.
///
/// **A case is added here only with a `serde_json::Value` in its production path.** The four
/// user-signed cases are NOT here and must not be: `transfer::usd_class_transfer_payload` and
/// `builder_fee::approve_builder_fee_payload` serialize from the STRUCT precisely so their key
/// order is the SDK's declaration order in every feature configuration — each says so at the
/// call. Listing one would discard a pin the source genuinely holds.
const ORDER_UNSTABLE_CASES: &[&str] = &["l1_limit_order_with_vault", "l1_limit_order_no_vault"];

/// The official Rust SDK's test wallet — the key every other signing test in this crate uses. A
/// real secp256k1 key with no funds and no account; it is public in the SDK's own repository.
const KEY: &str = "e908f86dbb4d55ac876378565aafeabc187f6690f046459397b17d9b9a19688e";

/// The fixed nonce for the USER-SIGNED cases. The whole payload is deterministic — RFC-6979 ECDSA
/// over a fixed digest — so freezing the signature alongside the JSON is stable, and is what makes
/// ONE comparison cover both the wire keys and the hashed type string.
const NONCE: u64 = 1_700_000_000_123;

/// The nonce for the two L1 cases, and NOT an arbitrary one: it is the official Rust SDK's own
/// test nonce (`action.hash(1583838, None)`), the same value `signing_vectors.rs` signs at.
///
/// Choosing it buys a CORRECTNESS anchor inside an otherwise stability-only file. The
/// no-vault case's action, key and nonce are now exactly the SDK's
/// `test_limit_order_action_hashing` inputs, so the signature frozen in that payload must equal the
/// SDK's PUBLISHED hex — asserted by
/// [`the_frozen_l1_payload_carries_the_official_sdk_signature`]. The vault case differs from it in
/// one input only (the vault marker folded into `action_hash`), which is as close as a
/// `vaultAddress` witness can get to a vector nobody published.
const L1_NONCE: u64 = 1_583_838;

/// The official Rust SDK's hardcoded mainnet signature for its `test_limit_order_action_hashing`
/// vector — `0x` ‖ r(32B) ‖ s(32B) ‖ v(1B), transcribed verbatim from
/// `src/exchange/exchange_client.rs`. Identical to the value `signing_vectors.rs` asserts; spelled
/// again here because this file needs it as an INPUT rather than as its own expectation.
const SDK_LIMIT_ORDER_MAINNET_SIG: &str = "0x77957e58e70f43b6b68581f2dc42011fc384538a2e5b7bf42d5b936f19fbb67360721a8598727230f67080efee48c812a6a4442013fd3b0eed509171bef9f23f1c";

/// A fixed builder address for the `approveBuilderFee` cases (an arbitrary well-formed 20-byte
/// address; nothing is approved by signing it offline).
const BUILDER: &str = "0x0c8de5f0362f6e4e9f0a4e3c1e1d2f3a4b5c6d7e";

/// Repo-root-relative read. `CARGO_MANIFEST_DIR` (never CWD), the idiom every fixture suite in
/// this workspace uses. This crate sits at `crates/bridges/hyperliquid`, so the root is three up.
fn read_repo(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{rel} could not be read ({e}).\nThese bytes freeze what this build signs and posts. \
             There is no regenerator and regeneration is not a supported operation — restore it \
             from git."
        )
    })
}

/// One frozen case: the exact `/exchange` POST body, and (for the user-signed pair) the EIP-712
/// digest that body's signature was produced over.
struct Frozen {
    digest: Option<String>,
    payload: String,
}

/// Parse [`FIXTURE`] into `case name -> `[`Frozen`].
fn frozen_cases() -> BTreeMap<String, Frozen> {
    let text = read_repo(FIXTURE);
    let doc: Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{FIXTURE} is not valid JSON: {e}"));
    let cases = doc
        .get("cases")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{FIXTURE} has no `cases` object"));
    cases
        .iter()
        .map(|(name, v)| {
            let payload = v
                .get("payload")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{FIXTURE}: case `{name}` has no string `payload`"))
                .to_string();
            let digest = v.get("digest").and_then(Value::as_str).map(str::to_string);
            (name.clone(), Frozen { digest, payload })
        })
        .collect()
}

/// Render a JSON document with every object's keys in BYTE-SORTED order, recursively.
///
/// ⚠ This normalizes ORDER and nothing else. Key names, values, array order, number spelling and
/// nesting all survive verbatim, so two documents with equal canonical renderings differ in no
/// way a venue can observe — and any `#[serde(rename)]` change (a different key), any signature
/// change (a different value) and any dropped or added field (a different key set) still produce
/// different renderings. It is deliberately spelled here rather than reached for through
/// `Value`'s `PartialEq`: the assertion below compares STRINGS so the failure message can print
/// both sides, and sorting here is independent of whatever map type `serde_json` was built with.
fn canonical(v: &Value) -> String {
    fn go(v: &Value, out: &mut String) {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
                keys.sort_unstable();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String((*k).to_string()).to_string());
                    out.push(':');
                    go(&map[*k], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    go(item, out);
                }
                out.push(']');
            }
            leaf => out.push_str(&leaf.to_string()),
        }
    }
    let mut out = String::new();
    go(v, &mut out);
    out
}

/// Parse one side of a payload comparison, naming which side failed.
fn parse_payload(name: &str, side: &str, text: &str) -> Value {
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("[{name}] the {side} payload is not valid JSON ({e}): {text}"))
}

fn signer(net: Network) -> Signer {
    Signer::from_private_key(KEY, net).expect("the SDK test key is a valid secp256k1 key")
}

/// The SDK's canonical test order (asset 1, buy 3.5 @ 2000.0, Ioc) — the same wire
/// `signing_vectors.rs` signs, so the L1 cases differ from a golden-gated one ONLY in the envelope.
fn base_order() -> Action {
    Action::Order(OrderAction {
        orders: vec![OrderWire {
            asset: 1,
            is_buy: true,
            limit_px: "2000.0".to_string(),
            sz: "3.5".to_string(),
            reduce_only: false,
            order_type: OrderKind::Limit(LimitParams { tif: "Ioc".to_string() }),
            cloid: None,
        }],
        grouping: "na".to_string(),
        builder: None,
    })
}

/// What this build produces, per case name. The inputs live here in Rust; only the EXPECTATIONS
/// come from the fixture.
fn produced() -> BTreeMap<String, Frozen> {
    let chain_id = u128::from(USER_SIGNED_CHAIN_ID);
    let mut out = BTreeMap::new();

    let mut push = |name: &str, digest: Option<[u8; 32]>, payload: Vec<u8>| {
        out.insert(
            name.to_string(),
            Frozen {
                digest: digest.map(|d| format!("0x{}", hex::encode(d))),
                payload: String::from_utf8(payload).expect("an /exchange body is UTF-8 JSON"),
            },
        );
    };

    // ---- usdClassTransfer (user-signed EIP-712) -----------------------------------------------
    // Both directions and both chains: `toPerp` true/false and `hyperliquidChain`
    // Mainnet/Testnet are each a distinct signed message, so one case cannot stand for the other.
    for (name, net, amount, to_perp) in [
        ("usd_class_transfer_mainnet_to_perp", Network::Mainnet, 25.5_f64, true),
        ("usd_class_transfer_testnet_from_perp", Network::Testnet, 100.0_f64, false),
    ] {
        let s = signer(net);
        let digest = eip712::usd_class_transfer_digest(
            net.hyperliquid_chain(),
            &format!("{amount}"),
            to_perp,
            NONCE,
            chain_id,
        );
        let payload = transfer::usd_class_transfer_payload(&s, amount, to_perp, net, NONCE)
            .expect("the transfer body serializes");
        push(name, Some(digest), payload);
    }

    // ---- approveBuilderFee (user-signed EIP-712, same domain, different primaryType) -----------
    for (name, net, rate) in [
        ("approve_builder_fee_mainnet", Network::Mainnet, "0.01%"),
        ("approve_builder_fee_testnet", Network::Testnet, "0%"),
    ] {
        let s = signer(net);
        let digest = eip712::approve_builder_fee_digest(
            net.hyperliquid_chain(),
            rate,
            BUILDER,
            NONCE,
            chain_id,
        );
        let payload = builder_fee::approve_builder_fee_payload(&s, BUILDER, rate, net, NONCE)
            .expect("the approve_builder_fee body serializes");
        push(name, Some(digest), payload);
    }

    // ---- L1 /exchange envelope — here for `vaultAddress` and nothing else ----------------------
    // The WITH-vault case is what carries the key into the frozen bytes; the WITHOUT-vault case
    // pins the other half of the same contract, that an absent vault is OMITTED and never `null`.
    let s = signer(Network::Mainnet);
    let order = base_order();
    let with_vault = exchange_payload(&order, &s, Some([0x11; 20]));
    push("l1_limit_order_with_vault", None, with_vault);
    let no_vault = exchange_payload(&order, &s, None);
    push("l1_limit_order_no_vault", None, no_vault);

    out
}

/// The bytes `HyperliquidTransport::exchange` posts, built with no transport and no network, at
/// [`L1_NONCE`].
fn exchange_payload(action: &Action, signer: &Signer, vault: Option<[u8; 20]>) -> Vec<u8> {
    let body = transport::exchange_body(action, signer, L1_NONCE, vault);
    serde_json::to_vec(&body).expect("an /exchange body re-serializes")
}

/// ⚠ THE WITNESS. Every frozen payload (and, for the user-signed pair, every frozen digest) is
/// still exactly what this build produces.
///
/// Collects EVERY divergence before failing rather than stopping at the first, so one run names
/// everything that moved — a partial report invites a fixture edit that chases the diff.
#[test]
fn the_signed_payloads_this_build_produces_are_the_frozen_ones() {
    let frozen = frozen_cases();
    let actual = produced();

    let missing: Vec<&String> = actual.keys().filter(|k| !frozen.contains_key(*k)).collect();
    let extra: Vec<&String> = frozen.keys().filter(|k| !actual.contains_key(*k)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{FIXTURE} does not describe the same case set this test builds.\n  built but not \
         frozen: {missing:?}\n  frozen but not built: {extra:?}\nA case that is built and not \
         frozen is guarded by nothing; a case that is frozen and not built is a shrunken witness."
    );

    let mut report = String::new();
    for (name, a) in &actual {
        let f = &frozen[name];
        if a.digest != f.digest {
            report.push_str(&format!(
                "\n[{name}] EIP-712 DIGEST MOVED — a `HyperliquidTransaction:…(…)` type string in \
                 {EIP712_SRC} changed, so this build now signs a DIFFERENT MESSAGE while producing \
                 a perfectly valid signature.\n    frozen: {:?}\n    actual: {:?}",
                f.digest, a.digest
            ));
        }
        if a.payload != f.payload {
            // Byte-for-byte is the primary comparison and stays so for every case. Only a case
            // declared ORDER-UNSTABLE may be re-judged, and only against a rendering that
            // normalizes KEY ORDER and nothing else — see the module doc.
            let order_unstable = ORDER_UNSTABLE_CASES.contains(&name.as_str());
            let frozen_c = canonical(&parse_payload(name, "frozen", &f.payload));
            let actual_c = canonical(&parse_payload(name, "actual", &a.payload));
            if !order_unstable || frozen_c != actual_c {
                let note = if order_unstable {
                    "\n    (this case is ORDER-UNSTABLE, so key order alone was excused — the \
                     canonical renderings below still differ, which means a KEY or a VALUE moved)"
                } else {
                    "\n    (this case is NOT in ORDER_UNSTABLE_CASES: it is serialized from a \
                     struct, so its byte order IS this repo's to hold. Do not add it there to \
                     make this green.)"
                };
                report.push_str(&format!(
                    "\n[{name}] PAYLOAD MOVED — a `#[serde(rename = \"…\")]` argument in one of \
                     {PINNED_SRC:?} changed, or the signature under it did.{note}\n    frozen: \
                     {}\n    actual: {}\n    frozen (key-sorted): {frozen_c}\n    actual \
                     (key-sorted): {actual_c}",
                    f.payload, a.payload
                ));
            }
        }
    }

    assert!(
        report.is_empty(),
        "THE SIGNED PAYLOAD CHANGED.{report}\n\nThese bytes are what a real transfer, grant or \
         vault order is signed over and posted as. The frozen side is the authority: restore the \
         source. Editing {FIXTURE} to match makes the break permanent and silent — that is the \
         precise failure this file exists to prevent."
    );
}

/// ⚠ Every [`ORDER_UNSTABLE_CASES`] row still names a case this file builds, and every row is
/// still EARNED.
///
/// A stale row excuses nothing today and everything tomorrow: rename a case and its row silently
/// starts guarding a name nobody emits, while a NEW case reusing that name inherits an excuse
/// nobody argued for it. The second half is the one that matters — a row is only legitimate for a
/// payload built through `serde_json::to_value`, so this test also re-reads `transport.rs` and
/// requires that seam to still be there. If `exchange_body` is ever changed to serialize from the
/// struct (which would make all six cases byte-stable and is the better fix, at the cost of
/// moving the frozen L1 bytes), this goes red and the rows come out.
#[test]
fn every_order_unstable_row_names_a_built_case_and_a_real_to_value_seam() {
    let built = produced();
    let stale: Vec<&&str> =
        ORDER_UNSTABLE_CASES.iter().filter(|c| !built.contains_key(**c)).collect();
    assert!(
        stale.is_empty(),
        "ORDER_UNSTABLE_CASES names case(s) this file does not build: {stale:?}\nDelete the row — \
         it excuses a key order nothing emits, and would silently excuse a future case that \
         reuses the name.\nBuilt: {:?}",
        built.keys().collect::<Vec<_>>()
    );

    let transport = read_repo("crates/bridges/hyperliquid/src/transport.rs");
    assert!(
        transport.contains("serde_json::to_value"),
        "crates/bridges/hyperliquid/src/transport.rs no longer builds its /exchange body through \
         `serde_json::to_value`, so the L1 payload's key order is now this repo's to hold and \
         ORDER_UNSTABLE_CASES has nothing left to excuse.\nRemove the rows and let the witness \
         compare bytes — regenerating the two L1 payloads in {FIXTURE} is then the CORRECT act, \
         because the source changed on purpose rather than the build configuration changing under \
         it."
    );
}

/// THE ONE CORRECTNESS ANCHOR IN THIS FILE: the frozen no-vault L1 payload's signature is the
/// official Rust SDK's own published vector, not something this build minted.
///
/// Everything else here is a stability pin, and a file of stability pins can drift as a whole
/// without any single test noticing — regenerate once and every expectation moves together. This
/// test ties one frozen payload to an EXTERNAL, provenance-bearing value: the SDK's inputs are
/// reproduced exactly (its test key, its action, its nonce), so its published signature is the
/// only signature that payload may carry. A fixture regenerated off a broken signing core cannot
/// satisfy it.
///
/// It reads the signature out of the FROZEN bytes rather than re-signing, so it is a statement
/// about the fixture, not about this build — the witness above is what ties the build to the
/// fixture.
#[test]
fn the_frozen_l1_payload_carries_the_official_sdk_signature() {
    let frozen = &frozen_cases()["l1_limit_order_no_vault"].payload;
    let v: Value = serde_json::from_str(frozen)
        .unwrap_or_else(|e| panic!("{FIXTURE}: l1_limit_order_no_vault payload is not JSON: {e}"));
    let sig = &v["signature"];
    let (r, s) = (
        sig["r"].as_str().expect("frozen signature has r"),
        sig["s"].as_str().expect("frozen signature has s"),
    );
    let vbyte = sig["v"].as_u64().expect("frozen signature has v");
    let combined = format!("0x{}{}{vbyte:02x}", &r[2..], &s[2..]);

    assert_eq!(
        combined, SDK_LIMIT_ORDER_MAINNET_SIG,
        "the frozen L1 payload does not carry the official SDK's published signature for its own \
         test vector.\nThis case reproduces the SDK's exact inputs (key, action, nonce \
         {L1_NONCE}), so the SDK's hex is the ONLY correct value. If this went red alongside a \
         regenerated fixture, the regeneration captured a BROKEN signing core — restore the \
         source, not the fixture."
    );
}

/// ⚠ THE COMPLETENESS HALF for the WIRE keys: every `#[serde(rename)]` argument [`PINNED_SRC`]
/// declares appears verbatim in the frozen payloads.
///
/// Without it the witness above is only as good as the cases' COVERAGE: a pin added later — or one
/// quietly dropped from every payload — would be guarded by nothing while the test stayed green.
/// It reads the sources as TEXT and spells no expected key of its own; the expectation is
/// "whatever the source pins, the frozen bytes carry". So a NEW user-signed key arrives with a
/// frozen payload carrying it, which is the rule this protocol wants anyway.
#[test]
fn every_pinned_wire_key_appears_in_the_frozen_payloads() {
    let mut pins: Vec<(String, &str)> = Vec::new();

    for src in PINNED_SRC {
        let text = read_repo(src);
        // ATTRIBUTE lines only — a `rename` quoted inside a doc comment is prose ABOUT a pin, not
        // a pin, and all three of these files quote their own attributes in doc blocks.
        const OPEN: &str = "rename = \"";
        for line in text.lines() {
            let trimmed = line.trim_start();
            if !trimmed.starts_with("#[serde(") {
                continue;
            }
            let mut rest = trimmed;
            while let Some(i) = rest.find(OPEN) {
                rest = &rest[i + OPEN.len()..];
                let Some(end) = rest.find('"') else { break };
                pins.push((rest[..end].to_string(), src));
                rest = &rest[end..];
            }
        }
    }
    pins.sort();
    pins.dedup();

    assert!(
        !pins.is_empty(),
        "no `#[serde(rename)]` attribute was found in any of {PINNED_SRC:?} — either every wire-key \
         pin was deleted (a permanent wire break) or this scanner stopped matching the sources' \
         shape. Both are failures; neither is a reason to weaken the scan."
    );

    // The payloads are stored as JSON STRINGS inside the fixture, so search the DECODED text.
    let haystack: String =
        frozen_cases().values().map(|f| f.payload.clone()).collect::<Vec<_>>().join("\n");

    let missing: Vec<&(String, &str)> =
        pins.iter().filter(|(k, _)| !haystack.contains(&format!("\"{k}\""))).collect();
    assert!(
        missing.is_empty(),
        "wire key(s) pinned in source that NO frozen payload carries: {missing:?}\nEither a pin's \
         ARGUMENT was changed — restore it, the frozen bytes are the authority — or a NEW pin \
         shipped with no payload covering it, in which case add a case to {FIXTURE} that emits \
         it.\nPins declared: {pins:?}"
    );
}

/// ⚠ THE COMPLETENESS HALF for the HASHED type strings: every `HyperliquidTransaction:…` primaryType
/// [`EIP712_SRC`] declares is named by a frozen case.
///
/// The type string never appears in the posted JSON — it is hashed into `typeHash` — so the payload
/// scan above cannot see it. Recording it in the fixture makes the coverage question answerable:
/// a THIRD user-signed action added to `eip712.rs` without a frozen digest reddens here instead of
/// shipping with its hashed spelling guarded by nothing.
#[test]
fn every_user_signed_primary_type_is_named_by_a_frozen_case() {
    let src = read_repo(EIP712_SRC);
    let frozen = read_repo(FIXTURE);

    // `hash_struct` calls only — the same literal appears in this module's doc comments, and prose
    // about a type string is not a type string.
    const OPEN: &str = "\"HyperliquidTransaction:";
    let mut types: Vec<String> = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if let Some(i) = trimmed.find(OPEN) {
            let rest = &trimmed[i + 1..];
            if let Some(end) = rest.find('"') {
                types.push(rest[..end].to_string());
            }
        }
    }
    types.sort();
    types.dedup();

    assert!(
        !types.is_empty(),
        "no `\"HyperliquidTransaction:…\"` primaryType literal was found in {EIP712_SRC} — either \
         both user-signed schemes were deleted or this scanner stopped matching the source's \
         shape. Both are failures; neither is a reason to weaken the scan."
    );

    let missing: Vec<&String> = types.iter().filter(|t| !frozen.contains(*t)).collect();
    assert!(
        missing.is_empty(),
        "{EIP712_SRC} declares EIP-712 primaryType(s) that {FIXTURE} does not name: \
         {missing:?}\nEvery character of that string is hashed into the digest. Either it was \
         edited — restore it — or a new user-signed action shipped without a frozen case.\nTypes \
         declared: {types:?}"
    );
}
