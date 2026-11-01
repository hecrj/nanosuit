use nanosuit_core as core;

mod diff;
mod font;
mod highlight;
mod item;
mod locale;
mod markdown;
mod repository;
mod tool;
mod widget;

use crate::core::sandbox;
use crate::core::session;
use crate::core::{Config, Project, Session};
use crate::diff::Diff;
use crate::font::Font;
use crate::item::Item;
use crate::markdown::Markdown;
use crate::repository::Repository;
use crate::tool::Tool;
use crate::widget::{context_led, popover};

use iced::gradient;
use iced::keyboard;
use iced::padding;
use iced::task;
use iced::time::{Duration, Instant, every, milliseconds, seconds};
use iced::widget::operation;
use iced::widget::{
    center, column, container, row, rule, scrollable, space, sticky, text, text_editor, toggler,
};
use iced::{Center, Color, Fill, Fit, Shrink, Subscription, Task, Theme, Widget};
use iced_palace::widget::typewriter;

use function::Binary;
use reason::Reason;
use reason::model;

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::process;

/// The flag that runs the app without the bubblewrap sandbox.
const LIVE: &str = "--we-doin-it-live";

/// The flag that resumes a session instead of starting a fresh
/// one: the newest, or the one at the path that follows it.
const RESUME: &str = "--resume";

/// The maximum width of the content area
const MAX_CONTENT_WIDTH: u32 = 770;

fn main() -> Result<(), iced::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing_subscriber::filter::LevelFilter::WARN.into())
                .from_env_lossy(),
        )
        .init();

    let mut args = env::args().skip(1).peekable();

    let mut live = false;
    let mut resume = false;
    let mut resume_path = None;

    while let Some(option) = args.peek().cloned()
        && option.starts_with("--")
    {
        let _ = args.next();

        match option.as_str() {
            LIVE => {
                live = true;
            }
            RESUME => {
                resume = true;
                resume_path = if args.peek().is_some_and(|path| !path.starts_with("--")) {
                    args.next()
                } else {
                    None
                };
            }
            _ => {
                eprintln!("unknown option: {option}");
                process::exit(1);
            }
        }
    }

    let prompt = args.next();

    let project = Project::current_dir().expect("the current directory must be resolvable");

    let config = Config::load(&project)
        .inspect_err(|error| log::error!("configuration failed to load: {error}"))
        .unwrap_or_default();

    // Re-execute under a sandbox, panicking when that is
    // not possible; `--we-doin-it-live` skips the sandbox.
    if live {
        log::warn!("running unsandboxed: --we-doin-it-live was passed");
    } else {
        match sandbox::enter(&config.sandbox, &project) {
            Ok(()) => log::info!("running sandboxed"),
            Err(error) => {
                panic!("sandboxing failed: {error}; pass --we-doin-it-live to run unsandboxed")
            }
        }
    }

    // Fresh by default; `--resume` picks up the newest session,
    // or the one at the path that follows the flag.
    let session = match resume_path {
        Some(path) => session::File::existing(&project, &path).unwrap_or_else(|| {
            eprintln!("no such session: {path}");
            std::process::exit(1);
        }),
        None if resume => {
            session::File::latest(&project).unwrap_or_else(|| session::File::new(&project))
        }
        None => session::File::new(&project),
    };

    let server = reason::Settings {
        url: env::var("NANOSUIT_SERVER_URL")
            .as_deref()
            .unwrap_or("http://127.0.0.1:9931")
            .parse()
            .expect("server url must be valid"),
        api_key: env::var("NANOSUIT_SERVER_API_KEY")
            .ok()
            .map(reason::Key::new),
    };

    iced::application(
        move || Nanosuit::new(&project, prompt.as_deref(), &session, &server),
        Nanosuit::update,
        Nanosuit::view,
    )
    .title("nanosuit")
    .subscription(Nanosuit::subscription)
    .theme(Theme::CatppuccinMocha)
    .font(Font::MONOSPACE)
    .run()
}

