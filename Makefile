backend-fmt:
	cd backend && cargo fmt --all

backend-build:
	cd backend && cargo build --release

backend-lint:
	cd backend && cargo clippy --all-targets --all-features -- -D warnings

backend-doc:
	cd backend && cargo doc --no-deps

backend-test:
	cd backend && cargo test

backend-audit:
	cd backend && cargo audit

frontend-install:
	cd frontend && npm install

frontend-lint:
	cd frontend && npm run lint

frontend-format-check:
	cd frontend && npm run format:check

frontend-typecheck:
	cd frontend && npm run typecheck

frontend-build:
	cd frontend && npm run build

check: backend-lint backend-test frontend-lint frontend-format-check frontend-typecheck

check-strict: backend-lint backend-doc backend-test frontend-lint frontend-format-check frontend-typecheck

enterprise-validate:
	cd backend && $(MAKE) quality
	cd frontend && npm run enterprise:check

audit: backend-audit
