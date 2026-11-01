use crate::core::Project;
use crate::tool;

use iced::{Element, Never};
use sipper::sipper;

use std::borrow::Cow;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

pub trait Call {
    fn run(&self, project: &Path) -> Run;

    fn title(&self, _project: &Project) -> Option<Cow<'_, str>> {
        None
    }

    fn gauge(&self, _duration: Duration) -> Option<Gauge> {
        None
    }

    fn view(&self) -> Option<Element<'_, Never>> {
        None
    }
}

pub enum Gauge {
    Duration(Duration),
    Edit { additions: u64, deletions: u64 },
}

pub type Output = Result<tool::Output, reason::Error>;
pub type Run = Pin<Box<dyn sipper::Core<Output = Output, Item = String> + Send>>;

pub fn straw<F>(f: impl FnOnce(sipper::Sender<String>) -> F + Send + 'static) -> Run
where
    F: Future<Output = Output> + Send,
{
    Box::pin(sipper(async move |sender| f(sender).await))
}

pub fn future(f: impl Future<Output = Output> + Send + 'static) -> Run {
    Box::pin(sipper(move |_sender| f))
}
