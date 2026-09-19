//! ⚠ THE CORRECTNESS GATE for Hyperliquid's two USER-SIGNED actions — **offline, not `#[ignore]`d,
//! and therefore run by every CI lane that builds this crate**.
//!
//! # What this is, and why it is not the file next door
//!
//! `crates/bridges/hyperliquid/tests/signed_payload_fixtures.rs` freezes bytes RECORDED FROM THIS
//! IMPLEMENTATION. It proves the payload did not change and — its own module doc says so — cannot
//! prove it was ever right; a value minted from our own output would pin an error as faithfully as
//! a truth. `crates/bridges/hyperliquid/tests/hyperliquid_user_signed_smoke.rs` asks the VENUE
//! instead, and gets a real answer, but it needs the network and is `#[ignore]`d, so no CI lane
//! ever runs it: in CI those two payloads were guarded by a stability pin alone.
//!
//! This file closes that gap. `fixtures/hyperliquid_signed/venue_certified_user_signed.json` holds
//! `(key, nonce, action) -> signature` triples that **Hyperliquid itself confirmed on testnet**, by
//! naming back the address it recovered from each exact signature; every test here re-signs the
//! same action with the same key at the same nonce and requires the same bytes. A network proof
//! became a permanent gate.
//!
//! # ⚠ The one rule that keeps this a CORRECTNESS gate
//!
//! **A value in that fixture may never be refreshed from this build's own output.** Regenerating it
//! would silently turn every test below back into a stability pin — it would freeze whatever this
//! build now produces and go on calling it certified, which is precisely the hole this file exists
//! to close. The only thing that can re-certify a case is another TESTNET run of the smoke's
//! `certify_a_fresh_vector_for_the_offline_gate`, which posts to the venue, asserts the recovered
//! address, and prints a replacement block; its nonce, its recovered address, its verbatim
//! `venue_response` and its `certified_on` then move together. **A red test here means the SOURCE
//! moved: restore the source.**
//!
//! ⚠ The nonce is what makes that rule structural rather than a preference. Each frozen nonce is a
//! wall-clock ms from the certification run, and Hyperliquid refuses a nonce outside a window
//! around now *before* it recovers a signer — so a frozen case can never be re-posted, and a re-run
//! mints a NEW triple rather than re-confirming an old one.
//!
//! # The provenance, and how it differs from `signing_vectors.rs`'s
//!
//! Both files carry external authority and the two kinds are not interchangeable.
//! `crates/bridges/hyperliquid/tests/signing_vectors.rs`'s values are authoritative because the
//! venue PUBLISHED them: anyone can re-read them from the SDK, forever. These are authoritative
//! because the venue CONFIRMED them in one dated run, and the written record of that run — the
//! date, the recovered address and the verbatim reply, all in the fixture — is the only evidence
//! that exists. That is the perishable kind, which is why this file also gates the provenance
//! fields themselves and not just the bytes.
//!
//! # Safety
//!
//! Nothing here touches the network, reads a credential or constructs a `config::Env`. The key it
//! signs with is the fixture's funds-less throwaway; signing offline moves nothing anywhere.

use std::path::PathBuf;

use serde_json::Value;
use vike_hyperliquid::config::Network;
use vike_hyperliquid::consts::USER_SIGNED_CHAIN_ID;
use vike_hyperliquid::signing::{Signer, eip712};
use vike_hyperliquid::{builder_fee, transfer};

/// The venue-certified vectors. ⚠ Spelled as a REPO-RELATIVE path in a module-level `const` and
/// read at RUNTIME rather than through `include_str!` — the same shape the stability fixture next
/// door uses, and for the same two reasons: it puts the path inside
/// `crates/vike-ops/tests/path_key_gate.rs`'s scan (which excludes `include_*!` arguments by
/// construction), so a moved or deleted fixture reddens a second independent gate; and it keeps the
/// bytes out of any compilation unit a rename pass is editing.
const VECTORS: &str = "fixtures/hyperliquid_signed/venue_certified_user_signed.json";

/// The file whose EIP-712 primaryType strings these signatures certify. Scanned as TEXT by
/// [`every_user_signed_primary_type_has_a_venue_certified_case`].
const EIP712_SRC: &str = "crates/bridges/hyperliquid/src/signing/eip712.rs";

/// The only network a certified case may name: a vector can only come from the testnet
/// `/exchange`, because that is the only host the smoke may post to.
const CERTIFIED_NETWORK: &str = "Testnet";

