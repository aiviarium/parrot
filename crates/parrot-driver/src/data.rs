//! An egg's driver data (spec 31) for the harness version a driver has seen.
//!
//! A driver starts with the egg's own files. When the harness reports its version, from
//! `--version` or from its `initialize` handshake, [`DataCell::observe`] picks the version shim
//! that holds it and loads its files in place of the egg's. A process serves one harness binary,
//! so one version applies to every binding.

use std::sync::RwLock;

use parrot_proto::driver_data::{Builtin, DriverData, read, shim_for};

struct Loaded {
    data: &'static DriverData,
    /// The shim folder in use, if any.
    shim: Option<String>,
    version: Option<String>,
    error: Option<String>,
}

/// The data in use, replaced when a newly seen version selects another shim.
pub struct DataCell {
    builtin: Builtin,
    loaded: RwLock<Option<Loaded>>,
}

impl DataCell {
    pub const fn new(builtin: Builtin) -> Self {
        DataCell {
            builtin,
            loaded: RwLock::new(None),
        }
    }

    fn load(&self, version: Option<&str>) -> Loaded {
        let driver = read("driver.kdl", self.builtin.driver)
            .map(|(t, _)| t)
            .unwrap_or_default();
        let shim = shim_for(&driver, version).map(|s| s.dir);
        let (data, error) = match DriverData::load_for(self.builtin, version) {
            Ok(d) => (d, None),
            Err(e) => (
                DriverData::builtin(self.builtin).expect("the compiled-in data parses"),
                Some(format!(
                    "the egg's driver data is invalid, using the built-in copy: {e}"
                )),
            ),
        };
        // Each distinct shim is loaded once per process, so leaking it costs a few copies at most.
        Loaded {
            data: Box::leak(Box::new(data)),
            shim,
            version: version.map(str::to_owned),
            error,
        }
    }

    fn with<T>(&self, f: impl FnOnce(&Loaded) -> T) -> T {
        if let Some(l) = self.loaded.read().unwrap().as_ref() {
            return f(l);
        }
        let mut w = self.loaded.write().unwrap();
        if w.is_none() {
            *w = Some(self.load(None));
        }
        f(w.as_ref().expect("just loaded"))
    }

    /// The data in use.
    pub fn get(&self) -> &'static DriverData {
        self.with(|l| l.data)
    }

    /// Why the egg's data was not used, if it wasn't.
    pub fn load_error(&self) -> Option<String> {
        self.with(|l| l.error.clone())
    }

    /// The harness version last observed.
    pub fn version(&self) -> Option<String> {
        self.with(|l| l.version.clone())
    }

    /// The shim folder in use.
    pub fn shim(&self) -> Option<String> {
        self.with(|l| l.shim.clone())
    }

    /// Takes the version the harness reported. When it selects another shim than the one in
    /// use, the data is loaded again and `true` is returned.
    pub fn observe(&self, version: &str) -> bool {
        let Some(version) = crate::probe::version_in(version) else {
            return false;
        };
        let driver = read("driver.kdl", self.builtin.driver)
            .map(|(t, _)| t)
            .unwrap_or_default();
        let shim = shim_for(&driver, Some(&version)).map(|s| s.dir);
        let mut w = self.loaded.write().unwrap();
        match w.as_mut() {
            Some(l) if l.shim == shim => {
                l.version = Some(version);
                false
            }
            _ => {
                let changed = w.is_some();
                *w = Some(self.load(Some(&version)));
                changed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILTIN: Builtin = Builtin {
        driver: "driver \"command\"\ndata-version \"1\"\nshim \">=2.0.0\" dir=\"shims/2\"\n",
        options: "option \"mode\" label=\"Mode\" {\n  choice \"a\"\n}\n",
        levels: "",
        correlate: "",
        shims: &[(
            "shims/2",
            "options.kdl",
            "option \"mode\" label=\"Mode\" {\n  choice \"a\"\n  choice \"b\"\n}\n",
        )],
    };

    #[test]
    fn a_version_in_a_shim_range_loads_the_shim() {
        let cell = DataCell::new(BUILTIN);
        assert_eq!(cell.get().options.get("mode").unwrap().choice.len(), 1);
        assert!(!cell.observe("1.9.0 (Example)"));
        assert_eq!(cell.version().as_deref(), Some("1.9.0"));
        assert!(cell.observe("example-cli/2.1.0 (linux)"));
        assert_eq!(cell.shim().as_deref(), Some("shims/2"));
        assert_eq!(cell.get().options.get("mode").unwrap().choice.len(), 2);
        assert!(!cell.observe("2.2.0"));
        assert!(!cell.observe("no version here"));
    }
}
