//! Client tokens (spec 3.4, 35, E8): created, listed and revoked by the owner or over a link, and
//! checked on each WebSocket upgrade. Secrets are kept only as SHA-256 hashes, in the runtime's
//! store as projections of kind `token`. A `tokens.json` of an older runtime is imported once.

use parrot_core::Peer;
use parrot_proto::rpc::RpcError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Turns the secret from a `parrot.token.<token>` subprotocol into the connection's peer.
pub trait TokenCheck: Send + Sync + 'static {
    /// The peer for a valid, unexpired token, or `None` to refuse the upgrade.
    fn check(&self, token: &str) -> Option<Peer>;
}

/// A fixed table of tokens, for tests and embedding.
#[derive(Debug, Default, Clone)]
pub struct StaticTokens(pub HashMap<String, Peer>);

impl TokenCheck for StaticTokens {
    fn check(&self, token: &str) -> Option<Peer> {
        self.0.get(token).cloned()
    }
}

/// Every scope a token may carry.
const SCOPES: [&str; 9] = [
    "read",
    "prompt",
    "answer",
    "configure",
    "manage",
    "delegate",
    "workspace",
    "destructive",
    "functions",
];

/// Secrets start with this, so they are easy to spot in a leak scan.
const SECRET_PREFIX: &str = "pt_";

#[derive(Debug, Clone)]
struct Record {
    /// The `Token` as listed, with `provides`.
    token: Value,
    /// Hex SHA-256 of the secret.
    hash: String,
}

/// The store projection that holds tokens.
const TOKEN_KIND: &str = "token";

/// Where tokens are written through to.
#[derive(Default)]
enum Backing {
    #[default]
    Memory,
    File(PathBuf),
    Store(Mutex<parrot_store::Store>),
}

impl std::fmt::Debug for Backing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Backing::Memory => f.write_str("Memory"),
            Backing::File(p) => write!(f, "File({})", p.display()),
            Backing::Store(_) => f.write_str("Store"),
        }
    }
}

/// Tokens in memory, written through to the store or a file when there is one.
#[derive(Debug, Default)]
pub struct TokenStore {
    backing: Backing,
    records: Mutex<Vec<Record>>,
}

fn record_of(v: &Value) -> Option<Record> {
    Some(Record {
        hash: v["hash"].as_str()?.to_string(),
        token: v["token"].clone(),
    })
}

fn hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The time now in RFC 3339, UTC, to the second.
fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Whether an RFC 3339 UTC time is in the past. Times in the same `Z` form compare as text.
fn expired(expires: &str, now: &str) -> bool {
    let cut = |s: &str| s.get(..19).unwrap_or(s).to_string();
    cut(expires) <= cut(now)
}

fn strings(v: &Value) -> Option<Vec<String>> {
    v.as_array()
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
}

impl TokenStore {
    /// A store kept only in memory.
    pub fn memory() -> TokenStore {
        TokenStore::default()
    }

    /// A store in `path`, read now if it exists.
    pub fn open(path: PathBuf) -> std::io::Result<TokenStore> {
        let records = read_file(&path)?;
        Ok(TokenStore {
            backing: Backing::File(path),
            records: Mutex::new(records),
        })
    }