/// Repo-root-relative read. `CARGO_MANIFEST_DIR` (never CWD), the idiom every fixture suite in
/// this workspace uses. This crate sits at `crates/bridges/hyperliquid`, so the root is three up.
fn read_repo(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{rel} could not be read ({e}).\nIt holds the only record of what Hyperliquid \
             certified, and there is no regenerator — restore it from git."
        )
    })
}

fn vectors() -> Value {
    serde_json::from_str(&read_repo(VECTORS))
        .unwrap_or_else(|e| panic!("{VECTORS} is not valid JSON: {e}"))
}

/// Every case, as `(name, body)`, sorted so a failure report reads the same way twice.
fn cases(doc: &Value) -> Vec<(String, Value)> {
    let map = doc
        .get("cases")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{VECTORS} has no `cases` object"));
    assert!(
        !map.is_empty(),
        "{VECTORS} carries NO cases, so every test in this file passes vacuously. A file of zero \
         certified vectors is not a correctness gate — restore it from git, or re-certify with \
         the smoke's certify_a_fresh_vector_for_the_offline_gate."
    );
    let mut out: Vec<(String, Value)> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The fixture's throwaway signer. Built from the fixture's own `signer.private_key`, which is the
/// ONE spelling of that key in the repository — the smoke reads it from here too, so the key the
/// venue certified and the key this gate re-signs with cannot drift apart.
fn signer() -> Signer {
    let doc = vectors();
    let key = doc["signer"]["private_key"]
        .as_str()
        .unwrap_or_else(|| panic!("{VECTORS} has no string `signer.private_key`"))
        .to_string();
    Signer::from_private_key(&key, Network::Testnet).unwrap_or_else(|e| {
        panic!(
            "{VECTORS}'s `signer.private_key` is not a valid secp256k1 key ({e}). Every frozen \
             signature in that file was made with it — restore it from git rather than \
             substituting one."
        )
    })
}

fn field<'a>(case: &'a Value, name: &str, which: &str) -> &'a str {
    case.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{VECTORS}: case `{which}` has no string `{name}`"))
}

fn nonce_of(case: &Value, which: &str) -> u64 {
    case.get("nonce")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{VECTORS}: case `{which}` has no integer `nonce`"))
}

/// Rebuild a case through the PRODUCTION payload builder, returning `(payload, digest)`.
///
/// The dispatch is exhaustive by panic: an unrecognised `action` must not pass silently, because a
/// case nothing rebuilds is a case nothing gates.
fn rebuild(case: &Value, which: &str, s: &Signer) -> (String, String) {
    let network = field(case, "network", which);
    assert_eq!(
        network, CERTIFIED_NETWORK,
        "{VECTORS}: case `{which}` names network `{network}`. A certified vector can only come \
         from the TESTNET /exchange — the smoke posts nowhere else, by construction — so this case \
         cannot be what it claims to be."
    );
    let nonce = nonce_of(case, which);
    let chain_id = u128::from(USER_SIGNED_CHAIN_ID);

    let (payload, digest) = match field(case, "action", which) {
        "usdClassTransfer" => {
            let amount = case
                .get("amount")
                .and_then(Value::as_f64)
                .unwrap_or_else(|| panic!("{VECTORS}: case `{which}` has no number `amount`"));
            let to_perp = case
                .get("to_perp")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| panic!("{VECTORS}: case `{which}` has no bool `to_perp`"));
            (
                transfer::usd_class_transfer_payload(s, amount, to_perp, Network::Testnet, nonce)
                    .expect("the production transfer payload serializes"),
                eip712::usd_class_transfer_digest(
                    CERTIFIED_NETWORK,
                    &format!("{amount}"),
                    to_perp,
                    nonce,
                    chain_id,
                ),
            )
        }
        "approveBuilderFee" => {
            let builder = field(case, "builder", which);
            let rate = field(case, "max_fee_rate", which);
            (
                builder_fee::approve_builder_fee_payload(s, builder, rate, Network::Testnet, nonce)
                    .expect("the production builder-fee payload serializes"),
                eip712::approve_builder_fee_digest(
                    CERTIFIED_NETWORK,
                    rate,
                    builder,
                    nonce,
                    chain_id,
                ),
            )
        }
        other => panic!(
            "{VECTORS}: case `{which}` names action `{other}`, which this gate cannot rebuild. A \
             case nothing rebuilds is certified by the venue and gated by nobody — add the arm \
             here, do NOT drop the case."
        ),
    };

    (
        String::from_utf8(payload).expect("an /exchange body is UTF-8 JSON"),
        format!("0x{}", hex::encode(digest)),
    )
}

