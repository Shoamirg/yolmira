//! Command-line arguments: the same `--instance` / `--home` as the CLI, so tests can
//! point a UI at an isolated daemon.

use std::path::PathBuf;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UiArgs {
    pub instance: Option<String>,
    pub home: Option<PathBuf>,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<UiArgs, String> {
    let mut out = UiArgs::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--instance" => {
                out.instance = Some(it.next().ok_or("--instance needs a name")?);
            }
            "--home" => {
                out.home = Some(it.next().ok_or("--home needs a folder")?.into());
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_arguments_means_the_users_daemon() {
        assert_eq!(parse(strings(&[])), Ok(UiArgs::default()));
    }

    #[test]
    fn instance_and_home_are_read() {
        let a = parse(strings(&["--instance", "e2e-1", "--home", r"C:\t\h"])).unwrap();
        assert_eq!(a.instance.as_deref(), Some("e2e-1"));
        assert_eq!(a.home, Some(PathBuf::from(r"C:\t\h")));
    }

    #[test]
    fn missing_values_and_unknown_arguments_are_errors() {
        assert!(parse(strings(&["--instance"])).is_err());
        assert!(parse(strings(&["--home"])).is_err());
        assert!(parse(strings(&["--stop-daemon"])).is_err());
    }
}
