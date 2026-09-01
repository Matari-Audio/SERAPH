// THESIS: Grok Build's quiet full-screen workbench, with agent state visible without opening another tool.
// OWN-WORLD: GrokNight neutrals, flat transcript, rounded prompt chrome, restrained TokyoNight status accents.
// STORY: Work in the main chat, watch parallel agents and shared tasks in the dock, press Down for the whole roster.
// FIRST VIEWPORT: Location and agent rail, dominant transcript, Grok Dock V2, rounded composer, and status line.
// FORM: pinned Grok Build shell and Dock V2 plus Prime Agent navigation.
// FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance.
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use crossterm::{ExecutableCommand, QueueableCommand};
use futures_util::StreamExt;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use ratatui_textarea::TextArea;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};

use crate::grok_ui::{self, DockData, DockItem, DockRow, GROKNIGHT, Section};
use crate::tasks::{TaskSnapshot, TaskSummary};

const FRAME_INTERVAL: Duration = Duration::from_millis(80);

const BG: Color = GROKNIGHT.bg_base;
const PANEL: Color = GROKNIGHT.bg_highlight;
const TEXT: Color = GROKNIGHT.text_primary;
const MUTED: Color = GROKNIGHT.gray;
const BLUE: Color = GROKNIGHT.command_blue;
const MAGENTA: Color = GROKNIGHT.accent_running;
const GREEN: Color = GROKNIGHT.accent_success;
const RED: Color = GROKNIGHT.accent_error;
const YELLOW: Color = GROKNIGHT.warning;

#[derive(Debug, Clone)]
pub struct AgentSummary {
    pub id: u64,
    pub status: &'static str,
    pub prompt: String,
    pub result: Option<String>,
}

#[derive(Debug)]
pub enum UiEvent {
    AccountChanged(Option<String>),
    Ready(bool),
    ModelChanged {
        name: String,
        efforts: Vec<String>,
        selected_effort: usize,
    },
    LoginPending {
        login_id: String,
    },
    LoginStarted {
        login_id: String,
        auth_url: String,
        instructions: Option<String>,
    },
    LoginDeviceCode {
        code: String,
        url: String,
    },
    LoginPrompt {
        kind: String,
        message: String,
        placeholder: Option<String>,
        options: Vec<(String, String)>,
    },
    LoginProgress(String),
    LoginFinished {
        success: bool,
        error: Option<String>,
    },
    LoginCancelled,
    AssistantDelta(String),
    AssistantDone,
    AgentsChanged(Vec<AgentSummary>),
    TasksChanged(TaskSnapshot),
    Notice(String),
    BackgroundError(String),
    Error(String),
}

#[derive(Debug)]
pub enum UiCommand {
    Login(String),
    CancelLogin {
        login_id: String,
    },
    AnswerLoginPrompt(String),
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

enum AuthPrompt {
    Select {
        message: String,
        options: Vec<(String, String)>,
        cursor: usize,
    },
    Input {
        message: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Chat,
    Agents,
}

struct App {
    messages: Vec<Message>,
    composer: TextArea<'static>,
    account: Option<String>,
    model: String,
    efforts: Vec<String>,
    selected_effort: usize,
    login_id: Option<String>,
    auth_url: Option<String>,
    auth_instructions: Option<String>,
    auth_device: Option<(String, String)>,
    auth_prompt: Option<AuthPrompt>,
    auth_input: TextArea<'static>,
    auth_progress: Option<String>,
    auth_pending: bool,
    ready: bool,
    busy: bool,
    streaming: bool,
    show_help: bool,
    show_agents_menu: bool,
    agents_menu_cursor: usize,
    agents: Vec<AgentSummary>,
    tasks: TaskSnapshot,
    dock_focused: bool,
    dock_cursor: usize,
    subagents_expanded: bool,
    tasks_expanded: bool,
    view: View,
    selected_agent: usize,
    tick: usize,
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
            auth_url: None,
            auth_instructions: None,
            auth_device: None,
            auth_prompt: None,
            auth_input: new_auth_input(None),
            auth_progress: None,
            auth_pending: false,
            ready: false,
            busy: false,
            streaming: false,
            show_help: false,
            show_agents_menu: false,
            agents_menu_cursor: 0,
            agents: Vec::new(),
            tasks: TaskSnapshot {
                total: 0,
                tasks: Vec::new(),
            },
            dock_focused: false,
            dock_cursor: 0,
            subagents_expanded: true,
            tasks_expanded: true,
            view: View::Chat,
            selected_agent: 0,
            tick: 0,
        }
    }

