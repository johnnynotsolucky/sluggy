use miette::{Context, IntoDiagnostic};
use serde::Deserialize;
use sluggy_core::{
	common::http::ContentEncoding,
	store::{InMemoryStore, NoStore},
};
use std::{env, path::PathBuf, str::FromStr};
use toml::Value;

use crate::server::{ServerConfig as SluggyServerConfig, Store as ServerStore};
use sluggy_core::generate::config::Config as SluggyGenerateConfig;

pub const DEFAULT_OUT_DIR: &str = "./out";
pub const DEFAULT_CONTENT_DIR: &str = "./content";
pub const DEFAULT_CSS_DIR: &str = "./css";
pub const DEFAULT_TEMPLATES_DIR: &str = "./templates";
pub const DEFAULT_ASSETS_DIR: &str = "./assets";
pub const DEFAULT_DATA_DIR: &str = "./data";

pub const PROTECTED_COMPRESSION_DIR_NAME: &str = "___compressed";
pub const PROCESSED_IMAGES_DIR: &str = "___processed_images";

#[derive(Debug, Default, Deserialize)]
pub struct Config {
	pub out_dir: Option<PathBuf>,
	pub compress_content: Option<bool>,
	pub compressed_content_dir: Option<PathBuf>,
	pub processed_images_dir: Option<PathBuf>,
	pub generate: GenerateConfig,
	pub serve: ServeConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct GenerateConfig {
	pub content_dir: Option<PathBuf>,
	pub css_dir: Option<PathBuf>,
	pub template_dir: Option<PathBuf>,
	pub assets_dir: Option<PathBuf>,
	pub data_dir: Option<PathBuf>,
	pub base_url: Option<String>,
	pub minify: Option<bool>,
	#[serde(default)]
	pub taxonomies: Vec<String>,
	#[serde(flatten)]
	pub extra: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServeConfig {
	#[serde(default = "default_true")]
	pub generate: bool,
	#[serde(default = "default_true")]
	pub watch: bool,
	pub host: Option<String>,
	pub port: Option<u16>,
	#[serde(default)]
	pub content_encoding: ContentEncoding,
	#[serde(default)]
	pub store: Store,
}

impl Default for ServeConfig {
	fn default() -> Self {
		Self {
			generate: default_true(),
			watch: default_true(),
			host: Option::default(),
			port: Option::default(),
			content_encoding: ContentEncoding::default(),
			store: Store::default(),
		}
	}
}

fn default_true() -> bool {
	true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Store {
	None,
	InMemory,
}

impl Default for Store {
	fn default() -> Self {
		Self::InMemory
	}
}

fn canonicalize(path: PathBuf) -> miette::Result<PathBuf> {
	path.canonicalize()
		.into_diagnostic()
		.wrap_err(format!("Failed to canonicalize {}", path.display()))
}

impl TryFrom<Config> for (SluggyGenerateConfig, SluggyServerConfig) {
	type Error = miette::Error;

	fn try_from(config: Config) -> Result<Self, Self::Error> {
		let serve_dir = canonicalize(
			config
				.out_dir
				.unwrap_or(PathBuf::from_str(DEFAULT_OUT_DIR).into_diagnostic()?),
		)?;

		if !serve_dir.exists() {
			std::fs::create_dir_all(&serve_dir).into_diagnostic()?
		}

		let compress_content = config.compress_content.unwrap_or(true);

		let compressed_content_dir = config
			.compressed_content_dir
			.unwrap_or(PathBuf::from_str(PROTECTED_COMPRESSION_DIR_NAME).unwrap());

		let processed_images_dir = config
			.processed_images_dir
			.unwrap_or(PathBuf::from_str(PROCESSED_IMAGES_DIR).unwrap());

		let generate_config = config.generate;

		let base_url = match generate_config.base_url {
			None => env::var("BASE_URL").ok(),
			Some(base_url) => Some(base_url),
		};

		let base_url = if let Some(base_url) = base_url {
			if !base_url.ends_with('/') {
				format!("{base_url}/")
			} else {
				base_url
			}
		} else {
			"/".into()
		};

		let generate_config = SluggyGenerateConfig {
			content_dir: canonicalize(
				generate_config
					.content_dir
					.unwrap_or(PathBuf::from_str(DEFAULT_CONTENT_DIR).into_diagnostic()?),
			)?,
			css_dir: canonicalize(
				generate_config
					.css_dir
					.unwrap_or(PathBuf::from_str(DEFAULT_CSS_DIR).into_diagnostic()?),
			)?,
			template_dir: canonicalize(
				generate_config
					.template_dir
					.unwrap_or(PathBuf::from_str(DEFAULT_TEMPLATES_DIR).into_diagnostic()?),
			)?,
			assets_dir: canonicalize(
				generate_config
					.assets_dir
					.unwrap_or(PathBuf::from_str(DEFAULT_ASSETS_DIR).into_diagnostic()?),
			)?,
			data_dir: canonicalize(
				generate_config
					.data_dir
					.unwrap_or(PathBuf::from_str(DEFAULT_DATA_DIR).into_diagnostic()?),
			)?,
			processed_images_dir,
			out_dir: serve_dir.clone(),
			base_url,
			minify: generate_config.minify.unwrap_or(true),
			extra: generate_config.extra,
			compress_content,
			compressed_content_dir: compressed_content_dir.clone(),
			taxonomies: generate_config.taxonomies,
		};

		let server_config = config.serve;
		let port = match server_config.port {
			None => match env::var("PORT") {
				Ok(port) => port
					.parse()
					.into_diagnostic()
					.wrap_err(format!("Failed to parse PORT {port}"))?,
				Err(_) => 8000,
			},
			Some(port) => port,
		};

		let host = match server_config.host {
			None => match env::var("HOST") {
				Ok(host) => host,
				Err(_) => "0.0.0.0".into(),
			},
			Some(host) => host,
		};

		let server_config = SluggyServerConfig {
			generate: server_config.generate,
			watch: server_config.watch,
			host,
			port,
			serve_dir,
			compress_content,
			compressed_content_dir,
			content_encoding: server_config.content_encoding,
			store: match server_config.store {
				Store::None => ServerStore::NoStore(NoStore::new()),
				Store::InMemory => ServerStore::InMemoryStore(InMemoryStore::new()),
			},
		};

		Ok((generate_config, server_config))
	}
}
