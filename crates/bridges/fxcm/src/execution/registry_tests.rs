use super::*;

#[test]
fn a_bound_id_names_its_order_client_id_until_it_is_forgotten() {
    let mut t = OrderClientIdLookup::default();
    t.bind("416262101", "fxt2fx0801");
    assert_eq!(t.order_client_id_of("416262101").as_deref(), Some("fxt2fx0801"));
    t.forget("416262101");
    assert_eq!(t.order_client_id_of("416262101"), None);
    assert!(t.order_client_id.as_map().is_empty(), "the table forgets it");
}

#[test]
fn an_unbound_or_empty_id_names_nothing() {
    let mut t = OrderClientIdLookup::default();
    t.bind("", "c1");
    assert_eq!(t.order_client_id_of(""), None);
    assert_eq!(t.order_client_id_of("999"), None);
}

#[test]
fn past_the_bound_the_oldest_binding_goes() {
    let mut t = OrderClientIdLookup::default();
    for i in 0..=MAX_BINDINGS {
        t.bind(&i.to_string(), "c");
    }
    assert_eq!(t.order_client_id_of("0"), None, "the oldest went");
    assert_eq!(t.order_client_id_of(&MAX_BINDINGS.to_string()).as_deref(), Some("c"));
    assert_eq!(t.order_client_id.as_map().len(), MAX_BINDINGS);
}