    fn is_composer_empty(&self) -> bool {
        self.composer.lines().iter().all(String::is_empty)
    }

    fn set_dock_focused(&mut self, focused: bool) {
        self.dock_focused = focused;
        self.composer.set_block(composer_block(if focused {
            GROKNIGHT.prompt_border
        } else {
            GROKNIGHT.prompt_border_active
        }));
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
            UiEvent::LoginPending { login_id } => {
                self.auth_pending = true;
                self.login_id = Some(login_id);
                self.auth_progress = Some("Preparing authentication".into());
            }
            UiEvent::LoginStarted {
                login_id,
                auth_url,
                instructions,
            } => {
                self.auth_pending = true;
                self.login_id = Some(login_id);
                self.auth_url = Some(auth_url.clone());
                self.auth_instructions = instructions;
                self.auth_device = None;
                self.auth_progress = None;
                if let Err(error) = webbrowser::open(&auth_url) {
                    self.messages.push(Message {
                        role: Role::Error,
                        text: format!("Could not open browser: {error}"),
                    });
                }
            }
            UiEvent::LoginDeviceCode { code, url } => {
                self.auth_device = Some((code, url.clone()));
                self.auth_prompt = None;
                self.auth_progress = Some("Waiting for authentication...".into());
                let _ = webbrowser::open(&url);
            }
            UiEvent::LoginPrompt {
                kind,
                message,
                placeholder,
                options,
            } => {
                self.auth_prompt = Some(if kind == "select" {
                    AuthPrompt::Select {
                        message,
                        options,
                        cursor: 0,
                    }
                } else {
                    self.auth_input = new_auth_input(placeholder.as_deref());
                    AuthPrompt::Input { message }
                });
                self.auth_progress = None;
            }
            UiEvent::LoginProgress(message) => self.auth_progress = Some(message),
            UiEvent::LoginFinished { success, error } => {
                self.auth_pending = false;
                self.login_id = None;
                self.auth_url = None;
                self.auth_instructions = None;
                self.auth_device = None;
                self.auth_prompt = None;
                self.auth_progress = None;
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
            UiEvent::LoginCancelled => {
                self.auth_pending = false;
                self.login_id = None;
                self.auth_url = None;
                self.auth_instructions = None;
                self.auth_device = None;
                self.auth_prompt = None;
                self.auth_progress = None;
            }
            UiEvent::AssistantDelta(delta) => {
                if self.streaming {
                    if let Some(message) = self
                        .messages
                        .iter_mut()
                        .rev()
                        .find(|message| matches!(message.role, Role::Assistant))
                    {
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
            UiEvent::AgentsChanged(agents) => {
                self.agents = agents;
                self.selected_agent = self.selected_agent.min(self.agents.len().saturating_sub(1));
                self.agents_menu_cursor = self.agents_menu_cursor.min(self.agents.len() + 1);
            }
            UiEvent::TasksChanged(tasks) => self.tasks = tasks,
            UiEvent::Notice(text) => self.messages.push(Message {
                role: Role::System,
                text,
            }),
            UiEvent::BackgroundError(text) => self.messages.push(Message {
                role: Role::Error,
                text,
            }),
            UiEvent::Error(text) => {
                self.busy = false;
                self.streaming = false;
                self.auth_pending = false;
                self.login_id = None;
                self.auth_prompt = None;
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
        let agents_menu_key = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('\\' | '4'));
        let all_agents_key = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('g' | 'G'));
        if self.show_agents_menu && !self.auth_pending {
            match key.code {
                KeyCode::Esc => self.show_agents_menu = false,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.agents_menu_cursor = self.agents_menu_cursor.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.agents_menu_cursor =
                        (self.agents_menu_cursor + 1).min(self.agents.len() + 1)
                }
                KeyCode::Enter => {
                    self.show_agents_menu = false;
                    match self.agents_menu_cursor {
                        0 => self.view = View::Agents,
                        1 => {}
                        index => {
                            self.selected_agent = index - 2;
                            self.view = View::Agents;
                        }
                    }
                }
                _ if agents_menu_key => self.show_agents_menu = false,
                _ if all_agents_key => {
                    self.show_agents_menu = false;
                    self.view = View::Agents;
                }
                _ => {}
            }
            return true;
        }
        if agents_menu_key && !self.auth_pending && self.view == View::Chat {
            self.set_dock_focused(false);
            self.show_help = false;
            self.show_agents_menu = true;
            self.agents_menu_cursor = 0;
            return true;
        }
        if self.dock_focused {
            let data = dock_data(self);
            let items = grok_ui::visible_items(&data);
            match key.code {
                KeyCode::Esc | KeyCode::Tab => self.set_dock_focused(false),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.dock_cursor = self.dock_cursor.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.dock_cursor = (self.dock_cursor + 1).min(items.len().saturating_sub(1))
                }
                KeyCode::Enter => match items.get(self.dock_cursor) {
                    Some(DockItem::Header(Section::Subagents)) => {
                        self.subagents_expanded = !self.subagents_expanded
                    }
                    Some(DockItem::Header(Section::Tasks)) => {
                        self.tasks_expanded = !self.tasks_expanded
                    }
                    Some(DockItem::Row(Section::Subagents, index)) => {
                        self.selected_agent = *index;
                        self.set_dock_focused(false);
                        self.view = View::Agents;
                    }
                    _ => {}
                },
                _ => {}
            }
            self.dock_cursor = self.dock_cursor.min(
                grok_ui::visible_items(&dock_data(self))
                    .len()
                    .saturating_sub(1),
            );
            return true;
        }
        if self.view == View::Agents {
            match key.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.view = View::Chat,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected_agent = self.selected_agent.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected_agent =
                        (self.selected_agent + 1).min(self.agents.len().saturating_sub(1));
                }
                KeyCode::Home => self.selected_agent = 0,
                KeyCode::End => self.selected_agent = self.agents.len().saturating_sub(1),
                _ => {}
            }
            return true;
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
            } else if self.auth_pending {
                self.auth_pending = false;
                self.auth_prompt = None;
                self.auth_progress = None;
            }
            return true;
        }
        if self.auth_pending {
            match (&mut self.auth_prompt, key.code) {
                (Some(AuthPrompt::Select { cursor, .. }), KeyCode::Up) => {
                    *cursor = cursor.saturating_sub(1)
                }
                (
                    Some(AuthPrompt::Select {
                        cursor, options, ..
                    }),
                    KeyCode::Down,
                ) => *cursor = (*cursor + 1).min(options.len().saturating_sub(1)),
                (
                    Some(AuthPrompt::Select {
                        cursor, options, ..
                    }),
                    KeyCode::Enter,
                ) => {
                    if let Some((id, _)) = options.get(*cursor) {
                        let _ = commands.try_send(UiCommand::Login(id.clone()));
                        self.auth_prompt = None;
                        self.auth_progress = Some("Preparing authentication".into());
                    }
                }
                (Some(AuthPrompt::Input { .. }), KeyCode::Enter) => {
                    let value = self.auth_input.lines().join("\n");
                    if !value.trim().is_empty() {
                        let _ = commands.try_send(UiCommand::AnswerLoginPrompt(value));
                        self.auth_prompt = None;
                        self.auth_progress = Some("Waiting for authentication...".into());
                    }
                }
                (Some(AuthPrompt::Input { .. }), _) => {
                    self.auth_input.input(key);
                }
                _ => {}
            }
            return true;
        }
        if self.is_composer_empty() {
            match key.code {
                KeyCode::Tab if !grok_ui::visible_items(&dock_data(self)).is_empty() => {
                    self.set_dock_focused(true);
                    self.dock_cursor = 0;
                    return true;
                }
                KeyCode::Down => {
                    self.view = View::Agents;
                    return true;
                }
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
                    self.auth_pending = true;
                    self.auth_prompt = Some(AuthPrompt::Select {
                        message: "Select OpenAI Codex login method:".into(),
                        options: vec![
                            ("browser".into(), "Browser login (default)".into()),
                            ("device_code".into(), "Device code login (headless)".into()),
                        ],
                        cursor: 0,
                    });
                    return true;
                }
                _ => {}
            }
        }
        if all_agents_key {
            self.view = View::Agents;
            return true;
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

    fn animating(&self) -> bool {
        self.agents.iter().any(|agent| agent.status == "running")
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
                        if app.view == View::Chat {
                            app.composer.insert_str(text);
                            app.show_help = false;
                            dirty = true;
                        }
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
            () = &mut frame_delay, if dirty || app.animating() => {
                app.tick = app.tick.wrapping_add(1);
                dirty = true;
            }
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
    if app.view == View::Agents {
        render_agents(frame, app);
        return;
    }

    let [
        header_area,
        transcript_area,
        dock_area,
        composer_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(dock_height(app).min(area.height.saturating_sub(6))),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(header_line(app, header_area.width)),
        header_area,
    );

    if app.show_help {
        let help = Text::from(vec![
            Line::styled("SERAPH", Style::default().fg(MAGENTA).bold()),
            Line::from(""),
            Line::from("Enter    send"),
            Line::from("Alt+Enter      newline"),
            Line::from("< / >    reasoning effort"),
            Line::from("Ctrl+\\        agents menu"),
            Line::from("↓ / Ctrl+G     all agents"),
            Line::from("L              sign in / switch account"),
            Line::from("? / Esc  close help"),
            Line::from("Ctrl+C   quit"),
        ]);
        frame.render_widget(
            Paragraph::new(help)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(PANEL))
                        .title(" Shortcuts "),
                )
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

    grok_ui::render_dock(frame.buffer_mut(), dock_area, &GROKNIGHT, &dock_data(app));
    frame.render_widget(&app.composer, composer_area);
    frame.render_widget(
        Paragraph::new(footer_line(app, footer_area.width)),
        footer_area,
    );
    if app.show_agents_menu && !app.auth_pending {
        render_agents_menu(frame, app);
    }
    if app.auth_pending {
        render_login(frame, app);
    }
}

fn render_login(frame: &mut Frame, app: &App) {
    let outer = frame.area();
    let width = outer.width.saturating_sub(2).min(88);
    let height = outer.height.saturating_sub(2).min(22);
    let area = ratatui::layout::Rect::new(
        outer.x + outer.width.saturating_sub(width) / 2,
        outer.y + outer.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(PANEL))
        .title(" Login to OpenAI (ChatGPT Plus/Pro) ")
        .title_bottom(Line::styled(
            " Complete this step to continue setup. ",
            Style::default().fg(MUTED),
        ));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block.style(Style::default().bg(BG).fg(TEXT)), area);

    if let Some(AuthPrompt::Select {
        message,
        options,
        cursor,
    }) = &app.auth_prompt
    {
        let mut lines = vec![
            Line::styled(message.clone(), Style::default().fg(TEXT).bold()),
            Line::from(""),
        ];
        lines.extend(options.iter().enumerate().map(|(index, (_, label))| {
            let selected = index == *cursor;
            Line::styled(
                format!("  {label}"),
                if selected {
                    Style::default().bg(PANEL).fg(TEXT).bold()
                } else {
                    Style::default().fg(TEXT)
                },
            )
        }));
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "↑/↓ select  Enter continue  Esc cancel",
            Style::default().fg(MUTED),
        ));
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    if let Some((code, url)) = &app.auth_device {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Browser sign-in", Style::default().fg(TEXT).bold()),
                Line::from(""),
                Line::styled(url.clone(), Style::default().fg(BLUE)),
                Line::from(""),
                Line::styled("Verification code", Style::default().fg(TEXT).bold()),
                Line::styled(code.clone(), Style::default().fg(TEXT).bold()),
                Line::from(""),
                Line::styled(
                    app.auth_progress
                        .as_deref()
                        .unwrap_or("Waiting for authentication..."),
                    Style::default().fg(MUTED),
                ),
                Line::styled("Esc cancel", Style::default().fg(MUTED)),
            ])
            .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    let mut lines = vec![
        Line::styled("Browser sign-in", Style::default().fg(TEXT).bold()),
        Line::styled(
            "The sign-in page should already be opening. If it did not open, use the link below.",
            Style::default().fg(MUTED),
        ),
        Line::from(""),
        Line::styled("Sign-in link", Style::default().fg(TEXT).bold()),
        Line::styled(
            app.auth_url.as_deref().unwrap_or_default().to_owned(),
            Style::default().fg(BLUE),
        ),
    ];
    if let Some(instructions) = &app.auth_instructions {
        lines.push(Line::styled("Next step", Style::default().fg(TEXT).bold()));
        lines.push(Line::styled(
            instructions.clone(),
            Style::default().fg(MUTED),
        ));
    }
    if let Some(AuthPrompt::Input { message }) = &app.auth_prompt {
        lines.push(Line::styled(
            "Manual fallback",
            Style::default().fg(TEXT).bold(),
        ));
        lines.push(Line::styled(message.clone(), Style::default().fg(MUTED)));
        let [text_area, input_area, hint_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(inner);
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), text_area);
        frame.render_widget(&app.auth_input, input_area);
        frame.render_widget(
            Paragraph::new("Enter submit  Esc cancel").style(Style::default().fg(MUTED)),
            hint_area,
        );
    } else {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            app.auth_progress.as_deref().unwrap_or("Esc cancel"),
            Style::default().fg(MUTED),
        ));
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

