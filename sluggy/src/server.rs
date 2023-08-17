use axum::{
	body::{Body, Bytes},
	extract::State,
	handler::Handler,
	http::{
		header, HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri,
	},
};
use sluggy_core::{
	common::http::ContentEncoding,
	error::{Error, Result},
	map_err,
	store::{Cache, InMemoryStore, NoStore},
	utils::can_compress,
};
use std::{
	fs,
	io::ErrorKind,
	net::TcpListener,
	path::{Component, Path, PathBuf},
	str::FromStr,
	sync::Arc,
	time::Duration,
};
use tokio::signal::{self, unix::SignalKind};
use tower_http::{
	classify::ServerErrorsFailureClass, set_header::SetResponseHeaderLayer, trace::TraceLayer,
};
use tracing::{field, instrument, Level, Span};

#[derive(Debug, Clone)]
pub struct ServerConfig {
	pub compress_content: bool,
	pub compressed_content_dir: PathBuf,
	pub serve_dir: PathBuf,
	pub generate: bool,
	pub watch: bool,
	pub host: String,
	pub port: u16,
	pub content_encoding: ContentEncoding,
	pub store: Store,
}

#[derive(Clone, Debug)]
pub enum Store {
	NoStore(NoStore<PathBuf, (HeaderValue, ContentBytes)>),
	InMemoryStore(InMemoryStore<PathBuf, (HeaderValue, ContentBytes)>),
}

impl Cache<PathBuf, (HeaderValue, ContentBytes)> for Store {
	type Output<'c> = (HeaderValue, ContentBytes) where Self: 'c;

	#[inline]
	fn get(&self, key: &PathBuf) -> Option<Self::Output<'_>> {
		match self {
			Self::NoStore(store) => store.get(key),
			Self::InMemoryStore(store) => store.get(key),
		}
	}

	#[inline]
	fn insert(&self, key: PathBuf, value: (HeaderValue, ContentBytes)) {
		match self {
			Self::NoStore(store) => store.insert(key, value),
			Self::InMemoryStore(store) => store.insert(key, value),
		}
	}

	#[inline]
	fn invalidate_all(&self) {
		match self {
			Self::NoStore(store) => store.invalidate_all(),
			Self::InMemoryStore(store) => store.invalidate_all(),
		}
	}
}

#[derive(Debug, Clone)]
pub struct ContentBytes {
	file_name: PathBuf,
	compressed_file_name: PathBuf,
	identity: Option<Option<Bytes>>,
	brotli: Option<Option<Bytes>>,
	gzip: Option<Option<Bytes>>,
	deflate: Option<Option<Bytes>>,
}

impl ContentBytes {
	#[instrument(level = "trace", skip(self))]
	#[inline]
	fn bytes_from_content_encoding(
		&self,
		content_encoding: &ContentEncoding,
	) -> Option<Option<Bytes>> {
		// TODO this is called again here. Can it be removed?
		if can_compress(&self.file_name) {
			match content_encoding {
				ContentEncoding::Brotli => self.brotli.clone(),
				ContentEncoding::Gzip => self.gzip.clone(),
				ContentEncoding::Deflate => self.deflate.clone(),
				ContentEncoding::Identity => self.identity.clone(),
			}
		} else {
			self.identity.clone()
		}
	}
}

type SharedConfig = Arc<ServerConfig>;

#[instrument(level = "debug", skip(headers))]
#[inline]
fn get_content_encoding(headers: &HeaderMap, config: &SharedConfig) -> ContentEncoding {
	let content_encoding = match headers.get("accept-encoding") {
		Some(value) => match value.to_str() {
			Ok(value) => {
				let mut algos = value
					.split(',')
					.filter_map(
						#[inline]
						|v| ContentEncoding::try_from(v.trim()).ok(),
					)
					.collect::<Vec<_>>();

				// Prefer config defined encoding
				if let Some(pos) = algos.iter().position(|a| *a == config.content_encoding) {
					let algo = algos.remove(pos);
					algos.insert(0, algo);
				}

				if !algos.is_empty() {
					algos.swap_remove(0)
				} else {
					ContentEncoding::Identity
				}
			}
			Err(_) => ContentEncoding::Identity,
		},
		_ => ContentEncoding::Identity,
	};

	content_encoding
}

