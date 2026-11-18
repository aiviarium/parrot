//! Where a CLI lives, and where the runtime keeps its files, on each OS (spec 3.3 and 35).
//!
//! The rules are pure functions of an environment so they can be tested for every OS on any
//! host: [`resolve_in`] reads a `PATH` and `PATHEXT`, and [`config_dir_in`] and [`state_dir_in`]
//! read the variables the OS uses.

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The OS whose path rules apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Macos,
    Windows,
}

impl Platform {
    /// The OS this build runs on.
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }

    fn is_unix(self) -> bool {
        !matches!(self, Self::Windows)
    }
}

/// The environment a CLI lookup reads.
#[derive(Clone, Copy, Debug)]
pub struct Search<'a> {
    /// Directories to search, as `PATH` lists them.
    pub path: Option<&'a OsStr>,
    /// The extensions to try, as `PATHEXT` lists them.
    pub pathext: Option<&'a OsStr>,
    pub platform: Platform,
}

/// The extensions a package manager installs on Windows when `PATHEXT` isn't set. `.PS1` isn't in
/// a default `PATHEXT`, but npm writes one beside the `.cmd` shim and some tools only ship it.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD;.PS1";

/// Finds the program `name` would start on this OS.
///
/// On Windows an npm or bun install leaves `foo.cmd`, `foo.ps1` and `foo` shims but no
/// `foo.exe`, so the name alone does not resolve. Returns `None` when nothing matches.
pub fn resolve_cli(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH");
    let pathext = env::var_os("PATHEXT");
    resolve_in(
        OsStr::new(name),
        &Search {
            path: path.as_deref(),
            pathext: pathext.as_deref(),
            platform: Platform::current(),
        },
    )
}

/// The pure form of [`resolve_cli`].
pub fn resolve_in(name: &OsStr, search: &Search<'_>) -> Option<PathBuf> {
    let given = Path::new(name);
    if name.is_empty() {
        return None;
    }
    let text = name.to_string_lossy();
    if text.contains('/') || text.contains('\\') {
        // A relative or absolute path is checked as given, plus the Windows extensions.
        let dir = given.parent().unwrap_or(Path::new(""));
        let file = given.file_name()?;
        return candidates(file, search)
            .into_iter()
            .map(|candidate| dir.join(candidate))
            .find(|candidate| is_program(candidate, search.platform));
    }
    for dir in env::split_paths(search.path?).filter(|dir| !dir.as_os_str().is_empty()) {
        for candidate in candidates(name, search) {
            let candidate = dir.join(candidate);
            if is_program(&candidate, search.platform) {
                return Some(candidate);
            }
        }
    }
    None
}

/// The names to try for `name`, in preference order.
fn candidates(name: &OsStr, search: &Search<'_>) -> Vec<OsString> {
    let mut out = vec![name.to_owned()];
    if !matches!(search.platform, Platform::Windows) || Path::new(name).extension().is_some() {
        return out;
    }
    let listed = search
        .pathext
        .map(|exts| exts.to_string_lossy().into_owned())
        .unwrap_or_else(|| DEFAULT_PATHEXT.to_owned());
    for ext in listed.split(';').map(str::trim).filter(|ext| !ext.is_empty()) {
        for ext in [ext.to_ascii_uppercase(), ext.to_ascii_lowercase()] {
            let mut candidate = name.to_owned();
            candidate.push(&ext);
            out.push(candidate);
        }
    }
    out
}

fn is_program(path: &Path, platform: Platform) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if platform.is_unix() && meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    #[cfg(not(unix))]
    let _ = platform;
    true
}

/// The user's home directory: `HOME` on Unix, `USERPROFILE` on Windows.
pub fn home_dir() -> Option<PathBuf> {
    home_dir_in(Platform::current(), |key| env::var_os(key))
}

/// The runtime's configuration directory (spec 3.3, 35).
pub fn config_dir() -> Option<PathBuf> {
    config_dir_in(Platform::current(), |key| env::var_os(key))
}

/// The runtime's state directory: the database and frames live here (spec 35).
pub fn state_dir() -> Option<PathBuf> {
    state_dir_in(Platform::current(), |key| env::var_os(key))
}

/// Where a binding's transcript is kept, under the state directory.
pub fn transcript_dir() -> Option<PathBuf> {
    state_dir().map(|dir| dir.join("transcripts"))
}

/// The pure form of [`home_dir`].
pub fn home_dir_in(platform: Platform, get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let key = if platform.is_unix() { "HOME" } else { "USERPROFILE" };
    non_empty(&get, key).map(PathBuf::from)
}

/// The pure form of [`config_dir`].
pub fn config_dir_in(platform: Platform, get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    match platform {
        Platform::Macos => Some(home_dir_in(platform, &get)?.join("Library/Application Support/parrot")),
        Platform::Linux => {
            let base = match non_empty(&get, "XDG_CONFIG_HOME").map(PathBuf::from) {
                Some(dir) => dir,
                None => home_dir_in(platform, &get)?.join(".config"),
            };
            Some(base.join("parrot"))
        }
        Platform::Windows => Some(PathBuf::from(non_empty(&get, "APPDATA")?).join("parrot")),
    }
}

/// The pure form of [`state_dir`].
pub fn state_dir_in(platform: Platform, get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    match platform {
        Platform::Macos => Some(home_dir_in(platform, &get)?.join("Library/Application Support/parrot")),
        Platform::Linux => {
            let base = match non_empty(&get, "XDG_STATE_HOME").map(PathBuf::from) {
                Some(dir) => dir,
                None => home_dir_in(platform, &get)?.join(".local/state"),
            };
            Some(base.join("parrot"))
        }
        Platform::Windows => Some(PathBuf::from(non_empty(&get, "LOCALAPPDATA")?).join("parrot")),
    }
}

