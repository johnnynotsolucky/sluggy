use self::loader::EntryData;

use super::{
	config::Config,
	sections::{Section, SectionHandle},
	syntect::SyntectAdapter,
};
use chrono::{serde::ts_seconds_option, DateTime, Utc};
use comrak::{ComrakExtensionOptions, ComrakOptions, ComrakPlugins, ComrakRenderOptions};
use dashmap::DashMap;
use json_pointer::Resolve;

use crate::{
	err,
	error::{Error, Result},
	map_err,
};
use regex::Regex;
use serde_derive::Serialize;
use serde_json::json;
use std::{
	ffi::OsStr,
	fs::File,
	io::BufRead,
	path::{Path, PathBuf},
	str::FromStr,
	sync::Arc,
};
use toml::Table;
use tracing::instrument;

pub(crate) mod loader;

const FRONTMATTER_MARKER: &str = "+++";

const TEMPLATE_EXT: &str = "tpl";
const MARKDOWN_EXT: &str = "md";
const HTML_EXT: &str = "html";
const XML_EXT: &str = "xml";

#[derive(Debug)]
pub struct Content {
	pub entries: DashMap<PathBuf, Entry>,
	pub sections: DashMap<SectionHandle, Section>,
	pub taxonomies: DashMap<String, DashMap<String, Vec<PathBuf>>>,
	pub config: Arc<Config>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
	pub slug: Option<String>,
	pub layout: Option<String>,
	pub url: String,
	pub path: PathBuf,
	pub file_path: PathBuf,
	pub file_type: FileType,
	#[serde(with = "ts_seconds_option")]
	pub published: Option<DateTime<Utc>>,
	#[serde(with = "ts_seconds_option")]
	pub updated: Option<DateTime<Utc>>,
	pub section_handle: Option<SectionHandle>,
	pub is_renderable: bool,
	#[serde(default, flatten)]
	pub extra: Table,
}

#[derive(Debug, Serialize)]
struct GenerateData<'t, 'e> {
	taxonomies: &'t DashMap<String, DashMap<String, Vec<PathBuf>>>,
	#[serde(flatten)]
	extra: &'e Table,
}

impl Entry {
	#[instrument(level = "debug", skip(entry_data, taxonomies, config))]
	#[inline]
	pub(crate) fn try_from_entry_data(
		entry_data: EntryData,
		taxonomies: &DashMap<String, DashMap<String, Vec<PathBuf>>>,
		config: Arc<Config>,
	) -> Result<Vec<Self>> {
		let mut entries = vec![];

		// If we're not generating a entry from the generate_from field
		match entry_data.frontmatter.generate_from {
			None => {
				let fs_meta = entry_data.fs_meta;
				let parent = Self {
					path: entry_data.path,
					slug: fs_meta.slug(),
					url: format!("{}{}", config.base_url, fs_meta.url().to_string_lossy()),
					file_path: fs_meta.path(),
					file_type: fs_meta.file_type(),
					published: entry_data.published,
					updated: entry_data.updated,
					section_handle: entry_data.section_handle,
					layout: entry_data.frontmatter.layout,
					is_renderable: fs_meta.is_renderable(),
					extra: entry_data.frontmatter.extra,
				};

				entries.push(parent);
			}
			Some(generate_from) => {
				let selector = generate_from.selector;
				let index_on = generate_from.index_on;
				let index_pattern = generate_from.index_pattern.unwrap_or(".*".into());
				let index_pattern = map_err!(
					Regex::from_str(&index_pattern),
					RegexError(format!(
						"Failed to parse pattern into regex: \"{index_pattern}\""
					)),
				)?;
				let filename_format = generate_from.filename_format.unwrap_or("{}".into());

				let json_value = map_err!(
					serde_json::to_value(GenerateData {
						taxonomies,
						extra: &entry_data.frontmatter.extra,
					}),
					SerdeJsonError("failed to serialize taxonomies"),
				)?;

				let array = match json_value.resolve(&selector)? {
					Some(value) if value.is_array() => value.as_array().cloned().unwrap(),
					Some(value) if value.is_object() => value
						.as_object()
						.cloned()
						.unwrap()
						.into_iter()
						.map(|(key, value)| {
							json!({
								"key": key,
								"value": value,
							})
						})
						.collect::<Vec<_>>(),
					None => {
						return Err(err!(Validation("Value not found")));
					}
					_ => {
						return Err(err!(Validation("Selected property must be an array")));
					}
				};

				for (index, item) in array.iter().enumerate() {
					let index_value = match index_on.as_ref() {
						None => {
							if item.is_string() {
								item.as_str().unwrap().to_string()
							} else {
								format!("{index}")
							}
						}
						Some(name) if item.is_object() => {
							let obj = item.as_object().unwrap();
							match obj.get(name) {
								None => {
									return Err(err!(Validation(format!(
										"Failed to find key {name}"
									))));
								}
								Some(value) if value.is_string() => {
									value.as_str().unwrap().to_string()
								}
								_ => {
									return Err(err!(Validation(format!(
										"Value at {name} must be a string"
									))));
								}
							}
						}
						_ => {
							return Err(err!(Validation(format!(
								"When `index_on` is set, the item must be an object"
							))));
						}
					};

					let mut filename = filename_format.clone();
					if let Some(captures) = index_pattern.captures(&index_value) {
						if index_pattern.captures_len() == 1 {
							if let Some(capture_match) = captures.get(0) {
								filename = filename.replace("[]", capture_match.as_str());
							}
						} else {
							for (capture_index, capture_name) in
								index_pattern.capture_names().enumerate()
							{
								if let Some(capture_name) = capture_name {
									if let Some(capture_match) = captures.name(capture_name) {
										filename = filename.replace(
											&format!("[{capture_name}]"),
											capture_match.as_str(),
										);
									}
								} else if let Some(capture_match) = captures.get(capture_index) {
									filename = filename.replacen("[]", capture_match.as_str(), 1);
								}
							}
						}
					}

					let fs_meta = entry_data.fs_meta.clone();

					let mut path = entry_data.path.clone();
					let mut url = fs_meta.url().clone();
					path.set_file_name(&filename);
					url.set_file_name(&filename);

					let mut entry = Self {
						path,
						slug: fs_meta.slug(),
						url: format!("{}{}", config.base_url, url.to_string_lossy()),
						file_path: fs_meta.path(),
						file_type: fs_meta.file_type(),
						published: entry_data.published,
						updated: entry_data.updated,
						section_handle: entry_data.section_handle.clone(),
						layout: entry_data.frontmatter.layout.clone(),
						is_renderable: fs_meta.is_renderable(),
						extra: entry_data.frontmatter.extra.clone(),
					};

					let value = map_err!(
						toml::Value::try_from(item),
						TomlSerializeError("Failed to convert generate JSON to TOML"),
					)?;

					entry.extra.insert("generate".into(), value);

					entries.push(entry);
				}
			}
		}

		Ok(entries)
	}

