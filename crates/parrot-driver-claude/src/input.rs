//! A prompt's text, context and attachments (spec 13.1) as stream-json content blocks.
//!
//! Claude Code passes user content blocks to the Messages API as they are, so images and PDFs go
//! as `image` and `document` blocks with a base64 or URL source, and context entries and text
//! files as `document` blocks with a text source. Anything else is left out with a notice.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use parrot_proto::prompt::{Attachment, ContextEntry};
use serde_json::{Value, json};

/// Image types the Messages API takes.
pub const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/// The largest local file read into a prompt.
pub const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// What a prompt becomes: content blocks, and the attachments left out with the reason.
#[derive(Debug, Default, PartialEq)]
pub struct Input {
    pub blocks: Vec<Value>,
    pub skipped: Vec<(String, String)>,
}

impl Input {
    /// Whether the prompt is only text, so it can go as a plain string.
    pub fn is_plain(&self) -> bool {
        self.blocks.len() == 1 && self.skipped.is_empty()
    }
}

/// Builds the content of a prompt.
pub fn content(text: &str, context: &[ContextEntry], attachments: &[Attachment]) -> Input {
    let mut input = Input {
        blocks: vec![json!({"type": "text", "text": text})],
        skipped: vec![],
    };
    for c in context {
        let mut b = json!({
            "type": "document",
            "source": {"type": "text", "media_type": "text/plain", "data": c.text},
            "context": c.uri,
        });
        b["title"] = json!(c.title.clone().unwrap_or_else(|| c.uri.clone()));
        input.blocks.push(b);
    }
    for a in attachments {
        match block(a) {
            Ok(b) => input.blocks.push(b),
            Err(why) => input.skipped.push((a.name.clone(), why)),
        }
    }
    input
}

/// A local path named by `uri`: `file://` URLs and plain absolute paths.
pub fn local_path(uri: &str) -> Option<String> {
    if let Some(p) = uri.strip_prefix("file://") {
        // `file://localhost/x` and `file:///x` both name `/x`.
        let p = p.strip_prefix("localhost").unwrap_or(p);
        return Some(percent_decode(p));
    }
    std::path::Path::new(uri).is_absolute().then(|| uri.to_owned())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(h) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_remote(uri: &str) -> bool {
    uri.starts_with("https://") || uri.starts_with("http://")
}

/// The attachment's bytes as base64: its `data`, or the local file its `uri` names.
fn base64_of(a: &Attachment) -> Result<Option<String>, String> {
    if let Some(d) = &a.data {
        return Ok(Some(d.clone()));
    }
    let Some(path) = a.uri.as_deref().and_then(local_path) else {
        return Ok(None);
    };
    let meta = std::fs::metadata(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
    if meta.len() > MAX_BYTES {
        return Err(format!("{path} is larger than {MAX_BYTES} bytes"));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
    Ok(Some(STANDARD.encode(bytes)))
}

fn source(a: &Attachment) -> Result<Value, String> {
    if let Some(d) = base64_of(a)? {
        return Ok(json!({"type": "base64", "media_type": a.media_type, "data": d}));
    }
    match a.uri.as_deref() {
        Some(u) if is_remote(u) => Ok(json!({"type": "url", "url": u})),
        Some(u) => Err(format!("Claude Code can't fetch {u}")),
        None => Err("it has neither data nor a URI".into()),
    }
}

fn block(a: &Attachment) -> Result<Value, String> {
    let t = a.media_type.as_str();
    if IMAGE_TYPES.contains(&t) {
        return Ok(json!({"type": "image", "source": source(a)?}));
    }
    if t == "application/pdf" {
        return Ok(json!({"type": "document", "source": source(a)?, "title": a.name}));
    }
    if t.starts_with("text/") || t == "application/json" {
        let text = match base64_of(a)? {
            Some(d) => {
                let bytes = STANDARD
                    .decode(d.as_bytes())
                    .map_err(|e| format!("its data is not base64: {e}"))?;
                String::from_utf8(bytes).map_err(|_| "it is not UTF-8 text".to_owned())?
            }
            None => return Err("Claude Code takes text files only with their content".into()),
        };
        return Ok(json!({
            "type": "document",
            "source": {"type": "text", "media_type": "text/plain", "data": text},
            "title": a.name,
        }));
    }
    Err(format!("Claude Code doesn't take {t}"))
}

/// The `attachments` agent feature (spec 6).
pub fn feature() -> Value {
    let mut types: Vec<&str> = IMAGE_TYPES.to_vec();
    types.extend(["application/pdf", "text/*", "application/json"]);
    json!({
        "kinds": ["image", "file"],
        "maxBytes": MAX_BYTES,
        "types": types,
        "uri": ["https", "http", "file"],
        "context": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(name: &str, t: &str, data: Option<&str>, uri: Option<&str>) -> Attachment {
        Attachment {
            name: name.into(),
            media_type: t.into(),
            data: data.map(Into::into),
            uri: uri.map(Into::into),
        }
    }

    #[test]
    fn every_kind_becomes_a_block_or_a_reason() {
        let ctx = [ContextEntry {
            uri: "file:///work/notes.md".into(),
            title: None,
            text: "Notes".into(),
        }];
        let a = [
            att("dot.png", "image/png", Some("iVBORw0KGgo="), None),
            att(
                "cat.jpg",
                "image/jpeg",
                None,
                Some("https://example.test/cat.jpg"),
            ),
            att("spec.pdf", "application/pdf", Some("JVBERi0="), None),
            att("hi.txt", "text/plain", Some("aGVsbG8="), None),
            att("song.mp3", "audio/mpeg", Some("AAAA"), None),
        ];
        let i = content("Look", &ctx, &a);
        assert_eq!(i.blocks.len(), 6);
        assert_eq!(i.blocks[1]["source"]["data"], "Notes");
        assert_eq!(i.blocks[1]["title"], "file:///work/notes.md");
        assert_eq!(i.blocks[2]["source"]["type"], "base64");
        assert_eq!(
            i.blocks[3]["source"],
            json!({"type": "url", "url": "https://example.test/cat.jpg"})
        );
        assert_eq!(i.blocks[4]["type"], "document");
        assert_eq!(i.blocks[5]["source"]["data"], "hello");
        assert_eq!(i.skipped.len(), 1);
        assert_eq!(i.skipped[0].0, "song.mp3");
    }

    #[test]
    fn local_files_are_read() {
        let dir = std::env::temp_dir().join(format!("parrot-claude-input-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a b.png");
        std::fs::write(&p, b"PNG").unwrap();
        let uri = format!("file://{}", p.display()).replace(' ', "%20");
        let i = content("x", &[], &[att("a b.png", "image/png", None, Some(&uri))]);
        assert_eq!(i.blocks[1]["source"]["data"], "UE5H");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
