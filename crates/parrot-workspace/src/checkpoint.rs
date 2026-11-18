//! Checkpoints: a workspace recorded at the start of a turn (spec 35).
//!
//! A checkpoint is a commit on `refs/parrot/<session>/<turn>` holding the tracked changes and the
//! untracked files that are not ignored. It is built in a temporary index through `git write-tree`
//! and `git commit-tree`, so the real index and the working tree are left exactly as they were: the
//! user's staging survives a checkpoint, and a checkpoint cannot itself show up in `git status`.
//!
//! Restoring is for rewind with files (spec 9.4). It writes the checkpoint's files back into the
//! working tree and removes tracked files the checkpoint does not have.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::git::{self, Git, GitError};

/// Where checkpoints live (spec 35). Never a branch, so no fetch or push sees them.
pub const REF_PREFIX: &str = "refs/parrot/";

/// How long a checkpoint is kept by default.
pub const KEEP: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// A recorded turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub reference: String,
    pub commit: String,
    pub session: String,
    pub turn: u32,
    /// When the commit was made.
    pub at: SystemTime,
}

#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("{0} is not a git repository")]
    NotARepository(PathBuf),
    #[error("{0} is not a usable session id")]
    InvalidSession(String),
    #[error("there is no checkpoint of turn {turn} for session {session}")]
    NotFound { session: String, turn: u32 },
    #[error("cannot read or write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Git(#[from] GitError),
}

/// The ref a checkpoint of `turn` goes on.
pub fn reference(session: &str, turn: u32) -> Result<String, CheckpointError> {
    if !valid_session(session) {
        return Err(CheckpointError::InvalidSession(session.to_string()));
    }
    Ok(format!("{REF_PREFIX}{session}/{turn}"))
}

/// Records the workspace now.
pub fn record(workspace: &Path, session: &str, turn: u32) -> Result<Checkpoint, CheckpointError> {
    record_at(workspace, session, turn, SystemTime::now())
}

/// Records the workspace as it is, stamped with `at`; the runtime passes the current time and tests
/// pass an older one.
pub fn record_at(
    workspace: &Path,
    session: &str,
    turn: u32,
    at: SystemTime,
) -> Result<Checkpoint, CheckpointError> {
    let reference = reference(session, turn)?;
    require_repository(workspace)?;
    let git = Git::new(workspace);
    let index = TempIndex::new(workspace)?;
    let env = index_env(&index, workspace, Some(at))?;

    let parent = git::head(workspace)?;
    match &parent {
        Some(_) => run_with(&git, &["read-tree", "HEAD"], &env)?,
        None => run_with(&git, &["read-tree", "--empty"], &env)?,
    };
    // `add` writes to the temporary index only; the working tree is read, never written.
    run_with(&git, &["add", "--all", "--", "."], &env)?;
    let tree = run_with(&git, &["write-tree"], &env)?;

    let message = format!("checkpoint {session} turn {turn}");
    let mut args = vec!["commit-tree", tree.as_str(), "-m", message.as_str()];
    if let Some(parent) = parent.as_deref() {
        args.extend(["-p", parent]);
    }
    let commit = run_with(&git, &args, &env)?;

    match git::rev_parse(workspace, &reference)? {
        Some(previous) => git.run(&["update-ref", &reference, &commit, &previous])?,
        None => git.run(&["update-ref", &reference, &commit])?,
    };
    Ok(Checkpoint {
        reference,
        commit,
        session: session.to_string(),
        turn,
        at,
    })
}

/// The checkpoint of `turn`, if it was recorded.
pub fn lookup(workspace: &Path, session: &str, turn: u32) -> Result<Option<Checkpoint>, CheckpointError> {
    let reference = reference(session, turn)?;
    let Some(commit) = git::rev_parse(workspace, &reference)? else {
        return Ok(None);
    };
    let at = commit_time(workspace, &commit)?;
    Ok(Some(Checkpoint {
        reference,
        commit,
        session: session.to_string(),
        turn,
        at,
    }))
}

