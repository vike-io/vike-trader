use super::*;

fn sdk(number: Option<u32>, usable: bool) -> WireSdk {
    WireSdk {
        name: number.map_or_else(|| "sdk-dev".to_string(), |n| format!("sdk-{n}")),
        number,
        commit: "a3ca741107".to_string(),
        abi_version: Some(14),
        usable,
        why_unusable: if usable { String::new() } else { "retired".to_string() },
    }
}

fn listing(sdks: Vec<WireSdk>) -> WireSdkListing {
    WireSdkListing { root: "/srv/vike/vendor/vike-strategy-source".to_string(), sdks }
}

/// A pin names its own SDK (usable or not: the badge says why it does not build), an unlisted pin names
/// none, and no pin names the newest USABLE one: newest first, so an unusable newest is passed over.
#[test]
fn a_pin_names_its_sdk_and_no_pin_the_newest_usable_one() {
    let server = listing(vec![sdk(Some(5), false), sdk(Some(4), true), sdk(Some(1), false)]);
    assert_eq!(server.resolve(Some(1)).map(|s| s.name.as_str()), Some("sdk-1"));
    assert_eq!(server.resolve(Some(4)).map(|s| s.name.as_str()), Some("sdk-4"));
    assert_eq!(server.resolve(Some(9)), None);
    assert_eq!(server.resolve(None).map(|s| s.name.as_str()), Some("sdk-4"));
    assert_eq!(listing(Vec::new()).resolve(None), None);
}

/// A dev checkout builds every file against its live tree, so `sdk-dev` answers whatever the pin says.
#[test]
fn a_dev_checkout_answers_sdk_dev_whatever_the_pin() {
    let server = listing(vec![sdk(None, true)]);
    for pin in [None, Some(3)] {
        assert_eq!(server.resolve(pin).map(|s| s.name.as_str()), Some("sdk-dev"));
    }
}

/// An answer that omits the optional fields (an SDK whose marker could not be read, a folder with no
/// entries) still reads.
#[test]
fn the_optional_fields_default() {
    let unread: WireSdk =
        serde_json::from_str(r#"{"name":"sdk-2","number":2,"usable":false}"#).expect("reads");
    assert!(unread.commit.is_empty() && unread.abi_version.is_none());
    let folder: WireSdkFile =
        serde_json::from_str(r#"{"sdk":"sdk-2","path":"crates","folder":true}"#).expect("reads");
    assert!(folder.text.is_empty() && folder.entries.is_empty());
}
