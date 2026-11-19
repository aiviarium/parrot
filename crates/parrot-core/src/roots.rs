//! The environment's roots fence (spec 7, 8): every folder a client or an agent picks is inside
//! one of them.
//!
//! The comparison is [`parrot_workspace::fence`]'s, so `..` and symbolic links are resolved before
//! the prefix is tested and a link out of the roots is refused. A runtime with no roots set, as in
//! the tests here, fences nothing; `parrot serve` always sets at least the owner's home.

use crate::State;
use parrot_proto::rpc::RpcError;
use std::path::Path;

/// Refuses `path` unless it is inside one of the runtime's roots.
pub fn check(state: &State, path: &str) -> Result<(), RpcError> {
    let roots = &state.principals.roots;
    if roots.is_empty() {
        return Ok(());
    }
    parrot_workspace::fence::check_roots(Path::new(path), roots)
        .map(|_| ())
        .map_err(|e| crate::ceilings::forbidden("roots", e.to_string()))
}

/// Refuses a session's `cwd` unless it is inside its workspace, which is itself inside the roots.
pub fn check_cwd(state: &State, workspace: &str, cwd: &str) -> Result<(), RpcError> {
    check(state, cwd)?;
    match parrot_workspace::fence::check(Path::new(workspace), Path::new(cwd)) {
        Ok(_) => Ok(()),
        // A workspace that doesn't exist yet has nothing to compare with; its roots check stands.
        Err(parrot_workspace::fence::FenceError::Io { .. }) => Ok(()),
        Err(_) => Err(crate::ceilings::forbidden(
            "workspace",
            format!("{cwd} is outside the session's workspace"),
        )),
    }
}
