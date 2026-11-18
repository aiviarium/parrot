//! Running the `git` command.
//!
//! Every call runs in one directory and starts from a clean environment: inherited `GIT_*`
//! variables are dropped, so a hook, a test runner or a wrapper cannot redirect the call to another
//! repository or index. Locale is pinned to `C` so error messages, which end up in typed errors,
//! read the same everywhere, and prompts are off so a call never waits for input.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// How a `git` invocation failed.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("could not run git in {dir}: {source}")]
    Spawn {
        dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("git {command} failed: {stderr}")]
    Failed {
        command: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("git {command} wrote output that is not UTF-8")]
    NotUtf8 { command: String },
}

/// One changed path, as `git status --porcelain` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    /// The index and working tree letters, such as `M`, `??` or `D`. Trailing blanks are dropped.
    pub status: String,
    /// The path relative to the top of the worktree.
    pub path: String,
}

/// `git` in one directory.
#[derive(Debug, Clone, Copy)]
pub struct Git<'a> {
    dir: &'a Path,
}

impl<'a> Git<'a> {
    pub fn new(dir: &'a Path) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &'a Path {
        self.dir
    }

    /// Runs `git` with `args` and returns stdout without its trailing newline.
    pub fn run(&self, args: &[&str]) -> Result<String, GitError> {
        self.run_with(args, &[])
    }

    /// Runs `git` with extra environment on top of the clean one, for example a temporary
    /// `GIT_INDEX_FILE`.
    pub fn run_with(&self, args: &[&str], env: &[(&str, &OsStr)]) -> Result<String, GitError> {
        let output = self.output(args, env)?;
        let command = describe(args);
        if !output.status.success() {
            return Err(GitError::Failed {
                command,
                code: output.status.code(),
                stderr: text(&output.stderr),
            });
        }
        let mut stdout = String::from_utf8(output.stdout).map_err(|_| GitError::NotUtf8 { command })?;
        if stdout.ends_with('\n') {
            stdout.pop();
        }
        Ok(stdout)
    }

    /// Runs `git` and returns stdout untouched, for patches and other binary-safe output.
    pub fn bytes(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        let output = self.output(args, &[])?;
        if !output.status.success() {
            return Err(GitError::Failed {
                command: describe(args),
                code: output.status.code(),
                stderr: text(&output.stderr),
            });
        }
        Ok(output.stdout)
    }

    /// Whether a command succeeds; a non-zero exit is `Ok(false)` rather than an error.
    pub fn succeeds(&self, args: &[&str]) -> Result<bool, GitError> {
        Ok(self.output(args, &[])?.status.success())
    }

    fn output(&self, args: &[&str], env: &[(&str, &OsStr)]) -> Result<Output, GitError> {
        let mut command = Command::new("git");
        command.arg("-C").arg(self.dir).args(args);
        command.env_clear();
        for (key, value) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                continue;
            }
            command.env(key, value);
        }
        command
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0");
        for (key, value) in env {
            command.env(key, value);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.output().map_err(|source| GitError::Spawn {
            dir: self.dir.to_path_buf(),
            source,
        })
    }
}

