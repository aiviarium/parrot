//! Git worktrees: one branch per workspace, cut from a base ref.
//!
//! The runtime chooses the directory and usually the branch ([`default_branch`], which spec 8 gives
//! as `parrot/<session>`); this module only runs the git commands and reports what happened.

use std::path::{Path, PathBuf};

use crate::git::{self, Git, GitError};

/// Branch names of worktrees Parrot creates, unless the caller names one.
pub const BRANCH_PREFIX: &str = "parrot/";

/// The branch a worktree for `session` gets by default.
pub fn default_branch(session: &str) -> String {
    format!("{BRANCH_PREFIX}{session}")
}

/// What to create.
#[derive(Debug, Clone, Copy)]
pub struct NewWorktree<'a> {
    /// The repository the worktree belongs to.
    pub repo: &'a Path,
    /// The directory for the new worktree; it must not exist yet.
    pub path: &'a Path,
    /// The branch to check out. An existing branch is attached to, a new one is cut from `base`.
    pub branch: &'a str,
    /// Any commit, branch or tag the new branch starts at; `HEAD` when absent.
    pub base: Option<&'a str>,
}

/// An existing worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub repo: PathBuf,
    pub branch: String,
    /// The ref the branch was cut from, as the caller named it.
    pub base: String,
}

/// One entry of `git worktree list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub path: PathBuf,
    pub commit: String,
    /// The full ref, such as `refs/heads/main`; `None` when detached.
    pub branch: Option<String>,
    pub bare: bool,
    pub locked: bool,
}

/// How a worktree stands against its base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub dirty: bool,
    /// Changed paths relative to the top of the worktree.
    pub changed: Vec<String>,
    /// Commits on the worktree's branch that the base does not have.
    pub ahead: u32,
    /// Commits on the base that the worktree's branch does not have.
    pub behind: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("repository {0} is not a git repository")]
    NotARepository(PathBuf),
    #[error("worktree path {0} already exists")]
    Exists(PathBuf),
    #[error("cannot create {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} is not a known commit, branch or tag")]
    UnknownBase(String),
    #[error("worktree {path} has uncommitted changes: {changed:?}")]
    Dirty { path: PathBuf, changed: Vec<String> },
    #[error("worktree {path} has {commits} commits on {branch} that no other branch has")]
    Unmerged {
        path: PathBuf,
        branch: String,
        commits: u64,
    },
    #[error("could not read the commit counts from {0:?}")]
    Counts(String),
    #[error(transparent)]
    Git(#[from] GitError),
}

impl Worktree {
    pub fn status(&self) -> Result<Status, WorktreeError> {
        status(&self.path, &self.base)
    }
}

/// Creates a worktree, cutting a branch from `base` unless the branch already exists.
pub fn create(spec: NewWorktree<'_>) -> Result<Worktree, WorktreeError> {
    if !git::is_repository(spec.repo)? {
        return Err(WorktreeError::NotARepository(spec.repo.to_path_buf()));
    }
    if spec.path.exists() {
        return Err(WorktreeError::Exists(spec.path.to_path_buf()));
    }
    let base = spec.base.unwrap_or("HEAD");
    if let Some(parent) = spec.path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| WorktreeError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let path = spec.path.to_string_lossy().into_owned();
    let git = Git::new(spec.repo);
    let exists = git.succeeds(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/heads/{}", spec.branch),
    ])?;
    if exists {
        git.run(&["worktree", "add", "--quiet", &path, spec.branch])?;
    } else {
        if git::rev_parse(spec.repo, base)?.is_none() {
            return Err(WorktreeError::UnknownBase(base.to_string()));
        }
        git.run(&["worktree", "add", "--quiet", "-b", spec.branch, &path, base])?;
    }

    Ok(Worktree {
        path: spec.path.to_path_buf(),
        repo: spec.repo.to_path_buf(),
        branch: spec.branch.to_string(),
        base: base.to_string(),
    })
}

