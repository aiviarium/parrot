//! Push targets for notification events (spec 36): a webhook that receives
//! `{ event, session, title, text, url? }`, or an ntfy topic. Targets are configuration; bodies
//! carry titles and short text only, never items or code.
//!
//! The backend sees every record the core persists. An open request with `notify` other than
//! `none`, a task that ends, and a session that becomes `lost` are events. Delivery is HTTP or
//! HTTPS, off the actor thread.

use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use ureq::tls::{Certificate, PemItem, RootCerts, TlsConfig};

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
    /// Sessions that are `lost` now, so a session that comes back and is lost again notifies.
    lost: HashSet<String>,
    /// The roots HTTPS targets are checked against, in place of the public ones.
    roots: Option<Arc<Vec<Certificate<'static>>>>,
}

impl Notifier {
    pub fn new(targets: Vec<Target>) -> Notifier {
        Notifier {
            targets,
            ..Notifier::default()
        }
    }

    /// Checks HTTPS targets against the certificates in `pem` only, as for a self-hosted hook.
    pub fn trust(&mut self, pem: &[u8]) -> std::io::Result<()> {
        let mut certs = Vec::new();
        for item in ureq::tls::parse_pem(pem) {
            if let PemItem::Certificate(c) = item.map_err(std::io::Error::other)? {
                certs.push(c);
            }
        }
        if certs.is_empty() {
            return Err(std::io::Error::other("the file holds no certificate"));
        }
        self.roots = Some(Arc::new(certs));
        Ok(())
    }

    /// Looks at one persist record and delivers the event it is, if any.
    pub fn record(&mut self, record: &Value) {
        if self.targets.is_empty() {
            return;
        }
        let Some(body) = self.event(record) else {
            return;
        };
        for target in self.targets.clone() {
            let body = body.clone();
            let roots = self.roots.clone();
            tokio::spawn(async move {
                if let Err(e) = deliver(&target, &body, roots).await {
                    eprintln!("parrot: push notification failed: {e}");
                }
            });
        }
    }

    /// The body to push for one record, or `None` when it is no event or was already sent.
    fn event(&mut self, record: &Value) -> Option<Value> {
        if record["op"] != "put" {
            return None;
        }
        let mut body = match record["kind"].as_str()? {
            "request" => self.request(record)?,
            "tree" => self.task(record)?,
            "session" => self.session(record)?,
            _ => return None,
        };
        if let Some(u) = &self.url {
            let session = body["session"].as_str().unwrap_or_default();
            body["url"] = json!(format!("{u}{session}"));
        }
        Some(body)
    }

