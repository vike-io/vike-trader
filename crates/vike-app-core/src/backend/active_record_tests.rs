use super::*;

/// A record named `name` dialling `address`, unarmed — also the identity tests' fixture
/// (`crate::backend::identity`'s tests).
pub(crate) fn rec(name: &str, address: &str) -> BackendRecord {
    BackendRecord {
        name: name.to_string(),
        address: address.to_string(),
        observe_key: String::new(),
        control_key: None,
        control: false,
    }
}

/// The active record's ADDRESS without a file on disk — the REAL resolution ([`pick_active`]) plus
/// the field read, so these tests cannot pass while the shipped function disagrees with them.
fn pick(file: &BackendRegistry) -> Option<String> {
    pick_active(file).map(|b| b.address.trim().to_string())
}

#[test]
fn the_active_record_is_the_one_answered() {
    let f = BackendRegistry {
        backends: vec![rec("home", "<host>:9099"), rec("vps", "example:9099")],
        active: Some("vps".to_string()),
        refused: None,
    };
    assert_eq!(pick(&f).as_deref(), Some("example:9099"));
}

/// An empty registry answers nothing, so the caller falls through to its default rather than
/// dialling an empty string — which would look like a hang instead of a first run.
#[test]
fn an_empty_or_unpointed_registry_answers_nothing() {
    assert_eq!(pick(&BackendRegistry::default()), None);
    let orphan = BackendRegistry {
        backends: vec![rec("home", "<host>:9099")],
        active: Some("deleted".to_string()),
        refused: None,
    };
    assert_eq!(pick(&orphan), None, "an `active` naming no record must not answer");
}

/// The record answer carries the record's NAME and ARMING, not just the address: a startup connect
/// that kept only the address dialled an unnamed, CLI-synthetic record instead.
#[test]
fn the_record_answer_carries_the_name_and_arming_not_just_the_address() {
    let mut vps = rec("vps", "example:9099");
    vps.control = true;
    let f = BackendRegistry {
        backends: vec![rec("home", "<host>:9099"), vps],
        active: Some("vps".to_string()),
        refused: None,
    };

    let picked = pick_active(&f).expect("the active record answers");
    assert_eq!((picked.name.as_str(), picked.address.as_str()), ("vps", "example:9099"));
    assert!(picked.control, "the record's own arming rides along with it");
}

/// A blank address is treated as absent by the record answer too, not only by the address read: a
/// half-filled editor row is unfinished, not an instruction to dial nothing.
#[test]
fn a_blank_address_is_absent_for_the_record_answer_too() {
    let f = BackendRegistry {
        backends: vec![rec("half", "   ")],
        active: Some("half".to_string()),
        refused: None,
    };
    assert_eq!(pick(&f), None);
    assert!(
        pick_active(&f).is_none(),
        "the record answer and the address read cannot disagree — same resolution"
    );
}

/// A half-filled row in the Connections editor is a row the user has not finished, not an
/// instruction to dial nothing.
#[test]
fn a_blank_address_is_treated_as_absent() {
    for blank in ["", "   "] {
        let f = BackendRegistry {
            backends: vec![rec("home", blank)],
            active: Some("home".to_string()),
            refused: None,
        };
        assert_eq!(pick(&f), None, "{blank:?} was taken as an address");
    }
}

/// The default a viewer falls to (`vike_config::config::DEFAULT_TRADEHUB_ADDRESS`, the ladder's last
/// rung) must be loopback. A viewer that defaulted to a PUBLIC address would dial a stranger's machine
/// on first launch, which is the one first-run behaviour that could be worse than refusing to start.
#[test]
fn the_default_is_loopback_and_carries_a_port() {
    let default = vike_config::config::DEFAULT_TRADEHUB_ADDRESS;
    assert!(
        default.starts_with("127.0.0.1:") || default.starts_with("localhost:"),
        "{default} is not loopback — a viewer must not dial off-box by default"
    );
    let port = default.rsplit(':').next().unwrap_or("");
    assert!(port.parse::<u16>().is_ok(), "{default} carries no usable port");
}
