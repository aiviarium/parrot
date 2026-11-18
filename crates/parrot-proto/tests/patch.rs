use parrot_proto::patch::{apply, consolidate};
use proptest::prelude::*;
use serde_json::{Map, Value, json};

fn value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i32>().prop_map(|n| json!(n)),
        "[a-zé ]{0,6}".prop_map(Value::String),
    ]
}

fn set() -> impl Strategy<Value = Map<String, Value>> {
    prop::collection::btree_map("[a-d]", value(), 0..5).prop_map(|m| m.into_iter().collect())
}

proptest! {
    #[test]
    fn set_is_idempotent(base in set(), s in set()) {
        let base: Map<String, Value> = base.into_iter().filter(|(_, v)| !v.is_null()).collect();
        let patch = json!({ "id": "it_1", "set": s });
        let mut once = base.clone();
        apply(&mut once, &patch).unwrap();
        let mut twice = once.clone();
        apply(&mut twice, &patch).unwrap();
        prop_assert_eq!(once, twice);
    }

    #[test]
    fn appends_consolidate_to_concatenation(start in "[a-z]{0,4}", chunks in prop::collection::vec("[a-zé😀 ]{0,5}", 1..8)) {
        let mut log = vec![json!({ "id": "it_1", "kind": "assistant", "set": { "text": start }, "streaming": true })];
        for c in &chunks {
            log.push(json!({ "id": "it_1", "append": { "text": c } }));
        }
        log.push(json!({ "id": "it_1", "streaming": false }));
        let out = consolidate(&log).unwrap();
        prop_assert_eq!(out.len(), 3);
        let expected = format!("{start}{}", chunks.concat());
        prop_assert_eq!(&out[1]["set"]["text"], &json!(expected));

        let mut a = Map::new();
        for p in &log { apply(&mut a, p).unwrap(); }
        let mut b = Map::new();
        for p in &out { apply(&mut b, p).unwrap(); }
        prop_assert_eq!(a, b);
    }
}