/// ⚠ THE GATE. Every signature Hyperliquid certified is still exactly what this build produces from
/// the same key, the same nonce and the same action.
///
/// Collects EVERY divergence before failing rather than stopping at the first, so one run names
/// everything that moved — a partial report invites a fixture edit that chases the diff.
#[test]
fn this_build_still_produces_the_signature_hyperliquid_certified() {
    let doc = vectors();
    let s = signer();
    let mut report = String::new();

    for (name, case) in cases(&doc) {
        let (payload, digest) = rebuild(&case, &name, &s);
        let frozen_payload = field(&case, "payload", &name);
        let frozen_digest = field(&case, "digest", &name);
        let certified_on = field(&case, "certified_on", &name);

        if digest != frozen_digest {
            report.push_str(&format!(
                "\n[{name}] EIP-712 DIGEST MOVED — a `HyperliquidTransaction:…(…)` type string in \
                 {EIP712_SRC} changed, so this build now signs a DIFFERENT MESSAGE from the one \
                 Hyperliquid certified on {certified_on} while producing a perfectly valid \
                 signature.\n    certified: {frozen_digest}\n    this build: {digest}"
            ));
        }
        // ⚠ A BYTE comparison, and it is safe here for a reason that had to be MEASURED rather
        // than assumed — its sibling `signed_payload_fixtures.rs` carries the same shape and
        // reddened on `main` for exactly this. That file's two L1 cases reach the wire through
        // `src/transport.rs`'s `exchange_body`, which returns a `serde_json::Value`, and
        // `serde_json::Map` is a `BTreeMap` or an `IndexMap` depending on whether anything in the
        // build graph enables `serde_json/preserve_order` (`datafusion-physical-plan` does) — so
        // identical source emits different key order in a narrow `-p` build and in the roster
        // lane. NEITHER case here goes through a `Value`: both
        // `transfer::usd_class_transfer_payload` and `builder_fee::approve_builder_fee_payload`
        // serialize straight from their body STRUCT (each says so at the call), and struct field
        // order is declaration order in every feature configuration. So these bytes are this
        // repo's to hold and the comparison stays exact. If a case is ever added here whose
        // payload is built through `to_value`, it needs that file's `ORDER_UNSTABLE_CASES`
        // treatment — not a relaxed comparison over the whole file.
        if payload != frozen_payload {
            report.push_str(&format!(
                "\n[{name}] CERTIFIED PAYLOAD MOVED — the bytes Hyperliquid recovered \
                 {} from on {certified_on} are not the bytes this build now \
                 produces.\n    certified: {frozen_payload}\n    this build: {payload}",
                field(&case, "recovered_address", &name)
            ));
        }
    }

    assert!(
        report.is_empty(),
        "THE VENUE-CERTIFIED SIGNATURE CHANGED.{report}\n\nThis is not a stability pin. Hyperliquid \
         rebuilt the EIP-712 typed data from these exact bytes and recovered THIS key's own \
         address from the signature under them, which is what certified the primaryType string, \
         every field name and type, the field order, the domain and the chain id. A difference \
         here means the source no longer produces what the venue agreed with.\n\n⚠ DO NOT \
         regenerate {VECTORS}. There is no regenerator, and refreshing a value from this build's \
         own output converts this gate back into a stability pin — the exact hole it was built to \
         close. Restore the source. If you genuinely intend a wire change, the ONLY way to move \
         these values is a fresh TESTNET certification run (see the file's `_regeneration`)."
    );
}

/// The address half of the certification: Hyperliquid named an address, and it must still be the
/// one this key derives.
///
/// Separate from the signature gate because it fails for a different reason: the signatures can be
/// byte-identical while address DERIVATION (keccak over the uncompressed point, last 20 bytes) is
/// broken, and that is the half the venue's echo actually tested.
#[test]
fn the_throwaway_key_still_derives_the_address_hyperliquid_named() {
    let doc = vectors();
    let ours = signer().address().to_ascii_lowercase();

    let declared = doc["signer"]["address"]
        .as_str()
        .unwrap_or_else(|| panic!("{VECTORS} has no string `signer.address`"))
        .to_ascii_lowercase();
    assert_eq!(
        declared, ours,
        "{VECTORS}'s `signer.address` is not what its own `signer.private_key` derives. Either the \
         key was substituted — every frozen signature was made with the old one — or address \
         derivation moved. Restore the source."
    );

    for (name, case) in cases(&doc) {
        let recovered = field(&case, "recovered_address", &name).to_ascii_lowercase();
        assert_eq!(
            recovered,
            ours,
            "[{name}] Hyperliquid recovered {recovered} from this case's signature on {}, but this \
             key now derives {ours}. That equality IS the certification — without it the case \
             proves nothing.",
            field(&case, "certified_on", &name)
        );
    }
}

