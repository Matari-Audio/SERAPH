use std::time::Duration;

use anyhow::Result;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use crossterm::{ExecutableCommand, QueueableCommand};
use futures_util::StreamExt;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs, Wrap};
use ratatui::{DefaultTerminal, Frame};
use ratatui_textarea::TextArea;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

// Derived from Grok Build's Apache-2.0 GrokNight palette.
const BG: Color = Color::Rgb(20, 20, 20);
const PANEL: Color = Color::Rgb(36, 36, 36);
const TEXT: Color = Color::Rgb(225, 225, 225);
const MUTED: Color = Color::Rgb(108, 108, 108);
const BLUE: Color = Color::Rgb(122, 162, 247);
const MAGENTA: Color = Color::Rgb(187, 154, 247);
const GREEN: Color = Color::Rgb(158, 206, 106);
const RED: Color = Color::Rgb(247, 118, 142);

#[derive(Debug)]
pub enum UiEvent {
    AccountChanged(Option<String>),
    Ready(bool),
    ModelChanged {
        name: String,
        efforts: Vec<String>,
        selected_effort: usize,
    },
    LoginStarted {
        login_id: String,
        auth_url: String,
    },
    LoginFinished {
        success: bool,
        error: Option<String>,
    },
    AssistantDelta(String),
    AssistantDone,
    Notice(String),
    Error(String),
}

#[derive(Debug)]
pub enum UiCommand {
    Login,
    CancelLogin {
        login_id: String,
    },
    Submit {
        text: String,
        effort: Option<String>,
    },
    Quit,
}

#[derive(Clone, Copy)]
enum Role {
    User,
    Assistant,
    System,
    Error,
}

struct Message {
    role: Role,
    text: String,
}

struct App {
    messages: Vec<Message>,
    composer: TextArea<'static>,
    account: Option<String>,
    model: String,
    efforts: Vec<String>,
    selected_effort: usize,
    login_id: Option<String>,
    auth_pending: bool,
    ready: bool,
    busy: bool,
    streaming: bool,
    show_help: bool,
}

