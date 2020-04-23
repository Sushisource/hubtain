use crate::models::ClientId;
use crate::server::{ClientApprover, SHUTDOWN_FLAG};
use anyhow::Error;
use crossterm::event::{self, Event};
use log::{Log, Metadata, Record};
use std::{
    sync::{
        atomic::Ordering,
        mpsc::{channel, Sender, SyncSender},
    },
    time::Duration,
};

#[derive(Debug)]
pub enum TermMsg {
    Log(String),
    ClientRequest(ClientId, Sender<bool>),
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
    async fn submit(&self, client_id: ClientId) -> Result<bool, Error> {
        // TODO: Unclear if async_std is actually handling this or if block_in_place needs
        //  to be stabilized
        let (tx, rx) = channel();
        self.tx
            .send(TermMsg::ClientRequest(client_id, tx))
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
