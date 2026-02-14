# Contributing to bergamot

Thank you for your interest in contributing! Here's how to get started.

## Getting started

1. Fork and clone the repository
2. Install Rust 1.85+ (or use `nix develop`)
3. Run the tests: `cargo test --workspace`

## Development workflow

```sh
cargo fmt           # Format code
cargo clippy -- -D warnings   # Lint
cargo test --workspace        # Run all tests
cargo build --release         # Build release binary
```

## Submitting changes

1. Create a feature branch from `main`
2. Make your changes with clear, focused commits
3. Ensure `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo test --workspace` all pass
4. Open a pull request with a description of what you changed and why

## Reporting bugs

Please open a GitHub issue with:

- bergamot version (`bergamot --version`)
- Operating system and architecture
- Steps to reproduce
- Expected vs. actual behaviour
- Relevant log output (with `--log-level debug` if possible)

## Areas where help is welcome

- Testing against real NNTP servers and reporting compatibility issues
- Sonarr/Radarr integration testing and RPC schema fixes
- Missing NZBGet feature implementations (see [gap analysis](docs/planning/production-readiness-gap-analysis.md))
- Documentation improvements
- Packaging for additional platforms

## License

By contributing, you agree that your contributions will be licensed under the GNU General Public License v2.0 or later.
