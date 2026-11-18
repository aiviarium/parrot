//! A folder workspace: an existing directory the runtime did not create.
//!
//! A folder may or may not be a repository, and may sit inside one. Status therefore never assumes
//! git: outside a repository the report says so, rather than failing.

use std::path::{Path, PathBuf};

use crate::git::{self, GitError};

/// Why a folder cannot be used as a workspace.
#[derive(Debug, thiserror::Error)]
pub enum DirError {
    #[error("workspace path {0} does not exist")]
    Missing(PathBuf),
    #[error("workspace path {0} is not a directory")]
    NotADirectory(PathBuf),
    #[error("workspace path {path} cannot be read: {source}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Git(#[from] GitError),
}

/// What a folder workspace looks like right now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirStatus {
    /// Whether the folder is inside a git worktree.
    pub git: bool,
    /// The branch `HEAD` points at; `None` when detached or not a repository.
    pub branch: Option<String>,
    /// The commit `HEAD` is at, if any.
    pub commit: Option<String>,
    pub dirty: bool,
    /// Changed paths, relative to the top of the worktree.
    pub changed: Vec<String>,
}

/// A checked folder workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirWorkspace {
    path: PathBuf,
}

impl DirWorkspace {
    /// Checks that `path` exists and is a directory. Nothing is created.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, DirError> {
        let path = path.into();
        let metadata = std::fs::metadata(&path).map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => DirError::Missing(path.clone()),
            _ => DirError::Unreadable {
                path: path.clone(),
                source,
            },
        })?;
        if !metadata.is_dir() {
            return Err(DirError::NotADirectory(path));
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the folder's git state, or reports that there is none.
    pub fn status(&self) -> Result<DirStatus, DirError> {
        if git::toplevel(&self.path)?.is_none() {
            return Ok(DirStatus::default());
        }
        let changed = git::status(&self.path)?;
        Ok(DirStatus {
            git: true,
            branch: git::branch(&self.path)?,
            commit: git::head(&self.path)?,
            dirty: !changed.is_empty(),
            changed: changed.into_iter().map(|entry| entry.path).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Repo;

    #[test]
    fn accepts_a_plain_folder() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = DirWorkspace::open(dir.path()).unwrap();
        assert_eq!(workspace.path(), dir.path());
        assert_eq!(workspace.status().unwrap(), DirStatus::default());
    }

    #[test]
    fn rejects_a_missing_path_and_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent");
        assert!(matches!(
            DirWorkspace::open(&missing),
            Err(DirError::Missing(path)) if path == missing
        ));

        let file = dir.path().join("file.txt");
        std::fs::write(&file, "hello\n").unwrap();
        assert!(matches!(
            DirWorkspace::open(&file),
            Err(DirError::NotADirectory(path)) if path == file
        ));
    }

    #[test]
    fn reports_a_repository_and_its_changes() {
        let repo = Repo::new();
        let workspace = DirWorkspace::open(repo.path()).unwrap();
        let status = workspace.status().unwrap();
        assert!(status.git);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.commit.as_deref(), Some(repo.head().as_str()));
        assert!(!status.dirty);
        assert!(status.changed.is_empty());

        repo.write("README.md", "# Changed\n");
        repo.write("draft.txt", "not ignored\n");
        let status = workspace.status().unwrap();
        assert!(status.dirty);
        let mut changed = status.changed;
        changed.sort();
        assert_eq!(changed, vec!["README.md".to_string(), "draft.txt".to_string()]);
    }

    #[test]
    fn reports_a_folder_inside_a_repository() {
        let repo = Repo::new();
        let nested = repo.path().join("src");
        std::fs::create_dir(&nested).unwrap();
        let status = DirWorkspace::open(&nested).unwrap().status().unwrap();
        assert!(status.git);
        assert_eq!(status.branch.as_deref(), Some("main"));
    }
}