/// Every worktree of a repository, the main one included.
pub fn list(repo: &Path) -> Result<Vec<Listed>, WorktreeError> {
    if !git::is_repository(repo)? {
        return Err(WorktreeError::NotARepository(repo.to_path_buf()));
    }
    let out = Git::new(repo).run(&["worktree", "list", "--porcelain"])?;
    let mut listed: Vec<Listed> = Vec::new();
    for line in out.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "worktree" => listed.push(Listed {
                path: PathBuf::from(value),
                commit: String::new(),
                branch: None,
                bare: false,
                locked: false,
            }),
            "HEAD" => {
                if let Some(entry) = listed.last_mut() {
                    entry.commit = value.to_string();
                }
            }
            "branch" => {
                if let Some(entry) = listed.last_mut() {
                    entry.branch = Some(value.to_string());
                }
            }
            "bare" => {
                if let Some(entry) = listed.last_mut() {
                    entry.bare = true;
                }
            }
            "locked" => {
                if let Some(entry) = listed.last_mut() {
                    entry.locked = true;
                }
            }
            _ => {}
        }
    }
    Ok(listed)
}

/// The status of the worktree at `path`, measured against `base`.
pub fn status(path: &Path, base: &str) -> Result<Status, WorktreeError> {
    if git::rev_parse(path, base)?.is_none() {
        return Err(WorktreeError::UnknownBase(base.to_string()));
    }
    let changed = git::status(path)?;
    let counts = Git::new(path).run(&["rev-list", "--left-right", "--count", &format!("{base}...HEAD")])?;
    let (behind, ahead) = parse_counts(&counts)?;
    Ok(Status {
        branch: git::branch(path)?,
        commit: git::head(path)?,
        dirty: !changed.is_empty(),
        changed: changed.into_iter().map(|entry| entry.path).collect(),
        ahead,
        behind,
    })
}

fn parse_counts(counts: &str) -> Result<(u32, u32), WorktreeError> {
    let mut parts = counts.split_whitespace();
    let left = parts.next().and_then(|part| part.parse().ok());
    let right = parts.next().and_then(|part| part.parse().ok());
    match (left, right) {
        (Some(behind), Some(ahead)) => Ok((behind, ahead)),
        _ => Err(WorktreeError::Counts(counts.to_string())),
    }
}

/// Removes a worktree. Without `force` a worktree with uncommitted changes is refused, and
/// `force` also discards the changes git needs a second `--force` for, such as in a submodule.
/// How many commits of the worktree's `HEAD` no other branch reaches, so removing the
/// worktree and later its branch would lose them.
fn unmerged(worktree: &Worktree) -> Result<u64, WorktreeError> {
    // `--exclude` before `--branches` names the branch relative to `refs/heads`.
    let own = format!("--exclude={}", worktree.branch);
    let mut args = vec!["rev-list", "--count", "HEAD", "--not"];
    if !worktree.branch.is_empty() {
        args.push(&own);
    }
    args.push("--branches");
    let out = Git::new(&worktree.path).run(&args)?;
    out.trim().parse().map_err(|_| WorktreeError::Counts(out.clone()))
}

