pub mod http {
	use crate::{
		error::{Error, Result},
		map_err,
	};
	use async_compression::tokio::bufread::{BrotliEncoder, GzipEncoder, ZlibEncoder};
	use axum::http::HeaderValue;
	use serde_derive::Deserialize;
	use tokio::io::AsyncReadExt;

	#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
	#[serde(rename_all = "kebab-case")]
	pub enum ContentEncoding {
		Brotli,
		Gzip,
		Deflate,
		Identity,
	}

	impl Default for ContentEncoding {
		fn default() -> Self {
			Self::Brotli
		}
	}

	impl From<&str> for ContentEncoding {
		#[inline]
		fn from(value: &str) -> Self {
			match value {
				"br" => ContentEncoding::Brotli,
				"gzip" => ContentEncoding::Gzip,
				"deflate" => ContentEncoding::Deflate,
				value => {
					tracing::debug!("{value} is not a supported content encoding");
					ContentEncoding::Identity
				}
			}
		}
	}

	impl ContentEncoding {
		#[inline]
		pub async fn read_to_end(&self, src: &[u8]) -> Result<Vec<u8>> {
			let mut out_buf = vec![];

			map_err!(
				match self {
					Self::Brotli => BrotliEncoder::new(src).read_to_end(&mut out_buf).await,
					Self::Gzip => GzipEncoder::new(src).read_to_end(&mut out_buf).await,
					Self::Deflate => ZlibEncoder::new(src).read_to_end(&mut out_buf).await,
					Self::Identity => {
						// TODO This is unnecessary work. Should just be able to return the original bytes.
						out_buf.extend(src);
						Ok(0)
					}
				},
				IoError("failed to encode source buffer"),
			)?;

			Ok(out_buf)
		}

		#[inline]
		pub fn extension(&self) -> Option<&str> {
			match self {
				Self::Brotli => Some("br"),
				Self::Gzip => Some("gz"),
				Self::Deflate => Some("zl"),
				Self::Identity => None,
			}
		}

		#[inline]
		pub fn to_header_value(&self) -> HeaderValue {
			HeaderValue::from_static(match self {
				Self::Brotli => "br",
				Self::Gzip => "gzip",
				Self::Deflate => "deflate",
				Self::Identity => "identity",
			})
		}
	}
}
