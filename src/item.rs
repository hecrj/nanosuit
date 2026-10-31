mod conversion;

use crate::core::Project;
use crate::diff;
use crate::font;
use crate::locale;
use crate::markdown::{self, Markdown};
use crate::repository;
use crate::tool;
use crate::widget::{self, arrow, collapsible};

use iced::border;
use iced::padding;
use iced::time;
use iced::widget::operation;
use iced::widget::{
    Text, center_x, column, container, progress_bar, right, row, rule, scrollable, text,
};
use iced::{Center, Fill, Fit, Font, Pixels, Shrink, Task, Theme, Widget, never};

use function::Binary;

pub enum Item {
    User(Markdown),
    Assistant(Reply),
    Tool(ToolRun),
    Review(repository::Review),
    Compaction(Compaction),
}

#[derive(Debug, Clone)]
pub enum Message {
    LinkClicked(markdown::Uri),
}

impl Item {
    pub fn to_message(&self) -> Option<reason::Message> {
        Some(match self {
            Item::User(markdown) => reason::Message::User(markdown.raw().to_owned()),
            Item::Assistant(reply) => reason::Message::Assistant(reason::Reply {
                reasoning: reply.reasoning.raw().to_owned(),
                content: reply.content.raw().to_owned(),
                tool_calls: reply.tool_calls.clone(),
            }),
            Item::Tool(tool) => reason::Message::Tool(reason::tool::Response {
                id: tool.call.id.clone(),
                content: tool.status.content().unwrap_or_default(),
            }),
            Item::Review(review) => reason::Message::User(review.prompt()),
            Item::Compaction { .. } => return None,
        })
    }

    pub fn duration(&self) -> Option<time::Duration> {
        match self {
            Item::Assistant(reply) => {
                let timings = reply.timings?;

                Some(timings.prompt.total + timings.predicted.total)
            }
            Item::Tool(_) => None, // TODO!
            Item::Compaction(compaction) => {
                let timings = compaction.reply.timings?;

                Some(timings.prompt.total + timings.predicted.total)
            }
            Item::User(_) | Item::Review(_) => None,
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::LinkClicked(uri) => {
                // TODO
                log::debug!("{uri}");
                Task::none()
            }
        }
    }

