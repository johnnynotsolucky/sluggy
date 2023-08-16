pub mod config;
pub mod content;
mod html;
mod sections;
mod syntect;
mod template;

use self::{
	config::Config,
	content::{Content, FileType},
};
use crate::{
	common::http::ContentEncoding,
	err,
	error::{Error, Result},
	lazyfn::LazyFn,
	map_err,
	utils::await_joinset,
};
use content::{loader::ContentLoader, Entry};
use dashmap::DashMap;
use html::{minifier::minify_html, rewriter::Rewriter};
use itertools::Itertools;
use lightningcss::{
	bundler::{Bundler, FileProvider},
	css_modules::{Config as CssModulesConfig, Pattern},
	stylesheet::{ParserFlags, ParserOptions, PrinterOptions},
	targets::Browsers,
};
use serde_derive::{Deserialize, Serialize};
use serde_json::json;
use std::{
	ffi::OsStr,
	fs::{self, File},
	io::Write,
	path::{Path, PathBuf},
	sync::Arc,
};
use tokio::{
	fs::File as TokioFile,
	io::{AsyncReadExt, AsyncWriteExt, BufReader},
	task::JoinSet,
};
use tracing::instrument;

const ONCE_OFF_TEMPLATE_NAME_PREFIX: &str = "___once_off_";

static EMBEDDABLE_CONTENT: LazyFn<DashMap<PathBuf, String>> = LazyFn::new(DashMap::new);

#[derive(Debug)]
pub struct Generator {
	pub config: Arc<Config>,
}

impl Generator {
	#[instrument(skip(config))]
	pub async fn generate(config: Arc<Config>) -> Result<()> {
		let generator = Arc::new(Generator {
			config: config.clone(),
		});

		let content_loader = ContentLoader::new(config.clone());

		content_loader.load().await?;

		// We need css transpiled first so that it can be embedded if required
		let mut join_set = JoinSet::new();
		generator.bundle_css(&mut join_set)?;
		await_joinset(join_set).await?;

		let mut join_set = JoinSet::new();
		generator.copy_static_files(&mut join_set).await?;

		let entries: DashMap<PathBuf, Entry> = content_loader
			.entries
			.clone() // TODO Don't like this clone yo
			.into_iter()
			.map(|(_path, entry)| {
				Entry::try_from_entry_data(entry, &content_loader.taxonomies, config.clone()).map(
					|entries| {
						entries
							.into_iter()
							.map(|entry| (entry.path.clone(), entry))
							.collect::<Vec<_>>()
					},
				)
			})
			.flatten_ok()
			.collect::<Result<_>>()?;

		let content = Arc::new(Content {
			entries,
			sections: content_loader.sections.clone(), // TODO this is slow
			taxonomies: content_loader.taxonomies.clone(), // TODO this is slow
			config: config.clone(),
		});

		template::setup_template_engine(&content)?;

		for entry in content.entries.iter() {
			let entry_path = entry.key().clone();
			let entry = entry.value();

			if entry.is_renderable {
				let mut file_path = entry_path.clone();
				if entry.file_type.is_rendered_to_html() {
					let is_index = entry_path
						.components()
						.last()
						.unwrap()
						.as_os_str()
						.to_string_lossy()
						.starts_with("index");

					if !is_index {
						file_path = entry_path.join("index");
					}

					file_path.set_extension(FileType::Html.as_ref());
				} else if !entry.file_type.is_template() {
					file_path.set_extension(entry.file_type.as_ref());
				} else {
					file_path.set_extension("");
				}

				let (template_name, template_raw) = if let Some(layout) = &entry.layout {
					(layout.clone(), None)
				} else {
					// If the file is markdown, but has no layout, then we generate it's html.
					let html = if entry.file_type.is_markdown() {
						entry.generate()?
					} else {
						// Otherwise just return raw
						entry.raw()?
					};
					(
						format!("{}{}", ONCE_OFF_TEMPLATE_NAME_PREFIX, entry.path.display()),
						Some(html),
					)
				};

				generator.dirs_exists(&file_path)?;
				join_set.spawn(render_entry(
					file_path,
					entry_path,
					template_name,
					template_raw,
					config.clone(),
				));
			} else {
				// Just copy the file to the out dir.
				map_err!(
					fs::copy(&entry.file_path, config.out_dir.join(&entry_path)),
					IoError(format!("Failed to copy file {}", entry_path.display())),
				)?;
			}
		}

		await_joinset(join_set).await?;

		template::unset_template_engine();

		Ok(())
	}

	#[inline]
	#[instrument(level = "debug", skip(self))]
	fn dirs_exists(&self, file_path: &Path) -> Result<()> {
		// Ensure dir exists
		let dir_path = match file_path.parent() {
			Some(path) => path.to_path_buf(),
			None => PathBuf::default(),
		};

		map_err!(
			fs::create_dir_all(self.config.out_dir.join(&dir_path)),
			IoError("failed to create dirs"),
		)?;

		map_err!(
			fs::create_dir_all(
				self.config
					.out_dir
					.join(&self.config.compressed_content_dir)
					.join(&dir_path),
			),
			IoError("failed to create dirs"),
		)?;

		Ok(())
	}

