# Stream Module

`StreamRequest::spawn()` launches the stream task; `StreamRequest::run()` is the main loop.
Submodules: `agent.rs` (sub-agent spawning), `tools.rs` (tool call helpers), `recovery.rs`
(length/iteration recovery), `phases.rs` (4 tool execution phases extracted from the loop).
Sub-agents use `sub_request.spawn()` (not `Box::pin(run())`) to preserve the Send bound —
`Box::pin` erases Send, preventing `tokio::spawn` for parallel execution.

Both Phase 2 (parallel) and Phase 3 (sequential) use `spawn_blocking` for
tool execution, so `block_on` is safe in any tool handler. Phase 3 also
handles `JoinError` (task panics) gracefully in the UI.
