mod agents;
mod capability;
mod codex;
mod grok_ui;
mod kernel;
mod pi_auth;
mod tasks;
mod tui;

use std::{
    collections::{BTreeMap, VecDeque},
    env,
    path::PathBuf,
    process::ExitCode,
    thread,
    time::Duration,
};

use agents::{AgentManager, ChildCommand, ChildEvent};
use anyhow::{Context, Result, bail};
use codex::{Codex, CodexEvent, LoginEvent, ToolResult};
use kernel::Kernel;
use seraph::edit_patch::{AppliedPatch, apply_prepared_edits, prepare_exact_patch};
use serde_json::{Value, json};
use tasks::TaskBoard;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};
use tui::{UiCommand, UiEvent};

const MAX_TOOL_RESULT_BYTES: usize = 32 * 1024;
const MAX_EDIT_PATCH_BYTES: usize = 512 * 1024;
const MAX_AGENT_RESULT_BYTES: usize = 2 * 1024;
const MAX_AGENT_MESSAGE_BYTES: usize = 16 * 1024;

enum LoginOutcome {
    Complete,
    Cancelled,
    Quit,
}

struct ToolHost {
    kernel: Option<Kernel>,
    task_board: Option<TaskBoard>,
    edits: BTreeMap<u64, AppliedPatch>,
    next_edit_handle: u64,
    project: PathBuf,
    agents: AgentManager,
}