struct Nanosuit {
    now: Instant,
    project: Project,
    session: session::File,
    repository: Repository,
    server: reason::Settings,
    tools: BTreeMap<&'static str, Tool>,
    connection: Connection,
    state: State,
    tasks: HashMap<Work, task::Handle>,
    models: BTreeMap<model::Id, reason::Model>,
    model: Option<model::Id>,
    messages: Vec<Item>,
    last_saved: usize,
    input: text_editor::Content,
    mode: Mode,
    heartbeats: usize,
    compact: bool,
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Chat,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Working,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Work {
    Completion,
    Compaction,
    Tool(reason::tool::Id),
}

#[derive(Debug, Clone)]
enum Connection {
    Disconnected,
    Connecting,
    Connected(Reason),
}

#[derive(Debug, Clone)]
enum Message {
    Connected(Result<(Reason, Vec<reason::Model>), reason::Error>),
    ModelsListed(Result<Vec<reason::Model>, reason::Error>),
    Heartbeat,
    SessionLoaded(Result<Session, reason::Error>),
    SessionSaved(Result<usize, reason::Error>),
    InputChanged(text_editor::Action),
    Send,
    ReplyProgressed(reason::Event),
    ReplyFinished(Result<reason::Stamp, reason::Error>),
    CompactionProgressed(reason::Event),
    CompactionReceived(Result<reason::Stamp, reason::Error>),
    ToolProgressed(reason::tool::Id, String),
    ToolFinished(reason::tool::Id, Result<tool::Output, reason::Error>),
    Item(usize, item::Message),
    Abort,
    Repository(repository::Message),
    ToggleCompact(bool),
    Tick,
}

impl Nanosuit {
    fn new(
        project: &Project,
        prompt: Option<&str>,
        session: &session::File,
        server: &reason::Settings,
    ) -> (Self, Task<Message>) {
        let (repository, load_repository) = repository::Repository::new(project.clone());

        let mut nanosuit = Self {
            now: Instant::now(),
            project: project.clone(),
            session: session.clone(),
            repository,
            server: server.clone(),
            tools: Tool::builtins(),
            connection: Connection::Disconnected,
            state: State::Idle,
            tasks: HashMap::new(),
            models: BTreeMap::new(),
            model: None,
            messages: Vec::new(),
            last_saved: 0,
            input: prompt
                .map(text_editor::Content::with_text)
                .unwrap_or_default(),
            mode: Mode::Chat,
            heartbeats: 0,
            compact: true,
        };

        let boot = {
            let load = Task::perform(Session::load(session), Message::SessionLoaded);

            Task::batch([nanosuit.connect(), load])
        };

        (
            nanosuit,
            Task::batch([
                if prompt.is_some() {
                    boot.chain(Task::done(Message::Send))
                } else {
                    boot
                },
                load_repository.map(Message::Repository),
            ]),
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        self.now = Instant::now();

        match message {
            Message::Connected(Ok((reason, models))) => {
                self.connection = Connection::Connected(reason);
                self.update_models(models)
            }
            Message::Heartbeat => {
                self.heartbeats += 1;

                let reconnect = match &self.connection {
                    Connection::Disconnected => self.connect(),
                    Connection::Connected(_) => self.list_models(),
                    Connection::Connecting => Task::none(),
                };

                Task::batch([reconnect, self.save()])
            }
            Message::ModelsListed(Ok(models)) => self.update_models(models),
            Message::SessionLoaded(Ok(session)) => {
                self.messages = session
                    .items
                    .into_iter()
                    .map(|item| Item::from_session(&self.tools, item))
                    .collect();

                self.last_saved = self.messages.len();

                operation::focus("input")
            }
            Message::SessionSaved(Ok(last_saved)) => {
                self.last_saved = last_saved;

                Task::none()
            }
            Message::InputChanged(action) => {
                self.input.perform(action);

                Task::none()
            }
            Message::Send => {
                let message = self.input.text();

                if message.trim().is_empty() {
                    return Task::none();
                }

                self.input = text_editor::Content::new();
                self.messages.push(Item::User(Markdown::new(&message)));

                let work = if self.tasks.is_empty() {
                    self.work()
                } else {
                    self.abort();

                    // Wait for a couple seconds to server slots return to idle
                    // This is necessary for proper reuse of prompt caches
                    Task::future(tokio::time::sleep(seconds(2)))
                        .discard()
                        .chain(self.work())
                };

                Task::batch([
                    work,
                    operation::scrollable::snap_to_end("scroll", operation::Animation::Auto),
                    operation::focus("input"),
                ])
            }
            Message::ReplyProgressed(event) => {
                let Some(Item::Assistant(reply)) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find(|message| !matches!(message, Item::Tool(_)))
                else {
                    return Task::none();
                };

                reply.timings = event.timings;

                match event.delta {
                    reason::Delta::PromptProcessed(progress) => {
                        reply.prompt = progress;

                        Task::none()
                    }
                    reason::Delta::ReasoningChanged(delta) => {
                        reply.reasoning.push_str(&delta);

                        Task::none()
                    }
                    reason::Delta::ContentChanged(delta) => {
                        reply.content.push_str(&delta);

                        Task::none()
                    }
                    reason::Delta::ToolCallsChanged(deltas) => {
                        let mut calls = Vec::new();

                        for delta in deltas {
                            match delta {
                                reason::tool::Delta::CallAdded(call) => {
                                    let last_call = reply.tool_calls.last().cloned();
                                    reply.tool_calls.push(call);

                                    let Some(call) = last_call else {
                                        continue;
                                    };

                                    calls.push(call);
                                }
                                reason::tool::Delta::ArgumentsChanged(delta) => {
                                    let Some(tool_call) = reply.tool_calls.last_mut() else {
                                        continue;
                                    };

                                    tool_call.arguments.push_str(&delta);
                                }
                            }
                        }

                        Task::batch(calls.into_iter().map(|call| self.run(call)))
                    }
                }
            }
            Message::ReplyFinished(Ok(stamp)) => {
                let _ = self.tasks.remove(&Work::Completion);

                if stamp == reason::Stamp::Stop {
                    self.state = State::Idle;
                }

                let Some(reply) = self.last_reply_mut() else {
                    return Task::none();
                };

                if let Some(call) = reply.tool_calls.last().cloned()
                    && stamp != reason::Stamp::LengthExceeded
                {
                    self.run(call)
                } else {
                    self.compact().unwrap_or_default()
                }
            }
            Message::CompactionProgressed(event) => {
                let Some(Item::Compaction(compaction)) = self.messages.last_mut() else {
                    return Task::none();
                };

                let new_tokens = event
                    .timings
                    .zip(compaction.reply.timings)
                    .map(|(now, old)| now.total_tokens().saturating_sub(old.total_tokens()))
                    .unwrap_or(0);

                compaction.reply.timings = event.timings;

                match event.delta {
                    reason::Delta::PromptProcessed(progress) => {
                        compaction.reply.prompt = progress;
                    }
                    reason::Delta::ReasoningChanged(delta) => {
                        compaction.reply.reasoning.push_str(&delta);
                        compaction.reasoning_tokens += new_tokens;
                    }
                    reason::Delta::ContentChanged(delta) => {
                        compaction.reply.content.push_str(&delta);
                        compaction.tokens += new_tokens;
                    }
                    reason::Delta::ToolCallsChanged(_) => {}
                }

                Task::none()
            }
            Message::CompactionReceived(Ok(_stamp)) => {
                let _ = self.tasks.remove(&Work::Compaction);

                // TODO: Handle length exceeded (?)

                let Some(Item::Compaction(compaction)) = self.messages.last_mut() else {
                    return Task::none();
                };

                compaction.reply.timings = Some(reason::Timings::default());
                compaction.is_finished = true;

                log::trace!(
                    "compaction received: content_len={} reasoning_len={}",
                    compaction.reply.content.raw().len(),
                    compaction.reply.reasoning.raw().len(),
                );

                match self.state {
                    State::Idle => Task::none(),
                    State::Working => self.work(),
                }
            }
            Message::ToolProgressed(id, line) => {
                let Some(tool) = self.messages.iter_mut().rev().find_map(|message| {
                    if let Item::Tool(tool) = message
                        && tool.call.id == id
                    {
                        Some(tool)
                    } else {
                        None
                    }
                }) else {
                    return Task::none();
                };

                let tool::Status::Running { logs, .. } = &mut tool.status else {
                    return Task::none();
                };

                logs.push(line);

                Task::none()
            }
            Message::ToolFinished(id, result) => {
                let Some(tool) = self.messages.iter_mut().rev().find_map(|message| {
                    if let Item::Tool(tool) = message
                        && tool.call.id == id
                    {
                        Some(tool)
                    } else {
                        None
                    }
                }) else {
                    return Task::none();
                };

                let _ = self.tasks.remove(&Work::Tool(id));

                let tool::Status::Running { logs, started_at } = &mut tool.status else {
                    return Task::none();
                };

                let duration = self.now.saturating_duration_since(*started_at);

                tool.status = match result {
                    Ok(output) => tool::Status::Success { output, duration },
                    Err(error) => tool::Status::Error {
                        error: error.to_string(),
                        output: tool::Output::from_iter(std::mem::take(logs)),
                        duration,
                    },
                };

                if self.tasks.is_empty() {
                    Task::batch([
                        self.work(),
                        self.repository.status().map(Message::Repository),
                    ])
                } else {
                    Task::none()
                }
            }
            Message::Abort => {
                self.abort();

                Task::none()
            }
            Message::Item(i, message) => {
                let Some(item) = self.messages.get_mut(i) else {
                    return Task::none();
                };

                item.update(message).map(Message::Item.with(i))
            }
            Message::Connected(Err(error)) | Message::ModelsListed(Err(error)) => {
                self.connection = Connection::Disconnected;
                self.abort();

                log::debug!("{error}");

                Task::none()
            }
            Message::ReplyFinished(Err(error)) | Message::CompactionReceived(Err(error)) => {
                self.abort();

                log::error!("{error}");

                Task::none()
            }
            Message::SessionLoaded(Err(error)) => {
                log::error!("session failed to load: {error}");

                Task::none()
            }
            Message::SessionSaved(Err(error)) => {
                log::warn!("session failed to save: {error}");

                Task::none()
            }
            Message::Repository(message) => match self.repository.update(message) {
                repository::Action::None => Task::none(),
                repository::Action::Run(task) => task.map(Message::Repository),
                repository::Action::Review(review) => {
                    self.messages.push(Item::Review(review));

                    self.mode = Mode::Chat;

                    self.abort();

                    Task::batch([
                        operation::scrollable::snap_to_end("scroll", operation::Animation::Instant),
                        self.work(),
                    ])
                }
                repository::Action::ToggleReview => {
                    if matches!(self.mode, Mode::Review) {
                        self.mode = Mode::Chat;

                        operation::scrollable::snap_to_end("scroll", operation::Animation::Instant)
                    } else {
                        self.mode = Mode::Review;

                        self.repository.diff().map(Message::Repository)
                    }
                }
            },
            Message::ToggleCompact(compact) => {
                self.compact = compact;

                Task::none()
            }
            Message::Tick => Task::none(),
        }
    }

    fn connect(&mut self) -> Task<Message> {
        self.connection = Connection::Connecting;

        Task::perform(Reason::connect(self.server.clone()), Message::Connected)
    }

    fn list_models(&self) -> Task<Message> {
        let Connection::Connected(reason) = self.connection.clone() else {
            return Task::none();
        };

        Task::perform(
            async move { reason.list_models().await },
            Message::ModelsListed,
        )
    }

    fn update_models(&mut self, models: Vec<reason::Model>) -> Task<Message> {
        self.models = models
            .into_iter()
            .map(|model| (model.id.clone(), model))
            .collect();

        if self
            .model
            .as_ref()
            .is_none_or(|model| !self.models.contains_key(model))
        {
            self.model = self.models.keys().next().cloned();
        }

        Task::none()
    }

    fn work(&mut self) -> Task<Message> {
        use iced::task::{Sipper, sipper};

        if !self.tasks.is_empty() {
            return Task::none();
        }

        self.state = State::Working;

        if let Some(compact) = self.compact() {
            return compact;
        }

        let Connection::Connected(reason) = &self.connection else {
            return Task::none();
        };

        let Some(model) = &self.model else {
            return Task::none();
        };

        let reason = reason.clone();
        let model = model.clone();

        let messages: Vec<_> = self
            .opener()
            .chain(self.context().iter().filter_map(Item::to_message))
            .collect();

        for message in &messages {
            log::trace!("{message:?}");
        }

        let tools: Vec<_> = self.tools().collect();

        let (reply, handle) = Task::sip(
            sipper(async move |sender| {
                reason.complete(&model, &messages, &tools).run(sender).await
            }),
            Message::ReplyProgressed,
            Message::ReplyFinished,
        )
        .abortable();

        self.tasks.insert(Work::Completion, handle.abort_on_drop());
        self.messages.push(Item::Assistant(item::Reply::default()));

        reply
    }

    fn run(&mut self, call: reason::tool::Call) -> Task<Message> {
        let state = match self.tools.get(call.name.as_str()) {
            Some(tool) => tool.parse(&call.arguments),
            None => {
                Err(std::io::Error::other(format!("unknown tool: {name}", name = call.name)).into())
            }
        };

        let (run, status) = match &state {
            Ok(state) => {
                let run = state.run(self.project.as_ref());

                let (run, handle) = Task::sip(
                    run,
                    Message::ToolProgressed.with(call.id.clone()),
                    Message::ToolFinished.with(call.id.clone()),
                )
                .abortable();

                self.tasks
                    .insert(Work::Tool(call.id.clone()), handle.abort_on_drop());

                (
                    run,
                    tool::Status::Running {
                        logs: Vec::new(),
                        started_at: Instant::now(),
                    },
                )
            }
            Err(_error) => (Task::none(), tool::Status::Invalid),
        };

        self.messages.push(Item::Tool(tool::Run {
            call,
            state,
            status,
        }));

        run
    }

    fn compact(&mut self) -> Option<Task<Message>> {
        use iced::task::{Sipper, sipper};

        /// Minimum amount of tokens compaction needs
        const COMPACTION_MIN_TOKENS: u64 = 4_000;
        /// Maximum amount of tokens compaction needs
        const COMPACTION_MAX_TOKENS: u64 = 10_000;
        /// Factor of context from last message to keep intact
        const CONTINUITY_CONTEXT: f32 = 0.2;

        const COMPACTION_PROMPT: &str = r#"The earlier part of this conversation is about to be removed to free up context.
Your next message will be the only one to remain, at the start of the conversation for the rest of the session.

Write a concise state handoff so the work can continue seamlessly without the original messages. Cover:
- The user's goal, and any explicit constraints or preferences
- Key decisions and their rationale (including rejected alternatives)
- Current state: files created or modified and why, what is done, what is in progress
- Errors encountered and how they were resolved or to be avoided
- Open questions and the immediate next steps
Omit chit-chat, raw tool output, and failed experiments (keep only the lesson).
If the conversation above already contains a previous summary, merge it into this one; the result must be self-contained.
Reply with only the summary, under 500 words. You cannot use any tools."#;

        let Connection::Connected(reason) = &self.connection else {
            return None;
        };

        let model = self.model.as_ref()?;
        let context_size = self.context_size()?;
        let timings = self.timings()?;

        let total_tokens = timings.total_tokens();
        let context_left = context_size.saturating_sub(total_tokens);
        let budget = (context_size / 8).clamp(COMPACTION_MIN_TOKENS, COMPACTION_MAX_TOKENS);

        if context_left > budget {
            return None;
        }

        let tokens_to_keep =
            ((total_tokens as f32 * CONTINUITY_CONTEXT).round() as u64).max(budget / 2);
        let context = self.context();

        // Find target reply
        let target = context
            .iter()
            .rev()
            .position(|item| {
                let Item::Assistant(item::Reply {
                    timings: Some(timings),
                    ..
                }) = item
                else {
                    return false;
                };

                log::trace!(
                    "compact candidate: total={} reply={}",
                    total_tokens,
                    timings.total_tokens()
                );

                total_tokens.saturating_sub(timings.total_tokens()) >= tokens_to_keep
            })
            .unwrap_or(context.len());

        // Compact tools as well
        let tools = context[context.len() - target..]
            .iter()
            .take_while(|item| matches!(item, Item::Tool(_)))
            .count();

        let end = context.len() - target + tools;

        log::debug!(
            "compact: total_tokens={} context_left={} context.len={} target={} end={}",
            total_tokens,
            context_left,
            context.len(),
            target,
            end
        );

        let prefix: Vec<_> = self
            .opener()
            .chain(context[..end].iter().filter_map(Item::to_message))
            .chain([reason::Message::User(COMPACTION_PROMPT.to_owned())])
            .collect();

        let reason = reason.clone();
        let model = model.clone();
        let tools: Vec<_> = self.tools().collect();

        self.messages.push(Item::Compaction(item::Compaction {
            reply: item::Reply::default(),
            tokens: 0,
            reasoning_tokens: 0,
            to: (self.messages.len() - context.len()) + end,
            is_finished: false,
        }));

        let (reply, handle) = Task::sip(
            sipper(async move |sender| reason.complete(&model, &prefix, &tools).run(sender).await),
            Message::CompactionProgressed,
            Message::CompactionReceived,
        )
        .abortable();

        self.tasks.insert(Work::Compaction, handle.abort_on_drop());

        Some(reply)
    }

    fn abort(&mut self) {
        self.state = State::Idle;
        self.tasks.clear();

        for message in &mut self.messages {
            if let Item::Tool(tool) = message
                && let tool::Status::Running { logs, started_at } = &mut tool.status
            {
                tool.status = tool::Status::Aborted {
                    output: tool::Output::from_iter(std::mem::take(logs)),
                    duration: self.now.saturating_duration_since(*started_at),
                };
            }
        }

        let index = self.messages.iter().rposition(|message| {
            matches!(
                message,
                Item::Compaction(item::Compaction {
                    is_finished: false,
                    ..
                })
            )
        });

        if let Some(index) = index {
            self.messages.remove(index);
        }
    }

    fn save(&self) -> Task<Message> {
        let new_events: Vec<_> = self
            .messages
            .iter()
            .enumerate()
            .skip(self.last_saved)
            .take_while(|(i, item)| match item {
                Item::User(_) => true,
                Item::Assistant(_) => i + 1 != self.messages.len() || self.tasks.is_empty(),
                Item::Tool(tool_run) => !matches!(tool_run.status, tool::Status::Running { .. }),
                Item::Compaction(compaction) => compaction.is_finished,
                Item::Review(_) => true,
            })
            .map(|(_, item)| item.to_session())
            .map(session::Event::ItemAdded)
            .collect();

        if new_events.is_empty() {
            return Task::none();
        }

        let session = self.session.clone();
        let new_last_saved = self.last_saved + new_events.len();

        Task::perform(
            async move {
                Session::append(&session, new_events).await?;

                Ok(new_last_saved)
            },
            Message::SessionSaved,
        )
    }

    fn view(&self) -> impl Widget<Message> {
        match self.mode {
            Mode::Chat => self.chat().boxed(),
            Mode::Review => self.review().boxed(),
        }
    }

    fn review(&self) -> impl Widget<Message> {
        let footer = sticky(
            container(
                column![
                    self.repository
                        .message()
                        .map(|message| message.map(Message::Repository)),
                    self.status_bar(),
                ]
                .spacing(10),
            )
            .padding(10)
            .style(|theme: &Theme| {
                container::Style::default().background(
                    gradient::Linear::new(0)
                        .add_stop(0.9, theme.seed().background.scale_alpha(0.9))
                        .add_stop(1.0, Color::TRANSPARENT),
                )
            }),
        );

        container(
            scrollable(column![
                self.repository.review().map(Message::Repository),
                footer
            ])
            .width(Fill)
            .height(Fill)
            .spacing(10)
            .padding(10),
        )
        .padding([0, 10])
    }

    fn chat(&self) -> impl Widget<Message> {
        let input = text_editor(&self.input)
            .id("input")
            .height(Fit.max(600))
            .padding(10)
            .placeholder("Type your query here...")
            .on_action(Message::InputChanged)
            .key_binding(|key_press| {
                if key_press.key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                    return Some(text_editor::Binding::Custom(Message::Abort));
                }

                if !key_press.is_focused {
                    return None;
                }

                if key_press.key == keyboard::Key::Named(keyboard::key::Named::Enter)
                    && !key_press.modifiers.shift()
                {
                    return Some(text_editor::Binding::Custom(Message::Send));
                }

                text_editor::Binding::from_key_press(key_press)
            });

        let footer = container(column![
            self.active_sequence()
                .and_then(|sequence| sequence
                    .status(self.now, &self.messages, true)
                    .map(|status| container(status).padding(padding::bottom(10)).boxed()))
                .unwrap_or_else(|| space().boxed()),
            column![input, self.status_bar()].spacing(10)
        ])
        .style(|theme: &Theme| {
            container::Style::default().background(
                gradient::Linear::new(0)
                    .add_stop(0.2, theme.seed().background)
                    .add_stop(0.8, theme.seed().background.scale_alpha(0.7))
                    .add_stop(1.0, Color::TRANSPARENT),
            )
        })
        .padding(padding::top(20).bottom(10));

        if self.messages.is_empty() {
            const TITLES: &[&str] = &[
                "Ready when you are.",
                "What are we building?",
                "Just say the word.",
            ];

            return center(
                column![
                    typewriter(TITLES[(self.heartbeats / 6) % TITLES.len()])
                        .size(font::TITLE)
                        .font(Font {
                            weight: font::Weight::Bold,
                            ..Font::MONOSPACE
                        }),
                    footer
                ]
                .align_x(Center)
                .width(Fit.max(MAX_CONTENT_WIDTH as f32 * 0.7)),
            )
            .padding([0, 10])
            .boxed();
        }

        container(
            scrollable(center(
                column![
                    if self.compact {
                        column(item::sequences(&self.messages).map(|sequence| {
                            sequence.compact(
                                self.now,
                                &self.project,
                                &self.messages,
                                self.state == State::Working,
                                Message::Item,
                            )
                        }))
                        .spacing(20)
                        .padding(padding::top(10))
                        .boxed()
                    } else {
                        column(item::sequences(&self.messages).map(|sequence| {
                            sequence.full(
                                self.now,
                                &self.project,
                                &self.messages,
                                self.state == State::Working,
                                Message::Item,
                            )
                        }))
                        .spacing(20)
                        .padding(padding::top(10))
                        .boxed()
                    },
                    space::vertical(),
                    sticky(footer),
                ]
                .width(Fit.max(MAX_CONTENT_WIDTH)),
            ))
            .id("scroll")
            .width(Fill)
            .height(Fill)
            .on_scroll(widget::snap.with(operation::Animation::Auto))
            .spacing(20)
            .padding(10),
        )
        .padding([0, 10])
        .boxed()
    }

