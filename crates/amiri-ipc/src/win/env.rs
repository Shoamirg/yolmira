//! The allowlisted child environment (ARCHITECTURE §9.6, decisions "Child environment"):
//! fresh Windows values plus standard system variables; nothing else is inherited.

use super::{from_wide, wide};
use std::io;
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ,
    RRF_RT_REG_SZ,
};
use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;

/// Identity and system-layout variables taken from the current process. `SystemRoot`,
/// `windir` and `ComSpec` are not in this list: they come from Windows itself, so a
/// launcher can't redirect every future shell by setting them.
/// Known gap (Slice B): the profile folders still come from the process environment;
/// they should come from `SHGetKnownFolderPath`.
pub const PASS_THROUGH: &[&str] = &[
    "SystemDrive",
    "PATHEXT",
    "OS",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "USERNAME",
    "USERDOMAIN",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];

const MACHINE_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const USER_ENV: &str = "Environment";
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_MORE_DATA: u32 = 234;

/// Raw inputs the child environment is built from. [`EnvSource::system`] reads this
/// machine; tests build one by hand to simulate damaged machines (missing `ComSpec`,
/// PATH without Windows PowerShell), so the handling stays tested on healthy ones.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvSource {
    /// `PASS_THROUGH` values from the current process.
    pub process: Vec<(String, String)>,
    /// `GetSystemWindowsDirectoryW`.
    pub windows_dir: Option<String>,
    /// Raw (unexpanded) machine environment values: `ComSpec`, `Path`.
    pub machine: Vec<(String, String)>,
    /// Raw (unexpanded) user environment values: `Path`, `TEMP`, `TMP`.
    pub user: Vec<(String, String)>,
}

impl EnvSource {
    pub fn system() -> Self {
        let process = PASS_THROUGH
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
            .collect();
        let windows_dir = windows_dir()
            .map_err(|e| tracing::warn!(error = %e, "GetSystemWindowsDirectoryW failed; SystemRoot omitted"))
            .ok();
        let read = |root, key, names: &[&str]| -> Vec<(String, String)> {
            names
                .iter()
                .filter_map(|n| read_raw(root, key, n).map(|v| (n.to_string(), v)))
                .collect()
        };
        Self {
            process,
            windows_dir,
            machine: read(HKEY_LOCAL_MACHINE, MACHINE_ENV, &["ComSpec", "Path"]),
            user: read(HKEY_CURRENT_USER, USER_ENV, &["Path", "TEMP", "TMP"]),
        }
    }

    pub fn machine_value(&self, name: &str) -> Option<&str> {
        lookup(&self.machine, name)
    }

    fn user_value(&self, name: &str) -> Option<&str> {
        lookup(&self.user, name)
    }
}

fn lookup<'a>(vars: &'a [(String, String)], name: &str) -> Option<&'a str> {
    vars.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// The environment every child of `amirid` gets (and `amirid` itself when launched).
pub fn child_env() -> Vec<(String, String)> {
    build_child_env(&EnvSource::system())
}

/// Pure: the allowlisted environment for the given inputs.
pub fn build_child_env(src: &EnvSource) -> Vec<(String, String)> {
    let mut vars = src.process.clone();
    if let Some(dir) = &src.windows_dir {
        vars.push(("SystemRoot".into(), dir.clone()));
        vars.push(("windir".into(), dir.clone()));
    }
    // Some machines lack the registry value (this dev machine did); the system shell
    // under the Windows directory is the documented default.
    let comspec = src
        .machine_value("ComSpec")
        .map(|raw| expand_with(raw, &vars))
        .or_else(|| src.windows_dir.as_deref().map(system_cmd));
    if let Some(c) = comspec {
        vars.push(("ComSpec".into(), c));
    }
    let path = [src.machine_value("Path"), src.user_value("Path")]
        .into_iter()
        .flatten()
        .map(|raw| expand_with(raw, &vars).trim_matches(';').to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(";");
    vars.push(("PATH".into(), path));
    for name in ["TEMP", "TMP"] {
        if let Some(v) = src.user_value(name) {
            vars.push((name.into(), expand_with(v, &vars)));
        }
    }
    vars
}

/// Machine PATH + user PATH from the registry, expanded against the allowlisted
/// variables. Not the caller's PATH.
pub fn registry_path() -> String {
    build_child_env(&EnvSource::system())
        .into_iter()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v)
        .unwrap_or_default()
}

/// `cmd.exe` under the Windows directory: used when `ComSpec` is missing.
pub fn system_cmd(windows_dir: &str) -> String {
    format!(r"{windows_dir}\System32\cmd.exe")
}

/// Windows PowerShell 5.1 by full path. Spawners use this, never a bare `powershell`
/// (S0: some machines' PATH lacks it).
pub fn windows_powershell(windows_dir: &str) -> String {
    format!(r"{windows_dir}\System32\WindowsPowerShell\v1.0\powershell.exe")
}

/// A registry value, unexpanded; `None` if absent. Read failures are logged.
fn read_raw(root: HKEY, key: &str, name: &str) -> Option<String> {
    registry_value(root, key, name)
        .map_err(|e| tracing::warn!(key, name, error = %e, "registry read failed; value omitted"))
        .ok()
        .flatten()
}

