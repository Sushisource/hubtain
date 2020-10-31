#[cfg(not(test))]
use crate::server::SHUTDOWN_FLAG;
#[cfg(not(test))]
use std::sync::atomic::Ordering;

use crate::{models::ClientId, server::ClientApprover};
use anyhow::{anyhow, Error};
use crossterm::event::{self, Event};
use futures::{
    channel::mpsc::{channel, Sender},
    executor::block_on,
    SinkExt, StreamExt,
};
use log::{LevelFilter, Log, Metadata, Record};
use std::{io::Write, sync::Once, time::Duration};

static LOGGER_INITTED: Once = Once::new();

/// Initializes a console logger for non-tui mode
pub fn init_console_logger() {
    LOGGER_INITTED.call_once(|| {
        env_logger::builder()
            .filter_level(LevelFilter::Info)
            .format(|buf, record| {
                writeln!(
                    buf,
                    "[{} {}] {}",
                    buf.default_level_style(record.level())
                        .value(record.level()),
                    buf.timestamp_seconds(),
                    record.args()
                )
            })
            .init();
    })
}

#[derive(Debug)]
pub enum TermMsg {
    Log(String),
    ClientRequest(ClientId, Sender<bool>),
    Input(Event),
}

#[derive(Constructor)]
pub struct TuiLogger {
    tx: Sender<TermMsg>,
}

impl Log for TuiLogger {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        self.tx
            .clone()
            .try_send(TermMsg::Log(record.args().to_string()))
            .expect("Logger can't write")
    }

    fn flush(&self) {}
}

/// Approver for use with the TUI
#[derive(Constructor)]
pub struct TuiApprover {
    tx: Sender<TermMsg>,
}

#[async_trait::async_trait]
impl ClientApprover for TuiApprover {
    async fn submit(&self, client_id: ClientId) -> Result<bool, Error> {
        let (tx, mut rx) = channel(8);
        self.tx
            .clone()
            .send(TermMsg::ClientRequest(client_id, tx))
            .await?;
        rx.next()
            .await
            .ok_or_else(|| anyhow!("No reply to client request"))
    }
}

/// Can be run in its own thread to forward crossterm events to the TUI
pub fn event_forwarder(mut tx: Sender<TermMsg>) -> crossterm::Result<()> {
    loop {
        #[cfg(not(test))]
        if SHUTDOWN_FLAG.load(Ordering::SeqCst) {
            break Ok(());
        }
        if event::poll(Duration::from_millis(100))? {
            block_on(tx.send(TermMsg::Input(event::read()?)))
                .expect("Must be able to forward event");
        }
    }
}
