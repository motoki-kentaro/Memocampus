//! Which registry a run may write (Issue #67, Slice 2 review S2-01).
//!
//! A run is either the product (what the Run key starts, with no verification option) or a
//! verification run. The decision is made once, from the command line and the environment, before
//! anything touches the registry, and it fixes the registry the run gets:
//!
//! - `Product`: the user's real Run key, writable. Only a start with no verification option at all.
//! - `Isolated(id)`: a private copy of the keys under `HKCU\Software\MemocampusTrayTest\<id>`,
//!   writable (it is deleted again by whoever created it).
//! - `ReadOnly`: the real keys may be read (to show the status) and **cannot be written**: a toggle,
//!   an update and a first-run registration all fail, in the registry layer itself, not by a check
//!   at each call site.
//!
//! `--selftest` drives toggles, so it is refused without an isolated registry before any registry
//! access happens. Any other verification option without one gets `ReadOnly`.

/// The command line and environment facts that matter, nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flags {
    pub registry_root: Option<String>,
    pub selftest: bool,
    pub run_seconds: bool,
    pub log: bool,
    pub mcp_exe: bool,
    pub mcp_args: bool,
    pub no_autostart: bool,
    pub headless: bool,
    /// `MEMOCAMPUS_APP_DIR` is set: another application directory, i.e. a test or a trial.
    pub app_dir_override: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMode {
    Product,
    Isolated(String),
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeError {
    /// `--selftest` needs `--registry-root`.
    SelftestNeedsIsolatedRegistry,
}

impl Flags {
    /// Any option or setting that exists only for verification.
    pub fn is_verification(&self) -> bool {
        self.registry_root.is_some()
            || self.selftest
            || self.run_seconds
            || self.log
            || self.mcp_exe
            || self.mcp_args
            || self.no_autostart
            || self.headless
            || self.app_dir_override
    }
}

pub fn decide(flags: &Flags) -> Result<RunMode, ModeError> {
    if flags.selftest && flags.registry_root.is_none() {
        return Err(ModeError::SelftestNeedsIsolatedRegistry);
    }
    if let Some(id) = &flags.registry_root {
        return Ok(RunMode::Isolated(id.clone()));
    }
    if flags.is_verification() {
        return Ok(RunMode::ReadOnly);
    }
    Ok(RunMode::Product)
}

impl RunMode {
    /// Only a product start registers itself for logon on its own.
    pub fn first_run_registration(&self) -> bool {
        matches!(self, RunMode::Product)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every combination of the eight inputs (seven options and the environment variable), with and
    /// without an isolated registry: 2^8 x 2 = **512 distinct combinations**. The loop runs 2^9 x 2 =
    /// 1024 evaluations because bit 8 is unused, so each combination is evaluated **twice** (the
    /// second pass guards against state). Each is checked against a rule written independently of
    /// `decide`.
    #[test]
    fn every_combination_gets_the_registry_its_options_allow() {
        let mut seen_product = 0;
        for bits in 0u32..(1 << 9) {
            for isolated in [false, true] {
                let flag = |n: u32| bits & (1 << n) != 0;
                let flags = Flags {
                    registry_root: isolated.then(|| "id-1".to_string()),
                    selftest: flag(0),
                    run_seconds: flag(1),
                    log: flag(2),
                    mcp_exe: flag(3),
                    mcp_args: flag(4),
                    no_autostart: flag(5),
                    headless: flag(6),
                    app_dir_override: flag(7),
                };
                // Bit 8 is unused: a second pass over the same flags guards against state.
                let any_option = bits & 0xff != 0;
                let expected = if flag(0) && !isolated {
                    Err(ModeError::SelftestNeedsIsolatedRegistry)
                } else if isolated {
                    Ok(RunMode::Isolated("id-1".into()))
                } else if any_option {
                    Ok(RunMode::ReadOnly)
                } else {
                    Ok(RunMode::Product)
                };
                let got = decide(&flags);
                assert_eq!(got, expected, "{flags:?}");
                if got == Ok(RunMode::Product) {
                    seen_product += 1;
                }
                // The one thing a product start does that a trial must not.
                assert_eq!(
                    got.as_ref().is_ok_and(RunMode::first_run_registration),
                    got == Ok(RunMode::Product)
                );
            }
        }
        // Product is reachable only with no option at all: 1 combination, evaluated twice.
        assert_eq!(seen_product, 2);
    }

    #[test]
    fn a_plain_start_is_the_product_and_any_single_option_is_not() {
        assert_eq!(decide(&Flags::default()), Ok(RunMode::Product));
        let one = |f: fn(&mut Flags)| {
            let mut flags = Flags::default();
            f(&mut flags);
            decide(&flags)
        };
        assert_eq!(one(|f| f.run_seconds = true), Ok(RunMode::ReadOnly));
        assert_eq!(one(|f| f.app_dir_override = true), Ok(RunMode::ReadOnly));
        assert_eq!(one(|f| f.log = true), Ok(RunMode::ReadOnly));
        assert_eq!(
            one(|f| f.selftest = true),
            Err(ModeError::SelftestNeedsIsolatedRegistry)
        );
    }
}
