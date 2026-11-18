//! Moving a workspace between machines: a branch as a git bundle, plus its uncommitted changes
//! (spec 8, 23.2).
//!
//! Commits always travel, as a self-contained bundle of the branch, so a repository that has never
//! seen the branch can receive it. Staged and unstaged changes travel as patches, and untracked
//! files as contents, each only when the caller asks for that [`Carry`]. Ignored files are never
//! read and checkpoint refs are never bundled: the bundle holds the branch alone.
//!
//! Nothing here talks to a git remote. A bundle is a file or a byte string, and the receiving side
//! fetches it from a temporary file, which is how it crosses a link without a shared filesystem.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use parrot_proto::workspace::Carry;

use crate::fence;
use crate::git::{self, Git, GitError};
use crate::worktree::{self, NewWorktree, Worktree, WorktreeError};

/// Where a packed bundle is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bundle {
    /// In memory, for a link that carries bytes.
    Bytes(Vec<u8>),
    /// Written by git to a file the caller named.
    Path(PathBuf),
}

/// An untracked file that travels with the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntrackedFile {
    /// Relative to the top of the workspace.
    pub path: String,
    pub contents: Vec<u8>,
    pub executable: bool,
}

/// A workspace packed for the link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub branch: String,
    pub bundle: Bundle,
    pub staged: Option<Vec<u8>>,
    pub unstaged: Option<Vec<u8>>,
    pub untracked: Vec<UntrackedFile>,
}

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("{0} is not a git repository")]
    NotARepository(PathBuf),
    #[error("branch {0} does not exist in the workspace")]
    UnknownBranch(String),
    #[error("the transfer has no bundle")]
    NoBundle,
    #[error("path {0} is not inside the workspace")]
    UnsafePath(String),
    #[error("cannot read or write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
}

/// The carries a rebind moves by default (spec 9.3): commits and uncommitted changes, no untracked
/// files.
pub fn default_carry() -> Vec<Carry> {
    vec![Carry::Commits, Carry::Staged, Carry::Unstaged]
}

impl Transfer {
    /// The bundle as bytes, reading the file when git wrote it to disk.
    pub fn bundle_bytes(&self) -> Result<Vec<u8>, TransferError> {
        match &self.bundle {
            Bundle::Bytes(bytes) => Ok(bytes.clone()),
            Bundle::Path(path) => std::fs::read(path).map_err(|source| TransferError::Io {
                path: path.clone(),
                source,
            }),
        }
    }
}

/// Packs the branch and what `include` asks for, with the bundle in memory.
///
/// The patches are the workspace's index and working tree as they are now, so callers pass the
/// branch the workspace has checked out.
pub fn pack(workspace: &Path, branch: &str, include: &[Carry]) -> Result<Transfer, TransferError> {
    pack_with(workspace, branch, include, None)
}

/// Packs like [`pack`], writing the bundle to `path` instead of keeping it in memory.
pub fn pack_to_file(
    workspace: &Path,
    branch: &str,
    include: &[Carry],
    path: &Path,
) -> Result<Transfer, TransferError> {
    pack_with(workspace, branch, include, Some(path))
}

fn pack_with(
    workspace: &Path,
    branch: &str,
    include: &[Carry],
    bundle_file: Option<&Path>,
) -> Result<Transfer, TransferError> {
    if !git::is_repository(workspace)? {
        return Err(TransferError::NotARepository(workspace.to_path_buf()));
    }
    let reference = format!("refs/heads/{branch}");
    if git::rev_parse(workspace, &reference)?.is_none() {
        return Err(TransferError::UnknownBranch(branch.to_string()));
    }
    let git = Git::new(workspace);

    let bundle = match bundle_file {
        Some(path) => {
            git.run(&["bundle", "create", "--quiet", &path.to_string_lossy(), &reference])?;
            Bundle::Path(path.to_path_buf())
        }
        None => Bundle::Bytes(git.bytes(&["bundle", "create", "--quiet", "-", &reference])?),
    };

    let staged = if carried(include, Carry::Staged) {
        non_empty(git.bytes(&["diff", "--cached", "--binary", "--full-index", "HEAD"])?)
    } else {
        None
    };
    let unstaged = if carried(include, Carry::Unstaged) {
        non_empty(git.bytes(&["diff", "--binary", "--full-index"])?)
    } else {
        None
    };
    let untracked = if carried(include, Carry::Untracked) {
        let mut files = Vec::new();
        for path in git::untracked(workspace)? {
            let full = workspace.join(&path);
            let contents = std::fs::read(&full).map_err(|source| TransferError::Io {
                path: full.clone(),
                source,
            })?;
            files.push(UntrackedFile {
                executable: is_executable(&full),
                path,
                contents,
            });
        }
        files
    } else {
        Vec::new()
    };

    Ok(Transfer {
        branch: branch.to_string(),
        bundle,
        staged,
        unstaged,
        untracked,
    })
}

