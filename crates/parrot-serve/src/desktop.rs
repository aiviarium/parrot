//! The desktop functions `parrot.desktop.*` (spec 29, E5), provided by the runtime through
//! Rookery's `box desktop` command on the host. Screenshots are written to a file and returned
//! by `uri`, never as image blocks (spec 27.1).

use parrot_proto::rpc::RpcError;
use serde_json::{Value, json};
use std::path::PathBuf;

/// The functions this provider offers, with their levels: input is `host`, screenshots `write`.
pub fn specs() -> Vec<Value> {
    let obj = json!({ "type": "object" });
    let spec = |name: &str, description: &str, effects: &[&str], level: &str| {
        json!({
            "name": format!("parrot.desktop.{name}"),
            "description": description,
            "input": obj,
            "output": obj,
            "effects": effects,
            "level": level,
        })
    };
    vec![
        spec(
            "screenshot",
            "Takes a screenshot of the display and returns its file by uri.",
            &["read"],
            "write",
        ),
        spec(
            "pointer",
            "Moves the pointer, clicks or scrolls at pixel coordinates.",
            &["write"],
            "host",
        ),
        spec(
            "keys",
            "Types text or presses keys held together.",
            &["write"],
            "host",
        ),
    ]
}

/// The `box` program: `PARROT_BOX`, else `box` on `PATH`.
fn program() -> PathBuf {
    std::env::var_os("PARROT_BOX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("box"))
}

/// Whether `box` is on this machine.
pub fn available() -> bool {
    let p = program();
    if p.components().count() > 1 {
        return p.is_file();
    }
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(&p).is_file()))
}

fn run(args: &[String]) -> Result<String, RpcError> {
    let out = std::process::Command::new(program())
        .arg("desktop")
        .args(args)
        .output()
        .map_err(|e| RpcError::new(-32020, format!("cannot run box: {e}")))?;
    if !out.status.success() {
        return Err(RpcError::new(-32020, "box desktop failed")
            .with_data(json!({ "detail": String::from_utf8_lossy(&out.stderr).trim() })));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn result(output: Value) -> Value {
    json!({ "output": output, "content": [{ "type": "text", "text": output.to_string() }] })
}

/// Runs one desktop function; `None` when `name` is not one.
pub fn call(name: &str, input: &Value, scratch: &std::path::Path) -> Option<Result<Value, RpcError>> {
    let r = match name.strip_prefix("parrot.desktop.")? {
        "screenshot" => {
            let file = scratch.join(format!("screenshot-{}.png", crate::dispatch::new_conn_id()));
            run(&["screenshot".into(), "--output".into(), file.display().to_string()])
                .map(|_| result(json!({ "uri": format!("file://{}", file.display()), "scale": 1 })))
        }
        "pointer" => {
            let action = input["action"].as_str().unwrap_or("click");
            let cmd = match action {
                "scroll" => "scroll",
                "move" => "move",
                _ => "click",
            };
            let mut args = json!({ "x": input["x"], "y": input["y"] });
            if let Some(b) = input["button"].as_str() {
                args["button"] = json!(b);
            }
            if action == "double" {
                args["double"] = json!(true);
            }
            if action == "scroll" {
                args["dx"] = input["dx"].clone();
                args["dy"] = input["dy"].clone();
            }
            run(&[cmd.into(), "--args".into(), args.to_string()]).map(|_| result(json!({})))
        }
        "keys" => {
            if let Some(text) = input["text"].as_str()
                && let Err(e) = run(&[
                    "type".into(),
                    "--args".into(),
                    json!({ "text": text }).to_string(),
                ])
            {
                return Some(Err(e));
            }
            match input["press"].as_array() {
                Some(keys) if !keys.is_empty() => {
                    let combo: Vec<String> = keys
                        .iter()
                        .filter_map(|k| k.as_str())
                        .map(|k| match k {
                            "Control" => "ctrl".to_string(),
                            "Alt" => "alt".into(),
                            "Shift" => "shift".into(),
                            "Meta" => "super".into(),
                            other => other.to_string(),
                        })
                        .collect();
                    run(&[
                        "key".into(),
                        "--args".into(),
                        json!({ "key": combo.join("+") }).to_string(),
                    ])
                    .map(|_| result(json!({})))
                }
                _ => Ok(result(json!({}))),
            }
        }
        other => Err(RpcError::new(
            -32002,
            format!("function not found: parrot.desktop.{other}"),
        )),
    };
    Some(r)
}