fn transcript_lines(messages: &[Message]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for message in messages {
        let safe = sanitize_display_text(&message.text);
        match message.role {
            Role::User => {
                let mut content = safe.lines();
                lines.push(Line::from(vec![
                    Span::styled(grok_ui::prompt_arrow(), Style::default().fg(BLUE).bold()),
                    Span::styled(
                        content.next().unwrap_or_default().to_owned(),
                        Style::default().fg(TEXT),
                    ),
                ]));
                lines.extend(content.map(|line| Line::from(format!("  {line}"))));
            }
            Role::Assistant => lines.extend(safe.lines().map(str::to_owned).map(Line::from)),
            Role::System => lines.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(GREEN)),
                Span::styled(safe, Style::default().fg(MUTED)),
            ])),
            Role::Error => lines.push(Line::from(vec![
                Span::styled("! ", Style::default().fg(RED).bold()),
                Span::styled(safe, Style::default().fg(RED)),
            ])),
        }
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
    composer.set_block(composer_block(GROKNIGHT.prompt_border_active));
    composer
}

fn composer_block(border: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            format!(" {}", grok_ui::prompt_arrow()),
            Style::default().fg(BLUE).bold(),
        ))
}

fn new_auth_input(placeholder: Option<&str>) -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text(placeholder.unwrap_or("Paste value"));
    input.set_placeholder_style(Style::default().fg(MUTED));
    input.set_style(Style::default().bg(BG).fg(TEXT));
    input.set_cursor_line_style(Style::default().bg(BG).fg(TEXT));
    input.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(GROKNIGHT.prompt_border_active)),
    );
    input
}

