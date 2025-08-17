AGENTS: quickstart for agents working in this repo

Build/lint/test

- Build: cargo build --all-targets --locked
- Run: cargo run --bin controller
- Generate CRDs: cargo run --bin crdgen | kubectl apply -f -
- Test (all): cargo test --all --locked
- Test (single): cargo test -- --ignored 'pattern' # or cargo test
  module::test_name
- Lint/format: nix fmt # treefmt (rustfmt, deno, alejandra). CI also runs cargo
  +nightly fmt.
- Check with Nix: nix flake check
- Build OCI image: nix build '.#controller-image'

Code style

- Language: Rust 2024 edition. Follow rustfmt.toml (max_width=110,
  imports_granularity=Crate, newline_style=Unix, reorder_impl_items=true).
- Imports: prefer crate-level grouped imports; avoid glob imports; order std,
  external, crate consistently; keep unused imports out.
- Types: prefer explicit types at public boundaries; use Result<T,
  anyhow::Error> or domain errors via thiserror where meaningful.
- Errors: use anyhow for context (with_context/Context); define typed errors
  with thiserror for library-like modules; never panic! in normal paths;
  propagate with ?.
- Naming: snake_case for functions/vars, CamelCase for types,
  SCREAMING_SNAKE_CASE for consts; modules use snake_case; keep file names
  aligned with main type.
- Async/concurrency: tokio runtime; avoid blocking in async; use tracing for
  spans/logging; instrument important paths.
- Telemetry/metrics: use tracing, tracing-opentelemetry when feature "telemetry"
  enabled; prometheus-client for metrics, expose on /metrics; respect RUST_LOG.
- API/CRDs: schemas with schemars/serde; keep serde attrs on structs;
  backwards-compatible changes only; update yaml/crd.yaml via crdgen.
- Testing: unit tests near code; integration tests use #[tokio::test]; prefer
  deterministic tests; use http/hyper/tower-test dev-deps.

Tools and rules

- Pre-commit uses treefmt; run nix develop to enter shell with hooks. No
  Cursor/.cursorrules or Copilot rules present.
- CI builds via GitHub Actions (.github/workflows), including rustfmt PR job and
  Nix-based build/test/push.
