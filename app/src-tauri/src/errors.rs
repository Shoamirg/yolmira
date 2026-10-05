//! Errors the UI shows: always `AmiriError` (what, where, why, next steps; §14).

use amiri_core::AmiriError;
use amiri_ipc::ClientError;

const WHERE: &str = "ui";

pub fn client_error(what: &str, e: ClientError) -> AmiriError {
    let base = AmiriError::new(what, WHERE, e.to_string());
    match e {
        ClientError::Rpc(r) => match r.data {
            Some(inner) => *inner,
            None => base.next_step("Run `amiri doctor`."),
        },
        ClientError::NotRunning => base.next_step("Start it with the banner's Start button."),
        ClientError::Impostor(_) => base
            .next_step("Another process owns AMIRI's pipe. Nothing was sent to it.")
            .next_step("Run `amiri doctor`."),
        // The daemon went away or stopped answering mid-call: the UI reconnects.
        ClientError::Closed | ClientError::Timeout => {
            base.next_step("Wait for the reconnect, or start the daemon from the banner.")
        }
        ClientError::Io(_) | ClientError::Protocol(_) => base.next_step("Run `amiri doctor`."),
    }
}

pub fn not_connected(what: &str) -> AmiriError {
    AmiriError::new(what, WHERE, "the UI is not connected to the AMIRI daemon")
        .next_step("Wait for the reconnect, or start the daemon from the banner.")
}

pub fn invalid(what: &str, why: impl Into<String>) -> AmiriError {
    AmiriError::new(what, WHERE, why).next_step("This is a UI bug; please report it.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use amiri_ipc::protocol::RpcError;

    #[test]
    fn every_ui_error_says_what_to_do_next() {
        let cases = [
            client_error("x", ClientError::NotRunning),
            client_error("x", ClientError::Closed),
            client_error("x", ClientError::Timeout),
            client_error("x", ClientError::Protocol("p".into())),
            client_error("x", ClientError::Impostor("pid 1".into())),
            client_error("x", ClientError::Rpc(RpcError::new(-32602, "bad"))),
            not_connected("x"),
            invalid("x", "y"),
        ];
        for e in cases {
            assert!(e.is_actionable(), "{e}");
        }
    }

    #[test]
    fn daemon_errors_pass_through_unchanged() {
        let inner = AmiriError::new("Create a shell", "daemon", "pwsh missing")
            .next_step("Install PowerShell 7.");
        let e = client_error("x", ClientError::Rpc(RpcError::amiri(inner.clone())));
        assert_eq!(e, inner);
    }
}