fn header_line(app: &App, width: u16) -> Line<'static> {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "project".into());
    let left = Line::from(vec![
        Span::styled(" SERAPH", Style::default().fg(TEXT).bold()),
        Span::styled(format!("  {cwd}"), Style::default().fg(MUTED)),
    ]);
    let menu_style = if app.show_agents_menu {
        Style::default().fg(BLUE).bold()
    } else {
        Style::default().fg(GROKNIGHT.gray_bright)
    };
    let right = vec![
        Span::styled("● main", Style::default().fg(GREEN)),
        Span::styled(
            format!(
                "  Agents {} {}",
                app.agents.len() + 1,
                if app.show_agents_menu { "▴" } else { "▾" }
            ),
            menu_style,
        ),
    ];
    joined_line(left, Line::from(right), width)
}

fn render_agents_menu(frame: &mut Frame, app: &App) {
    let outer = frame.area();
    let width = outer.width.min(42);
    let height = (app.agents.len() + 2).min(outer.height.saturating_sub(2) as usize) as u16;
    if width == 0 || height == 0 {
        return;
    }
    let area = ratatui::layout::Rect::new(outer.right() - width, outer.y + 1, width, height);
    let start = app
        .agents_menu_cursor
        .saturating_add(1)
        .saturating_sub(height as usize);
    let mut rows = Vec::with_capacity(height as usize);
    for index in start..(app.agents.len() + 2).min(start + height as usize) {
        let mut row = match index {
            0 => joined_line(
                Line::from(Span::styled(
                    " All agents",
                    Style::default().fg(TEXT).bold(),
                )),
                Line::from(Span::styled("Ctrl+G ", Style::default().fg(MUTED))),
                width,
            ),
            1 => joined_line(
                Line::from(Span::styled(" ● main", Style::default().fg(GREEN).bold())),
                Line::from(Span::styled("connected ", Style::default().fg(MUTED))),
                width,
            ),
            _ => {
                let agent = &app.agents[index - 2];
                joined_line(
                    Line::from(vec![
                        Span::raw(" "),
                        Span::styled(
                            status_icon(agent.status, app.tick),
                            Style::default().fg(status_color(agent.status)),
                        ),
                        Span::styled(
                            format!(" agent {}", agent.id),
                            Style::default().fg(TEXT).bold(),
                        ),
                    ]),
                    Line::from(Span::styled(
                        format!("{} ", agent.status),
                        Style::default().fg(MUTED),
                    )),
                    width,
                )
            }
        };
        if index == app.agents_menu_cursor {
            row.style = Style::default().bg(PANEL);
        }
        rows.push(row);
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(rows).style(Style::default().bg(BG).fg(TEXT)),
        area,
    );
}

