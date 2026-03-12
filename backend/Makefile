run:
	cargo run

fmt:
	cargo fmt

check:
	cargo check

test:
	cargo test

lint:
	cargo fmt --check
	cargo check

quality:
	cargo fmt --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test
