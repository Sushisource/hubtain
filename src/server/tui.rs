use crate::tui::{TermMsg, TuiLogger};
use log::LevelFilter;
use std::{
    collections::VecDeque,
    sync::mpsc::Receiver,
    sync::mpsc::{sync_channel, SyncSender},
};
use tui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListState, Text},
    Terminal,
};

pub struct ServerTui {
    rx: Receiver<TermMsg>,
    logs: VecDeque<String>,
    log_state: ListState,
    clients: VecDeque<String>,
    clients_state: ListState,
}

const LOG_SCROLLBACK: usize = 1000;

impl ServerTui {
    pub fn start() -> SyncSender<TermMsg> {
        let (tx, rx) = sync_channel::<TermMsg>(100);
        let tui = ServerTui::new(rx);
        let txc = tx.clone();
        log::set_logger(Box::leak(Box::new(TuiLogger::new(txc))))
            .map(|()| log::set_max_level(LevelFilter::Debug))
            .expect("Logger couldn't init");

        std::thread::spawn(|| tui.render());
        tx
    }

    fn new(rx: Receiver<TermMsg>) -> Self {
        ServerTui {
            rx,
            logs: VecDeque::with_capacity(LOG_SCROLLBACK),
            log_state: ListState::default(),
            clients: VecDeque::with_capacity(100),
            clients_state: ListState::default(),
        }
    }

    fn render(mut self) -> std::io::Result<()> {
        // Terminal initialization
        let stdout = std::io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.hide_cursor()?;

        loop {
            match self.rx.recv() {
                Ok(TermMsg::Log(s)) => {
                    self.logs.push_back(s);
                    if self.logs.len() > LOG_SCROLLBACK {
                        self.logs.pop_front();
                    }
                }
                Ok(TermMsg::ClientRequest(c)) => {
                    self.clients.push_back(c);
                }
                Ok(TermMsg::Quit) => break,
                Err(e) => {
                    // TODO: This won't display.
                    error!("Problem receiving in UI loop: {:?}", e)
                }
            };

            terminal.draw(|mut f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(80), Constraint::Percentage(20)].as_ref())
                    .split(f.size());

                let logblock = Block::default().title("Log").borders(Borders::ALL);
                f.render_widget(logblock, chunks[0]);
                let client_block = Block::default().title("Clients").borders(Borders::ALL);
                f.render_widget(client_block, chunks[1]);

                let style = Style::default().fg(Color::White).bg(Color::Black);
                let items = self.logs.iter().map(|s| Text::styled(s.to_string(), style));
                let items = List::new(items)
                    .block(logblock)
                    .style(style)
                    .highlight_style(style.fg(Color::LightGreen).modifier(Modifier::BOLD))
                    .highlight_symbol(">");
                f.render_stateful_widget(items, chunks[0], &mut self.log_state);

                let items = self
                    .clients
                    .iter()
                    .map(|s| Text::styled(s.to_string(), style));
                let items = List::new(items)
                    .block(client_block)
                    .style(style)
                    .highlight_style(style.fg(Color::LightGreen).modifier(Modifier::BOLD))
                    .highlight_symbol(">");
                f.render_stateful_widget(items, chunks[1], &mut self.clients_state);
            })?;
        }

        Ok(())
    }
}
