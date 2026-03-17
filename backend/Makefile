run:
	cargo run

fmt:
	cargo fmt --all

check:
	cargo check

test:
	cargo test

lint:
	cargo fmt --all --check
	cargo check

quality:
	cargo fmt --all --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test

change-gate:
	cargo fmt --all --check
	cargo check -q
	cargo test -q
