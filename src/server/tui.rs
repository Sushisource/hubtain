#[cfg(not(test))]
use crate::server::SHUTDOWN_FLAG;
#[cfg(not(test))]
use std::sync::atomic::Ordering;

use crate::{
    models::ClientId,
    tui::{event_forwarder, TermMsg, TuiLogger},
};
use anyhow::Error;
use atty::Stream;
use crossterm::{
    event::{Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{
    channel::mpsc::{channel, Receiver, Sender},
    executor::block_on,
    StreamExt,
};
use log::LevelFilter;
use std::{
    collections::VecDeque,
    io::{self, stdout, Write},
    thread::JoinHandle,
};
use tui::text::Span;
use tui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Text,
    widgets::{Block, Borders, List, ListItem, ListState},
    Terminal,
};

pub struct ServerTui {
    rx: Receiver<TermMsg>,
    logs: VecDeque<String>,
    log_state: ListState,
    clients: VecDeque<(ClientId, Sender<bool>)>,
    clients_state: ListState,
    name: String,
    file_name: String,
}

#[must_use]
pub struct TuiHandle {
    pub tx: Sender<TermMsg>,
    render_thread: JoinHandle<io::Result<()>>,
    event_thread: JoinHandle<crossterm::Result<()>>,
}

impl TuiHandle {
    pub fn join(self) {
        // In case it hasn't been already.
        #[cfg(not(test))]
        SHUTDOWN_FLAG.store(true, Ordering::SeqCst);
        self.render_thread
            .join()
            .expect("Render thread exited unclean")
            .unwrap();
        self.event_thread
            .join()
            .expect("Event thread exited unclean")
            .unwrap();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        // Can potentially print a nice little summary here after returning to normal buffer
    }
}

const LOG_SCROLLBACK: usize = 1000;

// TODO: Use part of client approver line as percent complete
impl ServerTui {
    pub fn is_interactive() -> bool {
        atty::is(Stream::Stdout)
    }

    pub fn start(name: String, file_name: String) -> Result<TuiHandle, Error> {
        let (tx, rx) = channel::<TermMsg>(100);
        let tui = ServerTui::new(name, rx, file_name);
        let txc = tx.clone();
        log::set_logger(Box::leak(Box::new(TuiLogger::new(txc))))
            .map(|()| log::set_max_level(LevelFilter::Debug))
            .expect("Logger couldn't init");

        crossterm::terminal::enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen)?;
        let render_thread = std::thread::spawn(|| tui.render());
        let txc = tx.clone();
        let event_thread = std::thread::spawn(|| event_forwarder(txc));
        Ok(TuiHandle {
            tx,
            render_thread,
            event_thread,
        })
    }

    fn new(name: String, rx: Receiver<TermMsg>, file_name: String) -> Self {
        ServerTui {
            rx,
            logs: VecDeque::with_capacity(LOG_SCROLLBACK),
            log_state: ListState::default(),
            clients: VecDeque::new(),
            clients_state: ListState::default(),
            name,
            file_name,
        }
    }

    fn render(mut self) -> std::io::Result<()> {
        // Terminal initialization
        let stdout = std::io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.autoresize()?;
        terminal.hide_cursor()?;

        macro_rules! quit {
            () => {
                #[cfg(not(test))]
                SHUTDOWN_FLAG.store(true, Ordering::SeqCst);
                break;
            };
        }

        loop {
            match block_on(self.rx.next()) {
                Some(TermMsg::Input(i)) => {
                    if let Event::Key(KeyEvent { code, modifiers }) = i {
                        match code {
                            KeyCode::Enter => {
                                // Client approved, remove it
                                self.clients_state.selected().map(|i| {
                                    self.clients.remove(i).map(|(name, mut tx)| {
                                        // TODO: This is a weird place to log this
                                        info!("Client downloading: {}", name);
                                        tx.try_send(true)
                                    })
                                });
                            }
                            KeyCode::Char('n') => {
                                // Client denied, remove it
                                self.clients_state.selected().map(|i| {
                                    self.clients.remove(i).map(|(_, mut tx)| tx.try_send(false))
                                });
                            }
                            // TODO: Arrow keys broken in raw mode
                            KeyCode::Char('k') => {
                                let curselect = self.clients_state.selected();
                                self.clients_state
                                    .select(curselect.map(|x| x.saturating_sub(1)));
                                if curselect.is_none() {
                                    self.clients_state.select(Some(0));
                                }
                            }
                            KeyCode::Char('j') => {
                                let curselect = self.clients_state.selected();
                                self.clients_state
                                    .select(curselect.map(|x| {
                                        (x + 1).min(self.clients.len().saturating_sub(1))
                                    }));
                                if curselect.is_none() {
                                    self.clients_state.select(Some(0));
                                }
                            }
                            // Have to handle ctrl-c because of terminal raw mode
                            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                                quit!();
                            }
                            KeyCode::Esc | KeyCode::Char('q') => {
                                quit!();
                            }
                            _ => (),
                        }
                    }
                }
                Some(TermMsg::Log(s)) => {
                    self.logs.push_back(s);
                    if self.logs.len() > LOG_SCROLLBACK {
                        self.logs.pop_front();
                    }
                }
                Some(TermMsg::ClientRequest(c, reply)) => {
                    self.clients.push_back((c, reply));
                }
                None => panic!("Ui loop channel died"),
            };

            terminal.draw(|f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(80), Constraint::Percentage(20)].as_ref())
                    .split(f.size());

                let style = Style::default().fg(Color::White);
                let logs: Vec<_> = self
                    .logs
                    .iter()
                    .map(|s| ListItem::new(Text::styled(s.to_string(), style)))
                    .collect();
                let title = format!(
                    " Server name: {} - Serving: {} ",
                    &self.name, &self.file_name
                );
                let logs = List::new(logs)
                    .block(Block::default().borders(Borders::ALL).title(title))
                    .style(style);
                f.render_stateful_widget(logs, chunks[0], &mut self.log_state);

                let items: Vec<_> = self
                    .clients
                    .iter()
                    .map(|s| {
                        let mut client_id = s.0.to_string();
                        let chop_id_at = chunks[1].width - 5;
                        if chop_id_at > 5 {
                            client_id.truncate(chop_id_at as usize);
                        }
                        ListItem::new(Span::styled(client_id, style))
                    })
                    .collect();
                let items = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Client requests (enter to approve, 'n' to deny)"),
                    )
                    .style(style)
                    .highlight_style(style.fg(Color::LightGreen).add_modifier(Modifier::BOLD))
                    .highlight_symbol(">");
                f.render_stateful_widget(items, chunks[1], &mut self.clients_state);
            })?;

            #[cfg(not(test))]
            if SHUTDOWN_FLAG.load(Ordering::SeqCst) {
                break;
            }
        }

        terminal.clear()?;

        Ok(())
    }
}

impl Drop for ServerTui {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
