//! A prompt's context and attachments (spec 13.1) for a terminal UI.
//!
//! Context goes into the prompt text. An attachment reaches the harness only through the
//! profile's `attach` action, which gets the file's local path as `$path`: Claude Code and Codex
//! take an image or file when its path is pasted. Attachments with data but no path are written
//! under the temporary directory first. A remote URL, or any attachment when the profile has no
//! `attach` action, is left out with a reason.

use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use parrot_proto::driver::BindingPromptParams;
use parrot_proto::prompt::Attachment;
use serde_json::{Value, json};

#[derive(Debug, Default, PartialEq)]
pub struct Input {
    /// The text to submit: context entries, then the prompt.
    pub text: String,
    /// Local paths to attach, in order.
    pub paths: Vec<String>,
    pub skipped: Vec<(String, String)>,
}

fn local_path(uri: &str) -> Option<String> {
    if let Some(p) = uri.strip_prefix("file://") {
        let p = p.strip_prefix("localhost").unwrap_or(p);
        return Some(p.replace("%20", " "));
    }
    std::path::Path::new(uri).is_absolute().then(|| uri.to_owned())
}

fn path_of(message: &str, a: &Attachment) -> Result<String, String> {
    if let Some(p) = a.uri.as_deref().and_then(local_path) {
        return Ok(p);
    }
    let Some(data) = &a.data else {
        return Err(match &a.uri {
            Some(u) => format!("the terminal can't fetch {u}"),
            None => "it has neither data nor a URI".into(),
        });
    };
    let bytes = STANDARD
        .decode(data.as_bytes())
        .map_err(|e| format!("its data is not base64: {e}"))?;
    let safe: String = a
        .name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || ".-_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path: PathBuf = std::env::temp_dir()
        .join("parrot-attachments")
        .join(message)
        .join(safe);
    std::fs::create_dir_all(path.parent().expect("has a parent"))
        .and_then(|_| std::fs::write(&path, bytes))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

/// Builds the input of a prompt; `can_attach` says whether the profile has an `attach` action.
pub fn input(p: &BindingPromptParams, can_attach: bool) -> Input {
    let mut parts: Vec<String> = p
        .context
        .iter()
        .flatten()
        .map(|c| format!("<context uri=\"{}\">\n{}\n</context>", c.uri, c.text))
        .collect();
    parts.push(p.text.clone());
    let mut input = Input {
        text: parts.join("\n\n"),
        ..Default::default()
    };
    for a in p.attachments.iter().flatten() {
        if !can_attach {
            input.skipped.push((
                a.name.clone(),
                "this harness's profile has no attach action".into(),
            ));
            continue;
        }
        match path_of(&p.message, a) {
            Ok(path) => input.paths.push(path),
            Err(why) => input.skipped.push((a.name.clone(), why)),
        }
    }
    input
}

/// The `attachments` agent feature for a profile with or without an `attach` action.
pub fn feature(can_attach: bool) -> Option<Value> {
    can_attach.then(|| json!({"kinds": ["image", "file"], "uri": ["file"], "context": true}))
}