fn non_empty(get: &impl Fn(&str) -> Option<OsString>, key: &str) -> Option<OsString> {
    get(key).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn touch(path: &Path, executable: bool) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
        #[cfg(unix)]
        if executable {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = executable;
    }

    #[test]
    fn unix_finds_an_executable_on_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        touch(&cwd.join("bin/tool"), true);
        let path = cwd.join("bin");
        let search = Search {
            path: Some(path.as_os_str()),
            pathext: None,
            platform: Platform::Linux,
        };
        assert_eq!(
            resolve_in(OsStr::new("tool"), &search),
            Some(cwd.join("bin/tool"))
        );
        assert_eq!(resolve_in(OsStr::new("missing"), &search), None);
    }

    #[test]
    fn unix_skips_a_file_that_is_not_executable() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("bin/tool"), false);
        let path = dir.path().join("bin");
        let search = Search {
            path: Some(path.as_os_str()),
            pathext: None,
            platform: Platform::Linux,
        };
        assert_eq!(resolve_in(OsStr::new("tool"), &search), None);
    }

    #[test]
    fn windows_finds_an_npm_cmd_shim() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        touch(&cwd.join("npm/codex.cmd"), true);
        touch(&cwd.join("npm/codex.ps1"), true);
        let path = cwd.join("npm");
        let search = Search {
            path: Some(path.as_os_str()),
            pathext: None,
            platform: Platform::Windows,
        };
        let found = resolve_in(OsStr::new("codex"), &search).expect("the shim is found");
        assert!(found.ends_with("codex.cmd"), "{}", found.display());
    }

    #[test]
    fn windows_prefers_exe_over_a_shim() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        touch(&cwd.join("bin/tool.exe"), true);
        touch(&cwd.join("bin/tool.cmd"), true);
        let path = cwd.join("bin");
        let search = Search {
            path: Some(path.as_os_str()),
            pathext: Some(OsStr::new(".EXE;.CMD")),
            platform: Platform::Windows,
        };
        assert!(
            resolve_in(OsStr::new("tool"), &search)
                .unwrap()
                .ends_with("tool.exe")
        );
    }

    #[test]
    fn windows_reads_pathext_when_given_one() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        touch(&cwd.join("bin/tool.PS1"), true);
        let path = cwd.join("bin");
        let search = Search {
            path: Some(path.as_os_str()),
            pathext: Some(OsStr::new(".EXE;.PS1")),
            platform: Platform::Windows,
        };
        assert!(
            resolve_in(OsStr::new("tool"), &search)
                .unwrap()
                .ends_with("tool.PS1")
        );
    }

    #[test]
    fn a_name_with_a_directory_is_not_searched_on_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        touch(&cwd.join("bin/tool"), true);
        let path = cwd.join("bin");
        let search = Search {
            path: Some(path.as_os_str()),
            pathext: None,
            platform: Platform::Linux,
        };
        assert_eq!(resolve_in(OsStr::new("./tool"), &search), None);
        touch(&cwd.join("work/tool"), true);
        let absolute = cwd.join("work/tool");
        assert_eq!(
            resolve_in(absolute.as_os_str(), &search),
            Some(cwd.join("work/tool"))
        );
    }

    #[test]
    fn macos_config_and_state_are_under_library() {
        let get = env(&[("HOME", "/Users/made-up")]);
        assert_eq!(
            config_dir_in(Platform::Macos, &get).unwrap(),
            PathBuf::from("/Users/made-up/Library/Application Support/parrot")
        );
        assert_eq!(
            state_dir_in(Platform::Macos, &get).unwrap(),
            PathBuf::from("/Users/made-up/Library/Application Support/parrot")
        );
    }

    #[test]
    fn windows_directories_come_from_appdata() {
        let get = env(&[
            ("USERPROFILE", "C:\\Users\\made-up"),
            ("APPDATA", "C:\\Users\\made-up\\AppData\\Roaming"),
            ("LOCALAPPDATA", "C:\\Users\\made-up\\AppData\\Local"),
        ]);
        assert_eq!(
            home_dir_in(Platform::Windows, &get).unwrap(),
            PathBuf::from("C:\\Users\\made-up")
        );
        assert_eq!(
            config_dir_in(Platform::Windows, &get).unwrap(),
            PathBuf::from("C:\\Users\\made-up\\AppData\\Roaming").join("parrot")
        );
        assert_eq!(
            state_dir_in(Platform::Windows, &get).unwrap(),
            PathBuf::from("C:\\Users\\made-up\\AppData\\Local").join("parrot")
        );
    }

    #[test]
    fn linux_directories_follow_the_xdg_variables() {
        let get = env(&[
            ("HOME", "/home/made-up"),
            ("XDG_CONFIG_HOME", "/home/made-up/.config-custom"),
            ("XDG_STATE_HOME", "/home/made-up/.state-custom"),
        ]);
        assert_eq!(
            config_dir_in(Platform::Linux, &get).unwrap(),
            PathBuf::from("/home/made-up/.config-custom/parrot")
        );
        assert_eq!(
            state_dir_in(Platform::Linux, &get).unwrap(),
            PathBuf::from("/home/made-up/.state-custom/parrot")
        );
    }

    #[test]
    fn a_missing_home_yields_no_directory() {
        let get = env(&[]);
        assert_eq!(config_dir_in(Platform::Linux, &get), None);
        assert_eq!(state_dir_in(Platform::Linux, &get), None);
        assert_eq!(config_dir_in(Platform::Windows, &get), None);
    }
}
