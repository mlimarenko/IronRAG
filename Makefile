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

backend-change-gate:
	cd backend && $(MAKE) change-gate

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

check: backend-change-gate frontend-lint frontend-format-check frontend-typecheck

check-strict: backend-change-gate backend-doc frontend-lint frontend-format-check frontend-typecheck

enterprise-validate:
	$(MAKE) backend-change-gate
	cd frontend && npm run enterprise:check

audit: backend-audit