fn dock_height(app: &App) -> u16 {
    grok_ui::desired_height(&dock_data(app))
}

fn dock_data(app: &App) -> DockData {
    DockData {
        subagents: app
            .agents
            .iter()
            .map(|agent| DockRow {
                icon: status_icon(agent.status, app.tick),
                color: status_color(agent.status),
                kind: format!("agent {}", agent.id),
                description: one_line(&agent.prompt, usize::MAX),
                activity: None,
                meta: agent.status.into(),
                killable: false,
            })
            .collect(),
        tasks: app.tasks.tasks.iter().map(task_dock_row).collect(),
        tasks_total: app.tasks.total,
        subagents_expanded: app.subagents_expanded,
        tasks_expanded: app.tasks_expanded,
        focused: app.dock_focused,
        cursor: app.dock_cursor,
        ..DockData::default()
    }
}

fn task_dock_row(task: &TaskSummary) -> DockRow {
    let owner = task
        .owner
        .as_deref()
        .map(|owner| one_line(owner, 12))
        .unwrap_or_else(|| "unclaimed".into());
    DockRow {
        icon: task_status_icon(&task.status),
        color: task_status_color(&task.status),
        kind: format!("#{}", task.id),
        description: one_line(&task.subject, usize::MAX),
        activity: None,
        meta: format!("{owner} · {}", task.status),
        killable: false,
    }
}

