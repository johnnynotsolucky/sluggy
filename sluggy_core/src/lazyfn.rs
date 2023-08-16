use std::{cell::Cell, mem::MaybeUninit, ops::Deref, sync::Once};

pub struct LazyFn<V, F = fn() -> V> {
	init: Once,
	init_fn: Cell<Option<F>>,
	data: Cell<MaybeUninit<V>>,
}

// TODO: SAFETY:
unsafe impl<V: Sync> Sync for LazyFn<V> {}

impl<V, F: FnOnce() -> V> LazyFn<V, F> {
	pub const fn new(init_fn: F) -> Self {
		Self {
			init: Once::new(),
			init_fn: Cell::new(Some(init_fn)),
			data: Cell::new(MaybeUninit::uninit()),
		}
	}

	fn get(&self) -> &V {
		// SAFETY: `call_once` can only ever be _called once_. So it guarantees that the data
		// being written to will also only ever be written once.
		self.init.call_once(move || unsafe {
			let init_fn = (*self.init_fn.as_ptr()).take().unwrap();
			(*self.data.as_ptr()).write(init_fn());
		});

		// SAFETY: Once `call_once` has completed, we know that the inner data has been written
		// and we can get a reference to it.
		unsafe { &*(*self.data.as_ptr()).as_ptr() }
	}
}

impl<V, F: FnOnce() -> V> Deref for LazyFn<V, F> {
	type Target = V;
	fn deref(&self) -> &Self::Target {
		self.get()
	}
}