    pub fn view<'a>(&'a self, project: &'a Project, compact: bool) -> impl Widget<Message> + 'a {
        match self {
            Item::Assistant(reply) => {
                let reasoning = if !compact && !reply.reasoning.is_empty() {
                    Some(reasoning(reply, compact))
                } else {
                    None
                };

                column![
                    upload(reply, false),
                    reasoning,
                    (!reply.content.is_empty()).then(|| markdown::view(
                        reply.content.items(),
                        Font::DEFAULT,
                        font::NORMAL
                    )
                    .map(Message::LinkClicked)),
                ]
                .spacing(15)
                .boxed()
            }
            Item::User(message) => right(
                container(
                    markdown::view(message.items(), Font::DEFAULT, font::NORMAL)
                        .map(Message::LinkClicked),
                )
                .padding(10)
                .style(container::rounded_box),
            )
            .boxed(),
            Item::Tool(tool) => {
                const TOOL_LOG_LINE_HEIGHT: f32 = 18.0;
                const MAX_TOOL_LOG_HEIGHT: f32 = TOOL_LOG_LINE_HEIGHT * 10.0 + 5.0 * 9.0; // 10 lines: 10 × 18px + 9 × 5px spacing

                let header = |_open| {
                    let label = container(text(&tool.call.name).size(font::SMALL))
                        .padding([2, 5])
                        .style(container::dark);

                    let title = tool.state.as_ref().ok().and_then(|state| {
                        Some(text!("{}", state.title(project)?).size(font::SMALL).style(
                            |theme: &Theme| text::Style {
                                color: match &tool.status {
                                    Status::Running { .. } => Some(theme.seed().warning),
                                    Status::Success { .. } => None,
                                    Status::Error { .. }
                                    | Status::Aborted { .. }
                                    | Status::Invalid => Some(theme.seed().danger),
                                },
                                selection: None,
                            },
                        ))
                    });

                    row![label, title].width(Fill).spacing(10).align_y(Center)
                };

                let content = || {
                    let arguments = match &tool.state {
                        Ok(state) => state
                            .view()
                            .map(|state| {
                                container(state.map(never)).width(Fill).style(|_theme| {
                                    container::Style::default().background(tool::BACKGROUND)
                                })
                            })
                            .boxed(),
                        Err(error) => text!("{error}")
                            .size(font::SMALL)
                            .style(text::danger)
                            .boxed(),
                    };

                    let output = match &tool.status {
                        Status::Running { logs, .. } => Some(
                            scrollable(
                                column(logs.iter().map(|line| {
                                    text(&line[..line.floor_char_boundary(200)])
                                        .wrapping(text::Wrapping::None)
                                        .ellipsis(text::Ellipsis::End)
                                        .size(font::SMALL)
                                        .line_height(Pixels(TOOL_LOG_LINE_HEIGHT))
                                }))
                                .spacing(5),
                            )
                            .id(tool.call.id.as_str().to_owned())
                            .width(Fill)
                            .height(Fit.max(MAX_TOOL_LOG_HEIGHT))
                            .on_scroll(widget::snap.with(operation::Animation::Auto))
                            .spacing(10)
                            .boxed(),
                        ),
                        Status::Success { output } if output.lines() > 0 => {
                            Some(truncate(output).boxed())
                        }
                        Status::Error { error, output } if output.lines() > 0 => Some(
                            column![truncate(output), text!("[{error}]").size(font::SMALL)]
                                .spacing(5)
                                .boxed(),
                        ),
                        Status::Aborted { output } if output.lines() > 0 => Some(
                            column![
                                truncate(output),
                                text("[execution aborted]").size(font::SMALL)
                            ]
                            .spacing(5)
                            .boxed(),
                        ),
                        Status::Success { .. }
                        | Status::Error { .. }
                        | Status::Aborted { .. }
                        | Status::Invalid => tool.status.content().map(|content| {
                            let trimmed = content.trim();

                            text(if trimmed.is_empty() {
                                "[No output]".to_owned()
                            } else {
                                trimmed.to_owned()
                            })
                            .size(font::SMALL)
                            .boxed()
                        }),
                    };

                    let output = output.map(|output| {
                        container(output)
                            .width(Fill)
                            .padding(10)
                            .style(|theme: &Theme| {
                                let palette = theme.seed();

                                let color = match tool.status {
                                    Status::Running { .. } => palette.warning,
                                    Status::Success { .. } => palette.success.scale_alpha(0.5),
                                    Status::Invalid
                                    | Status::Aborted { .. }
                                    | Status::Error { .. } => palette.danger,
                                };

                                let mut style = container::dark(theme);
                                style.border = style.border.color(color).width(1);
                                style
                            })
                    });

                    column![
                        arguments,
                        output.map(|output| container(output).padding(padding::all(10).top(0)))
                    ]
                    .spacing(10)
                };

                container(collapsible(
                    !compact,
                    move |open| header(open).padding(10),
                    content,
                ))
                .width(Fill)
                .padding(1)
                .style(container::bordered_box)
                .boxed()
            }
            Item::Review(review) => {
                let message = review
                    .message
                    .as_ref()
                    .map(|message| {
                        right(
                            container(
                                markdown::view(message.items(), Font::DEFAULT, font::NORMAL)
                                    .map(Message::LinkClicked),
                            )
                            .padding(10)
                            .style(container::rounded_box),
                        )
                    })
                    .boxed();

                let comments = review.comments.iter().map(|comment| {
                    let header = {
                        let label = container(text("review").size(font::SMALL))
                            .padding([2, 5])
                            .style(container::dark);

                        let title = text(comment.index.to_string()).size(font::SMALL);

                        row![label, title].spacing(10).padding(10).align_y(Center)
                    };

                    container(column![
                        header,
                        container(column(comment.hunk.view().map(diff::View::view)).map(never),)
                            .style(|_theme| {
                                container::Style::default().background(tool::BACKGROUND)
                            }),
                        container(
                            markdown::view(comment.content.items(), Font::DEFAULT, font::NORMAL)
                                .map(Message::LinkClicked),
                        )
                        .padding(10),
                    ])
                    .width(Fill)
                    .padding(1)
                    .style(container::bordered_box)
                    .boxed()
                });

                column([message].into_iter().chain(comments))
                    .spacing(10)
                    .boxed()
            }
            Item::Compaction(compaction) => {
                let notice = center_x(text(if compaction.is_finished {
                    format!("Compacted into {} tokens", compaction.tokens)
                } else if compaction.reply.content.is_empty() {
                    format!("Analyzing... {} tokens", compaction.reasoning_tokens)
                } else {
                    format!("Compacting... {} tokens", compaction.tokens)
                }));

                column![upload(&compaction.reply, false), notice]
                    .spacing(10)
                    .boxed()
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Reply {
    pub prompt: reason::Progress,
    pub reasoning: Markdown,
    pub content: Markdown,
    pub tool_calls: Vec<reason::tool::Call>,
    pub timings: Option<reason::Timings>,
}

pub struct ToolRun {
    pub call: reason::tool::Call,
    pub state: Result<Box<dyn tool::Call>, reason::Error>,
    pub status: Status,
}

#[derive(Debug)]
pub enum Status {
    Running { logs: Vec<String> },
    Success { output: tool::Output },
    Error { error: String, output: tool::Output },
    Aborted { output: tool::Output },
    Invalid,
}

impl Status {
    fn content(&self) -> Option<String> {
        match self {
            Status::Running { .. } => None, // Never send intermediate progress
            Status::Success { output } => Some(output.to_string()),
            Status::Error { error, output } => Some(format!("{output}\n[{error}]")),
            Status::Aborted { output } => Some(format!("{output}\n[execution aborted]")),
            Status::Invalid => Some("[invalid tool call]".to_owned()),
        }
    }
}

pub struct Compaction {
    pub reply: Reply,
    pub tokens: u64,
    pub reasoning_tokens: u64,
    pub to: usize,
    pub is_finished: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Sequence {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy)]
pub enum Turn {
    Standalone(usize),
    Work { start: usize, end: usize },
}

impl Sequence {
    pub fn last(items: &[Item]) -> Self {
        let start = items
            .iter()
            .rposition(Self::is_breakpoint)
            .unwrap_or_default();

        Self {
            start,
            end: items.len(),
        }
    }

    pub fn status<Message>(self, items: &[Item], is_active: bool) -> Option<impl Widget<Message>> {
        let total = total_elapsed(&items[self.start..self.end]);

        (total > time::Duration::ZERO).then(|| {
            let rule = move || {
                rule::horizontal(1).style(move |theme: &Theme| rule::Style {
                    color: if is_active {
                        theme.seed().primary
                    } else {
                        theme.palette().background.weakest.color
                    },
                    ..rule::default(theme)
                })
            };

            row![
                rule(),
                if is_active {
                    text!("Working... ({})", duration(total)).style(text::primary)
                } else {
                    text!("Worked for {}", duration(total)).style(text::secondary)
                }
                .font(font::BOLD)
                .size(font::TINY),
                rule()
            ]
            .spacing(20)
            .align_y(Center)
        })
    }

    pub fn compact<'a, T: Clone + 'static>(
        self,
        project: &'a Project,
        items: &'a [Item],
        is_working: bool,
        to_message: impl Fn(usize, Message) -> T + Copy + Send + 'static,
    ) -> impl Widget<T> + 'a {
        let window = &items[self.start..self.end];
        let is_active = is_working && self.end == items.len();

        column![
            column(turns(window).map(|turn| turn.view(project, window, is_active, to_message)))
                .spacing(20),
            if is_active {
                None
            } else {
                self.status(items, false)
            },
        ]
        .spacing(10)
    }

    pub fn full<'a, T: 'static>(
        self,
        project: &'a Project,
        items: &'a [Item],
        is_working: bool,
        to_message: impl Fn(usize, Message) -> T + Copy + Send + 'static,
    ) -> impl Widget<T> + 'a {
        let window = &items[self.start..self.end];
        let is_active = is_working && self.end == items.len();

        column![
            column(window.iter().enumerate().map(|(i, item)| {
                item.view(project, false)
                    .map(to_message.with(self.start + i))
            }))
            .spacing(20),
            if is_active {
                None
            } else {
                self.status(items, false)
            },
        ]
        .spacing(10)
    }

    fn is_breakpoint(item: &Item) -> bool {
        match item {
            Item::User(_) | Item::Review(_) => true,
            Item::Assistant(_) | Item::Tool(_) | Item::Compaction(_) => false,
        }
    }
}

impl Turn {
    fn view<'a, T: Clone + 'static>(
        self,
        project: &'a Project,
        messages: &'a [Item],
        is_active: bool,
        to_message: impl Fn(usize, Message) -> T + Copy + Send + 'static,
    ) -> impl Widget<T> + 'a {
        match self {
            Turn::Standalone(i) => messages[i]
                .view(project, true)
                .map(to_message.with(i))
                .boxed(),
            Turn::Work { start, end } => {
                let reply = if let Some(item @ Item::Assistant(reply)) = messages.get(end - 1)
                    && !reply.content.is_empty()
                {
                    Some(item.view(project, true).map(to_message.with(end - 1)))
                } else {
                    None
                };

                let items = move || {
                    column(
                        messages[start..end]
                            .iter()
                            .enumerate()
                            .filter_map(|(i, item)| {
                                Some(match item {
                                    Item::Assistant(reply) => column![
                                        upload(reply, i == 0),
                                        (!reply.reasoning.is_empty())
                                            .then(|| reasoning(reply, true)),
                                    ]
                                    .spacing(10)
                                    .map(to_message.with(start + i))
                                    .boxed(),
                                    Item::Tool(_) => item
                                        .view(project, true)
                                        .map(to_message.with(start + i))
                                        .boxed(),
                                    _ => return None,
                                })
                            }),
                    )
                };

                let group = if start + 1 == end {
                    items().boxed()
                } else {
                    let summary = summary(project, &messages[start..end]);

                    collapsible(
                        is_active && messages.len() == end && reply.is_none(),
                        move |open| {
                            container(
                                text!("{}  {}", summary, arrow(open))
                                    .size(font::SMALL)
                                    .font(font::BOLD),
                            )
                            .width(Fill)
                            .padding(10)
                            .style(|theme| container::Style {
                                text_color: None,
                                ..container::bordered_box(theme)
                            })
                        },
                        move || {
                            row![
                                rule::vertical(2).style(rule::weak),
                                items().padding(padding::top(10)).spacing(10)
                            ]
                            .spacing(10)
                            .padding(padding::left(10))
                            .height(Shrink)
                        },
                    )
                    .boxed()
                };

                column![group, reply].spacing(20).boxed()
            }
        }
    }
}

pub fn sequences(items: &[Item]) -> impl Iterator<Item = Sequence> {
    let mut items = items.iter().peekable();
    let mut end = 0;

    std::iter::from_fn(move || {
        let start = end;

        Some(loop {
            let item = items.next()?;
            end += 1;

            if Sequence::is_breakpoint(item) {
                break Sequence { start, end };
            } else {
                let next = items.peek().copied();

                if next.is_none_or(Sequence::is_breakpoint) {
                    break Sequence { start, end };
                }
            }
        })
    })
}

pub fn turns(items: &[Item]) -> impl Iterator<Item = Turn> {
    let mut items = items.iter().peekable();
    let mut i = 0;

    std::iter::from_fn(move || {
        let start = i;

        Some(loop {
            let item = items.next()?;
            i += 1;

            match item {
                Item::User(_) | Item::Review(_) | Item::Compaction(_) => {
                    break Turn::Standalone(start);
                }
                Item::Assistant(reply) if !reply.content.is_empty() => {
                    break Turn::Work { start, end: i };
                }
                Item::Assistant(_) | Item::Tool(_) => {
                    let next = items.peek();

                    match next {
                        None | Some(Item::User(_) | Item::Review(_) | Item::Compaction(_)) => {
                            break Turn::Work { start, end: i };
                        }
                        _ => {}
                    }
                }
            }
        })
    })
}

pub fn total_elapsed<'a>(items: impl IntoIterator<Item = &'a Item>) -> time::Duration {
    items.into_iter().filter_map(Item::duration).sum()
}

