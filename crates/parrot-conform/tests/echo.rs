use std::collections::VecDeque;

use parrot_conform::{Target, parse, run};
use serde_json::{Value, json};

/// Echoes `echo` params, creates items on `item/new`, and fails anything else.
#[derive(Default)]
struct Echo {
    out: VecDeque<Value>,
    items: Vec<Value>,
    seq: u64,
}

impl Target for Echo {
    fn send(&mut self, msg: Value) {
        let id = msg["id"].clone();
        match msg["method"].as_str() {
            Some("echo") => self
                .out
                .push_back(json!({"jsonrpc": "2.0", "id": id, "result": msg["params"]})),
            Some("item/new") => {
                let item = format!("it_{}", self.items.len() + 7);
                let text = msg["params"]["text"].clone();
                self.seq += 3;
                self.items.push(json!({"id": item, "text": text}));
                self.out
                    .push_back(json!({"jsonrpc": "2.0", "id": id, "result": {"id": item}}));
                self.out.push_back(json!({"jsonrpc": "2.0", "method": "update", "params": {
                    "subscription": "sub_1", "seq": self.seq, "at": "2026-03-04T05:06:07Z",
                    "update": {"type": "item", "item": {"id": item, "kind": "assistant", "set": {"text": text}}}
                }}));
            }
            _ => self.out.push_back(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "method not found"}})),
        }
    }
    fn recv(&mut self) -> Option<Value> {
        self.out.pop_front()
    }
    fn projection(&mut self, what: &Value) -> Value {
        if what["items"] == json!(true) {
            Value::Array(self.items.clone())
        } else {
            Value::Null
        }
    }
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn fixtures_pass() {
    for name in ["echo.jsonl", "items.jsonl"] {
        let steps = parse(&fixture(name)).unwrap();
        let report = run(&mut Echo::default(), &steps);
        assert!(report.passed(), "{name}: {:?}", report.mismatches);
        assert_eq!(report.steps, steps.len());
    }
}

#[test]
fn reports_mismatches() {
    let text = fixture("items.jsonl").replace("\"second\"}]", "\"other\"}]");
    let steps = parse(&text).unwrap();
    let report = run(&mut Echo::default(), &steps);
    assert_eq!(report.mismatches.len(), 1);
    assert_eq!(report.mismatches[0].line, 9);
    assert_eq!(report.mismatches[0].path, "$[1].text");

    // A target that goes quiet fails every expected message.
    let steps = parse(&fixture("echo.jsonl")).unwrap();
    struct Mute;
    impl Target for Mute {
        fn send(&mut self, _: Value) {}
        fn recv(&mut self) -> Option<Value> {
            None
        }
        fn projection(&mut self, _: &Value) -> Value {
            Value::Null
        }
    }
    assert_eq!(run(&mut Mute, &steps).mismatches.len(), 2);
}