/// `%NAME%` expansion using only `vars` (case-insensitive); unknown names stay as written.
pub fn expand_with(s: &str, vars: &[(String, String)]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match vars.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
                    Some((_, v)) if !name.is_empty() => out.push_str(v),
                    _ => out.push_str(&rest[start..start + end + 2]),
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn windows_dir() -> io::Result<String> {
    let mut buf = vec![0u16; 260];
    // SAFETY: `buf` holds `buf.len()` UTF-16 units.
    let n = unsafe { GetSystemWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 || n as usize >= buf.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(from_wide(&buf))
}

/// A REG_SZ / REG_EXPAND_SZ value, unexpanded; `Ok(None)` if absent. Retries if the
/// value grows between the size query and the read.
fn registry_value(root: HKEY, key: &str, name: &str) -> io::Result<Option<String>> {
    let (k, n) = (wide(key), wide(name));
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    for _ in 0..3 {
        let mut len = 0u32;
        // SAFETY: size query with null data; `len` receives the byte size.
        let rc = unsafe {
            RegGetValueW(
                root,
                k.as_ptr(),
                n.as_ptr(),
                flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut len,
            )
        };
        match rc {
            0 => {}
            ERROR_FILE_NOT_FOUND => return Ok(None),
            other => return Err(io::Error::from_raw_os_error(other as i32)),
        }
        let mut buf = vec![0u16; (len as usize).div_ceil(2) + 1];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: `buf` holds `len` bytes.
        let rc = unsafe {
            RegGetValueW(
                root,
                k.as_ptr(),
                n.as_ptr(),
                flags,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        match rc {
            0 => return Ok(Some(from_wide(&buf))),
            ERROR_MORE_DATA => continue,
            ERROR_FILE_NOT_FOUND => return Ok(None),
            other => return Err(io::Error::from_raw_os_error(other as i32)),
        }
    }
    Err(io::Error::other(format!(
        "registry value {key}/{name} kept growing while being read"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_env_drops_unlisted_variables_and_uses_registry_path() {
        // This test's own process may carry anything; none of it may leak.
        std::env::set_var("AMIRI_TEST_SECRET", "canary");
        let env = child_env();
        assert!(env.iter().all(|(k, _)| k != "AMIRI_TEST_SECRET"));
        let path = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(path, registry_path());
        assert!(path.to_lowercase().contains("system32"), "{path}");
        assert!(!path.contains('%'), "unexpanded variable in {path}");
    }

    #[test]
    fn system_variables_come_from_windows_not_the_caller() {
        // Not set_var: other tests in this binary read ComSpec concurrently.
        let env = child_env();
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let root = windows_dir().unwrap();
        assert_eq!(get("SystemRoot").as_deref(), Some(root.as_str()));
        assert_eq!(get("windir").as_deref(), Some(root.as_str()));
        let comspec = get("ComSpec").unwrap().to_lowercase();
        assert_eq!(
            comspec,
            format!(r"{}\system32\cmd.exe", root.to_lowercase())
        );
    }

    /// A healthy machine as seen through `EnvSource` (values are raw, like the registry's).
    fn healthy() -> EnvSource {
        EnvSource {
            process: vec![("USERPROFILE".into(), r"C:\Users\u".into())],
            windows_dir: Some(r"C:\Windows".into()),
            machine: vec![
                ("ComSpec".into(), r"%SystemRoot%\system32\cmd.exe".into()),
                (
                    "Path".into(),
                    r"%SystemRoot%\system32;%SystemRoot%\System32\WindowsPowerShell\v1.0\".into(),
                ),
            ],
            user: vec![
                ("Path".into(), r"%USERPROFILE%\bin".into()),
                ("TEMP".into(), r"%USERPROFILE%\AppData\Local\Temp".into()),
            ],
        }
    }

    fn get(env: &[(String, String)], k: &str) -> Option<String> {
        env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn healthy_machine_values_are_expanded_against_the_allowlist() {
        let env = build_child_env(&healthy());
        assert_eq!(
            get(&env, "ComSpec").as_deref(),
            Some(r"C:\Windows\system32\cmd.exe")
        );
        assert_eq!(
            get(&env, "PATH").as_deref(),
            Some(r"C:\Windows\system32;C:\Windows\System32\WindowsPowerShell\v1.0\;C:\Users\u\bin")
        );
        assert_eq!(
            get(&env, "TEMP").as_deref(),
            Some(r"C:\Users\u\AppData\Local\Temp")
        );
    }

    #[test]
    fn missing_comspec_falls_back_to_the_system_shell() {
        let mut src = healthy();
        src.machine.retain(|(k, _)| k != "ComSpec");
        let env = build_child_env(&src);
        assert_eq!(
            get(&env, "ComSpec").as_deref(),
            Some(r"C:\Windows\System32\cmd.exe")
        );
    }

    #[test]
    fn path_without_windows_powershell_is_passed_as_is_and_powershell_resolves_by_full_path() {
        let mut src = healthy();
        src.machine = vec![("Path".into(), r"%SystemRoot%\system32".into())];
        let env = build_child_env(&src);
        let path = get(&env, "PATH").unwrap();
        assert!(
            !path.to_lowercase().contains("windowspowershell"),
            "AMIRI doesn't edit PATH: {path}"
        );
        // Spawners don't depend on PATH for PowerShell.
        assert_eq!(
            windows_powershell(src.windows_dir.as_deref().unwrap()),
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
        );
    }

    #[test]
    fn windows_powershell_exists_on_this_machine() {
        let ps = windows_powershell(&windows_dir().unwrap());
        assert!(std::path::Path::new(&ps).is_file(), "{ps}");
    }

    #[test]
    fn nothing_from_windows_means_no_system_variables_but_no_panic() {
        let env = build_child_env(&EnvSource::default());
        assert_eq!(get(&env, "ComSpec"), None);
        assert_eq!(get(&env, "PATH").as_deref(), Some(""));
    }

    #[test]
    fn expansion_uses_only_the_given_variables() {
        let vars = vec![("SystemRoot".to_string(), r"C:\Windows".to_string())];
        assert_eq!(
            expand_with(r"%systemroot%\x;%NOPE%;100%", &vars),
            r"C:\Windows\x;%NOPE%;100%"
        );
    }
}
