---
name: cargo-check
description: Run cargo check and cargo test to verify the build compiles and tests pass
---

Run these commands in order, stopping on first failure:

1. `cargo check 2>&1` — verify types compile
2. `cargo test 2>&1` — run all unit tests

Report results concisely. If tests fail, show only the failing test names and error messages.