impl App {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
            composer: new_composer(),
            account: None,
            model: "loading".into(),
            efforts: Vec::new(),
            selected_effort: 0,
            login_id: None,
            auth_pending: false,
            ready: false,
            busy: false,
            streaming: false,
            show_help: false,
        }
    }

    fn is_composer_empty(&self) -> bool {
        self.composer.lines().iter().all(String::is_empty)
    }

    fn apply(&mut self, event: UiEvent) {
        match event {
            UiEvent::AccountChanged(account) => self.account = account,
            UiEvent::Ready(ready) => self.ready = ready,
            UiEvent::ModelChanged {
                name,
                efforts,
                selected_effort,
            } => {
                self.model = name;
                self.efforts = efforts;
                self.selected_effort = selected_effort.min(self.efforts.len().saturating_sub(1));
            }
            UiEvent::LoginStarted { login_id, auth_url } => {
                self.auth_pending = true;
                self.login_id = Some(login_id);
                let text = match webbrowser::open(&auth_url) {
                    Ok(()) => format!("Finish signing in in your browser: {auth_url}"),
                    Err(error) => format!("Open this URL to sign in: {auth_url}\n{error}"),
                };
                self.messages.push(Message {
                    role: Role::System,
                    text,
                });
            }
            UiEvent::LoginFinished { success, error } => {
                self.auth_pending = false;
                self.login_id = None;
                self.messages.push(Message {
                    role: if success { Role::System } else { Role::Error },
                    text: error.unwrap_or_else(|| {
                        if success {
                            "Signed in.".into()
                        } else {
                            "Sign-in failed.".into()
                        }
                    }),
                });
            }
            UiEvent::AssistantDelta(delta) => {
                if self.streaming {
                    if let Some(message) = self.messages.last_mut() {
                        message.text.push_str(&delta);
                    }
                } else {
                    self.messages.push(Message {
                        role: Role::Assistant,
                        text: delta,
                    });
                    self.streaming = true;
                }
            }
            UiEvent::AssistantDone => {
                self.busy = false;
                self.streaming = false;
            }
            UiEvent::Notice(text) => self.messages.push(Message {
                role: Role::System,
                text,
            }),
            UiEvent::Error(text) => {
                self.busy = false;
                self.streaming = false;
                self.messages.push(Message {
                    role: Role::Error,
                    text,
                });
            }
        }
    }

    fn step_effort(&mut self, delta: isize) -> Option<String> {
        let last = self.efforts.len().checked_sub(1)?;
        let next = self.selected_effort.saturating_add_signed(delta).min(last);
        if next == self.selected_effort {
            return None;
        }
        self.selected_effort = next;
        self.efforts.get(next).cloned()
    }

    fn submit(&mut self, commands: &mpsc::Sender<UiCommand>) {
        if !self.ready || self.auth_pending || self.busy {
            self.messages.push(Message {
                role: Role::Error,
                text: if self.auth_pending {
                    "Finish or cancel sign-in before sending a message.".into()
                } else if self.busy {
                    "Wait for the current turn to finish.".into()
                } else {
                    "Sign in before sending a message.".into()
                },
            });
            return;
        }
        let text = self.composer.lines().join("\n");
        if text.trim().is_empty() {
            return;
        }
        let effort = self.efforts.get(self.selected_effort).cloned();
        match commands.try_send(UiCommand::Submit {
            text: text.clone(),
            effort,
        }) {
            Ok(()) => {
                self.messages.push(Message {
                    role: Role::User,
                    text,
                });
                self.busy = true;
                self.composer = new_composer();
                self.show_help = false;
            }
            Err(error) => self.messages.push(Message {
                role: Role::Error,
                text: format!("Could not submit: {error}"),
            }),
        }
    }

    fn handle_key(&mut self, key: KeyEvent, commands: &mpsc::Sender<UiCommand>) -> bool {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            let _ = commands.try_send(UiCommand::Quit);
            return false;
        }
        if key.code == KeyCode::Esc {
            if self.show_help {
                self.show_help = false;
            } else if let Some(login_id) = self.login_id.clone() {
                match commands.try_send(UiCommand::CancelLogin { login_id }) {
                    Ok(()) => self.login_id = None,
                    Err(error) => self.messages.push(Message {
                        role: Role::Error,
                        text: format!("Could not cancel sign-in: {error}"),
                    }),
                }
            }
            return true;
        }
        if self.is_composer_empty() {
            match key.code {
                KeyCode::Char('?') => {
                    self.show_help = !self.show_help;
                    return true;
                }
                KeyCode::Char('<') => {
                    self.step_effort(-1);
                    return true;
                }
                KeyCode::Char('>') => {
                    self.step_effort(1);
                    return true;
                }
                KeyCode::Char('l' | 'L')
                    if self.login_id.is_none() && !self.auth_pending && !self.busy =>
                {
                    match commands.try_send(UiCommand::Login) {
                        Ok(()) => self.auth_pending = true,
                        Err(error) => {
                            self.messages.push(Message {
                                role: Role::Error,
                                text: format!("Could not start sign-in: {error}"),
                            });
                        }
                    }
                    return true;
                }
                _ => {}
            }
        }
        if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::ALT) {
            self.composer.insert_newline();
        } else if key.code == KeyCode::Enter {
            self.submit(commands);
        } else {
            self.show_help = false;
            self.composer.input(key);
        }
        true
    }
}

pub async fn run(
    mut events: mpsc::Receiver<UiEvent>,
    commands: mpsc::Sender<UiCommand>,
) -> Result<()> {
    let mut terminal = ratatui::try_init()?;
    let mut guard = TerminalGuard(true);
    if let Err(error) = terminal.backend_mut().execute(EnableBracketedPaste) {
        let _ = ratatui::try_restore();
        guard.0 = false;
        return Err(error.into());
    }
    let result = run_loop(&mut terminal, &mut events, &commands).await;
    let paste_restore = terminal.backend_mut().execute(DisableBracketedPaste);
    let restore = ratatui::try_restore();
    guard.0 = false;
    result?;
    paste_restore?;
    restore?;
    Ok(())
}

struct TerminalGuard(bool);

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if !self.0 {
            return;
        }
        let mut output = std::io::stdout();
        let _ = output.execute(EndSynchronizedUpdate);
        let _ = output.execute(DisableBracketedPaste);
        let _ = ratatui::try_restore();
    }
}

async fn run_loop(
    terminal: &mut DefaultTerminal,
    events: &mut mpsc::Receiver<UiEvent>,
    commands: &mpsc::Sender<UiCommand>,
) -> Result<()> {
    let mut app = App::new();
    let mut input = EventStream::new();
    let mut events_open = true;
    let mut dirty = true;
    let mut next_frame = Instant::now();

    loop {
        if dirty && Instant::now() >= next_frame {
            draw(terminal, &app)?;
            dirty = false;
            next_frame = Instant::now() + FRAME_INTERVAL;
        }

        let frame_delay = sleep_until(next_frame);
        tokio::pin!(frame_delay);
        tokio::select! {
            terminal_event = input.next() => {
                let Some(terminal_event) = terminal_event else { break };
                match terminal_event? {
                    Event::Key(key) => {
                        if !app.handle_key(key, commands) {
                            break;
                        }
                        dirty = true;
                    }
                    Event::Paste(text) => {
                        app.composer.insert_str(text);
                        app.show_help = false;
                        dirty = true;
                    }
                    Event::Resize(_, _) => dirty = true,
                    _ => {}
                }
            }
            event = events.recv(), if events_open => {
                match event {
                    Some(event) => {
                        app.apply(event);
                        dirty = true;
                    }
                    None => events_open = false,
                }
            }
            () = &mut frame_delay, if dirty => {}
        }
    }
    Ok(())
}