    /// Tokens kept in the runtime's store (spec 35). A `tokens.json` at `legacy` is imported
    /// once, then renamed to `tokens.json.imported`.
    pub fn with_store(store: parrot_store::Store, legacy: Option<PathBuf>) -> std::io::Result<TokenStore> {
        use parrot_store::Ops;
        let mut records = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = store
                .list(TOKEN_KIND, cursor.as_deref(), 1000)
                .map_err(std::io::Error::other)?;
            let full = page.len() == 1000;
            cursor = page.last().map(|(id, _)| id.clone());
            records.extend(page.iter().filter_map(|(_, v)| record_of(v)));
            if !full {
                break;
            }
        }
        if let Some(file) = legacy.filter(|f| f.is_file()) {
            for r in read_file(&file)? {
                let Some(id) = r.token["id"].as_str().map(str::to_string) else {
                    continue;
                };
                if records.iter().any(|x| x.token["id"] == id.as_str()) {
                    continue;
                }
                store
                    .put(TOKEN_KIND, &id, &json!({ "token": r.token, "hash": r.hash }))
                    .map_err(std::io::Error::other)?;
                records.push(r);
            }
            std::fs::rename(&file, file.with_extension("json.imported"))?;
        }
        Ok(TokenStore {
            backing: Backing::Store(Mutex::new(store)),
            records: Mutex::new(records),
        })
    }

    fn save(&self, records: &[Record]) -> Result<(), RpcError> {
        let path = match &self.backing {
            Backing::Memory => return Ok(()),
            Backing::Store(store) => return save_store(store, records),
            Backing::File(path) => path,
        };
        let all: Vec<Value> = records
            .iter()
            .map(|r| json!({ "token": r.token, "hash": r.hash }))
            .collect();
        let text = serde_json::to_string_pretty(&all).expect("JSON serializes");
        let tmp = path.with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let _ = std::fs::remove_file(&tmp);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            // The file is private from its first byte, not only once it is written.
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, path)
        };
        write().map_err(|e| RpcError::new(-32603, format!("cannot write the tokens: {e}")))
    }

    /// Runs a `token.*` operation from the core.
    pub fn op(&self, op: &str, params: Value) -> Result<Value, RpcError> {
        match op {
            "token.create" => self.create(&params),
            "token.list" => {
                let records = self.records.lock().unwrap();
                let by = params["by"].as_str();
                Ok(json!({ "tokens": records
                    .iter()
                    .filter(|r| by.is_none_or(|b| r.token["createdBy"] == b))
                    .map(|r| r.token.clone())
                    .collect::<Vec<_>>() }))
            }
            // Every token a link minted, when the link goes or stops offering sessions.
            "token.revoke" if params["all"].as_bool() == Some(true) => {
                let by = params["by"]
                    .as_str()
                    .ok_or_else(|| RpcError::new(-32602, "invalid params: by"))?;
                let mut records = self.records.lock().unwrap();
                let revoked: Vec<Value> = records
                    .iter()
                    .filter(|r| r.token["createdBy"] == by)
                    .map(|r| r.token["id"].clone())
                    .collect();
                records.retain(|r| r.token["createdBy"] != by);
                self.save(&records)?;
                Ok(json!({ "revoked": revoked }))
            }
            "token.revoke" => {
                let id = params["token"].as_str().unwrap_or_default();
                // A link revokes only the tokens it created itself.
                let by = params["by"].as_str();
                let mut records = self.records.lock().unwrap();
                let before = records.len();
                records.retain(|r| r.token["id"] != id || by.is_some_and(|b| r.token["createdBy"] != b));
                if records.len() == before {
                    return Err(RpcError::new(-32002, format!("token not found: {id}")));
                }
                self.save(&records)?;
                Ok(json!({}))
            }
            _ => Err(RpcError::new(-32601, format!("method not found: {op}"))),
        }
    }

    fn create(&self, p: &Value) -> Result<Value, RpcError> {
        let invalid = |m: &str| RpcError::new(-32602, format!("invalid params: {m}"));
        let name = p["name"].as_str().ok_or_else(|| invalid("name is required"))?;
        let scopes = strings(&p["scopes"]).ok_or_else(|| invalid("scopes is required"))?;
        if let Some(bad) = scopes.iter().find(|s| !SCOPES.contains(&s.as_str())) {
            return Err(invalid(&format!("unknown scope {bad}")));
        }
        let bytes: [u8; 32] = rand::random();
        let secret = format!("{SECRET_PREFIX}{}", parrot_link::base32::encode(&bytes));
        // The id is listed and logged, so it is drawn on its own rather than derived from the secret.
        let id_bytes: [u8; 16] = rand::random();
        let id = format!("tok_{}", &parrot_link::base32::encode(&id_bytes)[..26]);
        let mut token = json!({ "id": id, "name": name, "scopes": scopes, "created": now() });
        for field in ["sessions", "provides"] {
            if !p[field].is_null() {
                token[field] = json!(strings(&p[field]).ok_or_else(|| invalid(field))?);
            }
        }
        if let Some(e) = p["expires"].as_str() {
            token["expires"] = json!(e);
        }
        // The core sets `by` for a token minted over a link, which may later list and revoke it.
        if let Some(by) = p["by"].as_str() {
            token["createdBy"] = json!(by);
        }
        let mut records = self.records.lock().unwrap();
        records.push(Record {
            token: token.clone(),
            hash: hash(&secret),
        });
        self.save(&records)?;
        Ok(json!({ "token": token, "secret": secret }))
    }
}

