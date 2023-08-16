use std::path::PathBuf;

use serde_derive::Serialize;
use toml::Value;

#[derive(Debug, Clone, Serialize)]
pub struct Config {
	pub content_dir: PathBuf,
	pub compress_content: bool,
	pub compressed_content_dir: PathBuf,
	pub css_dir: PathBuf,
	pub template_dir: PathBuf,
	pub assets_dir: PathBuf,
	pub data_dir: PathBuf,
	pub out_dir: PathBuf,
	pub processed_images_dir: PathBuf,
	/// Always has a trailing slash
	pub base_url: String,
	pub minify: bool,
	pub taxonomies: Vec<String>,
	#[serde(flatten)]
	pub extra: Option<Value>,
}