	#[instrument(skip(self))]
	fn bundle_css(&self, join_set: &mut JoinSet<Result<()>>) -> Result<()> {
		Self::recursive_process(&self.config.css_dir, &mut |file| {
			match file.extension() {
				None => return Ok(()),
				Some(extension) => {
					if extension != OsStr::new("css") {
						return Ok(());
					}
				}
			}

			let file_name = file
				.file_name()
				.ok_or(err!(Validation("Expected a valid file name")))?;
			if file_name.to_string_lossy().starts_with('_') {
				return Ok(());
			}

			let file_provider = FileProvider::new();

			let parser_options = ParserOptions {
				filename: file.to_string_lossy().to_string(),
				css_modules: Some(CssModulesConfig {
					pattern: Pattern::parse("[local]")?,
					dashed_idents: false,
				}),
				source_index: 0,
				error_recovery: false,
				warnings: None,
				flags: ParserFlags::NESTING | ParserFlags::CUSTOM_MEDIA,
			};
			let mut bundler = Bundler::new(&file_provider, None, parser_options);
			let out = bundler.bundle(file)?;

			let printer_options = PrinterOptions {
				minify: self.config.minify,
				source_map: None,
				project_root: None,
				// TODO make this a config option
				targets: Browsers::from_browserslist(["> 0.2% and not dead"])?.into(),
				analyze_dependencies: None,
				pseudo_classes: None,
			};

			let css = out.to_css(printer_options)?.code;

			let css_dir_name = self
				.config
				.css_dir
				.file_name()
				.ok_or(err!(Validation("Invalid asset dir")))?;

			let to_file = map_err!(
				file.strip_prefix(&self.config.css_dir),
				StripPathPrefix("failed to strip css dir prefix"),
			)?;

			let to_path = self.config.out_dir.join(css_dir_name).join(to_file);

			create_dir_all(&self.config.out_dir, to_path.parent().unwrap())?;

			let mut file = map_err!(
				File::create(&to_path),
				IoError(format!("Failed to create {}", to_file.display())),
			)?;

			map_err!(
				file.write_all(css.as_bytes()),
				IoError(format!("Failed to write css to {}", to_file.display())),
			)?;

			EMBEDDABLE_CONTENT.insert(PathBuf::from(&css_dir_name).join(to_file), css);

			if self.config.compress_content {
				apply_compression(&to_path, join_set, self.config.clone())?;
			}

			Ok(())
		})?;

		Ok(())
	}

	#[instrument(skip_all)]
	async fn copy_static_files(&self, join_set: &mut JoinSet<Result<()>>) -> Result<()> {
		Self::recursive_process(&self.config.assets_dir, &mut |file| {
			let to_file = map_err!(
				file.strip_prefix(&self.config.assets_dir),
				StripPathPrefix("failed to strip assets dir prefix"),
			)?;

			let to_path = self
				.config
				.out_dir
				.join(
					self.config
						.assets_dir
						.file_name()
						.ok_or(err!(Validation("Invalid asset dir")))?,
				)
				.join(to_file);

			create_dir_all(&self.config.out_dir, to_path.parent().unwrap())?;

			map_err!(
				fs::copy(file, &to_path),
				IoError(format!("failed to copy to {}", to_path.display())),
			)?;

			if self.config.compress_content {
				apply_compression(&to_path, join_set, self.config.clone())?;
			}

			Ok(())
		})?;

		Ok(())
	}

	fn recursive_process<F>(path: &Path, f: &mut F) -> Result<()>
	where
		F: FnMut(&Path) -> Result<()>,
	{
		if path.is_dir() {
			for entry in map_err!(
				fs::read_dir(path),
				IoError(format!("failed to read dir {}", path.display())),
			)? {
				Self::recursive_process(&map_err!(entry, IoError("dir entry failed"))?.path(), f)?;
			}
		} else {
			f(path)?;
		}
		Ok(())
	}
}

#[instrument(level = "info", skip(template_raw, config))]
#[inline]
async fn render_entry(
	file_path: PathBuf,
	entry_path: PathBuf,
	template_name: String,
	template_raw: Option<String>,
	config: Arc<Config>,
) -> Result<()> {
	if let Some(template_raw) = &template_raw {
		template::add_once_off_template(&template_name, template_raw)?;
	}

	let out_file = render_template(
		&file_path,
		&template_name,
		json!({ // TODO use an actual struct man wtf is wrong with you?
			"entry_path": entry_path,
			"site": *config.clone(),
			"base": &config.base_url,
		}),
		&config.out_dir,
		&config,
	)?;

	if config.compress_content {
		let mut join_set = JoinSet::<Result<()>>::new();
		apply_compression(&out_file, &mut join_set, config.clone())?;
		await_joinset(join_set).await?;
	}

	Ok(())
}

