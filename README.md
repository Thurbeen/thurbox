# Thurbox

A TUI (Terminal User Interface) for orchestrating multiple
Claude Code instances with advanced repository
and git worktree management.

[![CI](https://github.com/Thurbeen/thurbox/workflows/CI/badge.svg)](https://github.com/Thurbeen/thurbox/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

![Thurbox Demo](./docs/media/thurbox-demo.gif)

## Features

- **Multi-Instance Orchestration**: Manage multiple Claude Code sessions simultaneously
- **Repository Management**: Track and switch between multiple repositories
- **Git Worktree Support**: Create, manage, and switch between git worktrees
- **Interactive TUI**: Clean and intuitive terminal interface built with Ratatui
- **Async Architecture**: Built on Tokio for high performance

## Installation

### From Binary (Recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

This installs the latest release to `~/.local/bin`. The script automatically:

- Fetches the latest version from GitHub (with API rate limit fallback)
- Detects your platform (Linux/macOS, x86_64/aarch64)
- Verifies checksums for security
- Extracts and installs the binary

**Custom installation directory:**

```bash
INSTALL_DIR=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

**Specific version:**

```bash
VERSION=v0.1.0 curl -fsSL https://raw.githubusercontent.com/Thurbeen/thurbox/main/scripts/install.sh | sh
```

### Prerequisites

- Rust 1.75 or later (for building from source)
- Git 2.30 or later
- tmux 3.2 or later
- claude CLI (<https://github.com/anthropics/claude-code>)

### From Source

```bash
git clone https://github.com/Thurbeen/thurbox.git
cd thurbox
cargo build --release
```

The binary will be available at `target/release/thurbox`.

## Development Setup

### Required Tools

All required development tools are documented in `Cargo.toml` under `[package.metadata.dev-tools]`.

#### Quick Installation

```bash
# Run the installation script
./scripts/install-dev-tools.sh
```

#### Manual Installation

Alternatively, install tools individually:

```bash
# Pre-commit framework (Rust-based)
cargo install prek

# Conventional commits tooling
cargo install cocogitto

# Modern test runner
cargo install cargo-nextest

# Module visualization
cargo install cargo-modules

# Dependency management
cargo install cargo-deny

# Security auditing
cargo install cargo-audit

# Markdown linting
cargo install rumdl

# Architecture validation (requires specific nightly)
rustup toolchain install nightly-2026-01-22
rustup component add --toolchain nightly-2026-01-22 rust-src rustc-dev llvm-tools-preview
cargo +nightly-2026-01-22 install cargo_pup
```

**Tip**: Install `cargo-binstall` for faster tool installation:

```bash
cargo install cargo-binstall
```

### Initial Setup

1. Clone the repository:

```bash
git clone https://github.com/Thurbeen/thurbox.git
cd thurbox
```

2. Install pre-commit hooks:

```bash
prek install
```

3. Verify setup:

```bash
cargo check
cargo test
```

## Development Workflow

### Running the Application

```bash
# Development mode
cargo run

# Release mode
cargo run --release
```

### Testing

```bash
# Run all tests
cargo test

# Run with nextest (recommended)
cargo nextest run

# Run specific test
cargo test test_name

# Run integration tests
cargo test --test integration_test

# Run architecture validation (requires nightly)
cargo +nightly test --test architecture_rules
```

### Code Quality

```bash
# Format code
cargo fmt

# Run linter
cargo clippy

# Check for issues
cargo check

# Generate documentation
cargo doc --open
```

### Watch Mode

For continuous development:

```bash
cargo install cargo-watch
cargo watch -x check -x test -x run
```

## Committing Changes

This project uses [Conventional Commits](https://www.conventionalcommits.org/).

### Using Cocogitto

```bash
# Create a feature commit
cog commit feat "add worktree management"

# Create a fix commit with scope
cog commit fix "resolve memory leak" api

# Breaking change
cog commit feat -B "redesign API interface" api
```

### Commit Types

- `feat`: New features (minor version bump)
- `fix`: Bug fixes (patch version bump)
- `docs`: Documentation changes
- `refactor`: Code refactoring
- `test`: Adding or updating tests
- `chore`: Maintenance tasks
- `perf`: Performance improvements

### Valid Scopes

- `api`: Claude API integration
- `cli`: Command-line interface
- `ui`: Terminal user interface
- `git`: Git operations
- `core`: Core functionality
- `docs`: Documentation
- `deps`: Dependencies
- `config`: Configuration

## Architecture

Thurbox follows a modular architecture with clear separation of concerns.

### Architectural Rules

The project enforces the following architectural rules (validated by cargo-pup):

1. **UI Layer Isolation**: UI components cannot directly access external APIs
2. **Git Module Independence**: Git module cannot depend on UI layer
3. **Claude Module Isolation**: Claude module is isolated from UI and Git
4. **No Circular Dependencies**: All modules maintain acyclic dependencies

## CI/CD

### Continuous Integration

The CI pipeline runs on every push and pull request:

- Code formatting check (`cargo fmt`)
- Linting (`cargo clippy`)
- Type checking (`cargo check`)
- Unit and integration tests
- Architecture validation
- Security audits
- Documentation build

## Contributing

We welcome contributions! Please follow these guidelines:

1. Fork the repository
2. Create a feature branch
3. Make your changes following our coding standards
4. Write tests for new functionality
5. Ensure all tests pass: `cargo nextest run`
6. Use conventional commits: `cog commit <type> "message"`
7. Submit a pull request

### Code Style

- Follow Rust naming conventions
- Maximum line width: 100 characters
- Use `rustfmt` for formatting
- Address all `clippy` warnings

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

- Built with [Ratatui](https://github.com/ratatui-org/ratatui) for the TUI
- Uses [Anthropic's Claude API](https://www.anthropic.com/api)
- Git operations powered by [git2-rs](https://github.com/rust-lang/git2-rs)

## Support

For issues, questions, or contributions, please visit our [GitHub repository](https://github.com/Thurbeen/thurbox).
