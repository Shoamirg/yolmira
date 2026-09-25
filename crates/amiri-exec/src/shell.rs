//! Which program each shell kind runs. Always a full path: bare names depend on a
//! PATH that may be damaged (S0: PowerShell missing from this machine's PATH).

use crate::ExecError;
use amiri_ipc::win::env::{system_cmd, windows_powershell};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum ShellKind {
    Cmd,
    /// Windows PowerShell 5.1.
    Powershell,
    /// PowerShell 7, if installed.
    Pwsh,
    /// `wsl.exe`, default distro unless one is named.
    Wsl {
        distro: Option<String>,
    },
}

impl ShellKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::Powershell => "powershell",
            Self::Pwsh => "pwsh",
            Self::Wsl { .. } => "wsl",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// Resolve against the child environment `env` (from `child_env`) and the Windows dir.
/// `elevated`: never search PATH (its user part is writable by unelevated programs).
pub fn resolve(
    kind: &ShellKind,
    env: &[(String, String)],
    windows_dir: &str,
    elevated: bool,
) -> Result<ShellCommand, ExecError> {
    let var = |k: &str| {
        env.iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.clone())
    };
    let cmd = |program: String, args: &[&str]| ShellCommand {
        program,
        args: args.iter().map(|a| a.to_string()).collect(),
    };
    Ok(match kind {
        ShellKind::Cmd => cmd(
            var("ComSpec").unwrap_or_else(|| system_cmd(windows_dir)),
            &[],
        ),
        ShellKind::Powershell => cmd(windows_powershell(windows_dir), &["-NoLogo"]),
        ShellKind::Pwsh => cmd(
            find_pwsh(env, elevated).ok_or(ExecError::NotInstalled("PowerShell 7 (pwsh)"))?,
            &["-NoLogo"],
        ),
        ShellKind::Wsl { distro } => {
            let wsl = format!(r"{windows_dir}\System32\wsl.exe");
            if !Path::new(&wsl).is_file() {
                return Err(ExecError::NotInstalled("WSL"));
            }
            let mut c = cmd(wsl, &[]);
            if let Some(d) = distro {
                if !valid_distro(d) {
                    return Err(ExecError::Invalid(format!("WSL distribution name {d:?}")));
                }
                c.args.extend(["-d".to_string(), d.clone()]);
            }
            c
        }
    })
}

/// PowerShell 7 from its install folder under Program Files (from the known folder, not
/// the caller); then, only when not elevated, from the child PATH.
fn find_pwsh(env: &[(String, String)], elevated: bool) -> Option<String> {
    let var = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let installed = var("ProgramFiles").map(|pf| format!(r"{pf}\PowerShell\7\pwsh.exe"));
    let on_path: Vec<String> = if elevated {
        Vec::new()
    } else {
        var("PATH")
            .unwrap_or_default()
            .split(';')
            .filter(|d| !d.is_empty())
            .map(|d| format!(r"{}\pwsh.exe", d.trim_end_matches('\\')))
            .collect()
    };
    installed
        .into_iter()
        .chain(on_path)
        .find(|p| Path::new(p).is_file())
}

/// WSL distribution names: letters, digits, `.`, `_`, `-`; not starting with `-`.
pub fn valid_distro(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_uses_comspec_or_falls_back_to_the_system_shell() {
        let with = vec![(
            "ComSpec".to_string(),
            r"C:\Windows\system32\cmd.exe".to_string(),
        )];
        assert_eq!(
            resolve(&ShellKind::Cmd, &with, r"C:\Windows", false)
                .unwrap()
                .program,
            r"C:\Windows\system32\cmd.exe"
        );
        assert_eq!(
            resolve(&ShellKind::Cmd, &[], r"C:\Windows", false)
                .unwrap()
                .program,
            r"C:\Windows\System32\cmd.exe"
        );
    }

    #[test]
    fn powershell_is_a_full_path_regardless_of_path() {
        let c = resolve(
            &ShellKind::Powershell,
            &[("PATH".into(), String::new())],
            r"C:\Windows",
            false,
        )
        .unwrap();
        assert_eq!(
            c.program,
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
        );
    }

    #[test]
    fn missing_pwsh_is_reported_not_guessed() {
        let env = vec![
            ("PATH".to_string(), r"C:\nowhere".to_string()),
            ("ProgramFiles".to_string(), r"C:\nowhere".to_string()),
        ];
        assert!(matches!(
            resolve(&ShellKind::Pwsh, &env, r"C:\Windows", false),
            Err(ExecError::NotInstalled(_))
        ));
    }

    #[test]
    fn elevated_never_takes_pwsh_from_path() {
        // A pwsh.exe planted in a user-writable PATH folder.
        let dir = std::env::temp_dir().join(format!("amiri-pwsh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pwsh.exe"), b"not really").unwrap();
        let env = vec![
            ("PATH".to_string(), dir.to_string_lossy().into_owned()),
            ("ProgramFiles".to_string(), r"C:\nowhere".to_string()),
        ];
        let normal = resolve(&ShellKind::Pwsh, &env, r"C:\Windows", false).unwrap();
        assert!(normal.program.starts_with(&*dir.to_string_lossy()));
        assert!(matches!(
            resolve(&ShellKind::Pwsh, &env, r"C:\Windows", true),
            Err(ExecError::NotInstalled(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn distro_names_are_validated() {
        assert!(valid_distro("Ubuntu-22.04") && valid_distro("amiri_s4.test"));
        for bad in ["", "-e", "--help", "a b", "a;b", "a\\b", &"x".repeat(65)] {
            assert!(!valid_distro(bad), "{bad:?}");
            let kind = ShellKind::Wsl {
                distro: Some(bad.to_string()),
            };
            if Path::new(r"C:\Windows\System32\wsl.exe").is_file() {
                assert!(matches!(
                    resolve(&kind, &[], r"C:\Windows", false),
                    Err(ExecError::Invalid(_))
                ));
            }
        }
    }

    #[test]
    fn kinds_serialize_as_tagged_lowercase() {
        let v = serde_json::to_value(ShellKind::Wsl {
            distro: Some("x".into()),
        })
        .unwrap();
        assert_eq!(v, serde_json::json!({ "kind": "wsl", "distro": "x" }));
        let k: ShellKind = serde_json::from_value(serde_json::json!({ "kind": "cmd" })).unwrap();
        assert_eq!(k, ShellKind::Cmd);
    }
}
