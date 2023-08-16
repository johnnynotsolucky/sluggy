use base64::prelude::*;
use futures::executor::block_on;
use imageless::{ImageOutputFormat, Operation};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
	collections::HashMap,
	fmt::Write,
	fs::{self, File as FsFile},
	io::BufWriter,
	path::PathBuf,
	str::FromStr,
	sync::Arc,
};
use tera::{Filter, Function, Tera};

use crate::generate::{
	content::{Content, Entry},
	sections::SectionHandle,
	Image,
};

pub(super) fn register_builtin_functions(tera: &mut Tera, content: &Arc<Content>) {
	tera.register_function(
		"render_content",
		make_render_content_fn(Arc::clone(content)),
	);
	tera.register_function("sections", make_sections_fn(Arc::clone(content)));
	tera.register_function("cr", carriage_return);
	tera.register_function("lb", line_break);
	tera.register_function("image", make_image_fn(Arc::clone(content)));
	tera.register_function("base64", make_base64_fn(Arc::clone(content)));

	tera.register_filter("entry", make_entry_filter(Arc::clone(content)));
}

#[inline]
fn carriage_return(_args: &HashMap<String, Value>) -> tera::Result<Value> {
	Ok(Value::String("\r".into()))
}

#[inline]
fn line_break(_args: &HashMap<String, Value>) -> tera::Result<Value> {
	Ok(Value::String("\n".into()))
}

fn make_image_fn(content: Arc<Content>) -> impl Function {
	Box::new(
		#[inline]
		move |args: &HashMap<String, Value>| -> tera::Result<Value> {
			let in_path: String = get_arg("in", args)?;
			let in_path = PathBuf::from(in_path);

			let out_format: ImageOutputFormat = get_arg("format", args)?;
			let operations: Vec<Operation> = get_arg("operations", args)?;

			let filename = in_path.file_name().ok_or(tera::Error::msg(format!(
				"Could not get filename from path: {}",
				in_path.display()
			)))?;

			let config = &content.config;

			let mut hasher = Sha256::default();
			hasher.update(serde_json::to_string(&operations).unwrap());
			let hash = hasher.finalize();
			let mut hex = String::with_capacity(hash.len() * 2);
			if let Err(error) = write!(hex, "{:x}", hash) {
				return Err(tera::Error::msg(format!(
					"Could not write hash to string: {}",
					error
				)));
			}

			let out_path = PathBuf::from(filename)
				.join(hex)
				.with_extension(out_format.extension());

			let full_out_path = config
				.out_dir
				.join(&config.processed_images_dir)
				.join(out_path);

			// TODO make less duplicated
			if let Err(error) = fs::create_dir_all(
				config
					.out_dir
					.join(&config.processed_images_dir)
					.join(filename),
			) {
				return Err(tera::Error::msg(format!(
					"Could not create directory: {}",
					error
				)));
			}

			match imageless::process_file(config.assets_dir.join(&in_path), operations) {
				Ok(image) => {
					let out_file = FsFile::create(&full_out_path)?;
					let mut out_buf = BufWriter::new(out_file);
					if let Err(error) = image.write_to(&mut out_buf, out_format) {
						return Err(tera::Error::msg(format!(
							"Unable to write image: {}",
							error
						)));
					}

					let image = Image::new(
						config,
						image.width(),
						image.height(),
						full_out_path,
						in_path,
					);

					serde_json::to_value(image).map_err(|error| {
						tera::Error::msg(format!("Could not serialize image: {}", error))
					})
				}
				Err(error) => Err(tera::Error::msg(format!(
					"Failed to process image: {}",
					error
				))),
			}
		},
	)
}

fn make_base64_fn(_content: Arc<Content>) -> impl Function {
	Box::new(
		#[inline]
		move |args: &HashMap<String, Value>| -> tera::Result<Value> {
			let encoded = match get_arg::<String>("value", args).ok() {
				Some(value) => BASE64_STANDARD.encode(value.as_bytes()),
				None => {
					let file = get_arg::<PathBuf>("file", args)?;
					let contents = fs::read(&file).map_err(|error| {
						tera::Error::msg(format!("Failed to read file: {file:?}: {error}",))
					})?;
					BASE64_STANDARD.encode(contents)
				}
			};

			Ok(Value::String(encoded))
		},
	)
}