fn describe(args: &[&str]) -> String {
    args.join(" ")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// The top of the worktree `dir` is in, or `None` when it is not in a repository.
pub fn toplevel(dir: &Path) -> Result<Option<PathBuf>, GitError> {
    match Git::new(dir).run(&["rev-parse", "--show-toplevel"]) {
        Ok(top) if !top.is_empty() => Ok(Some(PathBuf::from(top))),
        Ok(_) => Ok(None),
        // Outside a repository git exits non-zero with "not a git repository"; that is an answer,
        // not a failure to report.
        Err(GitError::Failed { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Whether `dir` is inside a git worktree.
pub fn is_repository(dir: &Path) -> Result<bool, GitError> {
    Ok(toplevel(dir)?.is_some())
}

/// The commit `rev` names, or `None` when it names nothing.
pub fn rev_parse(dir: &Path, rev: &str) -> Result<Option<String>, GitError> {
    match Git::new(dir).run(&["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]) {
        Ok(commit) => Ok(Some(commit)),
        // `--quiet` turns every "no such ref" and "ambiguous argument" into exit code 1.
        Err(GitError::Failed { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The branch `HEAD` points at, or `None` when `HEAD` is detached.
pub fn branch(dir: &Path) -> Result<Option<String>, GitError> {
    match Git::new(dir).run(&["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(name) if !name.is_empty() => Ok(Some(name)),
        Ok(_) => Ok(None),
        Err(GitError::Failed { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The commit `HEAD` is at, or `None` in a repository with no commits.
pub fn head(dir: &Path) -> Result<Option<String>, GitError> {
    rev_parse(dir, "HEAD")
}

/// The changed paths in `dir`, tracked or untracked, with untracked folders expanded.
pub fn status(dir: &Path) -> Result<Vec<Changed>, GitError> {
    let out = Git::new(dir).run(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    let mut records = out.split('\0');
    let mut changed = Vec::new();
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let (status, path) = record.split_at(3);
        let status = status.trim_end().to_string();
        // A rename or copy is followed by the original path as its own record.
        if status.starts_with('R') || status.starts_with('C') {
            let _ = records.next();
        }
        changed.push(Changed {
            status,
            path: path.to_string(),
        });
    }
    Ok(changed)
}

/// Whether anything in `dir` is modified, staged, deleted or untracked.
pub fn is_dirty(dir: &Path) -> Result<bool, GitError> {
    Ok(!status(dir)?.is_empty())
}

/// The untracked files in `dir`; ignored files are left out.
pub fn untracked(dir: &Path) -> Result<Vec<String>, GitError> {
    let out = Git::new(dir).run(&["ls-files", "--others", "--exclude-standard", "-z"])?;
    Ok(split_nul(&out))
}

/// A `-z` separated list as strings, with the empty tail dropped.
pub fn split_nul(out: &str) -> Vec<String> {
    out.split('\0')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// A configuration value, or `None` when it is not set.
pub fn config(dir: &Path, key: &str) -> Result<Option<String>, GitError> {
    match Git::new(dir).run(&["config", "--get", key]) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) => Ok(None),
        Err(GitError::Failed { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Values that make `git commit-tree` work in a repository without identity of its own.
pub fn identity(dir: &Path) -> Result<Vec<(String, OsString)>, GitError> {
    let name = config(dir, "user.name")?.unwrap_or_else(|| "Parrot".to_string());
    let email = config(dir, "user.email")?.unwrap_or_else(|| "parrot@parrot.invalid".to_string());
    let mut env = Vec::new();
    for key in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
        env.push((key.to_string(), OsString::from(name.clone())));
    }
    for key in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
        env.push((key.to_string(), OsString::from(email.clone())));
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Repo;

    #[test]
    fn reports_a_dirty_tree() {
        let repo = Repo::new();
        assert!(status(repo.path()).unwrap().is_empty());

        repo.write("notes.txt", "one\n");
        let changed = status(repo.path()).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].path, "notes.txt");
        assert_eq!(changed[0].status, "??");

        repo.write("README.md", "two\n");
        let changed = status(repo.path()).unwrap();
        assert_eq!(changed.len(), 2);
        assert!(
            changed
                .iter()
                .any(|entry| entry.path == "README.md" && entry.status.trim() == "M")
        );
        assert!(
            changed
                .iter()
                .any(|entry| entry.path == "notes.txt" && entry.status == "??")
        );
        assert!(is_dirty(repo.path()).unwrap());
        assert_eq!(untracked(repo.path()).unwrap(), vec!["notes.txt".to_string()]);
    }

    #[test]
    fn reports_a_failed_command_with_stderr() {
        let repo = Repo::new();
        let error = Git::new(repo.path())
            .run(&["rev-parse", "--verify", "refs/heads/missing"])
            .unwrap_err();
        match error {
            GitError::Failed {
                command,
                code,
                stderr,
            } => {
                assert_eq!(command, "rev-parse --verify refs/heads/missing");
                assert_eq!(code, Some(128));
                assert!(!stderr.is_empty(), "stderr was empty");
            }
            other => panic!("expected a failed command, got {other:?}"),
        }
    }

    #[test]
    fn tells_a_repository_from_a_plain_folder() {
        let repo = Repo::new();
        let top = toplevel(repo.path()).unwrap().unwrap();
        assert_eq!(top.canonicalize().unwrap(), repo.path().canonicalize().unwrap());
        assert!(is_repository(repo.path()).unwrap());
        assert_eq!(branch(repo.path()).unwrap().as_deref(), Some("main"));
        assert_eq!(head(repo.path()).unwrap(), Some(repo.head()));
        assert!(rev_parse(repo.path(), "nope").unwrap().is_none());

        let plain = tempfile::tempdir().unwrap();
        assert!(toplevel(plain.path()).unwrap().is_none());
        assert!(!is_repository(plain.path()).unwrap());
        assert!(branch(plain.path()).unwrap().is_none());
    }

    #[test]
    fn splits_nul_separated_paths() {
        assert_eq!(split_nul("a\0b\0"), vec!["a".to_string(), "b".to_string()]);
        assert!(split_nul("").is_empty());
    }
}