impl ToolHost {
    async fn shutdown(self) -> Result<()> {
        self.agents.shutdown().await;
        if let Some(kernel) = self.kernel {
            kernel.shutdown().await?;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        return run_chat().await;
    };
    if command == "__agent" {
        if env::var_os("SERAPH_AGENT_CHILD").as_deref() != Some(std::ffi::OsStr::new("1")) {
            bail!("internal agent admission missing");
        }
        if args.next().is_some() {
            bail!("internal agent accepts its prompt on stdin");
        }
        return run_headless_agent().await;
    }
    if command != "exec" {
        bail!("usage: seraph [exec '<python cell>' ['<python cell>' ...]]");
    }

    let cells: Vec<String> = args.collect();
    if cells.is_empty() {
        bail!("exec requires at least one Python cell");
    }

    let kernel = Kernel::spawn().await?;
    for code in cells {
        let output = kernel.execute(&code).await?;
        print!("{}", output.stdout);
        print!("{}", output.background_stdout);
        eprint!("{}", output.stderr);
        eprint!("{}", output.background_stderr);
        for value in output.emitted {
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        if output.truncated {
            eprintln!("warning: execution output was truncated");
        }
    }

    kernel.shutdown().await
}

async fn run_chat() -> Result<()> {
    let (events, event_rx) = mpsc::channel(64);
    let (commands, command_rx) = mpsc::channel(16);
    let controller_events = events.clone();
    let mut controller = tokio::spawn(async move {
        let result = run_controller(events, command_rx).await;
        if let Err(error) = &result {
            let _ = controller_events
                .send(UiEvent::Error(format!("{error:#}")))
                .await;
        }
        result
    });
    let ui_result = tui::run(event_rx, commands.clone()).await;
    let _ = commands.try_send(UiCommand::Quit);
    if let Err(error) = ui_result {
        controller.abort();
        return Err(error);
    }
    match tokio::time::timeout(Duration::from_secs(2), &mut controller).await {
        Ok(result) => result.context("join SERAPH controller")?,
        Err(_) => {
            controller.abort();
            let _ = controller.await;
            Ok(())
        }
    }
}

async fn run_headless_agent() -> Result<()> {
    let project = env::current_dir().context("read current directory")?;
    let mut codex = Codex::spawn().await?;
    if !signed_in(&codex.account(false).await?) {
        bail!("Codex account is signed out");
    }
    let (model, _, _) = select_model(&codex.models().await?)?;
    let thread_id = codex.start_thread(&project, Some(&model)).await?;
    let mut tools = ToolHost {
        kernel: None,
        task_board: None,
        edits: BTreeMap::new(),
        next_edit_handle: 1,
        project: project.clone(),
        agents: AgentManager::new(project, None)?,
    };
    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    let mut output = tokio::io::stdout();
    write_child_event(&mut output, &ChildEvent::Ready).await?;
    let initial = read_child_command(&mut commands)
        .await?
        .context("SERAPH agent control stream closed before start")?;
    let ChildCommand::Start { prompt } = initial else {
        bail!("first SERAPH agent command must be start");
    };
    validate_agent_prompt(&prompt)?;
    let agent_id = env::var("SERAPH_AGENT_ID").context("SERAPH agent id missing")?;
    let submission = codex
        .queue_turn(
            &thread_id,
            &prompt,
            &format!("seraph-agent-{agent_id}-initial"),
        )
        .await?;
    let mut turn_id = Some(codex.start_queued_turn(&thread_id, &submission).await?);
    write_child_event(
        &mut output,
        &ChildEvent::Running {
            key: None,
            result: None,
        },
    )
    .await?;

    let mut pending = VecDeque::new();
    let mut answer = String::new();
    let mut answer_truncated = false;
    let mut turn_error = None;
    let mut deadline = tokio::time::Instant::now() + Duration::from_secs(10 * 60);
    let mut shutting_down = false;
    let mut turn_stopping = false;
    let result: Result<()> = async {
        loop {
            let Some(active_turn) = turn_id.as_deref() else {
                let command = read_child_command(&mut commands)
                    .await?
                    .context("SERAPH agent control stream closed")?;
                match command {
                    ChildCommand::FollowUp { key, prompt } => {
                        validate_follow_up(&key, &prompt)?;
                        let submission = codex
                            .queue_turn(
                                &thread_id,
                                &prompt,
                                &format!("seraph-agent-{agent_id}-{key}"),
                            )
                            .await?;
                        write_child_event(
                            &mut output,
                            &ChildEvent::Queued {
                                key: key.clone(),
                                submission_id: submission.clone(),
                                starts_immediately: true,
                            },
                        )
                        .await?;
                        turn_id = Some(codex.start_queued_turn(&thread_id, &submission).await?);
                        deadline = tokio::time::Instant::now() + Duration::from_secs(10 * 60);
                        write_child_event(
                            &mut output,
                            &ChildEvent::Running {
                                key: Some(key),
                                result: None,
                            },
                        )
                        .await?;
                    }
                    ChildCommand::Interrupt => {
                        write_child_event(
                            &mut output,
                            &ChildEvent::Interrupted { accepted: false },
                        )
                        .await?;
                    }
                    ChildCommand::Shutdown => break,
                    ChildCommand::Start { .. } => bail!("agent was already started"),
                }
                continue;
            };

            enum Next {
                Event(Result<CodexEvent>),
                Command(Result<Option<ChildCommand>>),
                Timeout,
            }
            let next = {
                let event = codex.next_turn_event(&thread_id, active_turn);
                tokio::pin!(event);
                tokio::select! {
                    event = &mut event => Next::Event(event),
                    command = read_child_command(&mut commands) => Next::Command(command),
                    _ = tokio::time::sleep_until(deadline) => Next::Timeout,
                }
            };
            match next {
                Next::Command(Ok(Some(ChildCommand::FollowUp { key, prompt }))) => {
                    validate_follow_up(&key, &prompt)?;
                    let submission = codex
                        .queue_turn(
                            &thread_id,
                            &prompt,
                            &format!("seraph-agent-{agent_id}-{key}"),
                        )
                        .await?;
                    pending.push_back((key.clone(), submission.clone()));
                    write_child_event(
                        &mut output,
                        &ChildEvent::Queued {
                            key,
                            submission_id: submission,
                            starts_immediately: false,
                        },
                    )
                    .await?;
                }
                Next::Command(Ok(Some(ChildCommand::Interrupt))) => {
                    let accepted = codex.interrupt_turn(&thread_id, active_turn).await?;
                    turn_stopping = true;
                    write_child_event(&mut output, &ChildEvent::Interrupted { accepted }).await?;
                }
                Next::Command(Ok(Some(ChildCommand::Shutdown))) => {
                    shutting_down = true;
                    let _ = codex.interrupt_turn(&thread_id, active_turn).await?;
                    turn_stopping = true;
                }
                Next::Command(Ok(Some(ChildCommand::Start { .. }))) => {
                    bail!("agent was already started")
                }
                Next::Command(Ok(None)) => {
                    shutting_down = true;
                    let _ = codex.interrupt_turn(&thread_id, active_turn).await?;
                    turn_stopping = true;
                }
                Next::Command(Err(error)) | Next::Event(Err(error)) => return Err(error),
                Next::Timeout => {
                    let accepted = codex.interrupt_turn(&thread_id, active_turn).await?;
                    turn_stopping = true;
                    write_child_event(&mut output, &ChildEvent::Interrupted { accepted }).await?;
                    deadline = tokio::time::Instant::now() + Duration::from_secs(10 * 60);
                }
                Next::Event(Ok(CodexEvent::AgentMessageDelta(delta))) => {
                    if !answer_truncated
                        && append_bounded(&mut answer, &delta, MAX_AGENT_RESULT_BYTES)
                    {
                        answer_truncated = true;
                        let _ = codex.interrupt_turn(&thread_id, active_turn).await?;
                        turn_stopping = true;
                    }
                }
                Next::Event(Ok(CodexEvent::ToolCall(call))) => {
                    if turn_stopping {
                        codex
                            .respond_tool(
                                call,
                                ToolResult {
                                    text: "Turn interrupted before tool execution.".into(),
                                    success: false,
                                },
                            )
                            .await?;
                        continue;
                    }
                    enum ToolOutcome {
                        Complete(Result<String>),
                        Interrupted {
                            reason: &'static str,
                            acknowledge: bool,
                        },
                    }
                    let result = {
                        let execution = execute_tool(
                            &mut tools,
                            &call.thread_id,
                            &call.namespace,
                            &call.tool,
                            &call.arguments,
                        );
                        tokio::pin!(execution);
                        loop {
                            tokio::select! {
                                result = &mut execution => break ToolOutcome::Complete(result),
                                command = read_child_command(&mut commands) => match command? {
                                    Some(ChildCommand::FollowUp { key, prompt }) => {
                                        validate_follow_up(&key, &prompt)?;
                                        let submission = codex
                                            .queue_turn(
                                                &thread_id,
                                                &prompt,
                                                &format!("seraph-agent-{agent_id}-{key}"),
                                            )
                                            .await?;
                                        pending.push_back((key.clone(), submission.clone()));
                                        write_child_event(
                                            &mut output,
                                            &ChildEvent::Queued {
                                                key,
                                                submission_id: submission,
                                                starts_immediately: false,
                                            },
                                        )
                                        .await?;
                                    }
                                    Some(ChildCommand::Interrupt) => {
                                        break ToolOutcome::Interrupted {
                                            reason: "Turn interrupted before tool execution completed.",
                                            acknowledge: true,
                                        };
                                    }
                                    Some(ChildCommand::Shutdown) | None => {
                                        shutting_down = true;
                                        break ToolOutcome::Interrupted {
                                            reason: "Turn shut down before tool execution completed.",
                                            acknowledge: false,
                                        };
                                    }
                                    Some(ChildCommand::Start { .. }) => {
                                        bail!("agent was already started")
                                    }
                                },
                                _ = tokio::time::sleep_until(deadline) => {
                                    deadline = tokio::time::Instant::now() + Duration::from_secs(10 * 60);
                                    break ToolOutcome::Interrupted {
                                        reason: "Tool execution timed out and the turn was interrupted.",
                                        acknowledge: true,
                                    };
                                }
                            }
                        }
                    };
                    let interrupt = match &result {
                        ToolOutcome::Interrupted { acknowledge, .. } => Some(*acknowledge),
                        ToolOutcome::Complete(_) => None,
                    };
                    codex
                        .respond_tool(
                            call,
                            match result {
                                ToolOutcome::Complete(Ok(text)) => ToolResult {
                                    text,
                                    success: true,
                                },
                                ToolOutcome::Complete(Err(error)) => ToolResult {
                                    text: bounded_error(&error),
                                    success: false,
                                },
                                ToolOutcome::Interrupted { reason, .. } => ToolResult {
                                    text: reason.into(),
                                    success: false,
                                },
                            },
                        )
                        .await?;
                    if let Some(acknowledge) = interrupt {
                        let accepted = codex.interrupt_turn(&thread_id, active_turn).await?;
                        turn_stopping = true;
                        if acknowledge {
                            write_child_event(
                                &mut output,
                                &ChildEvent::Interrupted { accepted },
                            )
                            .await?;
                        }
                    }
                }
                Next::Event(Ok(CodexEvent::TurnError(error))) => turn_error = Some(error),
                Next::Event(Ok(CodexEvent::TurnCompleted(turn))) => {
                    match turn.get("status").and_then(Value::as_str) {
                        Some("completed" | "interrupted") => {}
                        Some("failed") => bail!(
                            "Codex turn failed: {}",
                            turn.get("error")
                                .map(Value::to_string)
                                .or(turn_error.take())
                                .unwrap_or_else(|| "unknown error".into())
                        ),
                        status => bail!("Codex turn ended with unexpected status {status:?}"),
                    }
                    let result = std::mem::take(&mut answer);
                    answer_truncated = false;
                    turn_error = None;
                    turn_id = None;
                    turn_stopping = false;
                    if shutting_down {
                        write_child_event(&mut output, &ChildEvent::Idle { result }).await?;
                        break;
                    }
                    if let Some((key, submission)) = pending.pop_front() {
                        turn_id = Some(codex.start_queued_turn(&thread_id, &submission).await?);
                        deadline = tokio::time::Instant::now() + Duration::from_secs(10 * 60);
                        write_child_event(
                            &mut output,
                            &ChildEvent::Running {
                                key: Some(key),
                                result: Some(result),
                            },
                        )
                        .await?;
                    } else {
                        write_child_event(&mut output, &ChildEvent::Idle { result }).await?;
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    if let Err(error) = &result {
        write_child_event(
            &mut output,
            &ChildEvent::Failed {
                error: bounded_error(error),
            },
        )
        .await?;
    }
    tools.shutdown().await?;
    codex.shutdown().await?;
    result?;
    write_child_event(&mut output, &ChildEvent::Stopped).await
}

async fn read_child_command(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
) -> Result<Option<ChildCommand>> {
    lines
        .next_line()
        .await
        .context("read SERAPH child command")?
        .map(|line| serde_json::from_str(&line).context("decode SERAPH child command JSON"))
        .transpose()
}

async fn write_child_event(output: &mut tokio::io::Stdout, event: &ChildEvent) -> Result<()> {
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    output
        .write_all(&line)
        .await
        .context("write SERAPH child event")?;
    output.flush().await.context("flush SERAPH child event")
}

fn validate_agent_prompt(prompt: &str) -> Result<()> {
    if prompt.trim().is_empty() || prompt.len() > 16 * 1024 {
        bail!("agent prompt must contain 1 to 16384 bytes");
    }
    Ok(())
}

fn validate_follow_up(key: &str, prompt: &str) -> Result<()> {
    validate_agent_prompt(prompt)?;
    if key.trim().is_empty() || key.len() > 128 {
        bail!("follow-up key must contain 1 to 128 bytes");
    }
    Ok(())
}

async fn run_controller(
    events: mpsc::Sender<UiEvent>,
    mut commands: mpsc::Receiver<UiCommand>,
) -> Result<()> {
    let mut codex = match Codex::spawn().await {
        Ok(codex) => codex,
        Err(error) => {
            let _ = events.send(UiEvent::Error(format!("{error:#}"))).await;
            return Ok(());
        }
    };
    let mut account = codex.account(false).await?;
    send_account(&events, &account).await?;
    let models = codex.models().await?;
    let (mut model, efforts, selected_effort) = select_model(&models)?;
    events
        .send(UiEvent::ModelChanged {
            name: model.clone(),
            efforts,
            selected_effort,
        })
        .await?;

    let cwd = env::current_dir().context("read current directory")?;
    watch_tasks(cwd.clone(), events.clone());
    let mut thread = if signed_in(&account) {
        Some(codex.start_thread(&cwd, Some(&model)).await?)
    } else {
        None
    };
    let mut tools = ToolHost {
        kernel: None,
        task_board: None,
        edits: BTreeMap::new(),
        next_edit_handle: 1,
        project: cwd.clone(),
        agents: AgentManager::new(cwd.clone(), Some(events.clone()))?,
    };
    events.send(UiEvent::Ready(thread.is_some())).await?;

    let result: Result<()> = async {
        while let Some(command) = commands.recv().await {
            match command {
                UiCommand::Login(method) => {
                    match login(&mut codex, &events, &mut commands, &method).await? {
                        LoginOutcome::Complete => {
                            events.send(UiEvent::Ready(false)).await?;
                            account = codex.account(false).await?;
                            send_account(&events, &account).await?;
                            let models = codex.models().await?;
                            let (next_model, efforts, selected_effort) = select_model(&models)?;
                            model = next_model;
                            events
                                .send(UiEvent::ModelChanged {
                                    name: model.clone(),
                                    efforts,
                                    selected_effort,
                                })
                                .await?;
                            thread = Some(codex.start_thread(&cwd, Some(&model)).await?);
                            events.send(UiEvent::Ready(true)).await?;
                            events
                                .send(UiEvent::LoginFinished {
                                    success: true,
                                    error: None,
                                })
                                .await?;
                        }
                        LoginOutcome::Cancelled => {}
                        LoginOutcome::Quit => break,
                    }
                }
                UiCommand::CancelLogin { login_id } => {
                    codex.cancel_login(&login_id).await?;
                }
                UiCommand::AnswerLoginPrompt(_) => {}
                UiCommand::Submit { text, effort } => {
                    let Some(thread_id) = thread.as_deref() else {
                        events
                            .send(UiEvent::Error("Sign in before sending a message.".into()))
                            .await?;
                        continue;
                    };
                    match run_turn(
                        &mut codex,
                        &mut tools,
                        &events,
                        &mut commands,
                        thread_id,
                        &text,
                        effort.as_deref(),
                    )
                    .await
                    {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(error) => events.send(UiEvent::Error(format!("{error:#}"))).await?,
                    }
                }
                UiCommand::Quit => break,
            }
        }
        Ok(())
    }
    .await;

    let tools_shutdown = tools.shutdown().await;
    let codex_shutdown = codex.shutdown().await;
    result?;
    tools_shutdown?;
    codex_shutdown
}

fn watch_tasks(project: PathBuf, events: mpsc::Sender<UiEvent>) {
    thread::spawn(move || {
        let database = project.join(".seraph/tasks.sqlite3");
        while !database.exists() {
            if events.is_closed() {
                return;
            }
            thread::sleep(Duration::from_millis(300));
        }
        let board = match TaskBoard::open(&project) {
            Ok(board) => board,
            Err(error) => {
                let _ = events.blocking_send(UiEvent::BackgroundError(format!(
                    "Could not watch task board: {error:#}"
                )));
                return;
            }
        };
        let mut previous = None;
        while !events.is_closed() {
            match board.snapshot(50) {
                Ok(snapshot) if previous.as_ref() != Some(&snapshot) => {
                    previous = Some(snapshot.clone());
                    if events
                        .blocking_send(UiEvent::TasksChanged(snapshot))
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = events.blocking_send(UiEvent::BackgroundError(format!(
                        "Could not read task board: {error:#}"
                    )));
                    return;
                }
            }
            thread::sleep(Duration::from_millis(300));
        }
    });
}

async fn login(
    codex: &mut Codex,
    events: &mpsc::Sender<UiEvent>,
    commands: &mut mpsc::Receiver<UiCommand>,
    method: &str,
) -> Result<LoginOutcome> {
    let login_id = codex.start_chatgpt_login(method).await?;
    events
        .send(UiEvent::LoginPending {
            login_id: login_id.clone(),
        })
        .await?;

    enum Outcome {
        Complete,
        Failed(anyhow::Error),
        Cancel,
        Quit,
    }
    let outcome = {
        loop {
            enum Next {
                Event(Result<LoginEvent>),
                Command(Option<UiCommand>),
            }
            let next = {
                let event = codex.next_login_event(&login_id);
                tokio::pin!(event);
                tokio::select! {
                    result = &mut event => Next::Event(result),
                    command = commands.recv() => Next::Command(command),
                }
            };
            match next {
                Next::Event(result) => match result {
                    Ok(LoginEvent::AuthUrl { url, instructions }) => {
                        events
                            .send(UiEvent::LoginStarted {
                                login_id: login_id.clone(),
                                auth_url: url,
                                instructions,
                            })
                            .await?;
                    }
                    Ok(LoginEvent::DeviceCode { code, url }) => {
                        events.send(UiEvent::LoginDeviceCode { code, url }).await?;
                    }
                    Ok(LoginEvent::Prompt {
                        kind,
                        message,
                        placeholder,
                        options,
                    }) => {
                        events
                            .send(UiEvent::LoginPrompt {
                                kind,
                                message,
                                placeholder,
                                options: options
                                    .into_iter()
                                    .map(|option| (option.id, option.label))
                                    .collect(),
                            })
                            .await?;
                    }
                    Ok(LoginEvent::Progress(message)) => {
                        events.send(UiEvent::LoginProgress(message)).await?;
                    }
                    Ok(LoginEvent::Complete) => break Outcome::Complete,
                    Err(error) => break Outcome::Failed(error),
                },
                Next::Command(command) => match command {
                    Some(UiCommand::CancelLogin { login_id: id }) if id == login_id => {
                        break Outcome::Cancel;
                    }
                    Some(UiCommand::AnswerLoginPrompt(value)) => {
                        codex.answer_login_prompt(&value).await?
                    }
                    Some(UiCommand::Quit) | None => break Outcome::Quit,
                    Some(_) => {}
                },
            }
        }
    };

    match outcome {
        Outcome::Complete => Ok(LoginOutcome::Complete),
        Outcome::Failed(error) => {
            events
                .send(UiEvent::LoginFinished {
                    success: false,
                    error: Some(format!("{error:#}")),
                })
                .await?;
            Ok(LoginOutcome::Cancelled)
        }
        Outcome::Cancel => {
            codex.cancel_login(&login_id).await?;
            events.send(UiEvent::LoginCancelled).await?;
            Ok(LoginOutcome::Cancelled)
        }
        Outcome::Quit => {
            codex.cancel_login(&login_id).await?;
            Ok(LoginOutcome::Quit)
        }
    }
}

async fn run_turn(
    codex: &mut Codex,
    tools: &mut ToolHost,
    ui: &mpsc::Sender<UiEvent>,
    commands: &mut mpsc::Receiver<UiCommand>,
    thread_id: &str,
    prompt: &str,
    effort: Option<&str>,
) -> Result<bool> {
    let turn_id = codex.start_turn(thread_id, prompt, effort).await?;
    let mut turn_error = None;

    loop {
        enum Next {
            Event(Result<CodexEvent>),
            Quit,
        }
        let next = {
            let event = codex.next_turn_event(thread_id, &turn_id);
            tokio::pin!(event);
            tokio::select! {
                event = &mut event => Next::Event(event),
                command = commands.recv() => match command {
                    Some(UiCommand::Quit) | None => Next::Quit,
                    Some(_) => {
                        ui.send(UiEvent::Notice("The current turn is still running.".into())).await?;
                        continue;
                    },
                }
            }
        };
        match next {
            Next::Event(Ok(CodexEvent::AgentMessageDelta(delta))) => {
                ui.send(UiEvent::AssistantDelta(delta)).await?
            }
            Next::Event(Ok(CodexEvent::ToolCall(call))) => {
                enum ToolOutcome {
                    Complete(Result<String>),
                    Quit,
                }
                let result = {
                    let execution = execute_tool(
                        tools,
                        &call.thread_id,
                        &call.namespace,
                        &call.tool,
                        &call.arguments,
                    );
                    tokio::pin!(execution);
                    loop {
                        tokio::select! {
                            result = &mut execution => break ToolOutcome::Complete(result),
                            command = commands.recv() => match command {
                                Some(UiCommand::Quit) | None => break ToolOutcome::Quit,
                                Some(_) => ui.send(UiEvent::Notice("The tool is still running.".into())).await?,
                            }
                        }
                    }
                };
                let ToolOutcome::Complete(result) = result else {
                    codex
                        .respond_tool(
                            call,
                            ToolResult {
                                text: "Turn interrupted before tool execution completed.".into(),
                                success: false,
                            },
                        )
                        .await?;
                    interrupt_turn(codex, thread_id, &turn_id).await?;
                    return Ok(true);
                };
                codex
                    .respond_tool(
                        call,
                        match result {
                            Ok(text) => ToolResult {
                                text,
                                success: true,
                            },
                            Err(error) => ToolResult {
                                text: bounded_error(&error),
                                success: false,
                            },
                        },
                    )
                    .await?;
            }
            Next::Event(Ok(CodexEvent::TurnError(error))) => turn_error = Some(error),
            Next::Event(Ok(CodexEvent::TurnCompleted(turn))) => {
                match turn.get("status").and_then(Value::as_str) {
                    Some("completed") => ui.send(UiEvent::AssistantDone).await?,
                    Some("interrupted") => {
                        ui.send(UiEvent::Error("Turn interrupted.".into())).await?
                    }
                    Some("failed") => bail!(
                        "Codex turn failed: {}",
                        turn.get("error")
                            .map(Value::to_string)
                            .or(turn_error)
                            .unwrap_or_else(|| "unknown error".into())
                    ),
                    status => bail!("Codex turn ended with unexpected status {status:?}"),
                }
                return Ok(false);
            }
            Next::Event(Err(error)) => return Err(error),
            Next::Quit => {
                interrupt_turn(codex, thread_id, &turn_id).await?;
                return Ok(true);
            }
        }
    }
}

async fn interrupt_turn(codex: &mut Codex, thread_id: &str, turn_id: &str) -> Result<()> {
    codex.interrupt_turn(thread_id, turn_id).await?;
    loop {
        match codex.next_turn_event(thread_id, turn_id).await? {
            CodexEvent::ToolCall(call) => {
                codex
                    .respond_tool(
                        call,
                        ToolResult {
                            text: "Turn interrupted before tool execution.".into(),
                            success: false,
                        },
                    )
                    .await?;
            }
            CodexEvent::TurnCompleted(_) => return Ok(()),
            CodexEvent::AgentMessageDelta(_) | CodexEvent::TurnError(_) => {}
        }
    }
}

async fn execute_tool(
    host: &mut ToolHost,
    caller: &str,
    namespace: &Option<String>,
    tool: &str,
    arguments: &Value,
) -> Result<String> {
    if namespace.as_deref() != Some("seraph") {
        bail!(
            "unknown dynamic tool {}.{tool}",
            namespace.as_deref().unwrap_or("")
        );
    }
    if tool == "agents" {
        return bounded_projection(
            serde_json::to_string(&execute_agents(&host.agents, arguments).await?)?,
            "Agent",
        );
    }
    if tool == "coordination" {
        if host.task_board.is_none() {
            host.task_board = Some(TaskBoard::open(&host.project)?);
        }
        return bounded_projection(
            execute_coordination(
                host.task_board
                    .as_mut()
                    .expect("task board was initialized"),
                caller,
                arguments,
            )?,
            "Coordination",
        );
    }
    if tool == "edit" {
        return execute_edit(host, arguments);
    }
    if tool != "python" {
        bail!("unknown dynamic tool seraph.{tool}");
    }
    let code = arguments
        .get("code")
        .and_then(Value::as_str)
        .context("seraph.python requires a string code argument")?;
    if host.kernel.is_none() {
        host.kernel = Some(Kernel::spawn().await?);
    }
    let output = host
        .kernel
        .as_ref()
        .expect("kernel was initialized")
        .execute(code)
        .await?;
    let projection = serde_json::to_string(&json!({
        "emitted": output.emitted,
        "stdout_bytes": output.stdout.len(),
        "stderr_bytes": output.stderr.len(),
        "background_stdout_bytes": output.background_stdout.len(),
        "background_stderr_bytes": output.background_stderr.len(),
        "truncated": output.truncated,
    }))?;
    bounded_projection(projection, "Python")
}

fn execute_edit(host: &mut ToolHost, arguments: &Value) -> Result<String> {
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .context("seraph.edit requires an action")?;
    if action == "rollback" {
        let handle = arguments
            .get("handle")
            .and_then(Value::as_u64)
            .filter(|handle| *handle > 0)
            .context("rollback requires a positive integer handle")?;
        host.edits
            .get(&handle)
            .with_context(|| format!("unknown session rollback handle {handle}"))?
            .rollback()?;
        host.edits.remove(&handle);
        return bounded_projection(
            json!({ "rolled_back": handle, "handle_scope": "session" }).to_string(),
            "Edit",
        );
    }
    if action != "apply" {
        bail!("unknown seraph.edit action {action:?}");
    }
    let patch = arguments
        .get("patch")
        .and_then(Value::as_str)
        .context("seraph.edit requires a string patch argument")?;
    if patch.len() > MAX_EDIT_PATCH_BYTES {
        bail!("edit patch exceeds 512 KiB");
    }
    let project = host
        .project
        .canonicalize()
        .context("resolve edit project root")?;
    let edits = prepare_exact_patch(&project, patch)?;
    let changed: Vec<_> = edits
        .iter()
        .map(|edit| {
            json!({
                "path": edit.target.strip_prefix(&project).unwrap_or(&edit.target),
                "before_bytes": edit.expected.byte_len(),
                "after_bytes": edit.next.byte_len(),
            })
        })
        .collect();
    let handle = host.next_edit_handle;
    let next_handle = handle
        .checked_add(1)
        .context("rollback handle space exhausted")?;
    let summary = bounded_projection(
        json!({
            "changed": changed,
            "rollback_handle": handle,
            "handle_scope": "session",
        })
        .to_string(),
        "Edit",
    )?;
    let applied = apply_prepared_edits(&project, &edits)?;
    host.edits.insert(handle, applied);
    host.next_edit_handle = next_handle;
    Ok(summary)
}

async fn execute_agents(manager: &AgentManager, arguments: &Value) -> Result<Value> {
    match arguments
        .get("action")
        .and_then(Value::as_str)
        .context("seraph.agents requires a string action")?
    {
        "spawn" => {
            let prompt = arguments
                .get("prompt")
                .and_then(Value::as_str)
                .context("spawn requires a string prompt")?;
            if prompt.len() > 16 * 1024 {
                bail!("agent prompt exceeds 16 KiB");
            }
            manager.spawn(prompt).await
        }
        "list" => {
            let after_id = arguments
                .get("after_id")
                .map(|value| {
                    value
                        .as_u64()
                        .context("after_id must be a non-negative integer")
                })
                .transpose()?
                .unwrap_or(0);
            let limit = arguments
                .get("limit")
                .map(|value| {
                    value
                        .as_u64()
                        .filter(|limit| (1..=200).contains(limit))
                        .context("limit must be an integer from 1 to 200")
                })
                .transpose()?
                .unwrap_or(50) as usize;
            Ok(manager.list(after_id, limit).await)
        }
        "wait" => {
            let ids = arguments
                .get("ids")
                .and_then(Value::as_array)
                .context("wait requires an ids array")?
                .iter()
                .map(|id| {
                    id.as_u64()
                        .filter(|id| *id > 0)
                        .context("agent IDs must be positive integers")
                })
                .collect::<Result<Vec<_>>>()?;
            manager.wait(&ids).await
        }
        "interrupt" => {
            let id = arguments
                .get("id")
                .and_then(Value::as_u64)
                .filter(|id| *id > 0)
                .context("interrupt requires a positive integer id")?;
            Ok(json!({ "id": id, "interrupted": manager.interrupt(id).await? }))
        }
        "follow_up" => {
            let id = arguments
                .get("id")
                .and_then(Value::as_u64)
                .filter(|id| *id > 0)
                .context("follow_up requires a positive integer id")?;
            let prompt = arguments
                .get("prompt")
                .and_then(Value::as_str)
                .context("follow_up requires a string prompt")?;
            let key = arguments
                .get("key")
                .and_then(Value::as_str)
                .context("follow_up requires a string idempotency key")?;
            manager.follow_up(id, prompt, key).await
        }
        "send" => {
            let recipient = arguments
                .get("recipient")
                .and_then(Value::as_str)
                .context("send requires a string recipient")?;
            let message = arguments
                .get("message")
                .and_then(Value::as_str)
                .context("send requires a string message")?;
            let key = arguments
                .get("key")
                .and_then(Value::as_str)
                .context("send requires a string idempotency key")?;
            if message.trim().is_empty()
                || serde_json::to_vec(message)?.len().saturating_sub(2) > MAX_AGENT_MESSAGE_BYTES
            {
                bail!("JSON-encoded message content must contain 1 to 16384 bytes");
            }
            if key.trim().is_empty() || key.len() > 128 {
                bail!("key must contain 1 to 128 bytes");
            }
            manager.send(recipient, message, key)
        }
        "receive" => {
            let limit = arguments
                .get("limit")
                .map(|value| {
                    value
                        .as_u64()
                        .filter(|limit| (1..=200).contains(limit))
                        .context("limit must be an integer from 1 to 200")
                })
                .transpose()?
                .unwrap_or(20) as usize;
            manager.receive(limit, MAX_TOOL_RESULT_BYTES)
        }
        action => bail!("unknown agent action {action:?}"),
    }
}

fn bounded_projection(projection: String, source: &str) -> Result<String> {
    if projection.len() > MAX_TOOL_RESULT_BYTES {
        bail!("{source} projection exceeded 32 KiB; request a smaller result");
    }
    Ok(projection)
}

fn execute_coordination(board: &mut TaskBoard, caller: &str, arguments: &Value) -> Result<String> {
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .context("seraph.coordination requires a string action")?;
    let result = match action {
        "create" => {
            let subject = arguments
                .get("subject")
                .and_then(Value::as_str)
                .context("create requires a string subject")?;
            if subject.len() > 512 {
                bail!("subject exceeds 512 bytes");
            }
            let blocked_by = arguments
                .get("blocked_by")
                .map(|value| {
                    value
                        .as_array()
                        .context("blocked_by must be an array")?
                        .iter()
                        .map(|id| {
                            id.as_i64()
                                .filter(|id| *id > 0)
                                .context("blocked_by IDs must be positive integers")
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            if blocked_by.len() > 64 {
                bail!("blocked_by exceeds 64 task IDs");
            }
            json!({ "created": board.create(subject, &blocked_by)? })
        }
        "list" => {
            let ready_only = arguments
                .get("ready_only")
                .map(|value| value.as_bool().context("ready_only must be a boolean"))
                .transpose()?
                .unwrap_or(false);
            let after_id = arguments
                .get("after_id")
                .map(|value| {
                    value
                        .as_i64()
                        .filter(|id| *id >= 0)
                        .context("after_id must be a non-negative integer")
                })
                .transpose()?
                .unwrap_or(0);
            let limit = arguments
                .get("limit")
                .map(|value| {
                    value
                        .as_u64()
                        .filter(|limit| (1..=200).contains(limit))
                        .context("limit must be an integer from 1 to 200")
                })
                .transpose()?
                .unwrap_or(50) as usize;
            return board.list_json(ready_only, after_id, limit);
        }
        "claim" => {
            let (claimed, attempt_id) = board.claim(task_id(arguments)?, caller)?;
            json!({ "claimed": claimed, "actor": caller, "attempt_id": attempt_id })
        }
        "complete" => match board.complete(task_id(arguments)?, caller)? {
            Some((unblocked, truncated)) => json!({
                "completed": true,
                "unblocked": unblocked,
                "unblocked_truncated": truncated,
            }),
            None => json!({
                "completed": false,
                "unblocked": [],
                "unblocked_truncated": false,
            }),
        },
        "fail" => json!({
            "failed": board.fail(task_id(arguments)?, caller)?,
        }),
        _ => bail!("unknown coordination action {action:?}"),
    };
    Ok(serde_json::to_string(&result)?)
}

fn task_id(arguments: &Value) -> Result<i64> {
    arguments
        .get("id")
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
        .context("action requires a positive integer id")
}

fn bounded_error(error: &anyhow::Error) -> String {
    let mut text = format!("{error:#}");
    if text.len() > MAX_TOOL_RESULT_BYTES {
        let mut end = MAX_TOOL_RESULT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

fn append_bounded(target: &mut String, text: &str, limit: usize) -> bool {
    for character in text.chars() {
        if target.len() + character.len_utf8() > limit {
            return true;
        }
        target.push(character);
    }
    false
}

fn signed_in(account: &Value) -> bool {
    !account.pointer("/account").is_none_or(Value::is_null)
}

async fn send_account(events: &mpsc::Sender<UiEvent>, account: &Value) -> Result<()> {
    let label = account.pointer("/account").and_then(|account| {
        let kind = account.get("type")?.as_str()?;
        Some(match kind {
            "chatgpt" => {
                let plan = account
                    .get("planType")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match account.get("email").and_then(Value::as_str) {
                    Some(email) => email.to_owned(),
                    None => format!("ChatGPT {plan}"),
                }
            }
            "apiKey" => "OpenAI API".into(),
            _ => kind.to_owned(),
        })
    });
    events.send(UiEvent::AccountChanged(label)).await?;
    Ok(())
}

fn select_model(models: &Value) -> Result<(String, Vec<String>, usize)> {
    let models = models
        .get("data")
        .and_then(Value::as_array)
        .context("model/list omitted data")?;
    let model = models
        .iter()
        .find(|model| model.get("isDefault").and_then(Value::as_bool) == Some(true))
        .or_else(|| models.first())
        .context("Codex returned no models")?;
    let name = model
        .get("id")
        .and_then(Value::as_str)
        .context("Codex model omitted id")?
        .to_owned();
    let efforts: Vec<String> = model
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| option.get("reasoningEffort")?.as_str().map(str::to_owned))
        .collect();
    let default = model.get("defaultReasoningEffort").and_then(Value::as_str);
    let selected = default
        .and_then(|default| efforts.iter().position(|effort| effort == default))
        .unwrap_or(0);
    Ok((name, efforts, selected))
}