fn read_file(path: &std::path::Path) -> std::io::Result<Vec<Record>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str::<Vec<Value>>(&text)
            .map_err(std::io::Error::other)?
            .iter()
            .filter_map(record_of)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Writes the tokens as they are now, deleting those that are gone, in one transaction.
fn save_store(store: &Mutex<parrot_store::Store>, records: &[Record]) -> Result<(), RpcError> {
    use parrot_store::Ops;
    let mut store = store.lock().unwrap();
    store
        .transaction(|tx| -> parrot_store::Result<()> {
            let stored: Vec<String> = tx
                .list(TOKEN_KIND, None, 100_000)?
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            for id in stored {
                if !records.iter().any(|r| r.token["id"] == id.as_str()) {
                    tx.delete(TOKEN_KIND, &id)?;
                }
            }
            for r in records {
                if let Some(id) = r.token["id"].as_str() {
                    tx.put(TOKEN_KIND, id, &json!({ "token": r.token, "hash": r.hash }))?;
                }
            }
            Ok(())
        })
        .map_err(|e| RpcError::new(-32603, format!("cannot store the tokens: {e}")))
}

impl TokenCheck for TokenStore {
    fn check(&self, secret: &str) -> Option<Peer> {
        let h = hash(secret);
        let now = now();
        let mut records = self.records.lock().unwrap();
        let r = records.iter_mut().find(|r| r.hash == h)?;
        if r.token["expires"].as_str().is_some_and(|e| expired(e, &now)) {
            return None;
        }
        r.token["lastUsed"] = json!(now);
        let peer = Peer::Token {
            token: r.token["id"].as_str()?.to_string(),
            scopes: strings(&r.token["scopes"]).unwrap_or_default(),
            sessions: strings(&r.token["sessions"]),
            provides: strings(&r.token["provides"]),
        };
        // `lastUsed` is kept across restarts. A failed write only loses that time.
        let _ = self.save(&records);
        Some(peer)
    }
}