    fn status_bar(&self) -> impl Widget<Message> {
        let repository = self
            .repository
            .summary()
            .map(|summary| summary.map(Message::Repository));

        let server = {
            let timings = self.timings();

            let info = timings.map(|timings| {
                row![
                    (timings.prompt.token > Duration::ZERO).then(|| {
                        text!(
                            "{tokens_per_second:0.2}↑",
                            tokens_per_second = 1.0 / timings.prompt.token.as_secs_f64(),
                        )
                        .style(text::secondary)
                        .size(font::SMALL)
                    }),
                    (timings.predicted.token > Duration::ZERO).then(|| {
                        text!(
                            "↓{tokens_per_second:0.2}",
                            tokens_per_second = 1.0 / timings.predicted.token.as_secs_f64(),
                        )
                        .style(text::success)
                        .size(font::SMALL)
                    })
                ]
                .spacing(10)
            });

            let models = if let Some(model) = self.model.as_ref() {
                text(model.as_str())
            } else {
                text("No models found!")
            }
            .size(font::SMALL)
            .width(Fit.max(200))
            .wrapping(text::Wrapping::None)
            .ellipsis(text::Ellipsis::End)
            .style(|theme: &Theme| {
                let palette = theme.seed();

                text::Style {
                    color: match &self.connection {
                        Connection::Disconnected => Some(palette.danger),
                        Connection::Connecting => Some(palette.warning),
                        Connection::Connected(_) => None,
                    },
                    selection: None,
                }
            });

            let context = context_led(self.context_size(), timings);

            row![info, models, context].spacing(10).align_y(Center)
        };

        let menu = popover(
            || {
                text(self.project.to_string())
                    .size(font::SMALL)
                    .line_height(1.0)
            },
            || {
                column![
                    toggler(self.compact)
                        .label("Compact")
                        .size(font::TINY)
                        .text_size(font::SMALL)
                        .on_toggle(Message::ToggleCompact),
                    rule::horizontal(1).style(rule::weak),
                    text!("nanosuit {}", env!("CARGO_PKG_VERSION"))
                        .size(font::TINY)
                        .style(text::secondary)
                ]
                .width(Shrink)
                .spacing(10)
            },
        );

        row![menu, repository, space::horizontal(), server,]
            .align_y(Center)
            .spacing(10)
    }

