use serde_derive::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct SectionMetadata {
	pub title: Option<String>,
	pub description: Option<String>,
	pub link_text: Option<String>,
	pub index_template: Option<String>,
	pub slug_pattern: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum SectionHandle {
	/// Root section
	Root,
	Handle(String),
}

impl Default for SectionHandle {
	fn default() -> Self {
		Self::Root
	}
}

impl From<&str> for SectionHandle {
	fn from(value: &str) -> Self {
		if value.is_empty() {
			Self::Root
		} else {
			Self::Handle(value.into())
		}
	}
}

#[derive(Clone, Debug, Serialize)]
pub struct Section {
	pub handle: SectionHandle,
	pub title: Option<String>,
	pub description: Option<String>,
	link_text: Option<String>,
	#[serde(serialize_with = "add_postfix_slash")]
	pub prefix: PathBuf,
	pub entries: Vec<PathBuf>,
}

impl Section {
	pub fn new(prefix: PathBuf, section_metadata: &SectionMetadata) -> Self {
		let handle = prefix
			.components()
			.map(|component| {
				let path: &Path = component.as_os_str().as_ref();
				format!("{}", path.to_path_buf().display())
			})
			.collect::<Vec<_>>()
			.join("_");

		let handle = if handle.is_empty() {
			SectionHandle::Root
		} else {
			SectionHandle::Handle(handle)
		};

		Self {
			handle,
			title: section_metadata.title.clone(),
			description: section_metadata.description.clone(),
			link_text: section_metadata.link_text.clone(),
			prefix,
			entries: vec![],
		}
	}
}

fn add_postfix_slash<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
where
	S: serde::Serializer,
{
	let mut str_repr = path
		.components()
		.as_path()
		.to_path_buf()
		.to_string_lossy()
		.to_string();
	if !str_repr.is_empty() {
		str_repr.push('/');
	}
	serializer.serialize_str(&str_repr)
}