fn one_line(text: &str, max_width: usize) -> String {
    let safe = sanitize_display_text(text);
    let line = safe.lines().next().unwrap_or_default();
    if Line::from(line).width() <= max_width {
        return line.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut truncated = String::new();
    let mut width = 0;
    for character in line.chars() {
        let character_width = Span::raw(character.to_string()).width();
        if width + character_width + 1 > max_width {
            break;
        }
        truncated.push(character);
        width += character_width;
    }
    truncated.push('…');
    truncated
}

fn footer_line(app: &App, width: u16) -> Line<'static> {
    let account = app.account.as_deref().unwrap_or("signed out");
    let effort = app
        .efforts
        .get(app.selected_effort)
        .map(String::as_str)
        .unwrap_or("default");
    joined_line(
        Line::from(Span::styled(
            format!(" {account}"),
            Style::default().fg(MUTED),
        )),
        Line::from(vec![
            Span::styled(
                app.model.clone(),
                Style::default().fg(GROKNIGHT.accent_model),
            ),
            Span::styled("  │  ", Style::default().fg(GROKNIGHT.gray_dim)),
            Span::styled(format!("< {effort} >"), Style::default().fg(MAGENTA)),
            Span::styled("  │  ? help  L account ", Style::default().fg(MUTED)),
        ]),
        width,
    )
}

