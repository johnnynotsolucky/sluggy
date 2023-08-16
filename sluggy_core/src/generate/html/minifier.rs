use crate::error::Result;
use minify_html_onepass::{in_place, Cfg};
use tracing::instrument;

#[instrument(level = "trace", skip(buf))]
#[inline]
pub(crate) fn minify_html(buf: &mut [u8]) -> Result<&[u8]> {
	let count = match in_place(
		buf,
		&Cfg {
			minify_css: false, // Handled by lightningcss
			minify_js: true,
		},
	) {
		Err(error) => return Err(error.into()),
		Ok(count) => count,
	};

	Ok(&buf[0..count])
}
