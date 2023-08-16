use dashmap::DashMap;
use std::{hash::Hash, marker::PhantomData};
use tracing::instrument;

pub trait Cache<K, V> {
	type Output<'c>
	where
		Self: 'c;

	fn get(&self, key: &K) -> Option<Self::Output<'_>>;

	fn insert(&self, key: K, value: V);

	fn invalidate_all(&self);
}

#[derive(Clone, Debug)]
pub struct InMemoryStore<K: Hash + Eq, V> {
	store: DashMap<K, V>,
}

impl<K: Hash + Eq, V> Default for InMemoryStore<K, V> {
	fn default() -> Self {
		Self::new()
	}
}

impl<K: Hash + Eq, V> InMemoryStore<K, V> {
	pub fn new() -> Self {
		Self {
			store: DashMap::new(),
		}
	}
}

impl<K, V> Cache<K, V> for InMemoryStore<K, V>
where
	K: Hash + Eq + Clone + std::fmt::Debug,
	V: Clone,
{
	type Output<'c> = V where Self: 'c;

	#[instrument(skip(self))]
	#[inline]
	fn get(&self, key: &K) -> Option<Self::Output<'_>> {
		self.store.get(key).map(|e| e.value().clone())
	}

	#[instrument(skip(self, value))]
	#[inline]
	fn insert(&self, key: K, value: V) {
		self.store.insert(key, value);
	}

	#[instrument(skip(self))]
	#[inline]
	fn invalidate_all(&self) {
		self.store.clear();
	}
}

#[derive(Clone, Debug)]
pub struct NoStore<K, V> {
	_phantom: PhantomData<(K, V)>,
}

impl<K, V> Default for NoStore<K, V> {
	fn default() -> Self {
		Self::new()
	}
}

impl<K, V> NoStore<K, V> {
	pub fn new() -> Self {
		Self {
			_phantom: PhantomData::default(),
		}
	}
}

impl<K, V> Cache<K, V> for NoStore<K, V>
where
	K: Hash + Eq + Clone + std::fmt::Debug,
	V: Clone,
{
	type Output<'c> = V where Self: 'c;

	#[instrument(skip(self))]
	#[inline]
	fn get(&self, key: &K) -> Option<Self::Output<'_>> {
		None
	}

	#[instrument(skip(self, _value))]
	#[inline]
	fn insert(&self, _key: K, _value: V) {}

	#[instrument(skip(self))]
	#[inline]
	fn invalidate_all(&self) {}
}