    fn subscription(&self) -> Subscription<Message> {
        let heartbeat = every(seconds(10)).map(|_| Message::Heartbeat);

        let tick = if self.state == State::Working {
            every(milliseconds(100)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        };

        Subscription::batch([heartbeat, tick])
    }

    fn context_size(&self) -> Option<u64> {
        self.model
            .as_ref()
            .and_then(|model| self.models.get(model))
            .and_then(|model| model.context_size)
    }

    fn context(&self) -> &[Item] {
        let start = self
            .last_compaction()
            .map(|compaction| compaction.to)
            .unwrap_or_default();

        &self.messages[start..]
    }

    fn last_reply_mut(&mut self) -> Option<&mut item::Reply> {
        let Some(Item::Assistant(reply)) = self
            .messages
            .iter_mut()
            .rev()
            .find(|message| !matches!(message, Item::Tool(_)))
        else {
            return None;
        };

        Some(reply)
    }

    fn last_compaction(&self) -> Option<&item::Compaction> {
        self.messages.iter().rev().find_map(|item| {
            let Item::Compaction(
                compaction @ item::Compaction {
                    is_finished: true, ..
                },
            ) = item
            else {
                return None;
            };

            Some(compaction)
        })
    }

    /// The system prompt, the compaction summary — if any — and the
    /// most recent user message before the compaction cutoff, the
    /// opener of the turn the boundary falls in, so the model retains
    /// the verbatim request that the summary only paraphrases.
    fn opener(&self) -> impl Iterator<Item = reason::Message> {
        const SYSTEM_PROMPT: &str = "You are an expert coding assistant. \
            The user wants your help to develop a project in the current directory.";

        let start = self.messages.len() - self.context().len();

        let last_user_turn = if let Some(Item::Assistant(_) | Item::Compaction(_)) =
            self.messages.get(start)
            && let Some(Item::User(markdown)) = self.messages[..start]
                .iter()
                .rev()
                .find(|item| matches!(item, Item::User(_)))
        {
            Some(reason::Message::User(markdown.raw().to_owned()))
        } else {
            None
        };

        let last_compaction = self.last_compaction().map(|compaction| {
            reason::Message::Assistant(reason::Reply {
                content: compaction.reply.content.raw().to_owned(),
                ..reason::Reply::default()
            })
        });

        [reason::Message::System(SYSTEM_PROMPT.to_owned())]
            .into_iter()
            .chain(last_compaction)
            .chain(last_user_turn)
    }

    fn tools(&self) -> impl Iterator<Item = reason::Tool> {
        self.tools.values().map(Tool::to_metadata)
    }

    fn timings(&self) -> Option<reason::Timings> {
        self.context().iter().rev().find_map(|item| {
            if let Item::Assistant(reply) | Item::Compaction(item::Compaction { reply, .. }) = item
            {
                reply.timings
            } else {
                None
            }
        })
    }

    fn active_sequence(&self) -> Option<item::Sequence> {
        if self.state != State::Working {
            return None;
        }

        Some(item::Sequence::last(&self.messages))
    }
}
