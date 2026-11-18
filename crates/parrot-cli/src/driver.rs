//! `parrot driver <name>`: serves a built-in driver on stdio (spec 30.5).

pub async fn run(name: String) -> Result<(), String> {
    match name.as_str() {
        "claude" => parrot_driver_claude::serve_stdio()
            .await
            .map_err(|e| format!("driver claude: {e}")),
        "codex" => parrot_driver_codex::serve_stdio()
            .await
            .map_err(|e| format!("driver codex: {e}")),
        "acp" => parrot_driver_acp::serve_stdio()
            .await
            .map_err(|e| format!("driver acp: {e}")),
        "pi" => parrot_driver_pi::serve_stdio()
            .await
            .map_err(|e| format!("driver pi: {e}")),
        "opencode" => parrot_driver_opencode::serve_stdio()
            .await
            .map_err(|e| format!("driver opencode: {e}")),
        "t3" => parrot_driver_t3::serve_stdio()
            .await
            .map_err(|e| format!("driver t3: {e}")),
        other => Err(format!("no built-in driver named {other}")),
    }
}
