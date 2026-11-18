//! The path rules of spec 8: a session may only touch files inside its workspace.
//!
//! A fence compares normalised paths after resolving `..` and symbolic links (spec 2), so a symlink
//! pointing out of the workspace, or a relative path that climbs out of it, is refused. The same
//! rules apply to a workspace itself, which must sit inside one of its environment's roots and
//! inside the principal's ceiling paths.

use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum FenceError {
    #[error("{path} is outside {root}")]
    Outside { root: PathBuf, path: PathBuf },
    #[error("{path} is not relative to {root}")]
    NotRelative { root: PathBuf, path: PathBuf },
    #[error("{path} is not inside any of {roots:?}")]
    OutsideRoots { path: PathBuf, roots: Vec<PathBuf> },
    #[error("cannot resolve {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Resolves a path a caller sent against the workspace, refusing one that leaves it.
pub fn resolve(workspace: &Path, relative: &str) -> Result<PathBuf, FenceError> {
    let relative = Path::new(relative);
    if relative.is_absolute() {
        return Err(FenceError::NotRelative {
            root: workspace.to_path_buf(),
            path: relative.to_path_buf(),
        });
    }
    check(workspace, &workspace.join(relative))
}

/// Resolves `path` and refuses it unless it is the workspace or inside it.
pub fn check(workspace: &Path, path: &Path) -> Result<PathBuf, FenceError> {
    let root = workspace.canonicalize().map_err(|source| FenceError::Io {
        path: workspace.to_path_buf(),
        source,
    })?;
    let candidate = resolve_components(path).map_err(|source| FenceError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if candidate.starts_with(&root) {
        Ok(candidate)
    } else {
        Err(FenceError::Outside {
            root,
            path: path.to_path_buf(),
        })
    }
}

/// Whether `path` is inside `workspace`, with both resolved.
pub fn contains(workspace: &Path, path: &Path) -> bool {
    check(workspace, path).is_ok()
}

/// Refuses a workspace that is not inside one of the environment's roots.
pub fn check_roots(path: &Path, roots: &[PathBuf]) -> Result<PathBuf, FenceError> {
    let candidate = resolve_components(path).map_err(|source| FenceError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if roots.is_empty() {
        return Ok(candidate);
    }
    for root in roots {
        let Ok(root) = root.canonicalize() else {
            continue;
        };
        if candidate.starts_with(&root) {
            return Ok(candidate);
        }
    }
    Err(FenceError::OutsideRoots {
        path: path.to_path_buf(),
        roots: roots.to_vec(),
    })
}

/// Refuses a workspace outside the principal's ceiling paths. An empty list allows everything,
/// as an empty ceiling does (spec 22).
pub fn check_ceiling(path: &Path, paths: &[String]) -> Result<PathBuf, FenceError> {
    let roots: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    check_roots(path, &roots)
}

/// The check of spec 8: inside a root of the environment and inside the ceiling paths.
pub fn check_allowed(roots: &[PathBuf], ceiling: &[String], path: &Path) -> Result<PathBuf, FenceError> {
    let path = check_roots(path, roots)?;
    check_ceiling(&path, ceiling)
}

/// Resolves a path one component at a time, so `..` after a symlink applies to the directory the
/// symlink points at, as the shell and the kernel do. Components that do not exist yet stay as
/// written, so a path about to be created still gets the fence's answer.
fn resolve_components(path: &Path) -> io::Result<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => {
                let candidate = out.join(part);
                match candidate.canonicalize() {
                    Ok(resolved) => out = resolved,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => out = candidate,
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[test]
    fn allows_paths_inside_the_workspace() {
        let dir = workspace();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(
            resolve(dir.path(), "src/main.rs").unwrap(),
            root.join("src/main.rs")
        );
        assert_eq!(
            resolve(dir.path(), "src/../README.md").unwrap(),
            root.join("README.md")
        );
        assert!(resolve(dir.path(), "not-created-yet.txt").is_ok());
        assert!(contains(dir.path(), &root.join("src")));
    }

    #[test]
    fn refuses_paths_that_climb_out() {
        let dir = workspace();
        for relative in ["../outside.txt", "src/../../outside.txt", "/etc/passwd"] {
            let error = resolve(dir.path(), relative).unwrap_err();
            assert!(
                matches!(error, FenceError::Outside { .. } | FenceError::NotRelative { .. }),
                "{relative} gave {error:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_that_points_out() {
        let dir = workspace();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("secret.txt"), "not yours\n").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("escape")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path().join("secret.txt"), dir.path().join("file")).unwrap();

        let error = resolve(dir.path(), "escape/secret.txt").unwrap_err();
        assert!(matches!(error, FenceError::Outside { .. }));
        // `..` applies after the symlink is resolved, so this leaves the workspace too.
        let error = resolve(dir.path(), "escape/../secret.txt").unwrap_err();
        assert!(matches!(error, FenceError::Outside { .. }));
        let error = resolve(dir.path(), "file").unwrap_err();
        assert!(matches!(error, FenceError::Outside { .. }));

        // A symlink that stays inside is allowed.
        std::os::unix::fs::symlink(dir.path().join("src"), dir.path().join("inner")).unwrap();
        assert!(resolve(dir.path(), "inner/main.rs").is_ok());
    }

    #[test]
    fn checks_roots_and_ceiling() {
        let dir = workspace();
        let other = tempfile::tempdir().unwrap();
        let roots = vec![dir.path().to_path_buf()];
        assert!(check_roots(&dir.path().join("src"), &roots).is_ok());
        assert!(check_roots(other.path(), &roots).is_err());
        assert!(check_roots(other.path(), &[]).is_ok());

        let ceiling = vec![dir.path().to_string_lossy().into_owned()];
        assert!(check_allowed(&roots, &ceiling, &dir.path().join("src")).is_ok());
        assert!(check_allowed(&roots, &ceiling, other.path()).is_err());
        assert!(check_ceiling(other.path(), &[]).is_ok());
    }
}
