//! The client commands: `parrot sessions`, `parrot prompt` and the rest, over the local socket.

mod render;
mod rpc;

use std::path::PathBuf;

use rpc::Rpc;
use serde_json::{Value, json};

/// Options every client command takes.
#[derive(clap::Args, Clone)]
pub struct Common {
    /// The runtime's socket instead of the default.
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// Print the raw JSON results and updates.
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Lists the agents the runtime can start.
    Agents {
        #[command(flatten)]
        common: Common,
    },
    /// Lists sessions.
    Sessions {
        #[command(flatten)]
        common: Common,
    },
    /// Creates a session with an agent in a folder, and starts it.
    New {
        /// The agent, such as `claude`.
        agent: String,
        /// The folder the agent works in; the current one by default.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// The session's title.
        #[arg(long)]
        title: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Sends a prompt to a session.
    Prompt {
        session: String,
        text: String,
        /// `auto`, `queue`, `steer` or `interrupt`.
        #[arg(long)]
        delivery: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Subscribes to a session and prints its updates until interrupted.
    Watch {
        session: String,
        /// Stop once the session reaches this state, such as `idle`.
        #[arg(long)]
        until: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Lists open requests, of one session or of all.
    Requests {
        session: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Answers a request with a choice id, or with an answer object as JSON.
    Answer {
        request: String,
        answer: String,
        #[command(flatten)]
        common: Common,
    },
    /// Interrupts a session's turn and pauses its queue.
    Cancel {
        session: String,
        #[command(flatten)]
        common: Common,
    },
    /// Stops a session's harness; the session and its log stay.
    Stop {
        session: String,
        #[command(flatten)]
        common: Common,
    },
    /// Shows a session's options.
    Options {
        session: String,
        #[command(flatten)]
        common: Common,
    },
    /// Sets one option of a session; the value is JSON, or else a string.
    Set {
        session: String,
        option: String,
        value: String,
        #[command(flatten)]
        common: Common,
    },
}

impl Command {
    fn common(&self) -> &Common {
        match self {
            Command::Agents { common }
            | Command::Sessions { common }
            | Command::New { common, .. }
            | Command::Prompt { common, .. }
            | Command::Watch { common, .. }
            | Command::Requests { common, .. }
            | Command::Answer { common, .. }
            | Command::Cancel { common, .. }
            | Command::Stop { common, .. }
            | Command::Options { common, .. }
            | Command::Set { common, .. } => common,
        }
    }
}

/// A command-line value as JSON: JSON when it parses, else a string.
fn json_or_string(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

pub async fn run(command: Command) -> Result<(), String> {
    let common = command.common().clone();
    let mut rpc = Rpc::connect(&rpc::socket(common.socket.as_deref())?).await?;
    let raw = common.json;
    let print = |v: &Value, human: &dyn Fn(&Value) -> String| {
        if raw {
            println!("{v}");
        } else {
            let text = human(v);
            if !text.is_empty() {
                println!("{text}");
            }
        }
    };
    match command {
        Command::Agents { .. } => {
            let r = rpc.call("agent/list", json!({})).await?;
            print(&r, &render::agents);
        }
        Command::Sessions { .. } => {
            let r = rpc.call("session/list", json!({})).await?;
            print(&r, &render::sessions);
        }
        Command::New {
            agent, dir, title, ..
        } => {
            let dir = match dir {
                Some(d) => d,
                None => std::env::current_dir().map_err(|e| e.to_string())?,
            };
            let dir = std::fs::canonicalize(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = dir.to_string_lossy();
            let mut params = json!({
                "agent": agent,
                "workspace": { "kind": "dir", "path": path },
                "cwd": path,
            });
            if let Some(t) = title {
                params["title"] = t.into();
            }
            let r = rpc.call("session/new", params).await?;
            print(&r, &|r| {
                r["session"]["id"].as_str().unwrap_or_default().to_string()
            });
        }
        Command::Prompt {
            session,
            text,
            delivery,
            ..
        } => {
            let mut params = json!({ "session": session, "text": text });
            if let Some(d) = delivery {
                params["delivery"] = d.into();
            }
            let r = rpc.call("session/prompt", params).await?;
            print(&r, &|r| {
                let mut s = format!(
                    "{} {}",
                    r["disposition"].as_str().unwrap_or("?"),
                    r["message"].as_str().unwrap_or("")
                );
                if let Some(q) = r["queued"].as_str() {
                    s.push_str(&format!(" (queue entry {q})"));
                }
                s
            });
        }
        Command::Watch { session, until, .. } => watch(&mut rpc, &session, until.as_deref(), raw).await?,
        Command::Requests { session, .. } => {
            let mut params = json!({ "status": "open" });
            if let Some(s) = session {
                params["session"] = s.into();
            }
            let r = rpc.call("request/list", params).await?;
            print(&r, &render::requests);
        }
        Command::Answer { request, answer, .. } => {
            let answer = match json_or_string(&answer) {
                v @ Value::Object(_) => v,
                Value::String(choice) => json!({ "choice": choice }),
                other => json!({ "value": other }),
            };
            let r = rpc
                .call("request/answer", json!({ "request": request, "answer": answer }))
                .await?;
            print(&r, &|_| "answered".into());
        }
        Command::Cancel { session, .. } => {
            let r = rpc.call("session/cancel", json!({ "session": session })).await?;
            print(&r, &|r| {
                format!(
                    "{}; {} queued",
                    if r["interrupted"] == true {
                        "interrupted"
                    } else {
                        "nothing to interrupt"
                    },
                    r["queued"].as_u64().unwrap_or(0)
                )
            });
        }
        Command::Stop { session, .. } => {
            let r = rpc.call("session/stop", json!({ "session": session })).await?;
            print(&r, &|_| "stopped".into());
        }
        Command::Options { session, .. } => {
            let r = match rpc.call("option/list", json!({ "session": session })).await {
                Ok(r) => r,
                // Runtimes without `option/list` still carry the options in a snapshot.
                Err(_) => {
                    let sub = rpc
                        .call("subscribe", json!({ "target": { "session": session } }))
                        .await?;
                    json!({ "options": sub["snapshot"]["sessions"][0]["options"] })
                }
            };
            print(&r, &render::options);
        }
        Command::Set {
            session,
            option,
            value,
            ..
        } => {
            let values = json!({ option: json_or_string(&value) });
            let r = rpc
                .call("option/set", json!({ "session": session, "values": values }))
                .await?;
            print(&r, &render::options);
        }
    }
    Ok(())
}

/// Prints the snapshot, then every update, until the connection ends, Ctrl-C, or `until`.
async fn watch(rpc: &mut Rpc, session: &str, until: Option<&str>, raw: bool) -> Result<(), String> {
    let sub = rpc
        .call("subscribe", json!({ "target": { "session": session } }))
        .await?;
    let mut view = render::Watch::default();
    let snap = &sub["snapshot"]["sessions"][0];
    if raw {
        println!("{sub}");
    } else {
        for line in view.snapshot(snap) {
            println!("{line}");
        }
    }
    let settled = |state: &str| until == Some(state);
    if snap["session"]["state"].as_str().is_some_and(settled) {
        return Ok(());
    }
    loop {
        let msg = tokio::select! {
            m = rpc.notification() => m,
            _ = tokio::signal::ctrl_c() => return Ok(()),
        };
        let Some(msg) = msg else {
            return Err("the runtime closed the connection".into());
        };
        if msg["method"] != "update" || msg["params"]["subscription"] != sub["subscription"] {
            continue;
        }
        let update = &msg["params"]["update"];
        if raw {
            println!("{msg}");
        } else if let Some(line) = view.update(update) {
            println!("{line}");
        }
        if update["type"] == "state" && update["state"].as_str().is_some_and(settled) {
            return Ok(());
        }
    }
}
