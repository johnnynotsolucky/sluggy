#[cfg(all(feature = "jemallocator", not(target_env = "msvc")))]
use tikv_jemallocator::Jemalloc;

#[cfg(all(feature = "jemallocator", not(target_env = "msvc")))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

use clap::{Parser, Subcommand};
use config::Config;
use dotenvy::dotenv;
use miette::{Context, IntoDiagnostic};
use opentelemetry_api::trace::Tracer;

use sluggy_core::{error::Result, store::Cache};

mod debouncer;
mod server;
mod watch;

use debouncer::DebouncedEvent;
use server::{serve, ServerConfig};
use sluggy_core::generate::{config::Config as GenerateConfig, Generator};
use std::{
	fs,
	io::{self},
	path::{Path, PathBuf},
	str::FromStr,
	sync::Arc,
	time::Duration,
};
use tokio::select;
use tracing::{instrument, Level};
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*, EnvFilter, Registry};
use watch::Watch;

mod config;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
	/// Config file
	#[arg(short, long)]
	config: Option<String>,

	/// Verbose logs
	#[arg(short, long)]
	verbose: bool,

	/// OpenTelemetry Protocol endpoint
	#[arg(long)]
	otlp_endpoint: Option<String>,

	/// Number of worker threads to use
	#[arg(long)]
	worker_threads: Option<usize>,

	#[command(subcommand)]
	command: Command,
}

#[derive(Clone, Debug, Subcommand, PartialEq, Eq)]
enum Command {
	/// Generate static site
	Generate,
	/// Serve site
	Serve,
}

impl Command {
	async fn exec(
		&self,
		generate_config: GenerateConfig,
		server_config: ServerConfig,
	) -> Result<()> {
		let generate_config = Arc::new(generate_config);
		let server_config = Arc::new(server_config);

		match self {
			Self::Generate => {
				Generator::generate(generate_config.clone()).await?;
			}
			Self::Serve => {
				if server_config.generate {
					Generator::generate(generate_config.clone()).await?;
				}

				let watcher = Watch::new(
					[
						generate_config.content_dir.clone(),
						generate_config.assets_dir.clone(),
						generate_config.template_dir.clone(),
						generate_config.css_dir.clone(),
						generate_config.out_dir.clone(),
					]
					.into_iter(),
					Duration::from_millis(250),
					{
						let server_config = server_config.clone();
						let generate_config = generate_config.clone();
						move |events: Vec<_>| {
							let server_config = server_config.clone();
							let generate_config = generate_config.clone();
							async move {
								if server_config.generate
									&& !notify_events_all(&events[..], &server_config.serve_dir)
								{
									let span = tracing::span!(Level::INFO, "reload_and_generate");
									let _enter = span.enter();

									if let Err(error) =
										Generator::generate(generate_config.clone()).await
									{
										tracing::event!(
											Level::ERROR,
											%error,
											"Unable to render templates"
										);
									}

									server_config.store.invalidate_all();
								} else if !server_config.generate
									&& notify_events_any(&events[..], &server_config.serve_dir)
								{
									let span = tracing::span!(Level::INFO, "invalidate_store_only");
									let _enter = span.enter();
									server_config.store.invalidate_all();
								}

								Ok(())
							}
						}
					},
				);

				let serve_handle = tokio::spawn({
					let server_config = server_config.clone();
					async move {
						if let Err(error) = serve(server_config).await {
							println!("Error: {error}");
						}
					}
				});

				let watch_handle = tokio::spawn({
					let server_config = server_config.clone();
					async move {
						if server_config.clone().watch {
							if let Err(error) = watcher.watch().await {
								println!("Error: {error}");
							}
						} else {
							futures::pending!()
						}
					}
				});

				select! {
					_ = serve_handle => {},
					_ = watch_handle => {},
				}
			}
		}

		Ok(())
	}
}

#[inline]
#[instrument(level = "debug", skip(events))]
fn notify_events_all(events: &[DebouncedEvent], prefix: &Path) -> bool {
	events.iter().all(|event| event.path.starts_with(prefix))
}

#[inline]
#[instrument(level = "debug", skip(events))]
fn notify_events_any(events: &[DebouncedEvent], prefix: &Path) -> bool {
	events.iter().any(|event| event.path.starts_with(prefix))
}

fn main() -> miette::Result<()> {
	dotenv().ok();

	let cli = Cli::parse();

	let config_file = &cli.config.as_ref();
	let config_file = PathBuf::from_str(config_file.unwrap_or(&"sluggy.toml".into()))
		.into_diagnostic()
		.wrap_err("Invalid config path")?
		.canonicalize();

	let config_file = match config_file {
		Err(_) if cli.config.is_none() => Config::default(),
		Err(error) => {
			return Err(error)
				.into_diagnostic()
				.wrap_err("Failed to find config file")
		}
		Ok(config_file) => toml::from_str(
			&fs::read_to_string(config_file)
				.into_diagnostic()
				.wrap_err("Failed to read config file")?,
		)
		.into_diagnostic()
		.wrap_err("Failed to parse config file")?,
	};

	let worker_threads = cli
		.worker_threads
		.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from));

	let runtime = tokio::runtime::Builder::new_multi_thread()
		.worker_threads(worker_threads)
		.enable_all()
		.build()
		.unwrap();

	let (generate_config, server_config) = config_file.try_into()?;

	runtime
		.block_on(exec(cli, generate_config, server_config))
		.into_diagnostic()?;

	Ok(())
}

async fn exec(
	cli: Cli,
	generate_config: GenerateConfig,
	server_config: ServerConfig,
) -> Result<()> {
	let env_filter =
		EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("sluggy=info"));
	let tracing_subscriber = Registry::default().with(env_filter);

	let fmt_layer = if cli.verbose {
		Some(
			tracing_subscriber::fmt::layer()
				.with_writer(io::stdout.with_max_level(Level::TRACE))
				.with_span_events(FmtSpan::CLOSE),
		)
	} else {
		// always show at least warnings
		Some(
			tracing_subscriber::fmt::layer()
				.with_writer(io::stdout.with_max_level(Level::WARN))
				.with_span_events(FmtSpan::CLOSE),
		)
	};

	// TODO why the fuck is this broken now?
	// let otlp_layer = match cli.otlp_endpoint {
	// 	Some(endpoint) => {
	// 		let otlp_export = opentelemetry_otlp::new_exporter()
	// 			.tonic()
	// 			.with_endpoint(endpoint);
	// 		let otlp_tracer = opentelemetry_otlp::new_pipeline()
	// 			.tracing()
	// 			.with_exporter(otlp_export)
	// 			.install_batch(opentelemetry_sdk::runtime::Tokio)?;
	//
	// 		Some(tracing_opentelemetry::layer().with_tracer(otlp_tracer))
	// 	}
	// 	None => None,
	// };

	let tracing_subscriber = tracing_subscriber.with(fmt_layer); //.with(otlp_layer);
	tracing::subscriber::set_global_default(tracing_subscriber)?;

	cli.command.exec(generate_config, server_config).await
}
