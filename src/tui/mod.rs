use crate::server::{ClientApprover, ClientId};
use anyhow::Error;
use async_std::io;
use log::{Log, Metadata, Record};
use std::sync::mpsc::SyncSender;

pub enum TermMsg {
    Quit,
    Log(String),
    ClientRequest(String),
}

#[derive(Constructor)]
pub struct TuiLogger {
    tx: SyncSender<TermMsg>,
}

impl Log for TuiLogger {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        self.tx
            .send(TermMsg::Log(record.args().to_string()))
            .expect("Logger can't write");
    }

    fn flush(&self) {}
}

/// Approver for use with the TUI
#[derive(Constructor)]
pub struct TuiApprover {
    tx: SyncSender<TermMsg>,
}

#[async_trait::async_trait]
impl ClientApprover for TuiApprover {
    async fn submit(&self, client_id: &ClientId) -> Result<bool, Error> {
        let pubkey_mnemonic = mnemonic::to_string(client_id);
        // TODO: This blocks and is a bad boi in a future
        self.tx.send(TermMsg::ClientRequest(pubkey_mnemonic))?;
        let mut input = String::new();
        io::stdin().read_line(&mut input).await?;
        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes") {
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