/// Writes a checkpoint back into the working tree.
///
/// Tracked files the checkpoint does not have are removed. Untracked files made since the
/// checkpoint are left alone, because nothing records which of them a turn created.
pub fn restore(workspace: &Path, session: &str, turn: u32) -> Result<Checkpoint, CheckpointError> {
    let checkpoint = lookup(workspace, session, turn)?.ok_or_else(|| CheckpointError::NotFound {
        session: session.to_string(),
        turn,
    })?;
    require_repository(workspace)?;
    let git = Git::new(workspace);
    let index = TempIndex::new(workspace)?;
    let env = index_env(&index, workspace, None)?;
    run_with(&git, &["read-tree", &checkpoint.commit], &env)?;

    let keep: HashSet<String> = git::split_nul(&run_with(
        &git,
        &["ls-tree", "-r", "--name-only", "-z", &checkpoint.commit],
        &env,
    )?)
    .into_iter()
    .collect();
    for path in git::split_nul(&git.run(&["ls-files", "-z"])?) {
        if keep.contains(&path) {
            continue;
        }
        match std::fs::remove_file(workspace.join(&path)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CheckpointError::Io {
                    path: workspace.join(path),
                    source,
                });
            }
        }
    }

    run_with(&git, &["checkout-index", "--all", "--force"], &env)?;
    Ok(checkpoint)
}

