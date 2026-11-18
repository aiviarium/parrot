//! `parrot eggs`: install, list, check and pack driver eggs (the `parrot` Hatchery target).

use crate::serve::default_state_dir;
use parrot_eggs::{Eggs, ParrotTarget};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Installs packages, such as `aiviarium/claude`, `claude@^0.1` or `./eggs/claude`.
    Install {
        #[arg(required = true)]
        specs: Vec<String>,
        /// A registry searched first: the `https://` URL of a static registry, a static
        /// registry folder, or a folder of package folders like the repository's `eggs/`.
        /// Without one, `PARROT_EGGS_REGISTRY` or the release registry,
        /// https://aiviarium.github.io/parrot/eggs.
        #[arg(long = "registry")]
        registries: Vec<String>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Removes an installed package, such as `aiviarium/claude`.
    Remove {
        name: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Lists each installed agent's egg.
    List {
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// One JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// Checks package folders; without any, the eggs the runtime would load.
    Check {
        dirs: Vec<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Packs package folders into eggs, building this platform's driver binaries first with
    /// `--build`.
    Pack {
        #[arg(required = true)]
        dirs: Vec<PathBuf>,
        /// Where to write the `.egg` files.
        #[arg(long, required_unless_present = "publish")]
        out: Option<PathBuf>,
        /// Publish into this static registry folder instead.
        #[arg(long)]
        publish: Option<PathBuf>,
        /// Build each command egg's driver into its variant folder before packing.
        #[arg(long)]
        build: bool,
        /// The target to build for; this machine's when left out.
        #[arg(long, requires = "build")]
        target: Option<String>,
        /// Build with optimisations, as a release does.
        #[arg(long, requires = "build")]
        release: bool,
        /// The workspace to run cargo in; the current directory when left out.
        #[arg(long, requires = "build")]
        workspace: Option<PathBuf>,
        /// Sign the published eggs with the Ed25519 key in this environment variable, so a
        /// registry can be checked against its public key. Never a command line argument.
        #[arg(long, requires = "publish")]
        sign_key_env: Option<String>,
    },
}

fn state(dir: Option<PathBuf>) -> Result<Eggs, String> {
    let dir = match dir {
        Some(d) => d,
        None => default_state_dir().map_err(|e| e.to_string())?,
    };
    Ok(Eggs::new(dir))
}

pub async fn run(args: Args) -> Result<(), String> {
    match args.command {
        Command::Install {
            specs,
            registries,
            state_dir,
        } => {
            let eggs = state(state_dir)?;
            for line in eggs.install(&specs, &registries)? {
                println!("installed {line}");
            }
            Ok(())
        }
        Command::Remove { name, state_dir } => {
            state(state_dir)?.remove(&name)?;
            println!("removed {name}");
            Ok(())
        }
        Command::List { state_dir, json } => {
            let (loaded, problems) = state(state_dir)?.load();
            for p in &problems {
                eprintln!("parrot: {p}");
            }
            if loaded.is_empty() && !json {
                println!("no driver eggs installed; try `parrot eggs install aiviarium/claude`");
            }
            for egg in loaded {
                let source = "installed";
                let (status, detail) = match &egg.driver {
                    Ok(_) => ("ok", None),
                    Err(e) => ("refused", Some(e.clone())),
                };
                if json {
                    let mut v = serde_json::json!({
                        "agent": egg.agent, "package": egg.package, "version": egg.version,
                        "source": source, "status": status, "data": egg.data_dir,
                    });
                    if let Some(d) = detail {
                        v["detail"] = d.into();
                    }
                    println!("{v}");
                } else {
                    let tail = detail.map(|d| format!("  {d}")).unwrap_or_default();
                    println!(
                        "{:<10} {:<22} {:<8} {:<9} {status}{tail}",
                        egg.agent, egg.package, egg.version, source
                    );
                }
            }
            Ok(())
        }
        Command::Check { dirs, state_dir } => {
            let target = ParrotTarget::default();
            let mut failed = 0;
            if dirs.is_empty() {
                let (loaded, problems) = state(state_dir)?.load();
                failed += problems.len();
                for p in problems {
                    eprintln!("parrot: {p}");
                }
                for egg in loaded {
                    let dir = egg.data_dir.parent().map(PathBuf::from).unwrap_or_default();
                    failed += report(
                        &dir.display().to_string(),
                        parrot_eggs::store::check_package(&dir, &target),
                    );
                }
            }
            for dir in dirs {
                failed += report(
                    &dir.display().to_string(),
                    parrot_eggs::store::check_package(&dir, &target),
                );
            }
            if failed > 0 {
                return Err(format!("{failed} problem(s) found"));
            }
            Ok(())
        }
        Command::Pack {
            dirs,
            out,
            publish,
            build,
            target: build_target,
            release,
            workspace,
            sign_key_env,
        } => {
            let target = ParrotTarget::default();
            let key = match sign_key_env {
                Some(var) => {
                    let text = std::env::var(&var)
                        .map_err(|_| format!("--sign-key-env {var}: the variable is not set"))?;
                    Some(parrot_eggs::signing::parse(&text).map_err(|e| format!("{var}: {e}"))?)
                }
                None => None,
            };
            if build {
                let opts = parrot_eggs::build::BuildOptions {
                    target: build_target,
                    release,
                    workspace: workspace.unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?),
                };
                for dir in &dirs {
                    for path in parrot_eggs::build::build_variants(dir, &opts)? {
                        println!("built {}", path.display());
                    }
                }
            }
            for dir in dirs {
                if let Some(registry) = &publish {
                    for line in parrot_eggs::store::publish_folder(&dir, registry, &target, key.as_ref())? {
                        println!("published {line}");
                    }
                    continue;
                }
                let out = out.as_ref().expect("clap requires --out");
                std::fs::create_dir_all(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
                for egg in parrot_eggs::store::pack(&dir, &target)? {
                    let path = out.join(egg.file_name());
                    std::fs::write(&path, egg.bytes())
                        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                    println!("{}", path.display());
                }
            }
            if let Some(key) = &key {
                println!("public key {}", parrot_eggs::signing::public_key(key));
            }
            Ok(())
        }
    }
}

fn report(what: &str, problems: Vec<String>) -> usize {
    if problems.is_empty() {
        println!("ok {what}");
    }
    for p in &problems {
        println!("problem {what}: {p}");
    }
    problems.len()
}
