//! Building a command egg's driver for one platform and putting it in its variant folder, so
//! `parrot eggs pack` can make a registry on any OS without a shell script.

use crate::driver::{DriverFile, DriverRun};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What to build and where from.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// The Rust target triple; this machine's when left out.
    pub target: Option<String>,
    /// Build with optimisations, as a release does.
    pub release: bool,
    /// The workspace to run `cargo` in, which is where the driver crates live.
    pub workspace: PathBuf,
}

/// The platform names in a target triple: Rust's OS name and architecture.
///
/// `Some(("macos", "aarch64"))` for `aarch64-apple-darwin`; `None` when the triple names no OS
/// this knows.
pub fn platform_of(triple: &str) -> Option<(String, String)> {
    let mut parts = triple.split('-');
    let arch = parts.next()?;
    let os = triple.split('-').find_map(|part| match part {
        "linux" => Some("linux"),
        "windows" => Some("windows"),
        "darwin" | "apple" | "macos" => Some("macos"),
        _ => None,
    })?;
    Some((os.to_string(), arch.to_string()))
}

/// This machine's target triple, as `rustc` reports it.
pub fn host_triple() -> Result<String, String> {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("cannot run rustc: {e}"))?;
    if !out.status.success() {
        return Err("rustc -vV failed".into());
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_string))
        .ok_or_else(|| "rustc -vV has no host line".to_string())
}

/// Every package folder under `src`, or `src` itself when it is one.
pub fn package_folders(src: &Path) -> Result<Vec<PathBuf>, String> {
    if src.join("package.kdl").is_file() {
        return Ok(vec![src.to_path_buf()]);
    }
    let entries = std::fs::read_dir(src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("package.kdl").is_file())
        .collect();
    folders.sort();
    if folders.is_empty() {
        return Err(format!("{} has no packages", src.display()));
    }
    Ok(folders)
}

/// One driver program to build and where it goes in its egg.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// The cargo binary name, such as `parrot-driver-claude`.
    pub bin: String,
    /// The file inside the egg's `parrot` folder, such as `variants/linux-x86_64/driver`.
    pub dest: PathBuf,
}

/// The command drivers of `folder` for the platform `os`/`arch`. A data-only egg has none.
pub fn programs(folder: &Path, os: &str, arch: &str) -> Result<Vec<Program>, String> {
    let data = folder.join(crate::target::TARGET);
    let path = data.join("driver.kdl");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let driver = DriverFile::parse(&text).map_err(|p| format!("{}: {}", path.display(), p.join("; ")))?;
    if matches!(driver.driver, DriverRun::Builtin { .. }) {
        return Ok(Vec::new());
    }
    let Some(command) = driver.command_for(os, arch) else {
        return Ok(Vec::new());
    };
    let (run, _) = command.map_err(|e| format!("{}: {e}", path.display()))?;
    let bin = Path::new(&run)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{}: {run} names no program", path.display()))?;
    Ok(vec![Program {
        bin,
        dest: PathBuf::from(run),
    }])
}

/// Builds each command egg's driver in `src` for `opts.target`, or this machine, and copies it
/// into the egg's variant folder. Returns the files that were written.
pub fn build_variants(src: &Path, opts: &BuildOptions) -> Result<Vec<PathBuf>, String> {
    let triple = match &opts.target {
        Some(target) => target.clone(),
        None => host_triple()?,
    };
    let (os, arch) = platform_of(&triple).ok_or_else(|| format!("no variant for the target {triple}"))?;

    let mut work = Vec::new();
    for folder in package_folders(src)? {
        for program in programs(&folder, &os, &arch)? {
            work.push((folder.clone(), program));
        }
    }
    if work.is_empty() {
        return Ok(Vec::new());
    }

    let mut args: Vec<OsString> = vec!["build".into(), "--locked".into()];
    let mut seen = Vec::new();
    for (_, program) in &work {
        if !seen.contains(&program.bin) {
            seen.push(program.bin.clone());
        }
    }
    for bin in &seen {
        args.push("--bin".into());
        args.push(bin.into());
    }
    if opts.release {
        args.push("--release".into());
    }
    if opts.target.is_some() {
        args.push("--target".into());
        args.push(triple.clone().into());
    }
    let status = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(&args)
        .current_dir(&opts.workspace)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build for {triple} failed"));
    }

    let profile = if opts.release { "release" } else { "debug" };
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| opts.workspace.join("target"));
    let bin_dir = match &opts.target {
        Some(target) => target_dir.join(target).join(profile),
        None => target_dir.join(profile),
    };
    let exe = if os == "windows" { ".exe" } else { "" };
    let mut written = Vec::new();
    for (folder, program) in work {
        let source = bin_dir.join(format!("{}{exe}", program.bin));
        let dest = folder.join(crate::target::TARGET).join(&program.dest);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::copy(&source, &dest)
            .map_err(|e| format!("cannot copy {} to {}: {e}", source.display(), dest.display()))?;
        written.push(dest);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(root: &Path, id: &str, driver: &str) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(dir.join("parrot")).unwrap();
        std::fs::write(
            dir.join("package.kdl"),
            format!(
                "format 1\npackage {{\n  group example\n  id {id}\n  version \"0.1.0\"\n}}\ntargets {{\n  parrot {{\n    requires {{\n      parrot \"^0.1\"\n    }}\n    variants {{\n      variant os=linux arch=x86_64\n    }}\n  }}\n}}\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("parrot/driver.kdl"), driver).unwrap();
        dir
    }

    #[test]
    fn a_triple_names_a_platform() {
        assert_eq!(
            platform_of("x86_64-unknown-linux-gnu"),
            Some(("linux".into(), "x86_64".into()))
        );
        assert_eq!(
            platform_of("aarch64-apple-darwin"),
            Some(("macos".into(), "aarch64".into()))
        );
        assert_eq!(
            platform_of("x86_64-pc-windows-msvc"),
            Some(("windows".into(), "x86_64".into()))
        );
        assert_eq!(platform_of("wasm32-wasip1"), None);
    }

    #[test]
    fn only_command_eggs_have_programs_to_build() {
        let dir = tempfile::tempdir().unwrap();
        let command = package(
            dir.path(),
            "tiny",
            "driver \"command\" {\n  run \"variants/{os}-{arch}/parrot-driver-tiny{exe}\"\n}\ndata-version \"1\"\n",
        );
        let builtin = package(
            dir.path(),
            "acp",
            "driver \"builtin\" name=\"acp\"\ncommand \"gemini\"\ndata-version \"1\"\n",
        );
        assert_eq!(
            programs(&command, "linux", "x86_64").unwrap(),
            vec![Program {
                bin: "parrot-driver-tiny".into(),
                dest: "variants/linux-x86_64/parrot-driver-tiny".into(),
            }]
        );
        assert!(programs(&builtin, "linux", "x86_64").unwrap().is_empty());
        assert_eq!(
            package_folders(dir.path()).unwrap(),
            vec![builtin, command.clone()]
        );
        assert_eq!(package_folders(&command).unwrap(), vec![command.clone()]);
    }
}
