//! A prompt's text, context and attachments (spec 13.1) as the app-server's `UserInput` list.
//!
//! | Prompt part | `UserInput` |
//! | --- | --- |
//! | context entries | `text`, before the prompt, each in a `<context>` element |
//! | image with data | `image` with a `data:` URL |
//! | image at an `https` URL | `image` with the URL |
//! | image at a local path or `file://` URL | `localImage` |
//! | audio, the same ways | `audio` or `localAudio` |
//! | any other file at a local path | `mention` of the path |
//! | text file with data | `text`, in a `<file>` element |
//! | other file with data | written under the temporary directory, then a `mention` |
//!
//! Anything else is left out with a reason, which the driver reports as a notice.

use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use parrot_proto::driver::BindingPromptParams;
use parrot_proto::prompt::Attachment;
use serde_json::{Value, json};

/// What a prompt becomes: the input items, and the attachments left out with the reason.
#[derive(Debug, Default, PartialEq)]
pub struct Input {
    pub items: Vec<Value>,
    pub skipped: Vec<(String, String)>,
}

/// A local path named by `uri`: `file://` URLs and plain absolute paths.
pub fn local_path(uri: &str) -> Option<String> {
    if let Some(p) = uri.strip_prefix("file://") {
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

fn text(t: String) -> Value {
    json!({"type": "text", "text": t, "text_elements": []})
}

/// Where an attachment with data but no path is written so Codex can read it.
fn spill_path(message: &str, name: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || ".-_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    std::env::temp_dir()
        .join("parrot-attachments")
        .join(message)
        .join(safe)
}

fn attachment(message: &str, a: &Attachment, texts: &mut Vec<String>) -> Result<Option<Value>, String> {
    let t = a.media_type.as_str();
    let image = t.starts_with("image/");
    let audio = t.starts_with("audio/");
    // Images and audio share their shapes: an inline URL, or a local path.
    let (inline, local) = if audio {
        ("audio", "localAudio")
    } else {
        ("image", "localImage")
    };
    if let Some(uri) = a.uri.as_deref() {
        if let Some(path) = local_path(uri) {
            return Ok(Some(if image || audio {
                json!({"type": local, "path": path})
            } else {
                json!({"type": "mention", "name": a.name, "path": path})
            }));
        }
        if (image || audio)
            && (uri.starts_with("https://") || uri.starts_with("http://") || uri.starts_with("data:"))
        {
            return Ok(Some(json!({"type": inline, "url": uri})));
        }
        if a.data.is_none() {
            return Err(format!("Codex can't fetch {uri}"));
        }
    }
    let Some(data) = &a.data else {
        return Err("it has neither data nor a URI".into());
    };
    if image || audio {
        return Ok(Some(
            json!({"type": inline, "url": format!("data:{t};base64,{data}")}),
        ));
    }
    let bytes = STANDARD
        .decode(data.as_bytes())
        .map_err(|e| format!("its data is not base64: {e}"))?;
    if t.starts_with("text/") || t == "application/json" {
        let body = String::from_utf8(bytes).map_err(|_| "it is not UTF-8 text".to_owned())?;
        texts.push(format!("<file name=\"{}\">\n{body}\n</file>", a.name));
        return Ok(None);
    }
    let path = spill_path(message, &a.name);
    std::fs::create_dir_all(path.parent().expect("spill paths have a parent"))
        .and_then(|_| std::fs::write(&path, bytes))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(Some(
        json!({"type": "mention", "name": a.name, "path": path.display().to_string()}),
    ))
}

/// Builds the input of a prompt.
pub fn input(p: &BindingPromptParams) -> Input {
    let mut texts = Vec::new();
    for c in p.context.iter().flatten() {
        texts.push(format!("<context uri=\"{}\">\n{}\n</context>", c.uri, c.text));
    }
    let mut rest = Vec::new();
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for a in p.attachments.iter().flatten() {
        match attachment(&p.message, a, &mut files) {
            Ok(Some(item)) => rest.push(item),
            Ok(None) => {}
            Err(why) => skipped.push((a.name.clone(), why)),
        }
    }
    texts.extend(files);
    texts.push(p.text.clone());
    let mut items = vec![text(texts.join("\n\n"))];
    items.extend(rest);
    Input { items, skipped }
}

/// The `attachments` agent feature (spec 6).
pub fn feature() -> Value {
    json!({
        "kinds": ["image", "audio", "file"],
        "types": ["image/*", "text/*", "*/*"],
        "uri": ["https", "http", "file", "data"],
        "context": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use parrot_proto::prompt::ContextEntry;

    fn att(name: &str, t: &str, data: Option<&str>, uri: Option<&str>) -> Attachment {
        Attachment {
            name: name.into(),
            media_type: t.into(),
            data: data.map(Into::into),
            uri: uri.map(Into::into),
        }
    }

    #[test]
    fn every_kind_maps() {
        let p = BindingPromptParams {
            binding: "b1".into(),
            subagent: None,
            message: "m-input-test".into(),
            text: "Look".into(),
            context: Some(vec![ContextEntry {
                uri: "file:///work/a.md".into(),
                title: None,
                text: "A".into(),
            }]),
            attachments: Some(vec![
                att("dot.png", "image/png", Some("iVBORw0KGgo="), None),
                att(
                    "cat.jpg",
                    "image/jpeg",
                    None,
                    Some("https://example.test/cat.jpg"),
                ),
                att("shot.png", "image/png", None, Some("file:///work/shot%20one.png")),
                att("main.rs", "text/x-rust", None, Some("/work/src/main.rs")),
                att("hi.txt", "text/plain", Some("aGVsbG8="), None),
                att("doc.pdf", "application/pdf", Some("JVBERi0="), None),
                att(
                    "far.pdf",
                    "application/pdf",
                    None,
                    Some("https://example.test/far.pdf"),
                ),
                att("note.wav", "audio/wav", None, Some("/work/note.wav")),
            ]),
            steer: false,
        };
        let i = input(&p);
        let t = i.items[0]["text"].as_str().unwrap();
        assert!(
            t.starts_with("<context uri=\"file:///work/a.md\">\nA\n</context>"),
            "{t}"
        );
        assert!(t.contains("<file name=\"hi.txt\">\nhello\n</file>"));
        assert!(t.ends_with("Look"));
        assert_eq!(i.items[1]["url"], "data:image/png;base64,iVBORw0KGgo=");
        assert_eq!(
            i.items[2],
            json!({"type": "image", "url": "https://example.test/cat.jpg"})
        );
        assert_eq!(
            i.items[3],
            json!({"type": "localImage", "path": "/work/shot one.png"})
        );
        assert_eq!(
            i.items[4],
            json!({"type": "mention", "name": "main.rs", "path": "/work/src/main.rs"})
        );
        assert_eq!(i.items[5]["type"], "mention");
        let spilled = i.items[5]["path"].as_str().unwrap();
        assert_eq!(std::fs::read(spilled).unwrap(), b"%PDF-");
        std::fs::remove_dir_all(std::path::Path::new(spilled).parent().unwrap()).unwrap();
        assert_eq!(
            i.items[6],
            json!({"type": "localAudio", "path": "/work/note.wav"})
        );
        assert_eq!(i.skipped.len(), 1);
        assert_eq!(i.skipped[0].0, "far.pdf");
    }
}
