# Code guidelines

- Don't write too much code in `lib.rs`/`mod.rs`. Keep it as small as possible.
- No trivial comments
- Minimal bloat (KISS, DRY, SRP)
- No unnecessary state (variables, fields, arguments)
- Each line of code should justify its existence
- Follow Rust idioms and best practices
- Latest Rust features can be used
- Descriptive variable and function names
- No wildcard imports
- Import types at top of file and use short names everywhere (e.g. `use std::sync::Arc;` then `Arc<T>`, never `std::sync::Arc<T>` inline)
- Keep consts at top of file, right after imports
- Explicit error handling with `Result<T, E>` over panics
- Place unit tests in the same file using `#[cfg(test)]` modules
- Add dependencies to global `Cargo.toml`, and then set workspace=true in specific package
- Try solving with existing dependencies before adding new ones
- Prefer well-maintained crates from crates.io
- Be mindful of allocations in hot paths
- Prefer structured logging (wide logs with a bunch of useful fields)
- Provide helpful error messages
- Make sure tests are not flaky (no weird sleeps)
- No inline magic numbers or strings
- In tests const error/status messages and assert against the shared constant
- Add #[derive(Copy)] only on structs with 1 primitive field
- NO TRIVIAL COMMENTS

# Commands
- `cargo run -r` to build in release mode 
- `cargo test` to verify if tests pass 
- `cargo clippy --all-targets 2>&1 | rg "warning|error"` to static-check code style 
- `cargo fmt` to format code style
- `OVEN_MODEL=claude-3-5-haiku-20241022 OVEN_BASE_URL="https://api.moonshot.cn/anthropic/v1/" ./target/release/oven .` to run