/// ⚠ THE PROVENANCE HALF. A certified value with no record of the run that certified it is
/// indistinguishable from a value somebody minted, which is the failure mode this whole file
/// exists to prevent — so the evidence fields are gated as strictly as the bytes.
#[test]
fn every_certified_case_carries_the_evidence_of_its_certification() {
    let doc = vectors();

    let regeneration = doc["_regeneration"]
        .as_str()
        .unwrap_or_else(|| panic!("{VECTORS} has no `_regeneration` field"));
    assert!(
        regeneration.contains("NO REGENERATOR"),
        "{VECTORS}'s `_regeneration` no longer says there is NO REGENERATOR. That sentence is what \
         stops the next reader refreshing these values from this build's own output and silently \
         turning a correctness gate back into a stability pin. Restore it."
    );

    for (name, case) in cases(&doc) {
        let recovered = field(&case, "recovered_address", &name).to_ascii_lowercase();
        let response = field(&case, "venue_response", &name);
        let certified_on = field(&case, "certified_on", &name);

        // A date, spelled as one — no regex dependency, and the placeholder the capture prints
        // cannot pass.
        let ymd: Vec<&str> = certified_on.split('-').collect();
        assert!(
            ymd.len() == 3
                && ymd[0].len() == 4
                && ymd[1].len() == 2
                && ymd[2].len() == 2
                && certified_on.chars().all(|c| c.is_ascii_digit() || c == '-'),
            "[{name}] `certified_on` is `{certified_on}`, not a YYYY-MM-DD date. The certification \
             run prints a placeholder there deliberately: a case whose date was never filled in is \
             a case nobody can date, and the date is half of what makes this evidence rather than \
             an assertion."
        );

        assert!(
            response.contains(&recovered),
            "[{name}] `venue_response` does not contain `recovered_address` ({recovered}). The \
             recovered address is READ OUT of the venue's reply, so a reply that does not name it \
             is not the reply this case came from.\n  response: {response}"
        );
        assert!(
            response.contains("Must deposit"),
            "[{name}] `venue_response` is not the `Must deposit before performing actions` \
             rejection. That branch IS the oracle — it is the one the venue takes for an address \
             it has never seen funded, after parsing the action and recovering the signer and \
             before executing anything. A different reply certifies something else, or nothing.\n \
             response: {response}"
        );
        assert!(
            !response.contains("Invalid nonce"),
            "[{name}] `venue_response` is a NONCE rejection, which short-circuits BEFORE recovery \
             and names no signer. This case reached no oracle and must not be frozen as though it \
             had.\n  response: {response}"
        );
    }
}

/// ⚠ THE COMPLETENESS HALF: every user-signed EIP-712 scheme this crate can sign has been put to
/// the venue, not merely frozen.
///
/// Without it the gate is only as good as the cases' coverage — a THIRD user-signed action could
/// ship with its hashed spelling guarded by a stability pin alone, which is the state this file was
/// created to end for the first two. The type string never appears in the posted JSON (it is hashed
/// into `typeHash`), so the fixture records it per case and this test matches it as raw text.
///
/// ⚠ A red here is not something to silence: it means a new action needs a TESTNET certification
/// run of the smoke's `certify_a_fresh_vector_for_the_offline_gate`, whose printed block is pasted
/// into the fixture. That is the whole cost of the discipline, and it is the point of it.
#[test]
fn every_user_signed_primary_type_has_a_venue_certified_case() {
    let src = read_repo(EIP712_SRC);
    let frozen = read_repo(VECTORS);

    // `hash_struct` call sites only — the same literal appears in doc comments, and prose about a
    // type string is not a type string.
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
        "{EIP712_SRC} declares user-signed EIP-712 primaryType(s) that {VECTORS} does not \
         name: {missing:?}\nEvery character of that string is hashed into the digest, so a scheme \
         with no certified case is guarded by a stability pin alone — nothing in this repository \
         has ever asked Hyperliquid whether it is right. Certify it: run the smoke's \
         certify_a_fresh_vector_for_the_offline_gate against testnet and paste its block.\nTypes \
         declared: {types:?}"
    );
}
