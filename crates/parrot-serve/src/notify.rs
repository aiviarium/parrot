//! Push targets for notification events (spec 36): a webhook that receives
//! `{ event, session, title, text, url? }`, or an ntfy topic. Targets are configuration; bodies
//! carry titles and short text only, never items or code.
//!
//! The backend sees every record the core persists; a request that opens with `notify` other
//! than `none` is an event. Delivery is plain HTTP (`http://` URLs), off the actor thread.

use serde_json::{Value, json};
use std::collections::HashSet;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        let mut body = json!({ "event": "request", "session": session, "title": title, "text": text });
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
    spec.split_whitespace()
        .filter_map(|t| {
            if let Some(url) = t.strip_prefix("webhook:") {
                return Some(Target::Webhook(url.to_string()));
            }
            let rest = t.strip_prefix("ntfy:")?;
            let (server, topic) = rest.rsplit_once('/')?;
            Some(Target::Ntfy {
                server: server.to_string(),
                topic: topic.to_string(),
            })
        })
        .collect()
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

/// A minimal HTTP/1.1 POST to an `http://` URL.
async fn post(url: &str, headers: &[(&str, &str)], body: String) -> std::io::Result<()> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Unsupported, "only http:// targets"))?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:80")
    };
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(&body);
    stream.write_all(req.as_bytes()).await?;
    let mut status = [0u8; 12];
    stream.read_exact(&mut status).await?;
    if !status[9..].starts_with(b"2") {
        return Err(std::io::Error::other(format!(
            "target answered {}",
            String::from_utf8_lossy(&status[9..])
        )));
    }
    Ok(())
}
