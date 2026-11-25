use super::*;

use std::net::TcpListener;
use std::time::{Duration, Instant};

use vike_datahub_client::proto::FEATURE_SYMBOL_SETS;
use vike_datahub_client::proto::{Request, Response};
use vike_datahub_client::test_support::accept_keyless;
use vike_hub_protocol::frame::{read_frame, write_frame};

use crate::test_support::datahub_dial;

/// A key-less fake datahub holding `held`: it answers the three verbs as the real one does, the list after the act.
fn fake_datahub(held: Vec<SymbolSet>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr").to_string();
    let held = std::sync::Arc::new(Mutex::new(held));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let held = std::sync::Arc::clone(&held);
            std::thread::spawn(move || {
                let mut s = stream;
                if !accept_keyless(&mut s, vec![FEATURE_SYMBOL_SETS.to_string()]) {
                    return;
                }
                while let Ok(req) = read_frame::<_, Request>(&mut s) {
                    let mut sets = held.lock().unwrap();
                    match req {
                        Request::SymbolSets => {}
                        Request::SetSymbolSet { set } => {
                            sets.retain(|d| d.name != set.name);
                            sets.push(set);
                        }
                        Request::DeleteSymbolSet { name } => sets.retain(|d| d.name != name),
                        _ => return,
                    }
                    if write_frame(&mut s, &Response::SymbolSets(sets.clone())).is_err() {
                        return;
                    }
                }
            });
        }
    });
    address
}

/// Wait for the next answer a thread sends, bounded.
fn next(link: &SymbolSetsLink) -> Result<SymbolSetStore, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(answer) = link.poll() {
            return answer;
        }
        assert!(Instant::now() < deadline, "no answer arrived");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn mine() -> SymbolSet {
    SymbolSet {
        name: "Mine".into(),
        symbols: vec!["AAA".into(), "BBB".into()],
        provider: "some-venue".into(),
        interval: "1m".into(),
        benchmark: String::new(),
        user: true,
    }
}

#[test]
fn following_a_dial_reads_once_and_an_empty_table_shows_no_list_writable() {
    let link = SymbolSetsLink::default();
    let dial = datahub_dial(&fake_datahub(Vec::new()));
    link.follow(Some(dial.clone()));
    let shown = next(&link).expect("answered");
    assert!(shown.sets.is_empty(), "no list is compiled in, so an empty table shows none");
    assert!(shown.writable, "a server answered, so a save reaches it");
    link.follow(Some(dial));
    std::thread::sleep(Duration::from_millis(100));
    assert!(link.poll().is_none(), "the same dial is not asked again");
}

#[test]
fn a_save_and_a_delete_answer_the_lists_after_the_act() {
    let link = SymbolSetsLink::default();
    let dial = datahub_dial(&fake_datahub(Vec::new()));
    link.save(Some(dial.clone()), mine());
    assert_eq!(next(&link).expect("saved").sets, vec![mine()]);
    link.delete(Some(dial), "Mine".into());
    assert!(next(&link).expect("deleted").sets.is_empty());
}

#[test]
fn no_backend_answers_in_words_and_follows_nothing() {
    let link = SymbolSetsLink::default();
    link.follow(None);
    assert!(link.poll().is_none());
    link.save(None, mine());
    assert!(next(&link).expect_err("no datahub").contains("no backend"));
}
