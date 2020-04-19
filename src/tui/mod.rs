use crate::server::{ClientApprover, ClientId, SHUTDOWN_FLAG};
use anyhow::Error;
use crossterm::event::{self, Event};
use log::{Log, Metadata, Record};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Sender, SyncSender};
use std::time::Duration;

pub enum TermMsg {
    Quit,
    Log(String),
    ClientRequest(String, Sender<bool>),
    Input(Event),
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
        // Chop identifier down to fit. 16 bytes still ought to be plenty unique
        let pubkey_mnemonic = mnemonic::to_string(&client_id[..16]);
        // TODO: Unclear if async_std is actually handling this or if block_in_place needs
        //  to be stabilized
        let (tx, rx) = channel();
        self.tx
            .send(TermMsg::ClientRequest(pubkey_mnemonic, tx))
            .expect("Couldn't send client");
        Ok(rx.recv()?)
    }
}

/// Can be run in it's own thread to forward crossterm events to the TUI
pub fn event_forwarder(tx: SyncSender<TermMsg>) -> crossterm::Result<()> {
    loop {
        if SHUTDOWN_FLAG.load(Ordering::SeqCst) {
            break Ok(());
        }
        if event::poll(Duration::from_millis(100))? {
            tx.send(TermMsg::Input(event::read()?))
                .expect("Must be able to forward result");
        }
    }
}
