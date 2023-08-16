use super::debouncer::{new_debouncer, DebouncedEvent, Debouncer};
use futures::{
	channel::mpsc::{channel, Receiver},
	SinkExt, StreamExt,
};
use notify::{RecommendedWatcher, RecursiveMode};
use sluggy_core::error::Result;
use std::{future::Future, path::Path, time::Duration};
use tokio::{runtime::Handle, task::block_in_place};

pub struct Watch<P, H, I>
where
	P: AsRef<Path>,
	H: WatchHandler,
	I: Iterator<Item = P>,
{
	paths: I,
	timeout: Duration,
	handler: H,
}

impl<P, H, I> Watch<P, H, I>
where
	P: AsRef<Path>,
	H: WatchHandler,
	I: Iterator<Item = P>,
{
	pub fn new(paths: I, timeout: Duration, handler: H) -> Self {
		Self {
			paths,
			timeout,
			handler,
		}
	}

	pub async fn watch(&mut self) -> Result<()> {
		let (mut debouncer, mut rx) = create_debounced_watcher(self.timeout)?;

		for path in self.paths.by_ref() {
			debouncer
				.watcher()
				.watch(path.as_ref(), RecursiveMode::Recursive)?;
		}

		while let Some(res) = rx.next().await {
			match res {
				Ok(events) => {
					if let Err(error) = self.handler.handle(events) {
						tracing::error!(?error, "Watcher handler error");
					}
				}
				Err(error) => {
					tracing::error!(?error, "Watcher error");
				}
			}
		}

		Ok(())
	}

	pub fn stop(&self) {}
}

pub trait WatchHandler {
	fn handle(&mut self, events: Vec<DebouncedEvent>) -> Result<()>;
}

impl<F, Fut> WatchHandler for F
where
	Fut: Future<Output = Result<()>>,
	F: FnMut(Vec<DebouncedEvent>) -> Fut,
{
	fn handle(&mut self, data: Vec<DebouncedEvent>) -> Result<()> {
		block_in_place(|| Handle::current().block_on((self)(data)))
	}
}

type DebouncedResult = Result<(
	Debouncer<RecommendedWatcher>,
	Receiver<std::result::Result<Vec<DebouncedEvent>, Vec<notify::Error>>>,
)>;
fn create_debounced_watcher(timeout: Duration) -> DebouncedResult {
	let (mut tx, rx) = channel(1);
	let debouncer = new_debouncer(timeout, None, move |res| {
		let handle = Handle::current();
		block_in_place(|| {
			handle.block_on(async {
				let _ = tx.send(res).await;
			});
		})
	})?;

	Ok((debouncer, rx))
}
