//! Throwaway repositories for tests that run real git.
//!
//! Each test gets its own temporary directory with a repository in `repo/`, configured with a
//! made-up identity so git never reads the machine's configuration.

use std::path::{Path, PathBuf};

use crate::git::Git;

pub struct Repo {
    dir: tempfile::TempDir,
    path: PathBuf,
}

impl Repo {
    /// A repository with one commit in `README.md`.
    pub fn new() -> Self {
        let repo = Self::empty();
        repo.write("README.md", "# A made-up project\n");
        repo.commit("first commit");
        repo
    }

    /// A repository with no commits yet.
    pub fn empty() -> Self {
        Self::empty_on("main")
    }

    /// A repository with no commits yet, whose checked-out branch is `branch`.
    pub fn empty_on(branch: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repo");
        std::fs::create_dir(&path).unwrap();
        let repo = Self { dir, path };
        repo.git(&["init", "--quiet", "--initial-branch", branch]);
        repo.git(&["config", "user.name", "Robin Feather"]);
        repo.git(&["config", "user.email", "robin@example.test"]);
        repo
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The directory holding the repository, for worktrees and bundles beside it.
    pub fn parent(&self) -> &Path {
        self.dir.path()
    }

    pub fn git(&self, args: &[&str]) -> String {
        Git::new(&self.path)
            .run(args)
            .unwrap_or_else(|error| panic!("git {args:?} failed: {error}"))
    }

    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        path
    }

    pub fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.path.join(relative)).unwrap()
    }

    pub fn commit(&self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--message", message]);
        self.head()
    }

    pub fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }

    /// A commit object's subject, read back for assertions.
    pub fn commit_message(&self, rev: &str) -> String {
        self.git(&["log", "-1", "--format=%s", rev])
    }
}