/// Receives a transfer as a new worktree of `repo`, on the carried branch, with the carried
/// changes applied on top.
pub fn unpack(repo: &Path, path: &Path, transfer: &Transfer) -> Result<Worktree, TransferError> {
    if !git::is_repository(repo)? {
        return Err(TransferError::NotARepository(repo.to_path_buf()));
    }
    let git = Git::new(repo);
    // The file has to stay alive until fetch has read it, so the guard is held here.
    let temporary = match &transfer.bundle {
        Bundle::Bytes(bytes) => Some(TempFile::write("parrot-bundle", bytes)?),
        Bundle::Path(_) => None,
    };
    let bundle_path = match &transfer.bundle {
        Bundle::Path(path) => path.clone(),
        Bundle::Bytes(_) => temporary
            .as_ref()
            .map(|file| file.path.clone())
            .ok_or(TransferError::NoBundle)?,
    };
    let bundle = bundle_path.to_string_lossy().into_owned();
    let reference = format!("refs/heads/{}", transfer.branch);
    git.run(&[
        "fetch",
        "--quiet",
        "--no-tags",
        &bundle,
        &format!("+{reference}:{reference}"),
    ])?;

    let worktree = worktree::create(NewWorktree {
        repo,
        path,
        branch: &transfer.branch,
        base: Some(&transfer.branch),
    })?;

    let applied = apply_changes(&worktree.path, transfer);
    if let Err(error) = applied {
        // A workspace that only half received its changes is worse than none, so it goes again.
        let _ = worktree::remove(&worktree, true);
        return Err(error);
    }
    Ok(worktree)
}

/// Receives a transfer as a repository of its own at `path`, on the carried branch, for a
/// machine that has no clone of the repository. `path` must not exist yet.
pub fn unpack_new(path: &Path, transfer: &Transfer) -> Result<PathBuf, TransferError> {
    if path.exists() {
        return Err(TransferError::Io {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::AlreadyExists, "the target exists"),
        });
    }
    if !valid_branch(&transfer.branch) {
        return Err(TransferError::UnknownBranch(transfer.branch.clone()));
    }
    std::fs::create_dir_all(path).map_err(|source| TransferError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let result = (|| {
        let bytes = transfer.bundle_bytes()?;
        let file = TempFile::write("parrot-bundle", &bytes)?;
        let git = Git::new(path);
        // A placeholder initial branch, so the carried branch is never the one checked out
        // while it is fetched.
        git.run(&["init", "--quiet", "--initial-branch=parrot-receiving"])?;
        let reference = format!("refs/heads/{}", transfer.branch);
        git.run(&[
            "fetch",
            "--quiet",
            "--no-tags",
            &file.path.to_string_lossy(),
            &format!("+{reference}:{reference}"),
        ])?;
        // The files arrive as they were committed, so the carried patches apply byte for byte
        // whatever line-ending conversion this machine's git does.
        git.run(&["config", "core.autocrlf", "false"])?;
        git.run(&["checkout", "--quiet", &transfer.branch])?;
        apply_changes(path, transfer)
    })();
    match result {
        Ok(()) => Ok(path.to_path_buf()),
        Err(e) => {
            // A half-received workspace is worse than none.
            let _ = std::fs::remove_dir_all(path);
            Err(e)
        }
    }
}

/// Whether a carried branch name is one git accepts and that can't be read as an option.
fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with('-')
        && !branch.contains("..")
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
}

fn apply_changes(workspace: &Path, transfer: &Transfer) -> Result<(), TransferError> {
    if let Some(patch) = &transfer.staged {
        apply(workspace, "parrot-staged", patch, true)?;
    }
    if let Some(patch) = &transfer.unstaged {
        apply(workspace, "parrot-unstaged", patch, false)?;
    }
    for file in &transfer.untracked {
        write_file(workspace, file)?;
    }
    Ok(())
}

fn apply(workspace: &Path, name: &str, patch: &[u8], index: bool) -> Result<(), TransferError> {
    let file = TempFile::write(name, patch)?;
    let patch = file.path.to_string_lossy().into_owned();
    let git = Git::new(workspace);
    let mut args = vec!["apply", "--binary", "--whitespace=nowarn"];
    if index {
        args.push("--index");
    }
    args.push(&patch);
    git.run(&args)?;
    Ok(())
}

