use super::{FileType, HTML_EXT, MARKDOWN_EXT};
use crate::{
	err,
	error::{Error, Error::FileLoaderError, Result},
	generate::{
		config::Config,
		content::FRONTMATTER_MARKER,
		sections::{Section, SectionHandle, SectionMetadata},
	},
	map_err,
	utils::{await_joinset, is_renderable},
};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use http::{HeaderMap, Method};
use regex::Regex;
use reqwest::{Client, Url};
use serde_derive::{Deserialize, Serialize};
use std::{
	fs::{File as FsFile, ReadDir},
	io::{self, BufRead},
	path::{Path, PathBuf},
	str::FromStr,
	sync::Arc,
};
use tokio::task::JoinSet;
use toml::{Table, Value};
use tracing::instrument;

pub const DEFAULT_SLUG_PATTERN: &str = r#"^(?P<slug>.*)"#;
pub const DEFAULT_DATE_SLUG_PATTERN: &str = r#"^(\d{4})-(\d{2})-(\d{2})-(?P<slug>.*)"#;
pub const SLUG_NAME: &str = "slug";
pub const DEFAULT_DATE_TIME_PATTERN: &str =
	r#"^(?P<year>\d{4})-(?P<month>\d{2})-(?P<day>\d{2})-.*"#;

const MANIFEST_FILE: &str = "section.toml";

#[derive(Debug, Clone)]
pub struct EntryData {
	pub path: PathBuf,
	pub fs_meta: EntryFsMeta,
	pub published: Option<DateTime<Utc>>,
	pub updated: Option<DateTime<Utc>>,
	pub section_handle: Option<SectionHandle>,
	pub frontmatter: Frontmatter,
}

#[derive(Debug)]
pub struct ContentLoader {
	pub config: Arc<Config>,
	pub entries: DashMap<PathBuf, EntryData>,
	pub sections: DashMap<SectionHandle, Section>,
	/// Confusing af tbh.
	///
	/// taxonomy key -> taxonomy value -> list of paths associated with it:
	///
	/// ```text
	/// categories [
	///   category-a [
	///     /entry/a,
	///     /entry/b,
	///     etc,
	///   ]
	/// ]
	/// ```
	pub taxonomies: DashMap<String, DashMap<String, Vec<PathBuf>>>,
}

impl ContentLoader {
	pub fn new(config: Arc<Config>) -> Arc<Self> {
		let taxonomies = DashMap::new();

		for taxonomy in &config.taxonomies {
			taxonomies.insert(taxonomy.clone(), DashMap::new());
		}

		Arc::new(Self {
			config,
			entries: DashMap::new(),
			sections: DashMap::new(),
			taxonomies,
		})
	}

	pub async fn load(self: &Arc<Self>) -> Result<()> {
		self.entries.clear();
		self.sections.clear();

		let mut join_set = JoinSet::new();
		self.clone()
			.load_recursive(self.config.content_dir.clone(), &mut join_set)?;
		await_joinset(join_set).await?;

		// Load taxonomies
		if !self.taxonomies.is_empty() {
			for entry in &self.entries {
				let path = entry.key();
				let entry = entry.value();

				let extra = &entry.frontmatter.extra;
				for mut taxonomy in self.taxonomies.iter_mut() {
					let taxonomy_key = taxonomy.key().clone();
					let taxonomy_paths = taxonomy.value_mut();

					if let Some(value) = extra.get(&taxonomy_key) {
						let values = if value.is_array() {
							// For multiple value taxonomies, like `tags`
							Some(
								value
									.as_array()
									.unwrap()
									.iter()
									.filter_map(|value| value.as_str())
									.collect::<Vec<_>>(),
							)
						} else if value.is_str() {
							// For single value taxonomies, like `category`
							Some(vec![value.as_str().unwrap()])
						} else {
							None
						};

						if let Some(values) = values {
							for value in values {
								if !taxonomy_paths.contains_key(value) {
									taxonomy_paths.insert(value.into(), vec![path.clone()]);
								} else {
									let mut paths = taxonomy_paths.get_mut(value).unwrap();
									paths.value_mut().push(path.clone());
								}
							}
						}
					}
				}
			}
		}

		Ok(())
	}

