//! Workspaces for the Parrot runtime: folders, git worktrees, merges, transfers and checkpoints.
//!
//! Everything here works on the filesystem and on repositories through the `git` command; the
//! crate owns no state of its own and does not know about sessions. The runtime decides which
//! workspace a session may use ([`fence`]), then calls the module that does the work:
//!
//! - [`dir`] validates a folder workspace and reports its status.
//! - [`worktree`] creates, lists, inspects and removes worktrees.
//! - [`merge`] merges one worktree's branch into another workspace.
//! - [`transfer`] packs a branch and its uncommitted changes for another machine.
//! - [`checkpoint`] records and restores a workspace at the start of a turn.
//!
//! Git calls run with a clean environment ([`git`]), so an inherited `GIT_DIR` or hook cannot point
//! a call at another repository.

pub mod checkpoint;
pub mod dir;
pub mod fence;
pub mod git;
pub mod merge;
pub mod transfer;
pub mod worktree;

#[cfg(test)]
mod testing;