fn write_file(workspace: &Path, file: &UntrackedFile) -> Result<(), TransferError> {
    // A carried path is remote input: the fence refuses one that is absolute or climbs out.
    let target =
        fence::resolve(workspace, &file.path).map_err(|_| TransferError::UnsafePath(file.path.clone()))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|source| TransferError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(&target, &file.contents).map_err(|source| TransferError::Io {
        path: target.clone(),
        source,
    })?;
    set_executable(&target, file.executable)?;
    Ok(())
}

fn carried(include: &[Carry], carry: Carry) -> bool {
    include.contains(&carry)
}

fn non_empty(bytes: Vec<u8>) -> Option<Vec<u8>> {
    if bytes.is_empty() { None } else { Some(bytes) }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> Result<(), TransferError> {
    use std::os::unix::fs::PermissionsExt;
    if !executable {
        return Ok(());
    }
    let mut permissions = std::fs::metadata(path)
        .map_err(|source| TransferError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    std::fs::set_permissions(path, permissions).map_err(|source| TransferError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> Result<(), TransferError> {
    Ok(())
}

/// A file holding bytes that are removed when it goes out of scope.
struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn write(name: &str, bytes: &[u8]) -> Result<Self, TransferError> {
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "{name}-{}-{}.tmp",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, bytes).map_err(|source| TransferError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(Self { path })
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Repo;

    fn all_carry() -> Vec<Carry> {
        vec![Carry::Commits, Carry::Staged, Carry::Unstaged, Carry::Untracked]
    }

    /// A source workspace with staged, unstaged and untracked changes.
    fn changed_workspace() -> Repo {
        let repo = Repo::new();
        repo.write("README.md", "# Carried\n");
        repo.commit("carried commit");
        repo.write("README.md", "# Carried, staged\n");
        repo.git(&["add", "README.md"]);
        repo.write("README.md", "# Carried, staged and unstaged\n");
        repo.write("notes.txt", "untracked too\n");
        repo.write("draft.txt", "untracked\n");
        repo
    }

    fn target() -> Repo {
        // A receiving repository with another branch checked out: fetching a branch it has checked
        // out is refused by git, and a workspace arrives on its own branch.
        Repo::empty_on("reception")
    }

    #[test]
    fn carries_commits_and_changes_into_a_new_worktree() {
        let source = changed_workspace();
        let transfer = pack(source.path(), "main", &all_carry()).unwrap();
        assert_eq!(transfer.branch, "main");
        assert!(transfer.staged.is_some());
        assert!(transfer.unstaged.is_some());
        assert_eq!(transfer.untracked.len(), 2);

        let destination = target();
        let path = destination.parent().join("carried");
        let worktree = unpack(destination.path(), &path, &transfer).unwrap();
        assert_eq!(worktree.branch, "main");
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "# Carried, staged and unstaged\n"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("notes.txt")).unwrap(),
            "untracked too\n"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("draft.txt")).unwrap(),
            "untracked\n"
        );

        let changed = git::status(&path).unwrap();
        // The staged patch went to the index and the unstaged one on top, so the file differs in
        // both columns.
        assert!(
            changed
                .iter()
                .any(|entry| entry.path == "README.md" && entry.status == "MM")
        );
        assert!(
            changed
                .iter()
                .any(|entry| entry.path == "notes.txt" && entry.status == "??")
        );
        assert!(
            changed
                .iter()
                .any(|entry| entry.path == "draft.txt" && entry.status == "??")
        );
    }

    #[test]
    fn carries_only_what_was_asked_for() {
        let source = changed_workspace();
        let transfer = pack(source.path(), "main", &[Carry::Commits]).unwrap();
        assert!(transfer.staged.is_none());
        assert!(transfer.unstaged.is_none());
        assert!(transfer.untracked.is_empty());

        let destination = target();
        let path = destination.parent().join("only-commits");
        unpack(destination.path(), &path, &transfer).unwrap();
        assert!(git::status(&path).unwrap().is_empty());
        assert!(!path.join("draft.txt").exists());
    }

    #[test]
    fn writes_and_reads_the_bundle_as_a_file() {
        let source = changed_workspace();
        let destination = target();
        let bundle = destination.parent().join("workspace.bundle");
        let transfer = pack_to_file(source.path(), "main", &[Carry::Commits], &bundle).unwrap();
        assert_eq!(transfer.bundle, Bundle::Path(bundle.clone()));
        assert!(bundle.is_file());
        assert_eq!(transfer.bundle_bytes().unwrap(), std::fs::read(&bundle).unwrap());

        let path = destination.parent().join("from-file");
        unpack(destination.path(), &path, &transfer).unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "# Carried\n"
        );
    }

    #[test]
    fn leaves_ignored_files_and_checkpoint_refs_behind() {
        let source = Repo::new();
        source.write(".gitignore", "*.log\n");
        source.commit("ignore logs");
        source.write("debug.log", "ignored\n");
        source.write("kept.txt", "kept\n");
        crate::checkpoint::record(source.path(), "ses_alpha", 1).unwrap();

        let transfer = pack(source.path(), "main", &all_carry()).unwrap();
        assert_eq!(transfer.untracked.len(), 1);
        assert_eq!(transfer.untracked[0].path, "kept.txt");

        let file = TempFile::write("parrot-heads", &transfer.bundle_bytes().unwrap()).unwrap();
        let heads = Git::new(source.path())
            .run(&["bundle", "list-heads", &file.path.to_string_lossy()])
            .unwrap();
        assert!(heads.contains("refs/heads/main"), "heads were {heads:?}");
        assert!(!heads.contains("refs/parrot"), "heads were {heads:?}");
    }

    #[test]
    fn refuses_an_unknown_branch_and_a_carried_escape() {
        let source = Repo::new();
        let error = pack(source.path(), "no-such-branch", &[Carry::Commits]).unwrap_err();
        assert!(matches!(error, TransferError::UnknownBranch(_)));

        let mut transfer = pack(source.path(), "main", &[Carry::Commits]).unwrap();
        transfer.untracked.push(UntrackedFile {
            path: "../escape.txt".to_string(),
            contents: b"nope".to_vec(),
            executable: false,
        });
        let destination = target();
        let path = destination.parent().join("escape");
        let error = unpack(destination.path(), &path, &transfer).unwrap_err();
        assert!(matches!(error, TransferError::UnsafePath(_)));
        assert!(!destination.parent().join("escape.txt").exists());
    }

    #[test]
    fn carries_an_executable_untracked_file() {
        let source = Repo::new();
        source.write("run.sh", "#!/bin/sh\necho made up\n");
        let script = source.path().join("run.sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).unwrap();
        }
        let _ = &script;

        let transfer = pack(source.path(), "main", &all_carry()).unwrap();
        let destination = target();
        let path = destination.parent().join("script");
        unpack(destination.path(), &path, &transfer).unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join("run.sh")).unwrap(),
            "#!/bin/sh\necho made up\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path.join("run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0);
        }
    }

    #[test]
    fn refuses_a_non_repository() {
        let dir = tempfile::tempdir().unwrap();
        let error = pack(dir.path(), "main", &[Carry::Commits]).unwrap_err();
        assert!(matches!(error, TransferError::NotARepository(_)));
        let error = unpack(
            dir.path(),
            &dir.path().join("wt"),
            &Transfer {
                branch: "main".to_string(),
                bundle: Bundle::Bytes(Vec::new()),
                staged: None,
                unstaged: None,
                untracked: Vec::new(),
            },
        )
        .unwrap_err();
        assert!(matches!(error, TransferError::NotARepository(_)));
    }

    #[test]
    fn carries_into_a_machine_without_the_repository() {
        let source = changed_workspace();
        let transfer = pack(source.path(), "main", &all_carry()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("arrived");
        unpack_new(&path, &transfer).unwrap();
        assert_eq!(git::branch(&path).unwrap().as_deref(), Some("main"));
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).unwrap(),
            "# Carried, staged and unstaged\n"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("draft.txt")).unwrap(),
            "untracked\n"
        );
        // A second transfer into the same place is refused rather than mixed in.
        assert!(unpack_new(&path, &transfer).is_err());
        let mut bad = transfer.clone();
        bad.branch = "--upload-pack=x".into();
        assert!(unpack_new(&dir.path().join("bad"), &bad).is_err());
        assert!(!dir.path().join("bad").exists());
    }

    #[test]
    fn defaults_to_commits_and_changes() {
        let carry = default_carry();
        assert!(carry.contains(&Carry::Commits));
        assert!(carry.contains(&Carry::Staged));
        assert!(carry.contains(&Carry::Unstaged));
        assert!(!carry.contains(&Carry::Untracked));
    }
}