	#[instrument(skip(self))]
	fn load_recursive(
		self: Arc<Self>,
		current: PathBuf,
		join_set: &mut JoinSet<Result<()>>,
	) -> Result<()> {
		let dir_entries = self.load_dir_entries(&current)?;

		let (section, section_metadata) =
			match std::fs::read_to_string(&current.join(MANIFEST_FILE)) {
				Ok(manifest_content) => {
					let section_metadata: SectionMetadata = map_err!(
						toml::from_str(&manifest_content),
						TomlDeserializeError("failed to parse section manifest content"),
					)?;

					let prefix = map_err!(
						current
							.strip_prefix(&self.config.content_dir)
							.map(|p| p.to_path_buf()),
						StripPathPrefix("cannot have a root section"),
					)?;

					let section = Section::new(prefix, &section_metadata);
					let section_handle = section.handle.clone();

					self.sections.insert(section.handle.clone(), section);

					(Some(section_handle), Some(section_metadata))
				}
				Err(_) => (None, None),
			};

		// TODO Should I just ignore errors here. Seems like if the file disappeared before it could be read, then that is fine.
		for entry in dir_entries.flatten() {
			let path = entry.path();
			let file_type =
				map_err!(path.metadata(), IoError("failed to fetch file metadata"))?.file_type();
			let file_name = path.file_name().and_then(|n| n.to_str()).unwrap();

			if file_type.is_dir() {
				self.clone().load_recursive(path, join_set)?;
			} else if file_type.is_file() && file_name != MANIFEST_FILE {
				let file_name = path.file_name().and_then(|n| n.to_str()).unwrap();
				if let Some(parent) = path.parent() {
					if file_name != MANIFEST_FILE {
						join_set.spawn(self.clone().load_entry(
							parent.to_path_buf(),
							path,
							section.clone(),
							section_metadata.clone(),
						));
					}
				}
			}
		}
		Ok(())
	}

	#[inline]
	#[instrument(level = "trace", skip(self))]
	fn load_dir_entries(&self, path: &Path) -> Result<ReadDir> {
		map_err!(
			std::fs::read_dir(path),
			IoError(format!("Unable to read dir entries for {}", path.display())),
		)
	}

