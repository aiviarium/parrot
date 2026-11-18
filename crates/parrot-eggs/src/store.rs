//! Installed eggs in `<state_dir>/eggs`, packing them, and loading them for the runtime. Nothing
//! is compiled in: with no egg installed, the runtime has no agents.

use crate::driver::DriverFile;
use crate::target::{ParrotTarget, TARGET};
use hatchery::{Client, Config, Manifest, RegistryConfig, Spec, block_on};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One driver egg, ready for the runtime.
#[derive(Debug, Clone)]
pub struct LoadedEgg {
    /// The agent id: the package id.
    pub agent: String,
    /// The package, `group/id`.
    pub package: String,
    pub version: String,
    /// The display name from `about`, or the id.
    pub name: String,
    /// The egg's `parrot` folder, passed to the driver as `PARROT_DRIVER_DATA`.
    pub data_dir: PathBuf,
    /// The driver, or why this egg can't be used: an unmet `requires` or invalid data.
    pub driver: Result<DriverFile, String>,
}

/// The eggs of one runtime state directory.
#[derive(Debug, Clone)]
pub struct Eggs {
    state_dir: PathBuf,
    target: ParrotTarget,
}

impl Eggs {
    /// The eggs of the runtime whose state directory is `state_dir`.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            target: ParrotTarget::default(),
        }
    }

    /// The same, for a runtime of another version.
    pub fn with_target(mut self, target: ParrotTarget) -> Self {
        self.target = target;
        self
    }

    /// The install folder, `<state_dir>/eggs`.
    pub fn dir(&self) -> PathBuf {
        self.state_dir.join("eggs")
    }

    /// A Hatchery client for the `parrot` target over `registries`, then the registries of
    /// `<state_dir>/hatchery.kdl`. A registry is an `http(s)://` URL of a static registry, or a
    /// folder: one with an `index.json` is a static registry, and a folder of package folders,
    /// like the in-repo `eggs/`, is packed into one first, held by the returned guard.
    pub fn client(&self, registries: &[String]) -> Result<(Client, Vec<tempfile::TempDir>), String> {
        let config_path = self.state_dir.join("hatchery.kdl");
        let mut config = if config_path.is_file() {
            Config::load(&config_path).map_err(|e| e.to_string())?
        } else {
            Config::empty()
        };
        let mut held = Vec::new();
        let mut lines = Vec::new();
        for r in registries {
            if r.starts_with("https://") || r.starts_with("http://") {
                let mut line = RegistryConfig::new("http", r.trim_end_matches('/'));
                // Releases are installed as soon as they are published.
                line.min_age = Some(std::time::Duration::ZERO);
                lines.push(line);
                continue;
            }
            let r = PathBuf::from(r);
            let location = if r.join("index.json").is_file() {
                r.clone()
            } else {
                let staged = tempfile::tempdir().map_err(|e| e.to_string())?;
                publish_folder(&r, staged.path(), &self.target)?;
                let path = staged.path().to_path_buf();
                held.push(staged);
                path
            };
            lines.push(RegistryConfig::new("dir", location.to_string_lossy()));
        }
        lines.append(&mut config.registries);
        config.registries = lines;
        let client = Client::builder()
            .config(config)
            .target(self.target.clone())
            .fetch(hatchery::fetch::HttpFetch::new())
            .cache_dir(self.state_dir.join("cache").join("hatchery"))
            .build()
            .map_err(|e| e.to_string())?;
        Ok((client, held))
    }

    /// Installs a package, such as `aiviarium/claude`, `claude@^0.1` or `./eggs/claude`.
    /// Without `registries`, and with none in `hatchery.kdl`, the default registry is searched:
    /// `PARROT_EGGS_REGISTRY`, else [`crate::DEFAULT_REGISTRY`]. Returns `group/id version` for
    /// each package installed.
    pub fn install(&self, spec: &str, registries: &[String]) -> Result<Vec<String>, String> {
        let spec = Spec::parse(spec).map_err(|e| e.to_string())?;
        let configured = self.state_dir.join("hatchery.kdl").is_file();
        let registries = if registries.is_empty() && !configured {
            vec![default_registry()]
        } else {
            registries.to_vec()
        };
        let (client, _held) = self.client(&registries)?;
        let dir = self.dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let report = block_on(client.add(&spec, &dir)).map_err(|e| e.to_string())?;
        Ok(report
            .packages
            .iter()
            .map(|c| format!("{} {}", c.name, c.version))
            .collect())
    }

    /// Removes an installed package.
    pub fn remove(&self, name: &str) -> Result<(), String> {
        let name = hatchery::Name::parse(name).map_err(|e| e.to_string())?;
        let (client, _held) = self.client(&[])?;
        client.remove(&name, &self.dir()).map_err(|e| e.to_string())
    }

    /// The installed packages that have a `parrot` target.
    pub fn installed(&self) -> Result<Vec<hatchery::Installed>, String> {
        let dir = self.dir();
        if !dir.join("eggs.lock").is_file() {
            return Ok(Vec::new());
        }
        let (client, _held) = self.client(&[])?;
        let all = client.installed(&dir).map_err(|e| e.to_string())?;
        Ok(all
            .into_iter()
            .filter(|p| p.targets.iter().any(|t| t == TARGET))
            .collect())
    }

    /// Every installed agent's egg. Problems reading the install folder are returned beside them.
    pub fn load(&self) -> (Vec<LoadedEgg>, Vec<String>) {
        let mut problems = Vec::new();
        let mut eggs: BTreeMap<String, LoadedEgg> = BTreeMap::new();
        match self.installed() {
            Ok(installed) => {
                for p in installed {
                    let egg = self.loaded(&p.manifest, p.dir.join(TARGET));
                    eggs.insert(egg.agent.clone(), egg);
                }
            }
            Err(e) => problems.push(format!("cannot read the installed eggs: {e}")),
        }
        (eggs.into_values().collect(), problems)
    }

    fn loaded(&self, manifest: &Manifest, data_dir: PathBuf) -> LoadedEgg {
        let name = manifest.name();
        let version = manifest.version().to_string();
        let driver = self.refusal(manifest).map_or_else(|| read_driver(&data_dir), Err);
        LoadedEgg {
            agent: name.id.clone(),
            package: name.to_string(),
            version,
            name: manifest.about.name.clone().unwrap_or_else(|| name.id.clone()),
            data_dir,
            driver,
        }
    }

    /// Why this runtime refuses an egg: no `parrot` target, or a `requires` line it doesn't meet.
    pub fn refusal(&self, manifest: &Manifest) -> Option<String> {
        let at = format!("{} {}", manifest.name(), manifest.version());
        let Some(target) = manifest.target(TARGET) else {
            return Some(format!("{at} has no parrot target"));
        };
        let (line, found) = hatchery::target::unmet(&self.target, &target.requires)?;
        let wanted = if line.values.is_empty() {
            "present".to_string()
        } else {
            line.values.join(" or ")
        };
        let found = found.unwrap_or_else(|| "absent".into());
        Some(if line.fact == TARGET {
            format!("{at} needs parrot {wanted}, and this runtime is {found}")
        } else {
            format!("{at} needs {} {wanted}, and here it is {found}", line.fact)
        })
    }
}

