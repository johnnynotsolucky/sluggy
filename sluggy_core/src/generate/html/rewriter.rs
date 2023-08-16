use crate::generate::config::Config;
use dashmap::DashMap;
use lol_html::{
	element,
	html_content::{ContentType, Element},
	HtmlRewriter, OutputSink, Settings,
};
use std::{
	error::Error,
	io::{ErrorKind, Write},
	path::PathBuf,
};
use tracing::instrument;

type HandlerResult = Result<(), Box<dyn Error + Send + Sync>>;
type ContentMap = &'static DashMap<PathBuf, String>;

struct Sink<'b> {
	buf: &'b mut Vec<u8>,
}

impl<'b> OutputSink for Sink<'b> {
	#[inline]
	fn handle_chunk(&mut self, chunk: &[u8]) {
		self.buf.extend_from_slice(chunk)
	}
}

pub(crate) struct Rewriter<'c: 'h, 'h> {
	rewriter: HtmlRewriter<'h, Sink<'c>>,
}

impl<'c, 'h> Rewriter<'c, 'h> {
	#[inline]
	pub(crate) fn new(config: &'c Config, buf: &'c mut Vec<u8>, content_map: ContentMap) -> Self {
		Rewriter {
			rewriter: HtmlRewriter::new(
				Settings {
					element_content_handlers: vec![
						// Rewrite insecure hyperlinks
						element!(
							"link[rel=\"stylesheet\"]",
							make_rewrite_link_stylesheet(config, content_map)
						),
						element!("a", make_rewrite_anchor_href(config)),
					],
					..Settings::default()
				},
				Sink { buf },
			),
		}
	}
}

#[instrument(level = "trace", skip(config, content_map))]
#[inline]
fn make_rewrite_link_stylesheet(
	config: &Config,
	content_map: ContentMap,
) -> impl FnMut(&mut Element) -> HandlerResult + '_ {
	|el| {
		let embed = el.get_attribute("embed");
		let href = el.get_attribute("href");

		match (&href, &embed) {
			(Some(href), Some(_embed)) => {
				if let Some(path) = href.strip_prefix("@/") {
					match content_map.get(&PathBuf::from(path)) {
						Some(css) => {
							el.replace(
								&format!("<style>{}</style>", css.value()),
								ContentType::Html,
							);
						}
						None => {
							tracing::warn!("css not found: {path}");
						}
					}
				}
			}
			(Some(href), None) => {
				if let Some(path) = href.strip_prefix("@/") {
					if let Err(error) =
						el.set_attribute("href", &format!("{}{path}", config.base_url))
					{
						tracing::warn!(?error, "rewrite link css failure");
					}
				}
			}
			_ => {}
		}

		Ok(())
	}
}

#[instrument(level = "trace", skip(config))]
#[inline]
fn make_rewrite_anchor_href(config: &Config) -> impl FnMut(&mut Element) -> HandlerResult + '_ {
	|el| {
		let href = el.get_attribute("href");

		if let Some(href) = &href {
			if let Some(path) = href.strip_prefix("@/") {
				if let Err(error) = el.set_attribute("href", &format!("{}{path}", config.base_url))
				{
					tracing::warn!(?error, "rewrite anchor href failure");
				}
			}
		}

		Ok(())
	}
}

impl<'c, 'h> Write for Rewriter<'c, 'h> {
	#[instrument(level = "trace", skip(self, buf))]
	#[inline]
	fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
		self.rewriter
			.write(buf)
			.map_err(|error| std::io::Error::new(ErrorKind::Other, error))?;
		Ok(buf.len())
	}

	fn flush(&mut self) -> std::io::Result<()> {
		Ok(())
	}
}