impl<T: TokenCheck> TokenCheck for std::sync::Arc<T> {
    fn check(&self, token: &str) -> Option<Peer> {
        (**self).check(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_stored_hashed_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let store = TokenStore::open(path.clone()).unwrap();
        let made = store
            .op(
                "token.create",
                json!({ "name": "phone", "scopes": ["answer"], "sessions": ["ses_a"], "provides": ["acme.*"] }),
            )
            .unwrap();
        let secret = made["secret"].as_str().unwrap();
        let file = std::fs::read_to_string(&path).unwrap();
        assert!(!file.contains(secret), "the secret is on disk");

        let again = TokenStore::open(path.clone()).unwrap();
        let Some(Peer::Token {
            scopes,
            sessions,
            provides,
            ..
        }) = again.check(secret)
        else {
            panic!("the token is not accepted");
        };
        assert_eq!(scopes, ["answer"]);
        assert_eq!(sessions.unwrap(), ["ses_a"]);
        assert_eq!(provides.unwrap(), ["acme.*"]);
        assert!(again.check("pt_wrong").is_none());
        let reopened = TokenStore::open(path.clone()).unwrap();
        let listed = reopened.op("token.list", json!({})).unwrap();
        assert!(
            listed["tokens"][0]["lastUsed"].is_string(),
            "lastUsed survives a restart: {listed}"
        );

        let id = made["token"]["id"].as_str().unwrap();
        again.op("token.revoke", json!({ "token": id })).unwrap();
        assert!(again.check(secret).is_none());
        assert!(
            store
                .op("token.create", json!({ "name": "x", "scopes": ["root"] }))
                .is_err()
        );
    }

    #[test]
    fn tokens_live_in_the_store_and_a_token_file_is_imported_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("parrot.db");
        let legacy = dir.path().join("tokens.json");
        // An older runtime's file, with a token minted over a link.
        let old = TokenStore::open(legacy.clone()).unwrap();
        let minted = old
            .op(
                "token.create",
                json!({ "name": "via", "scopes": ["read"], "sessions": ["ses_a"], "by": "lnk_a" }),
            )
            .unwrap();
        let old_secret = minted["secret"].as_str().unwrap().to_string();

        let tokens =
            TokenStore::with_store(parrot_store::Store::open(&db).unwrap(), Some(legacy.clone())).unwrap();
        assert!(!legacy.exists() && legacy.with_extension("json.imported").exists());
        assert!(tokens.check(&old_secret).is_some(), "the imported token works");
        let made = tokens
            .op("token.create", json!({ "name": "phone", "scopes": ["answer"] }))
            .unwrap();
        let secret = made["secret"].as_str().unwrap().to_string();
        drop(tokens);

        // Reopened from the store alone; the secret itself was never stored.
        let again =
            TokenStore::with_store(parrot_store::Store::open(&db).unwrap(), Some(legacy.clone())).unwrap();
        assert!(again.check(&secret).is_some());
        assert!(again.check(&old_secret).is_some());
        let listed = again.op("token.list", json!({ "by": "lnk_a" })).unwrap();
        assert_eq!(listed["tokens"].as_array().map(Vec::len), Some(1), "{listed}");
        let raw = std::fs::read(&db).unwrap();
        assert!(!raw.windows(secret.len()).any(|w| w == secret.as_bytes()));
        let id = minted["token"]["id"].as_str().unwrap();
        again
            .op("token.revoke", json!({ "token": id, "by": "lnk_a" }))
            .unwrap();
        drop(again);
        let last = TokenStore::with_store(parrot_store::Store::open(&db).unwrap(), None).unwrap();
        assert!(last.check(&old_secret).is_none(), "a revoked token stays revoked");
    }

    #[test]
    fn a_link_sees_and_revokes_only_its_own_tokens() {
        let store = TokenStore::memory();
        let own = store
            .op("token.create", json!({ "name": "owner", "scopes": ["read"] }))
            .unwrap();
        let linked = store
            .op(
                "token.create",
                json!({ "name": "via", "scopes": ["read"], "sessions": ["ses_a"], "by": "lnk_a" }),
            )
            .unwrap();
        let gone = store
            .op(
                "token.create",
                json!({ "name": "gone", "scopes": ["read"], "sessions": ["ses_b"], "by": "lnk_b" }),
            )
            .unwrap();
        let revoked = store
            .op("token.revoke", json!({ "by": "lnk_b", "all": true }))
            .unwrap();
        assert_eq!(revoked["revoked"], json!([gone["token"]["id"]]));
        assert!(store.check(gone["secret"].as_str().unwrap()).is_none());
        assert!(store.check(linked["secret"].as_str().unwrap()).is_some());
        assert_eq!(linked["token"]["createdBy"], "lnk_a");
        let listed = store.op("token.list", json!({ "by": "lnk_a" })).unwrap();
        assert_eq!(listed["tokens"].as_array().unwrap().len(), 1);
        let own_id = own["token"]["id"].as_str().unwrap();
        let e = store
            .op("token.revoke", json!({ "token": own_id, "by": "lnk_a" }))
            .unwrap_err();
        assert_eq!(e.code, -32002);
        assert!(store.check(own["secret"].as_str().unwrap()).is_some());
        let linked_id = linked["token"]["id"].as_str().unwrap();
        store
            .op("token.revoke", json!({ "token": linked_id, "by": "lnk_a" }))
            .unwrap();
        assert!(store.check(linked["secret"].as_str().unwrap()).is_none());
    }

    #[test]
    fn expired_tokens_are_refused() {
        let store = TokenStore::memory();
        let made = store
            .op(
                "token.create",
                json!({ "name": "old", "scopes": ["read"], "expires": "2001-01-01T00:00:00Z" }),
            )
            .unwrap();
        assert!(store.check(made["secret"].as_str().unwrap()).is_none());
    }
}
