//! Merging one workspace's branch into another (spec 8, Appendix E8).
//!
//! Both workspaces must be worktrees of the same repository, which is what makes the branch names
//! visible on both sides. `into` has to be clean first: a merge on top of uncommitted changes cannot
//! be aborted without losing them. Conflicts leave `into` exactly as it was and are returned as a
//! result rather than an error, so the caller can list them.

use std::path::{Path, PathBuf};

use parrot_proto::workspace::{MergeMethod, WorkspaceMergeResult};

use crate::git::{self, Git, GitError};

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("{0} is not a git repository")]
    NotARepository(PathBuf),
    #[error("{from} and {into} are not worktrees of the same repository")]
    NotSameRepository { from: PathBuf, into: PathBuf },
    #[error("workspace {path} has uncommitted changes: {changed:?}")]
    Dirty { path: PathBuf, changed: Vec<String> },
    #[error("workspace {0} has no branch checked out")]
    NoBranch(PathBuf),
    #[error("merge failed and was aborted: {stderr}")]
    Failed { stderr: String },
    #[error(transparent)]
    Git(#[from] GitError),
}

/// Merges the branch checked out in `from` into the one checked out in `into`.
///
/// On conflicts the result lists the conflicting files and `into` is left unchanged, with no
/// `commit`. An empty result means the merge had nothing to do.
pub fn merge(
    from: &Path,
    into: &Path,
    method: MergeMethod,
    message: Option<&str>,
) -> Result<WorkspaceMergeResult, MergeError> {
    let from_common = common_dir(from)?.ok_or_else(|| MergeError::NotARepository(from.to_path_buf()))?;
    let into_common = common_dir(into)?.ok_or_else(|| MergeError::NotARepository(into.to_path_buf()))?;
    if from_common != into_common {
        return Err(MergeError::NotSameRepository {
            from: from.to_path_buf(),
            into: into.to_path_buf(),
        });
    }

    let changed = git::status(into)?;
    if !changed.is_empty() {
        return Err(MergeError::Dirty {
            path: into.to_path_buf(),
            changed: changed.into_iter().map(|entry| entry.path).collect(),
        });
    }

    let branch = git::branch(from)?.ok_or_else(|| MergeError::NoBranch(from.to_path_buf()))?;
    if git::branch(into)?.is_none() {
        return Err(MergeError::NoBranch(into.to_path_buf()));
    }
    let reference = format!("refs/heads/{branch}");
    let counts = Git::new(into).run(&["rev-list", "--count", &format!("HEAD..{reference}")])?;
    if counts.trim() == "0" {
        return Ok(WorkspaceMergeResult::default());
    }

    let git = Git::new(into);
    let result = match method {
        MergeMethod::Merge => {
            let mut args = vec!["merge", "--quiet", "--no-edit"];
            if let Some(message) = message {
                args.extend(["--message", message]);
            }
            args.push(&reference);
            git.run(&args)
        }
        MergeMethod::Squash => {
            let message = message
                .map(str::to_string)
                .unwrap_or_else(|| format!("Squash merge of {branch}"));
            squash(&git, &reference, &message)
        }
    };

    match result {
        Ok(_) => {
            let commit = git::head(into)?;
            Ok(WorkspaceMergeResult {
                commit,
                conflicts: None,
            })
        }
        Err(GitError::Failed { stderr, .. }) => {
            let conflicts = unmerged(into)?;
            abort(into, method)?;
            if conflicts.is_empty() {
                Err(MergeError::Failed { stderr })
            } else {
                Ok(WorkspaceMergeResult {
                    commit: None,
                    conflicts: Some(conflicts),
                })
            }
        }
        Err(error) => {
            abort(into, method)?;
            Err(error.into())
        }
    }
}

/// The files a merge could not reconcile.
fn unmerged(into: &Path) -> Result<Vec<String>, GitError> {
    let out = Git::new(into).run(&["diff", "--name-only", "--diff-filter=U", "-z"])?;
    Ok(git::split_nul(&out))
}

/// Returns `into` to the commit it was on, discarding the half-done merge. `into` was clean before
/// the merge started, so a hard reset loses nothing of the caller's.
fn abort(into: &Path, method: MergeMethod) -> Result<(), GitError> {
    let git = Git::new(into);
    if method == MergeMethod::Merge && git.succeeds(&["merge", "--abort"])? {
        return Ok(());
    }
    git.run(&["reset", "--hard", "--quiet", "HEAD"])?;
    Ok(())
}

fn common_dir(path: &Path) -> Result<Option<PathBuf>, GitError> {
    if !git::is_repository(path)? {
        return Ok(None);
    }
    let out = Git::new(path).run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let directory = PathBuf::from(&out);
    Ok(Some(directory.canonicalize().unwrap_or(directory)))
}