	#[inline]
	#[instrument(level = "trace", skip(self))]
	fn read_skip_frontmatter(&self) -> Result<String> {
		let file = map_err!(
			File::open(&self.file_path),
			IoError(format!(
				"Failed to open content file {}",
				self.file_path.display()
			)),
		)?;

		let mut in_header = false;
		let mut content = String::default();
		for (idx, line) in std::io::BufReader::new(file).lines().enumerate() {
			let line = map_err!(line, IoError)?;
			if line.starts_with(FRONTMATTER_MARKER) && content.is_empty() {
				if !in_header && idx == 0 {
					in_header = true;
				} else if in_header {
					in_header = false;
				}
				// So that we don't accidentally process the last `+++` in the front matter.
				continue;
			}

			if !in_header {
				content.push_str(&line);
				content.push('\n');
			}
		}

		Ok(content)
	}

	#[instrument(level = "trace", skip(self))]
	#[inline]
	pub fn raw(&self) -> Result<String> {
		self.read_skip_frontmatter()
	}

	#[instrument(level = "trace", skip(self))]
	#[inline]
	pub fn generate(&self) -> Result<String> {
		let options = ComrakOptions {
			render: ComrakRenderOptions {
				unsafe_: true, // Allow rendering of raw HTML
				..ComrakRenderOptions::default()
			},
			extension: ComrakExtensionOptions {
				header_ids: Some(String::new()),
				footnotes: true,
				table: true,
				..ComrakExtensionOptions::default()
			},
			..ComrakOptions::default()
		};

		let mut plugins = ComrakPlugins::default();
		let syntect_adapter = SyntectAdapter;
		plugins.render.codefence_syntax_highlighter = Some(&syntect_adapter);
		let content = comrak::markdown_to_html_with_plugins(
			&self.read_skip_frontmatter()?,
			&options,
			&plugins,
		);

		Ok(content)
	}

	pub(crate) async fn render_by_path(
		path: &Path,
		content: Arc<Content>,
	) -> Result<Option<String>> {
		Ok(
			match content
				.entries
				.iter()
				.find(|map_entry| map_entry.value().path == path)
			{
				Some(entry) => {
					if entry.file_type.is_markdown() {
						Some(entry.generate()?)
					} else {
						Some(entry.raw()?)
					}
				}
				None => None,
			},
		)
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FileType {
	Template,
	Markdown,
	Html,
	Xml,
	None,
	Other(String),
}

impl FileType {
	#[inline]
	pub fn is_markdown(&self) -> bool {
		*self == Self::Markdown
	}

	#[inline]
	pub fn is_rendered_to_html(&self) -> bool {
		*self == Self::Markdown || *self == Self::Html
	}

	#[inline]
	pub fn is_template(&self) -> bool {
		*self == Self::Template
	}
}

impl AsRef<str> for FileType {
	fn as_ref(&self) -> &str {
		match self {
			Self::Template => TEMPLATE_EXT,
			Self::Markdown => MARKDOWN_EXT,
			Self::Html => HTML_EXT,
			Self::Xml => XML_EXT,
			Self::None => "",
			Self::Other(other) => other,
		}
	}
}

impl<'ext> From<&'ext str> for FileType {
	fn from(ext: &'ext str) -> Self {
		match ext {
			TEMPLATE_EXT => Self::Template,
			MARKDOWN_EXT => Self::Markdown,
			HTML_EXT => Self::Html,
			XML_EXT => Self::Xml,
			"" => Self::None,
			other => Self::Other(other.to_owned()),
		}
	}
}

impl<'ext> From<Option<&'ext OsStr>> for FileType {
	fn from(value: Option<&'ext OsStr>) -> Self {
		match value {
			Some(value) => Self::from(value.to_str().unwrap()),
			None => Self::None,
		}
	}
}