	#[inline]
	#[instrument(level = "debug", skip(self))]
	async fn load_entry(
		self: Arc<Self>,
		parent: PathBuf,
		path: PathBuf,
		section_handle: Option<SectionHandle>,
		section_metadata: Option<SectionMetadata>,
	) -> Result<()> {
		let prefix = parent
			.strip_prefix(&self.config.content_dir)
			.map(|p| p.to_path_buf())
			.unwrap_or_else(|_| PathBuf::new());

		let entry =
			{
				let current_section =
					match (&section_handle, section_metadata) {
						(Some(section_handle), Some(section_metadata)) => {
							let section = self.sections.get(section_handle).ok_or(err!(
								NotFound(format!("Section not found {section_handle:?}"))
							))?;
							let section = section.value().clone();
							Some((section, section_metadata))
						}
						_ => None,
					};

				EntryData::open(
					path.to_path_buf(),
					prefix,
					current_section,
					self.config.clone(),
				)
				.await?
			};

		if let Some(section_handle) = section_handle {
			let mut section =
				self.sections
					.get_mut(&section_handle)
					.ok_or(err!(NotFound(format!(
						"Section not found {section_handle:?}"
					))))?;
			section.entries.push(entry.path.clone());
		}

		self.entries.insert(entry.path.clone(), entry);

		Ok(())
	}
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Frontmatter {
	#[serde(default)]
	pub layout: Option<String>,
	#[serde(default)]
	pub published_at: Option<String>,
	#[serde(default)]
	pub load: Option<DashMap<String, DataLoader>>,
	#[serde(default)]
	pub generate_from: Option<GenerateFrom>,
	#[serde(default, flatten)]
	pub extra: Table,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerateFrom {
	/// Selector to retrieve generator data from. Uses [JSON pointers](https://www.rfc-editor.org/rfc/rfc6901)
	/// to query the dataset.
	///
	/// The result of the query *must* be an array. If the result is an object it will be transformed
	/// into an array of key/value pairs.
	///
	/// Example:
	///
	/// ```toml
	/// "/entry/my_list"
	/// ```
	pub(crate) selector: String,
	/// The field on each item to use for indexing the output file.
	///
	/// If `None`, then it is indexed on the index of the array.
	pub(crate) index_on: Option<String>,
	/// Pattern to extract values from the index field. Defaults to `".*"`.
	///
	/// Example, to match date parts: `'''^(?P<year>\d{4})-(?P<month>\d{2})-(?P<day>\d{2}).*$'''`
	pub(crate) index_pattern: Option<String>,
	/// The file name format. Defaults to `"[]"`
	///
	/// Interpolates matches from `index_pattern` to generate a filename
	/// Example: `"index_[year]_[month]_[day]"`
	pub(crate) filename_format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GenerateFromSelector {
	Basic(String),
	Advanced { advanced: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DataLoader {
	/// Load data from an HTTP request
	Request(DataRequest),
	/// Load data from a file. Relative to the current working dir if not absolute.
	File(PathBuf),
	/// Command?
	Command(Vec<String>),
}

impl DataLoader {
	#[inline]
	async fn load(self, config: &Arc<Config>) -> Result<Value> {
		Ok(match self {
			Self::Request(request) => {
				let client = Client::new();
				let mut builder = client
					.request(request.method, Url::from_str(&request.url)?)
					.headers(request.headers);

				if let Some(body) = request.body {
					builder = builder.json(&body);
				}

				let request = map_err!(
					builder.build(),
					ClientRequest("failed to build data loader request"),
				)?;

				let response = map_err!(
					map_err!(
						client.execute(request).await,
						ClientRequest("failed to execute data loader request"),
					)?
					.error_for_status(),
					ClientRequest("request failed"),
				)?;

				let value: Value = map_err!(
					response.json().await,
					ClientRequest("failed to parse JSON response"),
				)?;

				value
			}
			Self::File(filename) => {
				let path = if filename.starts_with("@/") {
					// We've already tested that the path starts with "@/", so this is safe.
					config.data_dir.join(filename.strip_prefix("@/").unwrap())
				} else {
					filename
				};

				let extension = path
					.extension()
					.ok_or(FileLoaderError {
						message: "unable to determine file type".into(),
						path: path.clone(),
					})?
					.to_string_lossy();

				let data: toml::Value = match extension.as_ref() {
					"toml" => map_err!(
						toml::from_str(&map_err!(
							std::fs::read_to_string(&path),
							IoError(format!("failed to read file {path:?}")),
						)?),
						TomlDeserializeError("failed to parse TOML file"),
					)?,
					"json" => map_err!(
						serde_json::from_str(&map_err!(
							std::fs::read_to_string(&path),
							IoError(format!("failed to read file {path:?}")),
						)?),
						SerdeJsonError("failed to parse JSON file"),
					)?,
					_ => {
						return Err(FileLoaderError {
							message: "unsupported file type".into(),
							path,
						});
					}
				};

				data
			}
			Self::Command(_command_parts) => {
				todo!()
			}
		})
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DataRequest {
	/// Request method. Defaults to `GET`
	#[serde(default, with = "http_serde::method")]
	pub method: Method,
	/// Request URL. Required.
	pub url: String,
	/// Optional request headers
	#[serde(default, with = "http_serde::header_map")]
	pub headers: HeaderMap,
	/// Optional request body
	pub body: Option<DataRequestBody>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DataRequestBody {
	/// Request body from a file
	Path(PathBuf),
	/// TOML value of request body
	Data(Value),
}

#[derive(Clone, Debug)]
pub struct File {
	path: PathBuf,
	// filename: &'path str,
	file_type: FileType,
	slug: Option<String>,
	url: PathBuf,
	/// Date read from the file name
	published: Option<DateTime<Utc>>,
	/// Whether this file can be rendered
	is_renderable: bool,
}

#[derive(Debug)]
struct EntryConfig {
	path: PathBuf,
	prefix: PathBuf,
	section: Option<(Section, SectionMetadata)>,
	config: Arc<Config>,
}

impl TryFrom<EntryConfig> for File {
	type Error = Error;

	fn try_from(entry_config: EntryConfig) -> std::result::Result<Self, Self::Error> {
		let file_type = FileType::from(entry_config.path.extension());
		let is_renderable = is_renderable(&entry_config.path);
		let filename = entry_config
			.path
			.file_name()
			.ok_or(err!(Validation("invalid file name")))?
			.to_string_lossy();
		let (slug, url) = extract_path_components(&file_type, &entry_config)?;

		Ok(Self {
			path: entry_config.path.clone(),
			// filename,
			file_type,
			slug,
			url,
			published: read_date_from_name(filename.as_ref())?,
			is_renderable,
		})
	}
}

#[derive(Clone, Debug)]
pub struct Dir {
	path: PathBuf,
	file_type: FileType,
	slug: Option<String>,
	url: PathBuf,
	/// Date read from the dir name
	published: Option<DateTime<Utc>>,
}

impl TryFrom<EntryConfig> for Dir {
	type Error = Error;

	fn try_from(entry_config: EntryConfig) -> std::result::Result<Self, Self::Error> {
		let parent = entry_config
			.path
			.parent()
			.ok_or(err!(Validation("path is root dir")))?;
		let file_type = FileType::from(entry_config.path.extension());
		let published = if parent != entry_config.config.content_dir {
			let parent = parent
				.components()
				.last()
				.ok_or(err!(Validation("no parent path component")))?
				.as_os_str()
				.to_str()
				.unwrap();
			read_date_from_name(parent)?
		} else {
			None
		};

		let (slug, url) = extract_path_components(&file_type, &entry_config)?;

		Ok(Self {
			path: entry_config.path.clone(),
			file_type,
			slug,
			url,
			published,
		})
	}
}

fn read_date_from_name(name: &str) -> Result<Option<DateTime<Utc>>> {
	let datetime_re = map_err!(
		Regex::from_str(DEFAULT_DATE_TIME_PATTERN),
		RegexError("failed to parse regex pattern"),
	)?;

	if let Some(captures) = datetime_re.captures(name) {
		let year = captures
			.name("year")
			.ok_or(err!(Validation(format!("year is required in {name}"))))?
			.as_str();

		let month = captures
			.name("month")
			.ok_or(err!(Validation(format!("month is required in {name}"))))?
			.as_str();

		let day = captures
			.name("day")
			.ok_or(err!(Validation(format!("day is required in {name}"))))?
			.as_str();

		let datetime = datetime_from_str(&format!("{year}-{month}-{day}"))?;
		Ok(Some(datetime))
	} else {
		Ok(None)
	}
}

fn extract_path_components(
	// parent: &Option<&'file str>,
	file_type: &FileType,
	entry_config: &EntryConfig,
) -> Result<(Option<String>, PathBuf)> {
	if file_type.is_rendered_to_html() {
		let file_stem = entry_config
			.path
			.file_stem()
			.ok_or(err!(Validation(format!(
				"no file name exists for {}",
				entry_config.path.display()
			))))?
			.to_str()
			.unwrap();

		// TODO need to just try configured slug pattern, default datetime pattern and then default slug pattern
		let slug_pattern = match &entry_config.section {
			Some((_, section_metadata)) => section_metadata
				.slug_pattern
				.clone()
				.unwrap_or(DEFAULT_SLUG_PATTERN.into()),
			None => DEFAULT_SLUG_PATTERN.into(),
		};
		let filename_re = map_err!(
			Regex::from_str(&slug_pattern),
			RegexError("failed to parse slug pattern"),
		)?;

		match filename_re.captures(file_stem) {
			Some(captures) => {
				let slug = captures.name(SLUG_NAME).ok_or(err!(Validation(format!(
					"{slug_pattern} did not match \"{SLUG_NAME}\" on {file_stem}"
				))))?;
				let slug = slug.as_str().to_string();

				let url =
					captures
						.iter()
						.skip(1)
						.fold(entry_config.prefix.clone(), |acc, capture| match capture {
							Some(pattern_match) => {
								let mut acc = acc.join(PathBuf::from(pattern_match.as_str()));
								acc.set_extension("");
								acc
							}
							None => acc,
						});

				Ok((Some(slug), url))
			}
			None => {
				let url = entry_config.prefix.join(file_stem);

				Ok((None, url))
			}
		}
	} else {
		Ok((
			None,
			entry_config.prefix.join(
				entry_config
					.path
					.file_name()
					.ok_or(err!(Validation(format!(
						"Invalid filename: {}",
						entry_config.path.display()
					))))?
					.to_string_lossy()
					.as_ref(),
			),
		))
	}
}

#[derive(Clone, Debug)]
pub enum EntryFsMeta {
	File(File),
	Index(Dir),
}

impl EntryFsMeta {
	#[inline]
	pub(crate) fn path(&self) -> PathBuf {
		match self {
			Self::File(file) => file.path.clone(),
			Self::Index(dir) => dir.path.clone(),
		}
	}

	#[inline]
	pub(crate) fn slug(&self) -> Option<String> {
		match self {
			Self::File(file) => file.slug.clone(),
			Self::Index(dir) => dir.slug.clone(),
		}
	}

	#[inline]
	pub(crate) fn url(&self) -> &PathBuf {
		match self {
			Self::File(file) => &file.url,
			Self::Index(dir) => &dir.url,
		}
	}

	#[inline]
	pub(crate) fn file_type(&self) -> FileType {
		match self {
			Self::File(file) => file.file_type.clone(),
			Self::Index(dir) => dir.file_type.clone(),
		}
	}

	#[inline]
	pub(crate) fn published(&self) -> Option<DateTime<Utc>> {
		match self {
			Self::File(file) => file.published,
			Self::Index(dir) => dir.published,
		}
	}

	#[inline]
	pub(crate) fn is_renderable(&self) -> bool {
		match self {
			Self::File(file) => file.is_renderable,
			Self::Index(_) => true, // An index is always renderable because it is the index file
		}
	}
}

impl TryFrom<EntryConfig> for EntryFsMeta {
	type Error = Error;

	#[inline]
	fn try_from(entry_config: EntryConfig) -> std::result::Result<Self, Self::Error> {
		let filename = entry_config.path.file_name().unwrap().to_str().unwrap();

		let index_re = map_err!(
			Regex::from_str(&format!(r#"^index\.({HTML_EXT}|{MARKDOWN_EXT})$"#)),
			RegexError("failed to parse regex string"),
		)?;
		if index_re.captures(filename).is_some() {
			Ok(Self::Index(Dir::try_from(entry_config)?))
		} else {
			Ok(Self::File(File::try_from(entry_config)?))
		}
	}
}

impl EntryData {
	#[inline]
	async fn load_from(fs_meta: EntryFsMeta, config: Arc<Config>) -> Result<Self> {
		let file_path = fs_meta.path();

		let mut frontmatter = {
			let file = map_err!(
				FsFile::open(&file_path),
				IoError(format!("failed to open file {}", file_path.display())),
			)?;

			if fs_meta.is_renderable() {
				let mut in_frontmatter = false;
				let mut frontmatter = String::default();
				for (line_index, line) in io::BufReader::new(file).lines().enumerate() {
					let line = map_err!(line, IoError)?;
					if line.starts_with(FRONTMATTER_MARKER) && (line_index == 0 || in_frontmatter) {
						if !in_frontmatter {
							in_frontmatter = true;
						} else {
							// No reason to carry on reading the file. We only care about the frontmatter at this point.
							break;
						}
					} else if in_frontmatter {
						frontmatter.push_str(&line);
						frontmatter.push('\n');
					}
				}

				if frontmatter.is_empty() {
					Frontmatter::default()
				} else {
					map_err!(
						toml::from_str(&frontmatter),
						TomlDeserializeError(format!(
							"Failed to parse header for {}",
							file_path.display()
						)),
					)?
				}
			} else {
				Frontmatter::default()
			}
		};

		if let Some(load) = frontmatter.load.take() {
			for (key, loader) in load.into_iter() {
				frontmatter.extra.insert(key, loader.load(&config).await?);
			}
		}

		let published = match &frontmatter.published_at {
			Some(published_at) => Some(datetime_from_str(published_at)?),
			None => fs_meta.published(),
		};

		Ok(Self {
			path: fs_meta.url().clone(),
			fs_meta,
			published,
			updated: published, // TODO why i even have this?
			section_handle: None,
			frontmatter,
		})
	}

	#[inline]
	pub async fn open(
		path: PathBuf,
		prefix: PathBuf,
		section: Option<(Section, SectionMetadata)>,
		config: Arc<Config>,
	) -> Result<Self> {
		let section_handle = section.as_ref().map(|section| section.0.handle.clone());

		let entry_config = EntryConfig {
			path,
			prefix,
			section,
			config: config.clone(),
		};

		let entry_fs_meta = EntryFsMeta::try_from(entry_config)?;
		let mut entry = EntryData::load_from(entry_fs_meta, config.clone()).await?;

		if entry.fs_meta.is_renderable() {
			entry.section_handle = section_handle;
		}

		Ok(entry)
	}
}

fn datetime_from_str(value: &str) -> Result<DateTime<Utc>> {
	Ok(DateTime::from_utc(
		NaiveDate::parse_from_str(value, "%Y-%m-%d")?
			.and_hms_opt(0, 0, 0)
			.unwrap(),
		Utc,
	))
}
