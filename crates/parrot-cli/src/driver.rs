//! `parrot driver <name>`: serves a built-in driver core on stdio (spec 30.5). The runtime builds
//! in only the general cores; harness drivers ship as binaries in their eggs.
//!
//! | Name | Core |
//! | --- | --- |
//! | `acp` | Any agent that speaks the Agent Client Protocol |
//! | `tui` | A harness driven through its terminal with the egg's `profile.kdl` |

pub async fn run(name: String) -> Result<(), String> {
    match name.as_str() {
        "acp" => parrot_driver_acp::serve_stdio()
            .await
            .map_err(|e| format!("driver acp: {e}")),
        "tui" => parrot_driver_tui::stdio::serve_stdio()
            .await
            .map_err(|e| format!("driver tui: {e}")),
        other => Err(format!(
            "no built-in driver named {other}; the built-in cores are acp and tui, and other drivers come from eggs"
        )),
    }
}