/// `git merge --squash` stages the result, so the caller still has to commit it.
fn squash(git: &Git<'_>, reference: &str, message: &str) -> Result<String, GitError> {
    git.run(&["merge", "--squash", reference])?;
    git.run(&["commit", "--quiet", "--message", message])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Repo;
    use crate::worktree::{NewWorktree, Worktree, create};

    fn worktree(repo: &Repo, name: &str, branch: &str) -> Worktree {
        create(NewWorktree {
            repo: repo.path(),
            path: &repo.parent().join(name),
            branch,
            base: Some("main"),
        })
        .unwrap()
    }

    fn commit(worktree: &Worktree, file: &str, contents: &str, message: &str) -> String {
        std::fs::write(worktree.path.join(file), contents).unwrap();
        let git = Git::new(&worktree.path);
        git.run(&["add", "--all"]).unwrap();
        git.run(&["commit", "--quiet", "--message", message]).unwrap();
        git.run(&["rev-parse", "HEAD"]).unwrap()
    }

    #[test]
    fn merges_a_branch_into_the_other_workspace() {
        let repo = Repo::new();
        let from = worktree(&repo, "from", "parrot/ses_alpha");
        let into = worktree(&repo, "into", "parrot/ses_beta");
        let tip = commit(&from, "change.txt", "from the branch\n", "branch work");

        let result = merge(&from.path, &into.path, MergeMethod::Merge, None).unwrap();
        assert!(result.conflicts.is_none());
        assert_eq!(result.commit.as_deref(), Some(tip.as_str()));
        assert_eq!(
            std::fs::read_to_string(into.path.join("change.txt")).unwrap(),
            "from the branch\n"
        );
        assert!(git::status(&into.path).unwrap().is_empty());
    }

    #[test]
    fn squash_merges_into_one_commit() {
        let repo = Repo::new();
        let from = worktree(&repo, "from", "parrot/ses_alpha");
        let into = worktree(&repo, "into", "parrot/ses_beta");
        commit(&from, "one.txt", "one\n", "first");
        commit(&from, "two.txt", "two\n", "second");
        let before = git::head(&into.path).unwrap().unwrap();

        let result = merge(&from.path, &into.path, MergeMethod::Squash, Some("two into into")).unwrap();
        let commit = result.commit.unwrap();
        assert_ne!(commit, before);
        assert_eq!(into_head_message(&into), "two into into");
        assert!(into.path.join("one.txt").is_file());
        assert!(into.path.join("two.txt").is_file());
    }

    fn into_head_message(worktree: &Worktree) -> String {
        Git::new(&worktree.path)
            .run(&["log", "-1", "--format=%s"])
            .unwrap()
    }

    #[test]
    fn reports_conflicts_and_leaves_the_workspace_alone() {
        let repo = Repo::new();
        let from = worktree(&repo, "from", "parrot/ses_alpha");
        let into = worktree(&repo, "into", "parrot/ses_beta");
        commit(&from, "shared.txt", "from\n", "from changes shared");
        commit(&into, "shared.txt", "into\n", "into changes shared");
        let head = git::head(&into.path).unwrap();

        let result = merge(&from.path, &into.path, MergeMethod::Merge, None).unwrap();
        assert_eq!(result.commit, None);
        assert_eq!(result.conflicts, Some(vec!["shared.txt".to_string()]));
        assert_eq!(git::head(&into.path).unwrap(), head);
        assert!(git::status(&into.path).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(into.path.join("shared.txt")).unwrap(),
            "into\n"
        );
    }

    #[test]
    fn refuses_a_dirty_target_and_another_repository() {
        let repo = Repo::new();
        let from = worktree(&repo, "from", "parrot/ses_alpha");
        let into = worktree(&repo, "into", "parrot/ses_beta");
        commit(&from, "change.txt", "change\n", "branch work");
        std::fs::write(into.path.join("README.md"), "edited\n").unwrap();

        let error = merge(&from.path, &into.path, MergeMethod::Merge, None).unwrap_err();
        assert!(matches!(error, MergeError::Dirty { .. }));

        std::fs::write(into.path.join("README.md"), "# A made-up project\n").unwrap();
        let other = Repo::new();
        let error = merge(&from.path, other.path(), MergeMethod::Merge, None).unwrap_err();
        assert!(matches!(error, MergeError::NotSameRepository { .. }));
    }

    #[test]
    fn does_nothing_when_the_branch_is_merged() {
        let repo = Repo::new();
        let from = worktree(&repo, "from", "parrot/ses_alpha");
        let into = worktree(&repo, "into", "parrot/ses_beta");
        let result = merge(&from.path, &into.path, MergeMethod::Merge, None).unwrap();
        assert_eq!(result, WorkspaceMergeResult::default());
    }
}
