set shell := ["bash", "-uc"]

default:
    @just --list

# --- one-shot ---

check:
    cargo check --all-targets

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --all-targets -- -D warnings

test *args:
    cargo test {{args}}

build:
    cargo build --release

run *args:
    cargo run -- {{args}}

ci: fmt-check clippy test

# --- watch loops (watchexec) ---

# Watch Rust sources and re-run check on change
watch-check:
    watchexec -e rs -r -- cargo check --all-targets

watch-clippy:
    watchexec -e rs -r -- cargo clippy --all-targets -- -D warnings

watch-test *args:
    watchexec -e rs -r -- cargo test {{args}}

watch-run *args:
    watchexec -e rs,toml -r -- cargo run -- {{args}}

# --- worker (Cloudflare) ---

worker-dev:
    npm run worker:dev

worker-migrate-local:
    npm run worker:migrations:local

worker-deploy-dry:
    npm run worker:deploy:dry-run