/// Reads and validates `driver.kdl` in a `parrot` folder.
pub fn read_driver(data_dir: &Path) -> Result<DriverFile, String> {
    let path = data_dir.join("driver.kdl");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    DriverFile::parse(&text).map_err(|p| format!("{}: {}", path.display(), p.join("; ")))
}

/// Checks a package folder as `hatchery pack --check` would for the `parrot` target, plus the
/// runtime's own `requires`. Returns every problem.
pub fn check_package(dir: &Path, target: &ParrotTarget) -> Vec<String> {
    let manifest = match Manifest::load(dir) {
        Ok(m) => m,
        Err(e) => return vec![e.to_string()],
    };
    let mut problems = Vec::new();
    if let Some(r) = Eggs::new(dir).with_target(target.clone()).refusal(&manifest) {
        problems.push(r);
    }
    let folder = dir.join(manifest.target(TARGET).map_or(TARGET, |t| t.dir()));
    let mut files = Vec::new();
    collect(&folder, &folder, &mut files);
    let found = crate::target::check_files(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())));
    problems.extend(found.into_iter().map(|p| p.to_string()));
    problems
}

/// The registry used when none is named: `PARROT_EGGS_REGISTRY`, else [`crate::DEFAULT_REGISTRY`].
pub fn default_registry() -> String {
    std::env::var(crate::REGISTRY_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| crate::DEFAULT_REGISTRY.to_string())
}

/// Packs a package folder into eggs (variants first, the base egg last), checking the `parrot`
/// target. Builds nothing. A variant whose folder doesn't exist is left out, so a developer
/// machine packs its own platform's variant and a release packs every platform's.
pub fn pack(dir: &Path, target: &ParrotTarget) -> Result<Vec<hatchery::Egg>, String> {
    let manifest = Manifest::load(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut opts = hatchery::PackOptions::new()
        .no_build(true)
        .check(target)
        .origin(hatchery::manifest::Origin::new());
    let mut declared = 0;
    let mut present = Vec::new();
    for t in &manifest.targets {
        for v in &t.variants {
            declared += 1;
            if dir.join(t.variant_dir(v)).is_dir() {
                let mut pick = hatchery::VariantPick::new(t.name.clone());
                if let Some(os) = &v.os {
                    pick = pick.os(os.clone());
                }
                if let Some(arch) = &v.arch {
                    pick = pick.arch(arch.clone());
                }
                present.push(pick);
            }
        }
    }
    if declared > 0 && present.len() < declared {
        opts = opts.base(true);
        for pick in present {
            opts = opts.variant(pick);
        }
    }
    hatchery::pack(dir, &opts).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Packs every package folder under `src` (or `src` itself, when it is one) and publishes the
/// eggs into the static registry at `registry`. Returns `group/id version` for each.
pub fn publish_folder(src: &Path, registry: &Path, target: &ParrotTarget) -> Result<Vec<String>, String> {
    let mut folders = Vec::new();
    if src.join("package.kdl").is_file() {
        folders.push(src.to_path_buf());
    } else {
        let entries = std::fs::read_dir(src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
        for e in entries.flatten() {
            if e.path().join("package.kdl").is_file() {
                folders.push(e.path());
            }
        }
        folders.sort();
    }
    if folders.is_empty() {
        return Err(format!("{} has no packages", src.display()));
    }
    std::fs::create_dir_all(registry).map_err(|e| format!("cannot create {}: {e}", registry.display()))?;
    let mut out = Vec::new();
    for folder in folders {
        for egg in pack(&folder, target)? {
            let published = hatchery::publish(&egg, &hatchery::PublishTo::Dir(registry.to_path_buf()), None)
                .map_err(|e| format!("cannot publish {}: {e}", egg.file_name()))?;
            out.push(format!("{} {}", published.name, published.version));
        }
    }
    Ok(out)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if let (Ok(rel), Ok(bytes)) = (path.strip_prefix(root), std::fs::read(&path)) {
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, bytes));
        }
    }
}