pub fn duration(duration: time::Duration) -> String {
    let seconds = duration.as_secs_f64();

    if seconds < 1.0 {
        format!("{}ms", duration.as_millis())
    } else if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else {
        let total = seconds.round() as u64;
        let (hours, minutes) = (total / 3600, (total % 3600) / 60);
        let seconds = total % 60;

        if hours > 0 {
            format!("{hours}h {minutes}m {seconds}s")
        } else {
            format!("{minutes}m {seconds}s")
        }
    }
}

pub fn summary(project: &Project, messages: &[Item]) -> String {
    let mut reasoning = time::Duration::ZERO;
    let mut command = String::new();
    let mut commands = 0;
    let mut reads = 0;
    let mut edits = 0;
    let mut writes = 0;

    for item in messages {
        match item {
            Item::Assistant(reply) if let Some(timings) = reply.timings => {
                reasoning += timings.reasoning;
            }
            Item::Tool(run) => match run.call.name.as_str() {
                "bash" => {
                    if command.is_empty()
                        && let Ok(state) = &run.state
                        && let Some(title) = state.title(project)
                    {
                        command = title.into_owned();
                    }

                    commands += 1
                }
                "read" => reads += 1,
                "edit" => edits += 1,
                "write" => writes += 1,
                _ => {}
            },
            _ => {}
        }
    }

    let inflect = |word: &str, count| {
        if count == 1 {
            word.to_owned()
        } else {
            format!("{word}s")
        }
    };

    let mut summary = [
        (commands > 0).then(|| {
            if commands == 1 && !command.is_empty() {
                command
            } else {
                format!("ran {commands} {}", inflect("command", commands))
            }
        }),
        (reads > 0).then(|| format!("read {reads} {}", inflect("file", reads))),
        (edits > 0).then(|| format!("edited {edits} {}", inflect("file", edits))),
        (writes > 0).then(|| format!("wrote {writes} {}", inflect("file", writes))),
        (reasoning > time::Duration::ZERO).then(|| format!("thought for {}", duration(reasoning))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ");

    if summary.is_empty() {
        summary = "Catching up...".to_owned();
    }

    let capital = summary.ceil_char_boundary(1);
    let c = summary[..capital].to_uppercase();
    summary.replace_range(..capital, &c);
    summary
}

pub fn upload(reply: &Reply, force: bool) -> Option<impl Widget<Message>> {
    if reply.prompt.total == reply.prompt.processed
        || (!force
            && reply
                .timings
                .is_some_and(|timings| timings.prompt.total <= time::Duration::from_secs(1)))
    {
        return None;
    }

    Some(center_x(
        progress_bar(
            0.0..=1.0,
            reply.prompt.processed as f32 / reply.prompt.total as f32,
        )
        .girth(10)
        .length(100)
        .style(progress_bar::secondary),
    ))
}

pub fn reasoning(reply: &Reply, compact: bool) -> impl Widget<Message> {
    const MAX_HEIGHT: f32 = font::SMALL * 1.5 * 10.0; // 10 lines

    let is_done = !reply.content.is_empty() || !reply.tool_calls.is_empty();

    let header = move || {
        match reply.timings {
            Some(timings) => {
                if is_done {
                    text!("Thought for {}", duration(timings.reasoning))
                } else {
                    text!("Thinking... ({})", duration(timings.reasoning))
                }
            }
            None => text(if is_done { "Thought" } else { "Thinking..." }),
        }
        .size(font::TINY)
        .font(font::BOLD)
        .width(Fill)
    };

    container(collapsible(
        !compact,
        move |_open| header(),
        move || {
            container(
                container(
                    scrollable(
                        markdown::view(reply.reasoning.items(), Font::MONOSPACE, font::TINY)
                            .map(Message::LinkClicked),
                    )
                    .width(Fill)
                    .height(Fit.max(MAX_HEIGHT))
                    .spacing(5)
                    .on_scroll(widget::snap.with(operation::Animation::Instant)),
                )
                .padding(10)
                .style(group),
            )
            .padding(padding::top(5))
        },
    ))
}

fn group(theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(theme.palette().secondary.strong.color),
        background: Some(theme.palette().background.weakest.color.into()),
        border: border::rounded(5),
        ..container::transparent(theme)
    }
}

pub fn truncate(output: &tool::Output) -> impl Widget<Message> {
    /// The first and last lines a long finished output
    /// keeps; the middle is elided
    const HEAD: usize = 10;
    const TAIL: usize = 20;

    fn line(line: &str) -> Text<'_> {
        text(&line[..line.floor_char_boundary(200)])
            .wrapping(text::Wrapping::None)
            .ellipsis(text::Ellipsis::End)
            .size(font::SMALL)
    }

    let total = output.lines();

    if total > HEAD + TAIL {
        let elided = total - HEAD - TAIL;

        let marker = match elided {
            1 => "... 1 line elided".to_owned(),
            elided => {
                format!("... {} lines elided", locale::thousands(elided as u64))
            }
        };

        column(
            output
                .head(HEAD)
                .map(line)
                .chain([text(marker).size(font::SMALL).style(text::secondary)])
                .chain(output.tail(TAIL).map(line)),
        )
    } else {
        column(output.all().map(line))
    }
    .spacing(5)
}