fn get_arg<T: DeserializeOwned>(name: &str, args: &HashMap<String, Value>) -> tera::Result<T> {
	match args.get(name) {
		Some(value) => serde_json::from_value::<T>(value.clone()).map_err(|error| {
			let as_str = value.as_str().unwrap();
			tera::Error::msg(format!(
				"Invalid arg '{name}': {value:?} ({as_str}): {error:?}",
			))
		}),
		_ => Err(tera::Error::msg(format!("Missing arg '{name}'"))),
	}
}

fn make_render_content_fn(content: Arc<Content>) -> impl Function {
	Box::new(
		#[inline]
		move |args: &HashMap<String, Value>| -> tera::Result<Value> {
			let content = content.clone();
			if let Some(path) = args.get("path") {
				match path.as_str() {
					Some(path) => block_on(async move {
						let path = match PathBuf::from_str(path) {
							Ok(path) => path,
							Err(_) => return Err(tera::Error::msg("Invalid value for `path`")),
						};

						match Entry::render_by_path(&path, content).await {
							Ok(content) => match content {
								Some(content) => Ok(Value::String(content)),
								None => Ok(Value::Null),
							},
							Err(error) => Err(tera::Error::msg(format!(
								"Failed to generate content: {error:?}"
							))),
						}
					}),
					None => Err(tera::Error::msg("`path` param must be a string")),
				}
			} else {
				Err(tera::Error::msg("Missing `path` param"))
			}
		},
	)
}

fn make_entry_filter(content: Arc<Content>) -> impl Filter {
	Box::new(
		#[inline]
		move |value: &Value, _args: &HashMap<String, Value>| -> tera::Result<Value> {
			let content = content.clone();

			// TODO make this fn less duplicatedededed

			if let Some(paths) = value.as_array() {
				let mut array = vec![];
				for path in paths {
					match path.as_str() {
						Some(path) => {
							let path = match PathBuf::from_str(path) {
								Ok(path) => path,
								Err(_) => return Err(tera::Error::msg("Invalid value for `path`")),
							};

							let entry = content
								.entries
								.get(&path)
								.map(|entry| serde_json::to_value(entry.value()).unwrap())
								.unwrap_or(Value::Null);

							array.push(entry);
						}
						None => return Err(tera::Error::msg("each item must be a string")),
					}
				}

				Ok(Value::Array(array))
			} else if let Some(path) = value.as_str() {
				let path = match PathBuf::from_str(path) {
					Ok(path) => path,
					Err(_) => return Err(tera::Error::msg("Invalid value for `path`")),
				};

				Ok(content
					.entries
					.get(&path)
					.map(|entry| serde_json::to_value(entry.value()).unwrap())
					.unwrap_or(Value::Null))
			} else {
				Err(tera::Error::msg(
					"input value must be a string or an array of strings",
				))
			}
		},
	)
}

fn make_sections_fn(content: Arc<Content>) -> impl Function {
	Box::new(
		#[inline]
		move |args: &HashMap<String, Value>| -> tera::Result<Value> {
			let content = content.clone();

			if args.is_empty() {
				let list = content
					.sections
					.iter()
					.map(|section| serde_json::to_value(section.value()).unwrap())
					.collect();
				Ok(Value::Array(list))
			} else if let Some(handle) = args.get("handle") {
				match handle.as_str() {
					Some(handle) => {
						let handle = SectionHandle::from(handle);

						let result = Ok(content
							.sections
							.get(&handle)
							.map(|section| serde_json::to_value(section.value()).unwrap())
							.unwrap_or(Value::Null));

						result
					}
					None => Err(tera::Error::msg("`handle` param must be a string")),
				}
			} else {
				Err(tera::Error::msg("Missing `handle` param"))
			}
		},
	)
}
