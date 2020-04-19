use crate::server::SHUTDOWN_FLAG;
use crate::tui::{event_forwarder, TermMsg, TuiLogger};
use crossterm::event::{Event, KeyCode, KeyEvent};
use log::LevelFilter;
use std::sync::atomic::Ordering;
use std::{
    collections::VecDeque,
    sync::mpsc::{sync_channel, Receiver, Sender, SyncSender},
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
    clients: VecDeque<(String, Sender<bool>)>,
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
        let txc = tx.clone();
        std::thread::spawn(|| event_forwarder(txc));
        tx
    }

    fn new(rx: Receiver<TermMsg>) -> Self {
        ServerTui {
            rx,
            logs: VecDeque::with_capacity(LOG_SCROLLBACK),
            log_state: ListState::default(),
            clients: VecDeque::new(),
            clients_state: ListState::default(),
        }
    }

    fn render(mut self) -> std::io::Result<()> {
        // Terminal initialization
        let stdout = std::io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.autoresize()?;
        terminal.hide_cursor()?;

        loop {
            match self.rx.recv() {
                Ok(TermMsg::Input(i)) => {
                    if let Event::Key(KeyEvent { code, .. }) = i {
                        match code {
                            KeyCode::Enter => {
                                // Client approved
                                self.clients_state
                                    .selected()
                                    .map(|i| self.clients.get(i).map(|(_, tx)| tx.send(true)));
                            }
                            KeyCode::Up => {
                                let curselect = self.clients_state.selected();
                                self.clients_state.select(curselect.map(|x| {
                                    if x > 0 {
                                        x - 1
                                    } else {
                                        x
                                    }
                                }));
                                if curselect.is_none() {
                                    self.clients_state.select(Some(0));
                                }
                            }
                            KeyCode::Down => {
                                let curselect = self.clients_state.selected();
                                self.clients_state
                                    .select(curselect.map(|x| (x + 1).min(self.clients.len() - 1)));
                                if curselect.is_none() {
                                    self.clients_state.select(Some(0));
                                }
                            }
                            KeyCode::Esc => {
                                // Quit
                                SHUTDOWN_FLAG.store(true, Ordering::SeqCst);
                                break;
                            }
                            _ => (),
                        }
                    }
                }
                Ok(TermMsg::Log(s)) => {
                    self.logs.push_back(s);
                    if self.logs.len() > LOG_SCROLLBACK {
                        self.logs.pop_front();
                    }
                }
                Ok(TermMsg::ClientRequest(c, reply)) => {
                    self.clients.push_back((c, reply));
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
                    .map(|s| Text::styled(s.0.to_string(), style));
                let items = List::new(items)
                    .block(client_block)
                    .style(style)
                    .highlight_style(style.fg(Color::LightGreen).modifier(Modifier::BOLD))
                    .highlight_symbol(">");
                f.render_stateful_widget(items, chunks[1], &mut self.clients_state);
            })?;
        }

        terminal.clear()?;

        println!("Done");
        Ok(())
    }
}
