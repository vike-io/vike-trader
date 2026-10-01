use super::*;

fn rec(name: &str, addr: &str) -> BackendRecord {
    BackendRecord {
        name: name.to_string(),
        addr: addr.to_string(),
        observe_key: "K".to_string(),
        control_key: None,
        control: false,
        datahub_observe_key: String::new(),
    }
}

/// `active_addr` without a file on disk — the REAL resolution ([`pick_active`]) plus the field
/// read, so these tests cannot pass while the shipped function disagrees with them.
fn pick(file: &BackendsFile) -> Option<String> {
    pick_active(file).map(|b| b.addr.trim().to_string())
}

#[test]
fn the_active_record_is_the_one_answered() {
    let f = BackendsFile {
        backends: vec![rec("home", "<host>:9099"), rec("vps", "example:9099")],
        active: Some("vps".to_string()),
    };
    assert_eq!(pick(&f).as_deref(), Some("example:9099"));
}

/// An empty registry answers nothing, so the caller falls through to its default rather than
/// dialling an empty string — which would look like a hang instead of a first run.
#[test]
fn an_empty_or_unpointed_registry_answers_nothing() {
    assert_eq!(pick(&BackendsFile::default()), None);
    let orphan = BackendsFile {
        backends: vec![rec("home", "<host>:9099")],
        active: Some("deleted".to_string()),
    };
    assert_eq!(pick(&orphan), None, "an `active` naming no record must not answer");
}

/// The record answer carries the KEY NAMES, which is the whole reason `active_record` exists:
/// a startup connect that kept only the address signed with the CLI-synthetic
/// `VIKE_TRADEHUB_OBSERVE_KEY` and failed `bad mac` against a node whose record named its own.
#[test]
fn the_record_answer_carries_the_key_names_not_just_the_address() {
    let mut vps = rec("vps", "example:9099");
    vps.observe_key = "PROD2_OBSERVE_KEY".to_string();
    vps.control_key = Some("PROD2_CONTROL_KEY".to_string());
    vps.control = true;
    let f = BackendsFile {
        backends: vec![rec("home", "<host>:9099"), vps],
        active: Some("vps".to_string()),
    };

    let picked = pick_active(&f).expect("the active record answers");
    assert_eq!(picked.addr, "example:9099");
    assert_eq!(picked.observe_key, "PROD2_OBSERVE_KEY");
    assert_eq!(picked.control_key.as_deref(), Some("PROD2_CONTROL_KEY"));
    assert!(picked.control, "the record's own arming rides along with it");
}

/// A blank address is treated as absent by BOTH answers, because they are one function: a
/// half-filled editor row is unfinished, not an instruction to dial nothing.
#[test]
fn a_blank_address_is_absent_for_the_record_answer_too() {
    let f = BackendsFile { backends: vec![rec("half", "   ")], active: Some("half".to_string()) };
    assert_eq!(pick(&f), None);
    assert!(pick_active(&f).is_none(), "the two answers cannot disagree — same resolution");
}

/// A half-filled row in the Connections editor is a row the user has not finished, not an
/// instruction to dial nothing.
#[test]
fn a_blank_address_is_treated_as_absent() {
    for blank in ["", "   "] {
        let f =
            BackendsFile { backends: vec![rec("home", blank)], active: Some("home".to_string()) };
        assert_eq!(pick(&f), None, "{blank:?} was taken as an address");
    }
}

/// The default must be loopback. A viewer that defaulted to a PUBLIC address would dial a
/// stranger's machine on first launch, which is the one first-run behaviour that could be
/// worse than refusing to start.
#[test]
fn the_default_is_loopback_and_carries_a_port() {
    assert!(
        DEFAULT_OBSERVE_ADDR.starts_with("127.0.0.1:")
            || DEFAULT_OBSERVE_ADDR.starts_with("localhost:"),
        "{DEFAULT_OBSERVE_ADDR} is not loopback — a viewer must not dial off-box by default"
    );
    let port = DEFAULT_OBSERVE_ADDR.rsplit(':').next().unwrap_or("");
    assert!(port.parse::<u16>().is_ok(), "{DEFAULT_OBSERVE_ADDR} carries no usable port");
}
