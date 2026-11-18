use std::path::PathBuf;
use std::time::{Duration, Instant};

use parrot_driver::host::{DRIVER_EXITED, ProcessDriver};
use parrot_driver::traits::Driver;
use serde_json::json;

fn echo_driver() -> PathBuf {
    let deps = std::env::current_exe().unwrap();
    let dir = deps.parent().unwrap().parent().unwrap().join("examples");
    dir.join(format!("echo_driver{}", std::env::consts::EXE_SUFFIX))
}

fn spawn(
    env: &[(&str, &str)],
) -> (
    ProcessDriver,
    tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
) {
    ProcessDriver::spawn(echo_driver(), Vec::<String>::new(), env.iter().copied()).unwrap()
}

fn start_params(binding: &str) -> serde_json::Value {
    json!({ "session": "s1", "binding": binding, "agent": "echo", "workspace": "/w", "cwd": "/w",
            "options": {}, "control": "native", "deny": [] })
}

#[tokio::test]
async fn call_round_trip() {
    let (mut driver, _updates) = spawn(&[]);
    let init = driver
        .call("initialize", json!({ "peer": { "kind": "runtime" } }))
        .await
        .unwrap();
    assert_eq!(init["peer"]["kind"], "driver");
    let err = driver.call("agent/list", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32601);
    driver.stop().await;
}

#[tokio::test]
async fn updates_reach_the_receiver() {
    let (mut driver, mut updates) = spawn(&[]);
    let started = driver.call("binding/start", start_params("b1")).await.unwrap();
    assert_eq!(started["level"], "read");
    let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(update["binding"], "b1");
    assert_eq!(update["update"]["type"], "state");
    assert_eq!(driver.bindings(), vec!["b1".to_owned()]);
    driver.stop().await;
}

#[tokio::test]
async fn exit_mid_call_fails_pending_calls() {
    let (mut driver, _updates) = spawn(&[]);
    let err = tokio::time::timeout(Duration::from_secs(5), driver.call("test/exit", json!({})))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, DRIVER_EXITED);
    assert!(err.message.contains("exited"), "{}", err.message);
    let stderr = err.data.unwrap()["stderr"].as_str().unwrap().to_owned();
    assert!(stderr.contains("giving up"), "{stderr}");
    // Later calls fail at once.
    let err = driver.call("initialize", json!({})).await.unwrap_err();
    assert_eq!(err.code, DRIVER_EXITED);
}

#[tokio::test]
async fn stop_stops_bindings_then_closes_stdin() {
    let (mut driver, _updates) = spawn(&[]);
    driver.call("binding/start", start_params("b1")).await.unwrap();
    driver.call("binding/start", start_params("b2")).await.unwrap();
    let status = driver.stop().await.unwrap();
    assert!(status.success());
    assert!(driver.bindings().is_empty());
    let stderr = driver.stderr();
    assert!(
        stderr.contains("stopped \"b1\"") && stderr.contains("stopped \"b2\""),
        "{stderr}"
    );
}

#[tokio::test]
async fn stop_kills_a_driver_that_ignores_stdin() {
    let (mut driver, _updates) = spawn(&[("ECHO_DRIVER_HANG", "1")]);
    driver.call("initialize", json!({})).await.unwrap();
    let begun = Instant::now();
    let status = driver.stop_with_grace(Duration::from_millis(300)).await.unwrap();
    assert!(!status.success());
    assert!(begun.elapsed() < Duration::from_secs(5));
}
