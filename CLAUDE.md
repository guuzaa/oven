# Code Style
- Don't write too much code in `lib.rs`/`mod.rs`. Keep it as small as possible.
- Don't add comments when modification. Keep comments as little as possble.

# Commands
- `cargo run -r` to build in release mode 
- `cargo test` to verify if tests pass 
- `cargo clippy --all-targets 2>&1 | rg "warning|error"` to static-check code style 
- `cargo fmt` to format code style
- `OVEN_MODEL=claude-3-5-haiku-20241022 ANTHROPIC_BASE_URL="https://api.moonshot.cn/anthropic/v1/" ./target/release/oven .` to run 