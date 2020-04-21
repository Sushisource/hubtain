use crate::{
    server::SHUTDOWN_FLAG,
    tui::{event_forwarder, TermMsg, TuiLogger},
};
use anyhow::Error;
use crossterm::{
    event::{Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use log::LevelFilter;
use std::{
    collections::VecDeque,
    io::{self, stdout, Write},
    sync::{
        atomic::Ordering,
        mpsc::{sync_channel, Receiver, Sender, SyncSender},
    },
    thread::JoinHandle,
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

pub struct TuiHandle {
    pub tx: SyncSender<TermMsg>,
    render_thread: JoinHandle<io::Result<()>>,
    event_thread: JoinHandle<crossterm::Result<()>>,
}

impl TuiHandle {
    pub fn join(self) {
        // In case it hasn't been already.
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

impl ServerTui {
    pub fn start() -> Result<TuiHandle, Error> {
        let (tx, rx) = sync_channel::<TermMsg>(100);
        let tui = ServerTui::new(rx);
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

        macro_rules! quit {
            () => {
                SHUTDOWN_FLAG.store(true, Ordering::SeqCst);
                break;
            };
        }

        loop {
            match self.rx.recv() {
                Ok(TermMsg::Input(i)) => {
                    if let Event::Key(KeyEvent { code, modifiers }) = i {
                        match code {
                            KeyCode::Enter => {
                                // Client approved
                                self.clients_state
                                    .selected()
                                    .map(|i| self.clients.get(i).map(|(_, tx)| tx.send(true)));
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
                Ok(TermMsg::Log(s)) => {
                    self.logs.push_back(s);
                    if self.logs.len() > LOG_SCROLLBACK {
                        self.logs.pop_front();
                    }
                }
                Ok(TermMsg::ClientRequest(c, reply)) => {
                    self.clients.push_back((c, reply));
                }
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

            if SHUTDOWN_FLAG.load(Ordering::SeqCst) {
                break;
            }
        }

        terminal.clear()?;

        Ok(())
    }
}
