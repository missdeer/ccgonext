# Integration Tests

This directory contains integration tests for CCGO that test interactions between multiple modules.

## Test Files

### `session_integration.rs`
Tests for session layer functionality including:
- ClaudeCode PTY-based reply detection
- Agent type detection and routing
- PTY offset tracking

**Note**: Some tests are marked with `#[ignore]` because they require the actual `claude` binary to be installed.

## Running Tests

```bash
# Run all integration tests (excluding ignored tests)
cargo test --test '*'

# Run all tests including ignored ones (requires claude binary)
cargo test --test '*' -- --ignored

# Run specific integration test file
cargo test --test session_integration

# Run specific test by name
cargo test --test session_integration test_claudecode_type_detection
```

## Test Organization

- **Unit tests**: Located in `src/` alongside implementation (test internal module behavior)
- **Integration tests**: Located in `tests/` (test cross-module interactions and public API)

Tests requiring external dependencies (like `claude` binary) should be marked with `#[ignore]` to prevent CI failures.
