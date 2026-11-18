//! Push targets for notification events (spec 36): a webhook that receives
//! `{ event, session, title, text, url? }`, or an ntfy topic. Targets are configuration; bodies
//! carry titles and short text only, never items or code.
//!
//! The backend sees every record the core persists; a request that opens with `notify` other
//! than `none` is an event. Delivery is HTTP or HTTPS, off the actor thread.

use serde_json::{Value, json};
use std::collections::HashSet;

/// The longest `text` a push body carries.
const TEXT_MAX: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// POST the JSON body to this URL.
    Webhook(String),
    /// POST the text to `<server>/<topic>` with a `Title` header.
    Ntfy { server: String, topic: String },
}

#[derive(Debug, Default)]
pub struct Notifier {
    pub targets: Vec<Target>,
    /// A link for clients to open, put in `url` with the session appended.
    pub url: Option<String>,
    seen: HashSet<String>,
}

impl Notifier {
    pub fn new(targets: Vec<Target>) -> Notifier {
        Notifier {
            targets,
            ..Notifier::default()
        }
    }

    /// Looks at one persist record and delivers the event it is, if any.
    pub fn record(&mut self, record: &Value) {
        if self.targets.is_empty() || record["op"] != "put" || record["kind"] != "request" {
            return;
        }
        let r = &record["value"]["request"];
        let id = r["id"].as_str().unwrap_or_default().to_string();
        if r["status"] != "open" || r["notify"] == "none" || r["notify"].is_null() || !self.seen.insert(id) {
            return;
        }
        let session = r["session"].as_str().unwrap_or_default();
        let title = r["title"].as_str().unwrap_or("A session needs an answer");
        let text: String = r["description"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(TEXT_MAX)
            .collect();
        // `request` lets a client answer straight from the push.
        let mut body = json!({ "event": "request", "session": session, "request": r["id"], "title": title, "text": text });
        if let Some(u) = &self.url {
            body["url"] = json!(format!("{u}{session}"));
        }
        for target in self.targets.clone() {
            let body = body.clone();
            tokio::spawn(async move {
                if let Err(e) = deliver(&target, &body).await {
                    eprintln!("parrot: push notification failed: {e}");
                }
            });
        }
    }
}

/// Reads targets from `PARROT_NOTIFY`: space-separated `webhook:<url>` or `ntfy:<server>/<topic>`.
pub fn targets_from_env() -> Vec<Target> {
    let Ok(spec) = std::env::var("PARROT_NOTIFY") else {
        return Vec::new();
    };
    spec.split_whitespace().filter_map(parse_target).collect()
}

/// One target: `webhook:<url>` or `ntfy:<server>/<topic>`.
pub fn parse_target(t: &str) -> Option<Target> {
    if let Some(url) = t.strip_prefix("webhook:") {
        return Some(Target::Webhook(url.to_string()));
    }
    let rest = t.strip_prefix("ntfy:")?;
    let (server, topic) = rest.rsplit_once('/')?;
    Some(Target::Ntfy {
        server: server.to_string(),
        topic: topic.to_string(),
    })
}

async fn deliver(target: &Target, body: &Value) -> std::io::Result<()> {
    match target {
        Target::Webhook(url) => post(url, &[("Content-Type", "application/json")], body.to_string()).await,
        Target::Ntfy { server, topic } => {
            let title = body["title"]
                .as_str()
                .unwrap_or_default()
                .replace(['\r', '\n'], " ");
            let text = body["text"].as_str().unwrap_or_default().to_string();
            post(&format!("{server}/{topic}"), &[("Title", &title)], text).await
        }
    }
}

/// POSTs `body` to an `http://` or `https://` URL, off the async threads.
async fn post(url: &str, headers: &[(&str, &str)], body: String) -> std::io::Result<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "push targets are http:// or https:// URLs",
        ));
    }
    let url = url.to_string();
    let headers: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(15)))
            .build()
            .into();
        let mut req = agent.post(&url);
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        // The URL may hold a secret path, so errors name only the status.
        match req.send(body) {
            Ok(_) => Ok(()),
            Err(ureq::Error::StatusCode(code)) => {
                Err(std::io::Error::other(format!("target answered {code}")))
            }
            Err(e) => Err(std::io::Error::other(e.to_string())),
        }
    })
    .await
    .map_err(std::io::Error::other)?
}