pub fn remove(worktree: &Worktree, force: bool) -> Result<(), WorktreeError> {
    if !force {
        let status = worktree.status()?;
        if status.dirty {
            return Err(WorktreeError::Dirty {
                path: worktree.path.clone(),
                changed: status.changed,
            });
        }
        let commits = unmerged(worktree)?;
        if commits > 0 {
            return Err(WorktreeError::Unmerged {
                path: worktree.path.clone(),
                branch: worktree.branch.clone(),
                commits,
            });
        }
    }
    let path = worktree.path.to_string_lossy().into_owned();
    // The second `--force` also removes what git only discards when asked twice, such as a
    // worktree with a modified submodule.
    let args: &[&str] = if force {
        &["worktree", "remove", "--force", "--force", &path]
    } else {
        &["worktree", "remove", &path]
    };
    Git::new(&worktree.repo).run(args)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Repo;

    fn new_worktree(repo: &Repo, name: &str, branch: &str) -> Worktree {
        let path = repo.parent().join(name);
        create(NewWorktree {
            repo: repo.path(),
            path: &path,
            branch,
            base: Some("main"),
        })
        .unwrap()
    }

    #[test]
    fn creates_a_branch_and_a_worktree() {
        let repo = Repo::new();
        let worktree = new_worktree(&repo, "feature", "parrot/ses_alpha");
        assert!(worktree.path.join("README.md").is_file());
        assert_eq!(worktree.branch, "parrot/ses_alpha");
        assert_eq!(worktree.base, "main");

        let branches = repo.git(&["branch", "--format=%(refname:short)"]);
        assert!(branches.lines().any(|line| line == "parrot/ses_alpha"));

        let status = worktree.status().unwrap();
        assert_eq!(status.branch.as_deref(), Some("parrot/ses_alpha"));
        assert!(!status.dirty);
        assert_eq!((status.ahead, status.behind), (0, 0));
    }

    #[test]
    fn reports_changes_and_distance_from_the_base() {
        let repo = Repo::new();
        let worktree = new_worktree(&repo, "feature", "parrot/ses_alpha");
        std::fs::write(worktree.path.join("README.md"), "# Worktree\n").unwrap();
        let status = worktree.status().unwrap();
        assert!(status.dirty);
        assert_eq!(status.changed, vec!["README.md".to_string()]);

        git::Git::new(&worktree.path)
            .run(&["commit", "--quiet", "--all", "--message", "worktree change"])
            .unwrap();
        let status = worktree.status().unwrap();
        assert!(!status.dirty);
        assert_eq!((status.ahead, status.behind), (1, 0));

        repo.write("main.txt", "on main\n");
        repo.commit("main moves on");
        let status = worktree.status().unwrap();
        assert_eq!((status.ahead, status.behind), (1, 1));
    }

    #[test]
    fn lists_every_worktree() {
        let repo = Repo::new();
        let worktree = new_worktree(&repo, "feature", "parrot/ses_alpha");
        let listed = list(repo.path()).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|entry| entry.path == worktree.path));
        let main = listed.iter().find(|entry| entry.path == repo.path()).unwrap();
        assert_eq!(main.branch.as_deref(), Some("refs/heads/main"));
        assert_eq!(main.commit, repo.head());
    }

    #[test]
    fn refuses_a_dirty_worktree_unless_forced() {
        let repo = Repo::new();
        let worktree = new_worktree(&repo, "feature", "parrot/ses_alpha");
        repo.write("unrelated.txt", "not in the worktree\n");
        std::fs::write(worktree.path.join("README.md"), "# Edited\n").unwrap();

        let error = remove(&worktree, false).unwrap_err();
        assert!(matches!(error, WorktreeError::Dirty { .. }));
        assert!(worktree.path.exists());

        remove(&worktree, true).unwrap();
        assert!(!worktree.path.exists());
        assert!(
            repo.git(&["branch", "--format=%(refname:short)"])
                .lines()
                .any(|line| line == "parrot/ses_alpha")
        );
    }

    #[test]
    fn removal_refuses_commits_no_other_branch_has() {
        let repo = Repo::new();
        let worktree = new_worktree(&repo, "feature", "parrot/ses_alpha");
        // A worktree with nothing of its own goes.
        let empty = new_worktree(&repo, "empty", "parrot/ses_beta");
        remove(&empty, false).unwrap();

        std::fs::write(worktree.path.join("notes.txt"), "work\n").unwrap();
        let git = Git::new(&worktree.path);
        git.run(&["add", "notes.txt"]).unwrap();
        git.run(&["commit", "--quiet", "-m", "Add notes"]).unwrap();
        let error = remove(&worktree, false).unwrap_err();
        assert!(
            matches!(error, WorktreeError::Unmerged { commits: 1, .. }),
            "{error}"
        );
        assert!(worktree.path.exists());

        // Once another branch has the commit, the worktree may go.
        repo.git(&["branch", "keep", "parrot/ses_alpha"]);
        remove(&worktree, false).unwrap();
        assert!(!worktree.path.exists());
    }

    #[test]
    fn refuses_a_path_in_use_and_an_unknown_base() {
        let repo = Repo::new();
        let path = repo.parent().join("feature");
        std::fs::create_dir(&path).unwrap();
        let error = create(NewWorktree {
            repo: repo.path(),
            path: &path,
            branch: "parrot/ses_alpha",
            base: None,
        })
        .unwrap_err();
        assert!(matches!(error, WorktreeError::Exists(_)));

        let error = create(NewWorktree {
            repo: repo.path(),
            path: &repo.parent().join("other"),
            branch: "parrot/ses_beta",
            base: Some("no-such-branch"),
        })
        .unwrap_err();
        assert!(matches!(error, WorktreeError::UnknownBase(_)));
    }

    #[test]
    fn names_a_default_branch_per_session() {
        assert_eq!(default_branch("ses_alpha"), "parrot/ses_alpha");
    }
}
