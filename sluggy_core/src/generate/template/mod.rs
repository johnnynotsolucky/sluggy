use crate::{error::Result, lazyfn::LazyFn, utils::LockResultExt};
use serde::Serialize;
use std::{
	io::Write,
	mem::MaybeUninit,
	sync::{Arc, RwLock},
};
use tera::{Context as TeraContext, Tera};
use tracing::instrument;

use crate::generate::content::Content;

use self::functions::register_builtin_functions;

pub(crate) mod functions;

static TEMPLATE_ENGINE: LazyFn<Arc<RwLock<MaybeUninit<Tera>>>> =
	LazyFn::new(|| Arc::new(RwLock::new(MaybeUninit::uninit())));

pub(crate) fn setup_template_engine(content: &Arc<Content>) -> Result<()> {
	let mut tera = Tera::new(&format!("{}/**/*", content.config.template_dir.display()))?;

	// Disable auto-escaping.
	tera.autoescape_on(vec![]);

	register_builtin_functions(&mut tera, content);

	TEMPLATE_ENGINE.write().acquire().write(tera);

	Ok(())
}

pub(crate) fn unset_template_engine() {
	*TEMPLATE_ENGINE.write().acquire() = MaybeUninit::uninit();
}

#[instrument(level = "trace", skip(raw))]
#[inline]
pub(crate) fn add_once_off_template(name: &str, raw: &str) -> Result<()> {
	let mut engine_lock = TEMPLATE_ENGINE.write().acquire();
	let engine = unsafe { &mut engine_lock.assume_init_mut() };
	Ok(engine.add_raw_template(name, raw)?)
}

#[instrument(level = "debug", skip(data, write))]
#[inline]
pub(crate) fn render_template(
	template_name: &str,
	data: impl Serialize,
	write: &mut impl Write,
) -> Result<()> {
	let engine_lock = TEMPLATE_ENGINE.read().acquire();
	let engine = unsafe { engine_lock.assume_init_ref() };
	Ok(engine.render_to(template_name, &TeraContext::from_serialize(data)?, write)?)
}