#[instrument(skip(config, on_error, headers))]
#[inline]
fn content_or(
	config: SharedConfig,
	path: PathBuf,
	headers: HeaderMap,
	on_error: impl Fn(ErrorKind) -> (StatusCode, HeaderMap, Bytes),
) -> (StatusCode, HeaderMap, Bytes) {
	let entry = config.store.get(&path);

	let entry = entry
		.map(
			#[inline]
			|(content_type, content_bytes)| {
				Some((true, StatusCode::OK, content_type, content_bytes))
			},
		)
		.unwrap_or_else(
			#[inline]
			|| {
				let (serve_dir, compressed_content_dir) = {
					(
						config.serve_dir.clone(),
						config.compressed_content_dir.clone(),
					)
				};

				let mut file_name = serve_dir.join(&path);
				let mut compressed_file_name = serve_dir.join(compressed_content_dir).join(&path);
				if file_name.is_dir() || !file_name.exists() {
					file_name = file_name.join("index.html");
					compressed_file_name = compressed_file_name.join("index.html");
				}

				// Directory traversal.
				if !file_name.components().all(|component| {
					matches!(
						component,
						Component::Prefix(_) | Component::RootDir | Component::Normal(_)
					)
				}) {
					file_name = serve_dir.join("_error/404/index.html");
					compressed_file_name = compressed_file_name.join("_error/404/index.html");
				}

				if file_name.exists() {
					let content_type = sluggy_core::utils::path_to_content_type(&file_name);

					let content_bytes = ContentBytes {
						file_name,
						compressed_file_name,
						identity: None,
						brotli: None,
						gzip: None,
						deflate: None,
					};

					config
						.store
						.insert(path.clone(), (content_type.clone(), content_bytes.clone()));

					Some((false, StatusCode::OK, content_type, content_bytes))
				} else {
					None
				}
			},
		);

	let (status_code, headers, bytes) = match entry {
		Some((cache_hit, status_code, content_type, mut content_bytes)) => {
			let content_encoding = if can_compress(&content_bytes.file_name) {
				get_content_encoding(&headers, &config)
			} else {
				ContentEncoding::Identity
			};

			let (content_encoding, bytes) = {
				let output_bytes;
				let mut content_encoding = content_encoding;

				match content_bytes.bytes_from_content_encoding(&content_encoding) {
					Some(bytes) => {
						// If nothing is found for the desired content encoding, then look for the
						// Identity content.
						if bytes.is_none() {
							output_bytes = match content_bytes
								.bytes_from_content_encoding(&ContentEncoding::Identity)
							{
								None => None,
								Some(bytes) => bytes,
							};
							content_encoding = ContentEncoding::Identity;
						} else {
							output_bytes = bytes;
						}
					}
					None => {
						let mut bytes = read_file(
							&content_bytes.file_name,
							&content_bytes.compressed_file_name,
							&content_encoding,
						);

						let store_bytes = bytes.clone();
						match content_encoding {
							ContentEncoding::Brotli => {
								content_bytes.brotli = Some(store_bytes);
							}
							ContentEncoding::Gzip => {
								content_bytes.gzip = Some(store_bytes);
							}
							ContentEncoding::Deflate => {
								content_bytes.deflate = Some(store_bytes);
							}
							ContentEncoding::Identity => {
								content_bytes.identity = Some(store_bytes);
							}
						}

						if !matches!(content_encoding, ContentEncoding::Identity) && bytes.is_none()
						{
							// If the encoding isn't already Identity, and the requested encoding couldn't be found,
							// fetch the original, unencoded content.
							bytes = read_file(
								&content_bytes.file_name,
								&content_bytes.compressed_file_name,
								&ContentEncoding::Identity,
							);
							content_bytes.identity = Some(bytes.clone());
							content_encoding = ContentEncoding::Identity;
						}

						config
							.store
							.insert(path, (content_type.clone(), content_bytes));

						output_bytes = bytes;
					}
				}

				(content_encoding, output_bytes)
			};

			match bytes {
				Some(bytes) => {
					let mut headers = HeaderMap::new();
					headers.append(
						HeaderName::from_static("x-sluggy-cache"),
						HeaderValue::from_static(if cache_hit { "HIT" } else { "MISS" }),
					);
					headers.append(header::CONTENT_TYPE, content_type);
					headers.append(header::CONTENT_ENCODING, content_encoding.to_header_value());
					headers.append(
						header::VARY,
						HeaderValue::from_name(header::CONTENT_ENCODING),
					);
					(status_code, headers, bytes)
				}
				None => on_error(ErrorKind::NotFound),
			}
		}
		None => on_error(ErrorKind::NotFound),
	};

	(status_code, headers, bytes)
}

