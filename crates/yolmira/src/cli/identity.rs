//! `yolmira identity show | import <file>` (team mode, decisions.md 2026-10-09). Works
//! on the files directly; no daemon needed. An import takes effect when the daemon
//! starts next.

use super::exit;
use super::output::Out;
use crate::identity::{self, Identity, Imported};
use serde_json::json;
use std::path::PathBuf;
use yolmira_core::YolmiraError;
use yolmira_ipc::Instance;

const WHERE: &str = "cli identity";

pub fn show(out: &Out, instance: &Instance) -> Result<u8, YolmiraError> {
    let id = Identity::load_or_create(&instance.config_dir(), &instance.home)
        .map_err(|e| YolmiraError::new("Show the identity", WHERE, e))?;
    out.value(
        || {
            format!(
                "person  {} ({})\ninstall {}\nperson file: {}",
                id.person.person_id,
                id.person.display_name,
                id.install.install_id,
                instance.config_dir().join(identity::PERSON_FILE).display()
            )
        },
        json!({
            "person_id": id.person.person_id,
            "display_name": id.person.display_name,
            "install_id": id.install.install_id,
        }),
    );
    Ok(exit::OK)
}

pub fn import(
    out: &Out,
    instance: &Instance,
    file: PathBuf,
    replace: bool,
) -> Result<u8, YolmiraError> {
    const WHAT: &str = "Import the identity";
    match identity::import(&instance.config_dir(), &file, replace)
        .map_err(|e| YolmiraError::new(WHAT, WHERE, e))?
    {
        Imported::Done(p) => {
            out.value(
                || {
                    format!(
                        "this machine now acts for {} ({}); restart the daemon to apply",
                        p.display_name, p.person_id
                    )
                },
                json!({ "imported": p.person_id, "restart_needed": true }),
            );
            Ok(exit::OK)
        }
        Imported::Conflict { current, incoming } => Err(YolmiraError::new(
            WHAT,
            WHERE,
            format!(
                "this machine already acts for {} ({}), not {} ({})",
                current.display_name, current.person_id, incoming.display_name, incoming.person_id
            ),
        )
        .next_step(
            "Pass --replace to switch persons; events already recorded keep the old person.",
        )),
    }
}
