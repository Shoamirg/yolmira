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

/// The environment every child of `amirid` gets (and `amirid` itself when launched).
pub fn child_env() -> Vec<(String, String)> {
    let mut vars: Vec<(String, String)> = PASS_THROUGH
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    let root = match windows_dir() {
        Ok(dir) => {
            vars.push(("SystemRoot".into(), dir.clone()));
            vars.push(("windir".into(), dir.clone()));
            Some(dir)
        }
        Err(e) => {
            tracing::warn!(error = %e, "GetSystemWindowsDirectoryW failed; SystemRoot omitted");
            None
        }
    };
    // Some machines lack the registry value (this dev machine does); the system shell
    // under the Windows directory is the documented default.
    let comspec = read_expanded(HKEY_LOCAL_MACHINE, MACHINE_ENV, "ComSpec", &vars)
        .or_else(|| root.map(|r| format!(r"{r}\System32\cmd.exe")));
    if let Some(c) = comspec {
        vars.push(("ComSpec".into(), c));
    }
    vars.push(("PATH".into(), path_from(&vars)));
    for name in ["TEMP", "TMP"] {
        if let Some(v) = read_expanded(HKEY_CURRENT_USER, USER_ENV, name, &vars) {
            vars.push((name.into(), v));
        }
    }
    vars
}

/// Machine PATH + user PATH from the registry, expanded against the allowlisted
/// variables. Not the caller's PATH.
pub fn registry_path() -> String {
    let base: Vec<(String, String)> = child_env()
        .into_iter()
        .filter(|(k, _)| k != "PATH")
        .collect();
    path_from(&base)
}

fn path_from(vars: &[(String, String)]) -> String {
    [
        (HKEY_LOCAL_MACHINE, MACHINE_ENV),
        (HKEY_CURRENT_USER, USER_ENV),
    ]
    .into_iter()
    .filter_map(|(root, key)| read_expanded(root, key, "Path", vars))
    .map(|p| p.trim_matches(';').to_string())
    .filter(|p| !p.is_empty())
    .collect::<Vec<_>>()
    .join(";")
}

/// A registry value expanded against `vars`; `None` if absent. Read failures are logged,
/// not silently treated as absent.
fn read_expanded(root: HKEY, key: &str, name: &str, vars: &[(String, String)]) -> Option<String> {
    match registry_value(root, key, name) {
        Ok(v) => v.map(|raw| expand_with(&raw, vars)),
        Err(e) => {
            tracing::warn!(key, name, error = %e, "registry read failed; value omitted from child environment");
            None
        }
    }
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

    #[test]
    fn expansion_uses_only_the_given_variables() {
        let vars = vec![("SystemRoot".to_string(), r"C:\Windows".to_string())];
        assert_eq!(
            expand_with(r"%systemroot%\x;%NOPE%;100%", &vars),
            r"C:\Windows\x;%NOPE%;100%"
        );
    }
}