fn joined_line(left: Line<'static>, right: Line<'static>, width: u16) -> Line<'static> {
    let gap = (width as usize).saturating_sub(left.width() + right.width());
    let mut spans = left.spans;
    spans.push(Span::raw(" ".repeat(gap.max(1))));
    spans.extend(right.spans);
    Line::from(spans)
}

fn render_agents(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let [header, current, external, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Percentage(52),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" All agents", Style::default().fg(TEXT).bold()),
            Span::styled(
                format!("  {} connected", app.agents.len() + 1),
                Style::default().fg(MUTED),
            ),
        ])),
        header,
    );

    let mut rows = vec![Line::from(vec![
        Span::styled("● ", Style::default().fg(GREEN)),
        Span::styled("main", Style::default().fg(TEXT).bold()),
        Span::styled("  this conversation", Style::default().fg(MUTED)),
    ])];
    let preview_rows = usize::from(
        app.agents
            .get(app.selected_agent)
            .and_then(|agent| agent.result.as_ref())
            .is_some(),
    );
    let agent_capacity = (current.height.saturating_sub(2) as usize)
        .saturating_sub(preview_rows)
        .max(1);
    let start = app
        .selected_agent
        .saturating_add(1)
        .saturating_sub(agent_capacity);
    for (index, agent) in app
        .agents
        .iter()
        .enumerate()
        .skip(start)
        .take(agent_capacity)
    {
        let selected = index == app.selected_agent;
        let style = if selected {
            Style::default().bg(PANEL).fg(TEXT)
        } else {
            Style::default().fg(TEXT)
        };
        let mut row = Line::from(vec![
            Span::styled(
                format!("{} ", status_icon(agent.status, app.tick)),
                Style::default()
                    .fg(status_color(agent.status))
                    .bg(style.bg.unwrap_or(BG)),
            ),
            Span::styled(format!("agent {}", agent.id), style.bold()),
            Span::styled(
                format!("  {:<9}  ", agent.status),
                Style::default()
                    .fg(status_color(agent.status))
                    .bg(style.bg.unwrap_or(BG)),
            ),
            Span::styled(sanitize_display_text(&agent.prompt), style),
        ]);
        row.style = style;
        rows.push(row);
        if selected && let Some(result) = &agent.result {
            let preview = sanitize_display_text(result)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            rows.push(Line::styled(
                format!("    {preview}"),
                Style::default().fg(MUTED),
            ));
        }
    }
    frame.render_widget(
        Paragraph::new(rows).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(PANEL))
                .title(" Current project "),
        ),
        current,
    );

    frame.render_widget(
        Paragraph::new(Line::styled(
            "  No agents from other projects are connected to this SERAPH process.",
            Style::default().fg(MUTED),
        ))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(PANEL))
                .title(" Other projects "),
        ),
        external,
    );
    frame.render_widget(
        Paragraph::new(joined_line(
            Line::from(Span::styled(" ↑/↓ select", Style::default().fg(MUTED))),
            Line::from(Span::styled("Esc / ← back ", Style::default().fg(BLUE))),
            footer.width,
        )),
        footer,
    );
}

fn status_icon(status: &str, tick: usize) -> &'static str {
    match status {
        "running" => grok_ui::BRAILLE_SPINNER_FRAMES[tick % grok_ui::BRAILLE_SPINNER_FRAMES.len()],
        "completed" => "●",
        "failed" => "●",
        _ => "○",
    }
}

fn status_color(status: &str) -> Color {
    match status {
        "running" => MAGENTA,
        "completed" => GREEN,
        "failed" => RED,
        _ => YELLOW,
    }
}

fn task_status_icon(status: &str) -> &'static str {
    match status {
        "in_progress" => "◆",
        "completed" | "failed" => "●",
        _ => "◇",
    }
}

fn task_status_color(status: &str) -> Color {
    match status {
        "in_progress" => MAGENTA,
        "completed" => GREEN,
        "failed" => RED,
        _ => YELLOW,
    }
}

fn sanitize_display_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if (character.is_control() && character != '\n')
                || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}
