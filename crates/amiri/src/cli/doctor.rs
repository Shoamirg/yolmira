//! `amiri doctor`: reports silent failure modes (ARCHITECTURE §14). Checks are pure
//! functions over what they read, so tests can simulate broken machines.
//! Phase 1 Slice D adds the daemon, database and elevation checks.

use amiri_ipc::win::env::{expand_with, windows_powershell, EnvSource};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    pub next_steps: Vec<String>,
}

pub fn checks(src: &EnvSource) -> Vec<Check> {
    vec![comspec(src), powershell_on_path(src)]
}

fn system_vars(src: &EnvSource) -> Vec<(String, String)> {
    src.windows_dir
        .iter()
        .flat_map(|d| {
            [
                ("SystemRoot".to_string(), d.clone()),
                ("windir".to_string(), d.clone()),
            ]
        })
        .collect()
}

/// Fix commands. They write the registry value directly as REG_EXPAND_SZ and keep
/// `%...%` entries unexpanded: reading PATH through .NET expands them, and writing it
/// back would store fixed paths and change the value type. A backup comes first.
const BACKUP: &str = r#"In PowerShell run as administrator, first back up: reg export "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment" "$HOME\env-backup.reg" /y"#;
const COMSPEC_FIX: &str = r#"Then: New-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment' -Name ComSpec -Value '%SystemRoot%\system32\cmd.exe' -PropertyType ExpandString -Force"#;
const PATH_FIX: &str = r#"Then: $key = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'; $raw = (Get-Item $key).GetValue('Path', '', 'DoNotExpandEnvironmentNames'); if ($raw -notmatch 'WindowsPowerShell\\v1\.0') { New-ItemProperty -Path $key -Name Path -Value ($raw.TrimEnd(';') + ';%SystemRoot%\System32\WindowsPowerShell\v1.0\') -PropertyType ExpandString -Force }"#;
const AFTER: &str =
    "Then sign out and back in. To undo, double-click env-backup.reg in your user folder.";

/// The machine environment's `ComSpec` must exist and point at a real file: other
/// programs start `cmd.exe` through it. AMIRI itself falls back to the system shell.
fn comspec(src: &EnvSource) -> Check {
    let fix = format!("{BACKUP}. {COMSPEC_FIX}. {AFTER}");
    let (status, detail) = match src.machine_value("ComSpec") {
        None => (
            Status::Fail,
            "the machine environment has no ComSpec variable".to_string(),
        ),
        Some(raw) => {
            let value = expand_with(raw, &system_vars(src));
            if Path::new(&value).is_file() {
                (Status::Ok, value)
            } else {
                (
                    Status::Fail,
                    format!("ComSpec points to {value}, which doesn't exist"),
                )
            }
        }
    };
    let next_steps = if status == Status::Ok {
        vec![]
    } else {
        vec![fix]
    };
    Check {
        name: "machine_comspec",
        status,
        detail,
        next_steps,
    }
}

/// Windows PowerShell's folder should be on the machine PATH. AMIRI starts it by full
/// path either way; other tools look for a bare `powershell`.
fn powershell_on_path(src: &EnvSource) -> Check {
    let Some(dir) = src.windows_dir.as_deref() else {
        return Check {
            name: "machine_path_powershell",
            status: Status::Warn,
            detail: "the Windows directory is unknown".into(),
            next_steps: vec!["Run `amiri doctor` again; report it if this persists.".into()],
        };
    };
    let ps_dir = normalize(windows_powershell(dir).trim_end_matches("powershell.exe"));
    let vars = system_vars(src);
    let on_path = src
        .machine_value("Path")
        .map(|raw| expand_with(raw, &vars))
        .is_some_and(|p| p.split(';').any(|entry| normalize(entry) == ps_dir));
    if on_path {
        return Check {
            name: "machine_path_powershell",
            status: Status::Ok,
            detail: ps_dir,
            next_steps: vec![],
        };
    }
    Check {
        name: "machine_path_powershell",
        status: Status::Warn,
        detail: format!("the machine PATH lacks {ps_dir}; AMIRI starts PowerShell by full path, other tools may not"),
        next_steps: vec![format!("{BACKUP}. {PATH_FIX}. {AFTER}")],
    }
}

fn normalize(p: &str) -> String {
    p.trim().trim_end_matches('\\').to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(windir: &str, comspec: Option<&str>, path: &str) -> EnvSource {
        let mut m = vec![("Path".to_string(), path.to_string())];
        if let Some(c) = comspec {
            m.push(("ComSpec".to_string(), c.to_string()));
        }
        EnvSource {
            windows_dir: Some(windir.into()),
            machine: m,
            ..Default::default()
        }
    }

    fn real_windir() -> String {
        std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())
    }

    const HEALTHY_PATH: &str =
        r"%SystemRoot%\system32;%SystemRoot%\System32\WindowsPowerShell\v1.0\";

    #[test]
    fn healthy_machine_passes_both() {
        let src = machine(
            &real_windir(),
            Some(r"%SystemRoot%\system32\cmd.exe"),
            HEALTHY_PATH,
        );
        let c = checks(&src);
        assert!(c.iter().all(|c| c.status == Status::Ok), "{c:?}");
    }

    #[test]
    fn missing_comspec_fails_with_the_fix() {
        let src = machine(&real_windir(), None, HEALTHY_PATH);
        let c = comspec(&src);
        assert_eq!(c.status, Status::Fail);
        let step = &c.next_steps[0];
        assert!(step.contains("reg export"), "backs up first: {c:?}");
        assert!(
            step.contains("-Name ComSpec") && step.contains("-PropertyType ExpandString"),
            "{c:?}"
        );
    }

    #[test]
    fn comspec_to_a_missing_file_fails() {
        let src = machine(&real_windir(), Some(r"C:\nope\cmd.exe"), HEALTHY_PATH);
        assert_eq!(comspec(&src).status, Status::Fail);
    }

    #[test]
    fn path_without_windows_powershell_warns_with_the_fix() {
        let src = machine(
            &real_windir(),
            Some(r"%SystemRoot%\system32\cmd.exe"),
            r"%SystemRoot%\system32",
        );
        let c = powershell_on_path(&src);
        assert_eq!(c.status, Status::Warn);
        let step = &c.next_steps[0];
        assert!(
            step.contains("reg export") && step.contains("DoNotExpandEnvironmentNames"),
            "{c:?}"
        );
        // The .NET setter would store %...% entries expanded and change the value type.
        assert!(!step.contains("SetEnvironmentVariable"), "{c:?}");
    }

    #[test]
    fn path_match_ignores_case_and_trailing_backslash() {
        let src = machine(
            r"C:\Windows",
            None,
            r"c:\windows\system32\windowspowershell\V1.0",
        );
        assert_eq!(powershell_on_path(&src).status, Status::Ok);
    }
}
