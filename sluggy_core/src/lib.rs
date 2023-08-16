pub mod common;
pub mod error;
pub mod generate;
pub(crate) mod lazyfn;
pub mod store;
pub mod utils;

pub fn add(left: usize, right: usize) -> usize {
	left + right
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn it_works() {
		let result = add(2, 2);
		assert_eq!(result, 4);
	}
}
