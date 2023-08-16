use crate::error::Result;
use http::HeaderValue;
use std::{ffi::OsStr, os::unix::prelude::OsStrExt, path::Path, sync::LockResult};
use tokio::task::JoinSet;
use tracing::instrument;

pub trait LockResultExt<T> {
	fn acquire(self) -> T;
}

impl<T> LockResultExt<T> for LockResult<T> {
	#[inline]
	#[instrument(level = "trace", skip_all)]
	fn acquire(self) -> T {
		// LockResult will only ever be an Err variant if there was a panic while there was
		// an exclusive lock (for RwLocks), or while a lock was in scope (for Mutexes).
		// Unwrapping will propagate the panic.
		// self.unwrap()
		match self {
			Ok(t) => t,
			Err(e) => e.into_inner(),
		}
	}
}

#[inline]
#[instrument(level = "trace", skip_all)]
pub async fn await_joinset(mut join_set: JoinSet<Result<()>>) -> Result<()> {
	while let Some(result) = join_set.join_next().await {
		match result {
			Ok(result) => match result {
				Ok(_) => {}
				Err(error) => return Err(error),
			},
			Err(error) => return Err(error)?,
		}
	}

	Ok(())
}

#[inline]
pub fn can_compress<P: AsRef<Path> + std::fmt::Debug>(path: P) -> bool {
	mime_guess::from_path(&path)
		.first_raw()
		.map(|mime| mime == "image/svg+xml" || !mime.starts_with("image/"))
		.unwrap_or(true)
}

const RENDERABLE_MIME_TYPES: [&str; 12] = [
	"text/plain",
	"text/html",
	"text/css",
	"text/markdown",
	"image/svg+xml",
	"text/xml",
	"text/x-toml",
	"text/x-yaml",
	"text/x-vcard",
	"application/json",
	"application/json5",
	"application/ld+json",
];

#[inline]
pub fn is_renderable<P: AsRef<Path>>(path: P) -> bool {
	if is_template_ext(&path) {
		// Return early if the extension is `tpl`
		return true;
	}

	mime_guess::from_path(path)
		.first_raw()
		.map(|mime| RENDERABLE_MIME_TYPES.contains(&mime))
		.unwrap_or(false)
}

#[inline]
pub fn is_template_ext<P: AsRef<Path>>(path: P) -> bool {
	if let Some(ext) = path.as_ref().extension() {
		if ext == OsStr::from_bytes(b"tpl") {
			return true;
		}
	}

	false
}

#[inline]
pub fn path_to_content_type<P: AsRef<Path>>(path: P) -> HeaderValue {
	let guess = mime_guess::from_path(path);
	guess
		.first_raw()
		.map(HeaderValue::from_static)
		.unwrap_or_else(|| HeaderValue::from_str(mime::APPLICATION_OCTET_STREAM.as_ref()).unwrap())
}
