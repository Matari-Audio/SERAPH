// THESIS: Grok Build's quiet full-screen workbench, with agent state visible without opening another tool.
// OWN-WORLD: GrokNight neutrals, flat transcript, rounded prompt chrome, restrained TokyoNight status accents.
// STORY: Work in the main chat, watch parallel agents at the top and below the prompt, press Down for the whole roster.
// FIRST VIEWPORT: Location and agent rail, dominant transcript, Prime-style agents tile, Grok-style composer and status line.
// FORM: pinned Grok Build shell plus Prime Agent navigation.
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
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use ratatui_textarea::TextArea;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};

const FRAME_INTERVAL: Duration = Duration::from_millis(80);

// Derived from Grok Build's Apache-2.0 GrokNight palette.
const BG: Color = Color::Rgb(20, 20, 20);
const PANEL: Color = Color::Rgb(36, 36, 36);
const TEXT: Color = Color::Rgb(225, 225, 225);
const MUTED: Color = Color::Rgb(132, 132, 132);
const BLUE: Color = Color::Rgb(122, 162, 247);
const MAGENTA: Color = Color::Rgb(187, 154, 247);
const GREEN: Color = Color::Rgb(158, 206, 106);
const RED: Color = Color::Rgb(247, 118, 142);
const YELLOW: Color = Color::Rgb(224, 175, 104);

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
    AgentsChanged(Vec<AgentSummary>),
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
    auth_pending: bool,
    ready: bool,
    busy: bool,
    streaming: bool,
    show_help: bool,
    agents: Vec<AgentSummary>,
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
            auth_pending: false,
            ready: false,
            busy: false,
            streaming: false,
            show_help: false,
            agents: Vec::new(),
            view: View::Chat,
            selected_agent: 0,
            tick: 0,
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
            UiEvent::AgentsChanged(agents) => {
                self.agents = agents;
                self.selected_agent = self.selected_agent.min(self.agents.len().saturating_sub(1));
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
            }
            return true;
        }
        if self.is_composer_empty() {
            match key.code {
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
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('g' | 'G'))
        {
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
                dirty = app.animating();
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
        agents_area,
        composer_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
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

    frame.render_widget(
        Paragraph::new(agent_summary_line(app, agents_area.width)),
        agents_area,
    );
    frame.render_widget(&app.composer, composer_area);
    frame.render_widget(
        Paragraph::new(footer_line(app, footer_area.width)),
        footer_area,
    );
}

fn transcript_lines(messages: &[Message]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for message in messages {
        let safe = sanitize_display_text(&message.text);
        match message.role {
            Role::User => {
                let mut content = safe.lines();
                lines.push(Line::from(vec![
                    Span::styled("❯ ", Style::default().fg(BLUE).bold()),
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
    composer.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(80, 80, 88)))
            .title(Span::styled(" ❯ ", Style::default().fg(BLUE).bold())),
    );
    composer
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
    let mut right = vec![Span::styled("● main", Style::default().fg(GREEN))];
    for agent in app.agents.iter().take(4) {
        right.push(Span::styled("  ", Style::default()));
        right.push(Span::styled(
            format!("{} a{}", status_icon(agent.status, app.tick), agent.id),
            Style::default().fg(status_color(agent.status)),
        ));
    }
    if app.agents.len() > 4 {
        right.push(Span::styled(
            format!("  +{}", app.agents.len() - 4),
            Style::default().fg(MUTED),
        ));
    }
    joined_line(left, Line::from(right), width)
}

fn agent_summary_line(app: &App, width: u16) -> Line<'static> {
    let running = app
        .agents
        .iter()
        .filter(|agent| agent.status == "running")
        .count();
    let left = Line::from(vec![
        Span::styled(" ◇ agents ", Style::default().fg(MAGENTA).bold()),
        Span::styled(
            format!("{} total · {running} running", app.agents.len()),
            Style::default().fg(MUTED),
        ),
    ]);
    joined_line(
        left,
        Line::from(Span::styled("↓ all agents ", Style::default().fg(BLUE))),
        width,
    )
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
                Style::default().fg(Color::Rgb(26, 188, 156)),
            ),
            Span::styled("  │  ", Style::default().fg(Color::Rgb(88, 88, 88))),
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
    const SPINNER: [&str; 4] = ["⋅", ":", "⸬", "⁙"];
    match status {
        "running" => SPINNER[tick % SPINNER.len()],
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