    /// A request that opened and asks to be pushed.
    fn request(&mut self, record: &Value) -> Option<Value> {
        let r = &record["value"]["request"];
        let id = r["id"].as_str().unwrap_or_default().to_string();
        if r["status"] != "open" || r["notify"] == "none" || r["notify"].is_null() || !self.seen.insert(id) {
            return None;
        }
        let title = r["title"].as_str().unwrap_or("A session needs an answer");
        let text: String = r["description"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(TEXT_MAX)
            .collect();
        // `request` lets a client answer straight from the push.
        Some(
            json!({ "event": "request", "session": r["session"], "request": r["id"], "title": title, "text": text }),
        )
    }

    /// A task that ended (spec 19): `done`, `failed`, `cancelled` or `interrupted`.
    fn task(&mut self, record: &Value) -> Option<Value> {
        let task = record["value"].get("task")?;
        let id = task["id"].as_str()?;
        let status = task["status"].as_str()?;
        if !matches!(status, "done" | "failed" | "cancelled" | "interrupted") {
            return None;
        }
        if !self.seen.insert(format!("task:{id}:{status}")) {
            return None;
        }
        let title = task["title"].as_str().unwrap_or("A task ended");
        let text: String = match task["error"].as_str() {
            Some(error) => format!("{status}: {error}"),
            None => status.to_string(),
        }
        .chars()
        .take(TEXT_MAX)
        .collect();
        Some(
            json!({ "event": "task", "session": record["id"], "task": id, "status": status, "title": title, "text": text }),
        )
    }

    /// A session that became `lost`: its agent exited without being asked to (spec 34).
    fn session(&mut self, record: &Value) -> Option<Value> {
        let s = &record["value"]["session"];
        let session = record["id"].as_str().or_else(|| s["id"].as_str())?.to_string();
        if s["state"] != "lost" {
            // A session that runs again may be lost again, and that is a new event.
            self.lost.remove(&session);
            return None;
        }
        if !self.lost.insert(session.clone()) {
            return None;
        }
        let title = s["title"].as_str().unwrap_or("A session was lost");
        Some(
            json!({ "event": "session", "session": session, "state": "lost", "title": title, "text": "the agent stopped unexpectedly" }),
        )
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

type Roots = Option<Arc<Vec<Certificate<'static>>>>;

async fn deliver(target: &Target, body: &Value, roots: Roots) -> std::io::Result<()> {
    match target {
        Target::Webhook(url) => {
            post(
                url,
                &[("Content-Type", "application/json")],
                body.to_string(),
                roots,
            )
            .await
        }
        Target::Ntfy { server, topic } => {
            let title = body["title"]
                .as_str()
                .unwrap_or_default()
                .replace(['\r', '\n'], " ");
            let text = body["text"].as_str().unwrap_or_default().to_string();
            post(&format!("{server}/{topic}"), &[("Title", &title)], text, roots).await
        }
    }
}

/// POSTs `body` to an `http://` or `https://` URL, off the async threads.
async fn post(url: &str, headers: &[(&str, &str)], body: String, roots: Roots) -> std::io::Result<()> {
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
        let mut config =
            ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(15)));
        if let Some(roots) = roots {
            config = config.tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::Specific(roots))
                    .build(),
            );
        }
        let agent: ureq::Agent = config.build().into();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::mpsc;

    /// A made-up self-signed certificate for 127.0.0.1, used only by this test.
    const CERT: &[u8] = include_bytes!("../fixtures/tls/push-test.crt");
    const KEY: &[u8] = include_bytes!("../fixtures/tls/push-test.key");

    /// An HTTPS server on 127.0.0.1 that sends each request body to the channel.
    fn https_server() -> (u16, mpsc::Receiver<String>) {
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let certs = vec![CertificateDer::from_pem_slice(CERT).unwrap()];
        let key = PrivateKeyDer::from_pem_slice(KEY).unwrap();
        let config =
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap();
        let config = Arc::new(config);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let conn = rustls::ServerConnection::new(config.clone()).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, stream);
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let body = loop {
                    let n = match tls.read(&mut chunk) {
                        Ok(0) | Err(_) => break None,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some(end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        break Some(text[end + 4..].to_string());
                    }
                };
                let Some(body) = body else { continue };
                let _ = tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = tls.flush();
                tls.conn.send_close_notify();
                let _ = tls.flush();
                let _ = tx.send(body);
            }
        });
        (port, rx)
    }

    fn request(id: &str) -> Value {
        json!({ "op": "put", "kind": "request", "value": { "request": {
            "id": id, "session": "s-1", "status": "open", "notify": "always",
            "title": "Run ls?", "description": "The agent asks to run ls."
        } } })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_https_webhook_gets_the_push_when_its_root_is_trusted() {
        let (port, bodies) = https_server();
        let target = Target::Webhook(format!("https://127.0.0.1:{port}/hook"));
        let mut notifier = Notifier::new(vec![target.clone()]);
        notifier.trust(CERT).unwrap();
        notifier.record(&request("r-1"));
        let body =
            tokio::task::spawn_blocking(move || bodies.recv_timeout(std::time::Duration::from_secs(10)))
                .await
                .unwrap()
                .expect("a push over HTTPS");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["event"], "request", "{body}");
        assert_eq!(body["request"], "r-1", "{body}");
        assert_eq!(body["title"], "Run ls?", "{body}");

        // Against the public roots the same server is refused.
        let refused = deliver(&target, &body, None).await;
        assert!(refused.is_err(), "a self-signed target was accepted");
    }

    #[test]
    fn trust_wants_a_certificate() {
        let mut notifier = Notifier::default();
        assert!(notifier.trust(b"not a pem file").is_err());
        assert!(notifier.trust(KEY).is_err());
    }

    fn task(status: &str, error: Option<&str>) -> Value {
        let mut task = json!({ "id": "tsk_1", "title": "Fix the parser", "status": status });
        if let Some(error) = error {
            task["error"] = json!(error);
        }
        json!({ "op": "put", "kind": "tree", "id": "s-1", "value": { "task": task } })
    }

    fn session(state: &str) -> Value {
        json!({ "op": "put", "kind": "session", "id": "s-1",
            "value": { "session": { "id": "s-1", "state": state, "title": "Port the parser" } } })
    }

    #[test]
    fn a_task_that_ends_is_an_event() {
        let mut notifier = Notifier {
            url: Some("https://app.example.test/s/".into()),
            ..Notifier::default()
        };

        let done = task("done", None);
        let body = notifier.event(&done).expect("a task end");
        assert_eq!(body["event"], "task", "{body}");
        assert_eq!(body["session"], "s-1", "{body}");
        assert_eq!(body["task"], "tsk_1", "{body}");
        assert_eq!(body["title"], "Fix the parser", "{body}");
        assert_eq!(body["text"], "done", "{body}");
        assert_eq!(body["url"], "https://app.example.test/s/s-1", "{body}");
        assert!(notifier.event(&done).is_none(), "the same end twice");

        // A failing task says why, and a running one is no event.
        let failed = task("failed", Some("the harness exited"));
        let body = notifier.event(&failed).expect("a task failure");
        assert_eq!(body["text"], "failed: the harness exited", "{body}");
        assert!(notifier.event(&task("running", None)).is_none());
        assert!(notifier.event(&task("awaiting", None)).is_none());
    }

    #[test]
    fn a_lost_session_is_an_event_each_time_it_is_lost() {
        let mut notifier = Notifier::default();
        let lost = session("lost");
        let body = notifier.event(&lost).expect("a lost session");
        assert_eq!(body["event"], "session", "{body}");
        assert_eq!(body["session"], "s-1", "{body}");
        assert_eq!(body["title"], "Port the parser", "{body}");
        assert!(notifier.event(&lost).is_none(), "the same loss twice");

        // The session coming back and being lost again is a new event.
        assert!(notifier.event(&session("running")).is_none());
        assert!(notifier.event(&lost).is_some());
    }

    #[test]
    fn a_request_still_notifies_once() {
        let mut notifier = Notifier {
            url: Some("https://app.example.test/s/".into()),
            ..Notifier::default()
        };
        let body = notifier.event(&request("r-1")).expect("a request");
        assert_eq!(body["event"], "request", "{body}");
        assert_eq!(body["request"], "r-1", "{body}");
        assert_eq!(body["url"], "https://app.example.test/s/s-1", "{body}");
        assert!(
            notifier.event(&request("r-1")).is_none(),
            "the same request twice"
        );
    }
}
