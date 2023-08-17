use json_pointer::Error as JsonPointerError;
use notify::Error as NotifyError;
use regex::Error as RegexError;
use serde_json::error::Error as SerdeJsonError;
use std::{path::PathBuf, result::Result as StdResult};
use thiserror::Error;
use tokio::task::JoinError;

pub type Result<T> = StdResult<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
	#[error("json pointer error")]
	JsonPointer(#[from] JsonPointerError),
	#[error(transparent)]
	NotifyError(#[from] NotifyError),
	#[error("join error")]
	JoinError(#[from] JoinError),
	#[error("{message}")]
	RegexError { message: String, source: RegexError },
	#[error("{0}")]
	Validation(String),
	#[error("{0}")]
	NotFound(String),
	#[error("{message}")]
	SerdeJsonError {
		message: String,
		source: SerdeJsonError,
	},
	#[error("{message}")]
	TomlSerializeError {
		message: String,
		source: toml::ser::Error,
	},
	#[error("{message}")]
	TomlDeserializeError {
		message: String,
		source: toml::de::Error,
	},
	#[error("{message}")]
	IoError {
		message: String,
		source: std::io::Error,
	},
	#[error("{message}")]
	StripPathPrefix {
		message: String,
		source: std::path::StripPrefixError,
	},
	#[error("failed to minify html")]
	MinifyHtmlError(String),
	#[error("template render error")]
	TemplateRenderError(#[from] tera::Error),
	#[error("{0}")]
	Css(String),
	#[error("css modules pattern parse")]
	CssModulesPatternParse(#[from] lightningcss::css_modules::PatternParseError),
	#[error("browserslist error")]
	Browserslist(#[from] browserslist::Error),
	#[error("datetime parse error")]
	DateTimeParse(#[from] chrono::ParseError),
	#[error("url parse error")]
	UrlParse(#[from] url::ParseError),
	#[error("client request error")]
	ClientRequest {
		message: String,
		source: reqwest::Error,
	},
	#[error("file loader error for {path:?}: {message}")]
	FileLoaderError { message: String, path: PathBuf },
	#[error("server error")]
	Server(#[from] hyper::Error),
	#[error("OTLP error")]
	TraceOtlp(#[from] opentelemetry_api::trace::TraceError),
	#[error("set global default error")]
	TraceSetGlobalDefault(#[from] tracing::subscriber::SetGlobalDefaultError),
}

impl From<minify_html_onepass::Error> for Error {
	fn from(value: minify_html_onepass::Error) -> Self {
		Self::MinifyHtmlError(value.error_type.message())
	}
}

impl<T: std::fmt::Display> From<lightningcss::error::Error<T>> for Error {
	fn from(value: lightningcss::error::Error<T>) -> Self {
		Self::Css(format!("{}", value))
	}
}

#[macro_export]
macro_rules! map_err {
	(
		@map_err_core
		$expr:expr,
		$variant:ident $msg:literal
	) => {
		$expr.map_err(
			#[inline]
			|error| Error::$variant {
				message: $msg.into(),
				source: error,
			}
		)
	};
	(
		@map_err_core
		$expr:expr,
		$variant:ident $msg:expr
	) => {
		$expr.map_err(
			#[inline]
			|error| Error::$variant {
				message: $msg,
				source: error,
			}
		)
	};
	(
		@map_err_core
		$expr:expr,
		$variant:ident
	) => {
		$expr.map_err(
			#[inline]
			|error| Error::$variant {
				message: "".into(),
				source: error,
			}
		)
	};
	(
		$expr:expr,
		$variant:ident$(($msg:literal))?$(,)?
	) => {
		map_err!(
			@map_err_core
			$expr,
			$variant $($msg)?
		)
	};
	(
		$expr:expr,
		$variant:ident$(($msg:expr))?$(,)?
	) => {
		map_err!(
			@map_err_core
			$expr,
			$variant $($msg)?
		)
	};
}

#[macro_export]
macro_rules! err {
	($variant:ident($msg:literal)) => {
		Error::$variant($msg.into())
	};
	($variant:ident($msg:expr)) => {
		Error::$variant($msg)
	};
}