#[instrument(level = "debug", skip(data))]
#[inline]
fn render_template(
	file_path: &Path,
	template: &str,
	data: serde_json::Value,
	out_dir: &Path,
	config: &Config,
) -> Result<PathBuf> {
	let out_file = out_dir.join(file_path);
	let mut file = map_err!(
		File::create(&out_file),
		IoError(format!(
			"failed to create out file for rendering {}",
			out_file.display()
		))
	)?;

	let mut buf = vec![];

	let mut rewriter = Rewriter::new(config, &mut buf, &EMBEDDABLE_CONTENT);
	template::render_template(template, data, &mut rewriter)?;
	drop(rewriter); // Drop this so we can exclusively borrow buf.

	let buf = if out_file.extension() == Some(OsStr::new("html")) && config.minify {
		minify_html(&mut buf)?
	} else {
		&buf[..]
	};

	map_err!(
		file.write(buf),
		IoError("failed to write rendered template to file")
	)?;

	Ok(out_file)
}

#[instrument(level = "debug", skip(join_set))]
#[inline]
fn apply_compression(
	path: &Path,
	join_set: &mut JoinSet<Result<()>>,
	config: Arc<Config>,
) -> Result<()> {
	if !path.is_dir() && crate::utils::can_compress(path) {
		let file_name = map_err!(
			path.strip_prefix(&config.out_dir),
			StripPathPrefix(format!(
				"failed to strip prefix {} from {}",
				config.out_dir.display(),
				path.display()
			))
		)?;

		join_set.spawn(compress_file(
			path.to_path_buf(),
			file_name.to_path_buf(),
			ContentEncoding::Brotli,
			config.clone(),
		));
		join_set.spawn(compress_file(
			path.to_path_buf(),
			file_name.to_path_buf(),
			ContentEncoding::Gzip,
			config.clone(),
		));
		join_set.spawn(compress_file(
			path.to_path_buf(),
			file_name.to_path_buf(),
			ContentEncoding::Deflate,
			config,
		));
	}
	Ok(())
}

#[instrument(level = "debug")]
#[inline]
async fn compress_file(
	file_path: PathBuf,
	name: PathBuf,
	content_encoding: ContentEncoding,
	config: Arc<Config>,
) -> Result<()> {
	let file = map_err!(
		TokioFile::open(&file_path).await,
		IoError(format!("failed to open {}", file_path.display()))
	)?;
	let mut reader = BufReader::new(file);
	let mut buffer = vec![];

	map_err!(
		reader.read_to_end(&mut buffer).await,
		IoError(format!("failed to read file {file_path:?}"))
	)?;

	let out_buf = content_encoding.read_to_end(&buffer[..]).await?;

	let mut file_path = config
		.out_dir
		.join(&config.compressed_content_dir)
		.join(&name);
	let mut extension = file_path.extension().unwrap_or_default().to_os_string();
	if let Some(content_encoding_extension) = content_encoding.extension() {
		extension.push(format!(".{content_encoding_extension}"));
	}
	file_path.set_extension(extension);

	let mut file_path_components = file_path.components().peekable();
	let mut parent_dir = vec![];
	while let Some(component) = file_path_components.next() {
		match file_path_components.peek() {
			Some(_) => parent_dir.push(component),
			None => break,
		}
	}
	let parent_dir = PathBuf::from_iter(parent_dir);
	create_dir_all(&config.out_dir, &parent_dir)?;

	let mut out_file = map_err!(
		TokioFile::create(&file_path).await,
		IoError(format!("failed to create out file {}", file_path.display()))
	)?;

	map_err!(
		out_file.write_all(&out_buf[..]).await,
		IoError(format!(
			"failed to write to out file {}",
			file_path.display()
		))
	)?;

	Ok(())
}

#[instrument(level = "debug")]
#[inline]
fn create_dir_all(out_dir: &Path, dir: &Path) -> Result<()> {
	let dir = out_dir.join(dir);
	map_err!(
		fs::create_dir_all(&dir),
		IoError(format!("failed to create dirs for {}", dir.display()))
	)?;
	Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Image {
	pub width: u32,
	pub height: u32,
	pub path: PathBuf,
	pub url: String,
	pub mime_type: String,
	original_path: PathBuf,
}

impl Image {
	pub fn new(
		config: &Arc<Config>,
		width: u32,
		height: u32,
		path: PathBuf,
		original_path: PathBuf,
	) -> Self {
		Self {
			width,
			height,
			url: format!(
				"{}{}",
				config.base_url,
				path.strip_prefix(&config.out_dir)
					.unwrap()
					.to_string_lossy()
			),
			mime_type: mime_guess::from_path(&path)
				.first_raw()
				.unwrap()
				.to_string(),
			path,
			original_path,
		}
	}
}
