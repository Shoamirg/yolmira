//! Folders left by the project's former name, AMIRI (decisions.md 2026-10-09, the
//! rename to Yolmira). Nothing is migrated automatically: doctor finds the old folders
//! and prints the commands that move them, so a person decides.

use super::Check;
use std::path::{Path, PathBuf};
use yolmira_ipc::Instance;

/// The old name's folder under `%LOCALAPPDATA%` and `%APPDATA%`.
const OLD_NAME: &str = "AMIRI";
/// The old daemon's database file name; Yolmira's is `yolmira.db`.
const OLD_DB_PREFIX: &str = "amiri.db";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyKind {
    /// `%LOCALAPPDATA%\AMIRI`: the database, logs, install id.
    Data,
    /// `%APPDATA%\AMIRI`: policies.toml, agents.toml, identity.toml.
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyFact {
    pub kind: LegacyKind,
    pub old: PathBuf,
    pub new: PathBuf,
    pub new_exists: bool,
}

/// The old folders that exist. Only for the user's own daemon: a named (test)
/// instance never looks at the user's folders.
pub fn gather(instance: &Instance) -> Vec<LegacyFact> {
    if instance.name.is_some() {
        return vec![];
    }
    let candidates = [
        (LegacyKind::Data, "LOCALAPPDATA", instance.home.clone()),
        (LegacyKind::Config, "APPDATA", instance.config_dir()),
    ];
    candidates
        .into_iter()
        .filter_map(|(kind, var, new)| {
            let old = PathBuf::from(std::env::var_os(var)?).join(OLD_NAME);
            // On a case-insensitive file system the old and new names never coincide,
            // but a --home pointing at the old folder must not be reported as legacy.
            (old.is_dir() && !same_path(&old, &new)).then(|| LegacyFact {
                kind,
                new_exists: new.exists(),
                old,
                new,
            })
        })
        .collect()
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

/// A PowerShell single-quoted literal.
fn ps(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "''"))
}

pub fn check(f: &LegacyFact) -> Check {
    let (name, what) = match f.kind {
        LegacyKind::Data => ("legacy_data", "data folder (database, logs)"),
        LegacyKind::Config => (
            "legacy_config",
            "settings folder (policies, agents, identity)",
        ),
    };
    let detail = format!(
        "found {} from the project's former name AMIRI; Yolmira doesn't read it \
         (no automatic migration). Yolmira's {what}: {}{}",
        f.old.display(),
        f.new.display(),
        if f.new_exists {
            " (already exists)"
        } else {
            ""
        },
    );
    let mut steps = vec![
        "First stop the old daemon if it still runs (this ends its shells): \
         Get-Process amirid -ErrorAction SilentlyContinue | Stop-Process"
            .to_string(),
        "Stop Yolmira's daemon too: yolmira daemon stop".to_string(),
    ];
    if f.new_exists {
        steps.push(format!(
            "Yolmira already has {}. To use the old one instead, set the new one aside: \
             Rename-Item -LiteralPath {} -NewName {}",
            f.new.display(),
            ps(&f.new),
            ps(Path::new(&format!(
                "{}.before-amiri-move",
                f.new.file_name().unwrap_or_default().to_string_lossy()
            ))),
        ));
    }
    steps.push(format!(
        "Move it: Move-Item -LiteralPath {} -Destination {}",
        ps(&f.old),
        ps(&f.new)
    ));
    if f.kind == LegacyKind::Data {
        steps.push(format!(
            "Rename the database files: Get-ChildItem -LiteralPath {} -Filter '{OLD_DB_PREFIX}*' \
             | Rename-Item -NewName {{ $_.Name -replace '^amiri', 'yolmira' }}",
            ps(&f.new)
        ));
    }
    steps.push("Then run yolmira doctor again.".to_string());
    steps.push(format!(
        "Or, if you don't need the old {what}, delete it: Remove-Item -Recurse -LiteralPath {}",
        ps(&f.old)
    ));
    let refs: Vec<&str> = steps.iter().map(String::as_str).collect();
    Check::warn(name, detail, &refs)
}
