## Summary

<!-- Brief description of what changed and why -->

## Review Checklist

- [ ] **Exhaustive matching**: No `_ =>` wildcards — explicit variant lists in all match arms
- [ ] **Test coverage**: New behavior has unit tests; new enums have round-trip tests
- [ ] **ToolName locations updated**: If a new tool variant was added, all exhaustive match locations updated:
  - `extract_args_summary()` (app.rs)
  - `extract_diff_content()` (app.rs)
  - `extract_tool_summary()` (export.rs)
  - `cache_key()` / `extract_path()` (context/cache.rs)
  - `compress_tool_output()` (context/compressor.rs)
  - `build_permission_summary()` / `extract_tool_path()` (stream.rs)
  - `is_write_tool()` / `intent_category()` / `tool_marker()` (tool/mod.rs)
- [ ] **Permission rules**: New tools have correct permission behavior in all profiles
- [ ] **Cache invalidation**: New write tools invalidate the tool result cache
- [ ] **CLAUDE.md updated**: Architecture changes documented
- [ ] **`cargo test` passes**: All tests green (lib + integration)
