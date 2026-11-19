//! The environment's roots fence end to end (spec 7, 8): a real `parrot serve` enforces its
//! configured roots and reports them in `env/list`.
#![cfg(unix)]

mod common;

use common::{Client, Serve, Setup};
use serde_json::json;

#[tokio::test]
async fn a_folder_outside_the_roots_is_refused() {
    let serve = Serve::start(Setup::default());
    let outside = tempfile::tempdir().unwrap();
    let mut client = Client::connect(&serve, "roots-client").await;
    client.initialize().await;

    // The test harness makes the serve directory the only root (`common::Serve`).
    let envs = client.ok("env/list", json!({})).await;
    let roots = envs["envs"][0]["roots"].as_array().expect("roots");
    assert_eq!(
        roots,
        &vec![json!(serve.dir.path().canonicalize().unwrap())],
        "{envs}"
    );

    // A folder inside it is accepted, and one outside it is refused.
    let work = serve.work_dir();
    client
        .ok(
            "session/new",
            json!({
                "agent": "claude",
                "workspace": { "kind": "dir", "path": work },
                "start": false,
            }),
        )
        .await;
    let resp = client
        .call(
            "session/new",
            json!({
                "agent": "claude",
                "workspace": { "kind": "dir", "path": outside.path() },
                "start": false,
            }),
        )
        .await;
    assert_eq!(resp["error"]["code"], json!(-32003), "{resp}");
    assert_eq!(resp["error"]["data"]["reason"], json!("roots"), "{resp}");
}