fn draw(terminal: &mut DefaultTerminal, app: &App) -> Result<()> {
    terminal.backend_mut().queue(BeginSynchronizedUpdate)?;
    let drawn = terminal.draw(|frame| render(frame, app)).map(|_| ());
    let ended = terminal.backend_mut().execute(EndSynchronizedUpdate);
    drawn?;
    ended?;
    Ok(())
}

fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame
        .buffer_mut()
        .set_style(area, Style::default().bg(BG).fg(TEXT));
    let [tabs_area, transcript_area, composer_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(area);

    let tab = Line::from(vec![
        Span::styled(" ● ", Style::default().fg(GREEN).bg(PANEL)),
        Span::styled("Main ", Style::default().fg(TEXT).bg(PANEL).bold()),
    ]);
    frame.render_widget(
        Tabs::new(vec![tab])
            .select(0)
            .style(Style::default().bg(PANEL).fg(MUTED))
            .highlight_style(Style::default().fg(TEXT).bg(PANEL)),
        tabs_area,
    );

    if app.show_help {
        let help = Text::from(vec![
            Line::styled("SERAPH", Style::default().fg(MAGENTA).bold()),
            Line::from(""),
            Line::from("Enter    send"),
            Line::from("Alt+Enter      newline"),
            Line::from("< / >    reasoning effort"),
            Line::from("L              sign in / switch account"),
            Line::from("? / Esc  close help"),
            Line::from("Ctrl+C   quit"),
        ]);
        frame.render_widget(
            Paragraph::new(help)
                .block(Block::default().borders(Borders::ALL).title(" Help "))
                .style(Style::default().bg(BG).fg(TEXT)),
            transcript_area,
        );
    } else {
        let lines = transcript_lines(&app.messages);
        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(BG).fg(TEXT));
        let scroll = paragraph
            .line_count(transcript_area.width)
            .saturating_sub(transcript_area.height as usize) as u16;
        frame.render_widget(paragraph.scroll((scroll, 0)), transcript_area);
    }

    frame.render_widget(&app.composer, composer_area);

    let account = app.account.as_deref().unwrap_or("signed out");
    let effort = app
        .efforts
        .get(app.selected_effort)
        .map(String::as_str)
        .unwrap_or("default");
    let mut footer = vec![
        Span::styled(format!(" {account}"), Style::default().fg(MUTED).bg(PANEL)),
        Span::styled("  ·  ", Style::default().fg(MUTED).bg(PANEL)),
        Span::styled(&app.model, Style::default().fg(BLUE).bg(PANEL)),
        Span::styled("  ·  ", Style::default().fg(MUTED).bg(PANEL)),
        Span::styled(
            format!("< {effort} >"),
            Style::default().fg(MAGENTA).bg(PANEL),
        ),
        Span::styled("  ·  ? help", Style::default().fg(MUTED).bg(PANEL)),
    ];
    footer.push(Span::styled(
        "  ·  L account",
        Style::default().fg(GREEN).bg(PANEL),
    ));
    frame.render_widget(
        Paragraph::new(Line::from(footer)).style(Style::default().bg(PANEL)),
        footer_area,
    );
}

fn transcript_lines(messages: &[Message]) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    for message in messages {
        let (label, color) = match message.role {
            Role::User => ("You", BLUE),
            Role::Assistant => ("SERAPH", MAGENTA),
            Role::System => ("System", GREEN),
            Role::Error => ("Error", RED),
        };
        lines.push(Line::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        lines.extend(message.text.lines().map(Line::from));
        lines.push(Line::from(""));
    }
    lines
}

fn new_composer() -> TextArea<'static> {
    let mut composer = TextArea::default();
    composer.set_placeholder_text("Ask SERAPH…");
    composer.set_placeholder_style(Style::default().fg(MUTED));
    composer.set_style(Style::default().bg(BG).fg(TEXT));
    composer.set_cursor_line_style(Style::default().bg(BG).fg(TEXT));
    composer.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(PANEL))
            .title(Span::styled(" ❯ ", Style::default().fg(MAGENTA).bold())),
    );
    composer
}