#[instrument(level = "trace")]
#[inline]
fn read_file(
	file_name: &Path,
	compressed_prefix: &Path,
	content_encoding: &ContentEncoding,
) -> Option<Bytes> {
	let out = match content_encoding {
		ContentEncoding::Identity => fs::read(file_name).ok(),
		_ => {
			let mut file_to_read = compressed_prefix.to_path_buf();
			let mut extension = file_name.extension().unwrap_or_default().to_os_string();
			if let Some(content_encoding_extension) = content_encoding.extension() {
				extension.push(format!(".{content_encoding_extension}"));
			}
			file_to_read.set_extension(extension);
			fs::read(&file_to_read).ok()
		}
	};

	out.map(Bytes::from_iter)
}

#[instrument(skip(config, headers))]
#[inline]
fn error_content(
	config: SharedConfig,
	status_code: StatusCode,
	headers: HeaderMap,
) -> (StatusCode, HeaderMap, Bytes) {
	let (_, content_type, bytes) = content_or(
		config,
		PathBuf::from_str(&format!("_error/{}/index.html", status_code.as_u16())).unwrap(),
		headers,
		#[inline]
		|_| {
			let mut headers = HeaderMap::new();
			headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
			(status_code, headers, Bytes::from(format!("{status_code}",)))
		},
	);
	(status_code, content_type, bytes)
}

#[instrument(skip(config, headers))]
#[inline]
async fn static_content_handler(
	State(config): State<SharedConfig>,
	uri: Uri,
	method: Method,
	headers: HeaderMap,
) -> Response<Body> {
	let (status_code, header_map, bytes) = match method {
		Method::GET => content_or(
			config.clone(),
			PathBuf::from(uri.path().trim_start_matches('/')),
			headers.clone(),
			#[inline]
			move |error_kind| {
				let (status_code, content_type, bytes) = match error_kind {
					ErrorKind::NotFound => {
						error_content(config.clone(), StatusCode::NOT_FOUND, headers.clone())
					}
					_ => error_content(
						config.clone(),
						StatusCode::INTERNAL_SERVER_ERROR,
						headers.clone(),
					),
				};

				let mut headers = HeaderMap::new();
				headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));

				(status_code, content_type, bytes)
			},
		),
		_ => error_content(config, StatusCode::FORBIDDEN, headers.clone()),
	};

	let mut response = Response::new(bytes.into());

	*response.status_mut() = status_code;
	let headers = response.headers_mut();

	headers.extend(header_map);

	response
}

pub async fn serve(config: Arc<ServerConfig>) -> Result<()> {
	let static_handler = static_content_handler
		.layer(SetResponseHeaderLayer::if_not_present(
			header::SERVER,
			HeaderValue::from_static("Sluggy"),
		))
		.layer(
			TraceLayer::new_for_http()
				.make_span_with(|request: &Request<_>| {
					tracing::info_span!(
						"request",
						status_code = field::Empty,
						method = %request.method(),
						uri = %request.uri(),
						version = ?request.version(),
					)
				})
				.on_response(|response: &Response<_>, latency: Duration, span: &Span| {
					span.record("status_code", response.status().as_u16());

					tracing::event!(
						Level::INFO,
						latency = %format_args!("{}μs", latency.as_micros()),
						status = %response.status().as_u16(),
						"on_response",
					);
				})
				.on_failure(
					|error: ServerErrorsFailureClass, latency: Duration, span: &Span| {
						let status_code = match error {
							ServerErrorsFailureClass::StatusCode(status_code) => status_code,
							_ => StatusCode::INTERNAL_SERVER_ERROR,
						};

						span.record("status_code", status_code.as_u16());

						tracing::event!(
							Level::WARN,
							classification = %error,
							latency = %format_args!("{}μs", latency.as_micros()),
							"on_failure"
						);
					},
				),
		)
		.with_state(config.clone());

	let address = format!("{}:{}", config.host, config.port);
	let listener = map_err!(
		TcpListener::bind(&address),
		IoError(format!("Unable to bind to {address}")),
	)?;
	let server = axum::Server::from_tcp(listener)?
		.serve(static_handler.into_make_service())
		.with_graceful_shutdown(shutdown_signal());

	Ok(server.await?)
}

async fn shutdown_signal() {
	let ctrl_c = async {
		signal::ctrl_c()
			.await
			.expect("Failed to install Ctrl+C handler");
	};

	#[cfg(unix)]
	let terminate = async {
		signal::unix::signal(SignalKind::terminate())
			.expect("Failed to install signal handler")
			.recv()
			.await;
	};

	#[cfg(not(unix))]
	let terminate = std::future::pending::<()>();

	tokio::select! {
		_ = ctrl_c => {},
		_ = terminate => {},
	}
}
