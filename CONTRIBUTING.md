# Contributing

## Development Setup

1. Install Rust toolchain (stable).
2. Clone the repository.
3. Run `cargo check`.
4. Run tests for touched crates before opening a PR.

## Coding Standards

- Keep changes scoped to the task.
- Preserve cross-platform behavior (`cfg(windows)` / `cfg(unix)` paths).
- Add or update tests when behavior changes.
- Prefer non-breaking CLI changes unless explicitly requested.

## Pull Requests

- Include a clear summary of what changed and why.
- Include verification steps and command output summaries.
- Update `README.md` when user-visible behavior changes.

## License

By contributing, you agree that your contributions are licensed under:

- MIT (`LICENSE-MIT`)
- Apache-2.0 (`LICENSE-APACHE`)

at the project maintainers' option, as described in `LICENSE`.
