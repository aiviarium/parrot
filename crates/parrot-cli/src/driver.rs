//! `parrot driver <name>`: serves a built-in driver on stdio (spec 30.5).

pub async fn run(name: String) -> Result<(), String> {
    match name.as_str() {
        "claude" => parrot_driver_claude::serve_stdio()
            .await
            .map_err(|e| format!("driver claude: {e}")),
        other => Err(format!("no built-in driver named {other}")),
    }
}