/// Every checkpoint in the workspace, oldest first.
pub fn list(workspace: &Path) -> Result<Vec<Checkpoint>, CheckpointError> {
    require_repository(workspace)?;
    let out = Git::new(workspace).run(&[
        "for-each-ref",
        "--format=%(refname)%00%(objectname)%00%(committerdate:unix)",
        REF_PREFIX,
    ])?;
    let mut checkpoints = Vec::new();
    for line in out.lines() {
        let mut fields = line.split('\0');
        let (Some(reference), Some(commit), Some(seconds)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some((session, turn)) = split_reference(reference) else {
            continue;
        };
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(seconds.parse::<u64>().unwrap_or_default());
        checkpoints.push(Checkpoint {
            reference: reference.to_string(),
            commit: commit.to_string(),
            session,
            turn,
            at,
        });
    }
    checkpoints.sort_by_key(|checkpoint| checkpoint.at);
    Ok(checkpoints)
}

/// Deletes every checkpoint of a session, as deleting the session does. Returns how many went.
pub fn prune_session(workspace: &Path, session: &str) -> Result<usize, CheckpointError> {
    let prefix = format!("{REF_PREFIX}{session}/");
    let mut removed = 0;
    for checkpoint in list(workspace)? {
        if checkpoint.reference.starts_with(&prefix) {
            delete(workspace, &checkpoint)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Deletes checkpoints made before `cutoff`.
pub fn prune_older_than(workspace: &Path, cutoff: SystemTime) -> Result<usize, CheckpointError> {
    let mut removed = 0;
    for checkpoint in list(workspace)? {
        if checkpoint.at < cutoff {
            delete(workspace, &checkpoint)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Deletes checkpoints older than [`KEEP`], as the runtime does on its own schedule.
pub fn prune_expired(workspace: &Path) -> Result<usize, CheckpointError> {
    let cutoff = SystemTime::now()
        .checked_sub(KEEP)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    prune_older_than(workspace, cutoff)
}

fn delete(workspace: &Path, checkpoint: &Checkpoint) -> Result<(), CheckpointError> {
    Git::new(workspace).run(&["update-ref", "-d", &checkpoint.reference, &checkpoint.commit])?;
    Ok(())
}

fn require_repository(workspace: &Path) -> Result<(), CheckpointError> {
    if git::is_repository(workspace)? {
        Ok(())
    } else {
        Err(CheckpointError::NotARepository(workspace.to_path_buf()))
    }
}

fn commit_time(workspace: &Path, commit: &str) -> Result<SystemTime, CheckpointError> {
    let seconds = Git::new(workspace).run(&["show", "--no-patch", "--format=%ct", commit])?;
    Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds.trim().parse().unwrap_or_default()))
}

fn split_reference(reference: &str) -> Option<(String, u32)> {
    let rest = reference.strip_prefix(REF_PREFIX)?;
    let (session, turn) = rest.rsplit_once('/')?;
    Some((session.to_string(), turn.parse().ok()?))
}

fn valid_session(session: &str) -> bool {
    !session.is_empty()
        && !session.starts_with('.')
        && !session.ends_with('.')
        && !session.contains("..")
        && session
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
}

/// A path for `GIT_INDEX_FILE` that is removed when the call ends, whatever the outcome.
struct TempIndex {
    path: PathBuf,
}

impl TempIndex {
    fn new(workspace: &Path) -> Result<Self, CheckpointError> {
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let git_dir = Git::new(workspace).run(&["rev-parse", "--absolute-git-dir"])?;
        let name = format!(
            "parrot-checkpoint-{}-{}.index",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        );
        Ok(Self {
            path: PathBuf::from(git_dir).join(name),
        })
    }

    fn as_os(&self) -> &OsStr {
        self.path.as_os_str()
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let mut lock = self.path.as_os_str().to_owned();
        lock.push(".lock");
        let _ = std::fs::remove_file(PathBuf::from(lock));
    }
}

/// The environment for a call on the temporary index: the index file, an identity for the commit
/// object, and a date when one was asked for.
fn index_env(
    index: &TempIndex,
    workspace: &Path,
    at: Option<SystemTime>,
) -> Result<Vec<(String, OsString)>, GitError> {
    let mut env = vec![("GIT_INDEX_FILE".to_string(), index.as_os().to_owned())];
    env.extend(git::identity(workspace)?);
    if let Some(at) = at {
        let seconds = at
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default();
        let date = OsString::from(format!("@{seconds} +0000"));
        env.push(("GIT_AUTHOR_DATE".to_string(), date.clone()));
        env.push(("GIT_COMMITTER_DATE".to_string(), date));
    }
    Ok(env)
}

/// Runs a call with an environment built by [`index_env`].
fn run_with(git: &Git<'_>, args: &[&str], env: &[(String, OsString)]) -> Result<String, GitError> {
    let refs: Vec<(&str, &OsStr)> = env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_os_str()))
        .collect();
    git.run_with(args, &refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Repo;

    fn env_refs(env: &[(String, OsString)]) -> Vec<(&str, &OsStr)> {
        env.iter()
            .map(|(key, value)| (key.as_str(), value.as_os_str()))
            .collect()
    }

    #[test]
    fn records_changes_without_touching_index_or_worktree() {
        let repo = Repo::new();
        repo.write(".gitignore", "*.log\n");
        repo.commit("ignore logs");
        repo.write("README.md", "changed\n");
        repo.write("new.txt", "brand new\n");
        repo.write("debug.log", "ignored\n");
        let status_before = repo.git(&["status", "--porcelain=v1"]);
        let staged_before = repo.git(&["diff", "--cached", "--name-only"]);

        let checkpoint = record(repo.path(), "ses_alpha", 1).unwrap();
        assert_eq!(checkpoint.reference, "refs/parrot/ses_alpha/1");
        assert_eq!(repo.git(&["status", "--porcelain=v1"]), status_before);
        assert_eq!(repo.git(&["diff", "--cached", "--name-only"]), staged_before);

        let tree = repo.git(&["ls-tree", "-r", "--name-only", "-z", &checkpoint.commit]);
        let mut files = git::split_nul(&tree);
        files.sort();
        assert_eq!(
            files,
            vec![
                ".gitignore".to_string(),
                "README.md".to_string(),
                "new.txt".to_string()
            ]
        );
        assert_eq!(
            repo.git(&["show", &format!("{}:README.md", checkpoint.commit)]),
            "changed"
        );
        assert_eq!(
            repo.git(&["show", &format!("{}:new.txt", checkpoint.commit)]),
            "brand new"
        );
        assert!(
            repo.git(&["branch", "--format=%(refname)"])
                .find("parrot")
                .is_none()
        );

        // The temporary index is gone, so nothing of the checkpoint stays in the git directory.
        let git_dir = repo.git(&["rev-parse", "--absolute-git-dir"]);
        let leftovers: Vec<String> = std::fs::read_dir(git_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("parrot-checkpoint"))
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[test]
    fn a_second_checkpoint_of_a_turn_replaces_the_first() {
        let repo = Repo::new();
        repo.write("README.md", "one\n");
        let first = record(repo.path(), "ses_alpha", 1).unwrap();
        repo.write("README.md", "two\n");
        let second = record(repo.path(), "ses_alpha", 1).unwrap();
        assert_ne!(first.commit, second.commit);
        assert_eq!(
            lookup(repo.path(), "ses_alpha", 1).unwrap().unwrap().commit,
            second.commit
        );
        assert_eq!(list(repo.path()).unwrap().len(), 1);
    }

    #[test]
    fn restores_files_and_removes_what_the_turn_added() {
        let repo = Repo::new();
        repo.write("README.md", "at the checkpoint\n");
        repo.write("kept.txt", "also here\n");
        record(repo.path(), "ses_alpha", 1).unwrap();

        repo.write("README.md", "after the turn\n");
        std::fs::remove_file(repo.path().join("kept.txt")).unwrap();
        repo.write("extra.txt", "made by the turn\n");
        repo.git(&["add", "extra.txt"]);

        restore(repo.path(), "ses_alpha", 1).unwrap();
        assert_eq!(repo.read("README.md"), "at the checkpoint\n");
        assert!(repo.path().join("kept.txt").is_file());
        assert!(!repo.path().join("extra.txt").exists());
    }

    #[test]
    fn restores_an_untracked_file_that_the_checkpoint_captured() {
        let repo = Repo::new();
        repo.write("draft.txt", "first draft\n");
        record(repo.path(), "ses_alpha", 1).unwrap();
        repo.write("draft.txt", "second draft\n");
        restore(repo.path(), "ses_alpha", 1).unwrap();
        assert_eq!(repo.read("draft.txt"), "first draft\n");
    }

    #[test]
    fn refuses_to_restore_without_a_checkpoint() {
        let repo = Repo::new();
        let error = restore(repo.path(), "ses_alpha", 7).unwrap_err();
        assert!(matches!(
            error,
            CheckpointError::NotFound { ref session, turn: 7 } if session == "ses_alpha"
        ));
        assert!(lookup(repo.path(), "ses_alpha", 7).unwrap().is_none());
    }

    #[test]
    fn prunes_one_session_and_leaves_the_other() {
        let repo = Repo::new();
        repo.write("README.md", "alpha\n");
        record(repo.path(), "ses_alpha", 1).unwrap();
        record(repo.path(), "ses_alpha", 2).unwrap();
        record(repo.path(), "ses_beta", 1).unwrap();
        repo.write("README.md", "beta\n");
        record(repo.path(), "ses_beta", 2).unwrap();

        assert_eq!(prune_session(repo.path(), "ses_alpha").unwrap(), 2);
        let left = list(repo.path()).unwrap();
        assert_eq!(left.len(), 2);
        assert!(left.iter().all(|checkpoint| checkpoint.session == "ses_beta"));
    }

    #[test]
    fn prunes_checkpoints_older_than_the_cutoff() {
        let repo = Repo::new();
        repo.write("README.md", "old\n");
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        record_at(repo.path(), "ses_alpha", 1, old).unwrap();
        record(repo.path(), "ses_alpha", 2).unwrap();

        let cutoff = SystemTime::now() - KEEP;
        assert_eq!(prune_older_than(repo.path(), cutoff).unwrap(), 1);
        let left = list(repo.path()).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].turn, 2);
        assert!(left[0].at > old);
    }

    #[test]
    fn reads_the_message_of_a_checkpoint() {
        let repo = Repo::new();
        let checkpoint = record(repo.path(), "ses_alpha", 3).unwrap();
        assert_eq!(
            repo.commit_message(&checkpoint.commit),
            "checkpoint ses_alpha turn 3"
        );
    }

    #[test]
    fn refuses_a_session_id_that_is_not_a_ref_component() {
        for session in ["", "ses/alpha", "..", "ses alpha", ".hidden"] {
            assert!(reference(session, 1).is_err(), "{session:?} was accepted");
        }
        assert_eq!(reference("ses_alpha.1", 2).unwrap(), "refs/parrot/ses_alpha.1/2");
    }

    #[test]
    fn refuses_a_folder_that_is_not_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let error = record(dir.path(), "ses_alpha", 1).unwrap_err();
        assert!(matches!(error, CheckpointError::NotARepository(_)));
    }

    // The environment helper is what makes a checkpoint leave the real index alone; this checks it
    // points `git` somewhere else.
    #[test]
    fn the_temporary_index_is_a_second_index() {
        let repo = Repo::new();
        repo.write("README.md", "changed\n");
        let index = TempIndex::new(repo.path()).unwrap();
        let env = index_env(&index, repo.path(), None).unwrap();
        let refs = env_refs(&env);
        let git = Git::new(repo.path());
        git.run_with(&["read-tree", "HEAD"], &refs).unwrap();
        git.run_with(&["add", "--all", "--", "."], &refs).unwrap();
        let tree = git.run_with(&["write-tree"], &refs).unwrap();
        assert_ne!(tree, repo.git(&["rev-parse", "HEAD^{tree}"]));
        assert_eq!(repo.git(&["diff", "--cached", "--name-only"]), "");
    }
}
