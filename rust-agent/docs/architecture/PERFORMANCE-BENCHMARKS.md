# Performance Benchmarks

Latency and throughput checks live as ignored tests so normal `cargo test` stays
fast and deterministic. Run them in release mode:

```bash
cargo test --release -- --ignored --nocapture
```

Each benchmark prints min, median, max, and a workload-specific throughput
counter. They assert correctness but do not enforce timing thresholds.

## Benchmarks

- `agent_loop::tests::benchmark_context_window_large_tool_history`
  Measures prompt-window construction and history pruning over a large transcript
  with tool call/output pairs.
- `agent_loop::tests::benchmark_tool_round_large_outputs`
  Measures a full tool round that reads many capped large files and stores the
  resulting tool transcript.
- `storage::tests::benchmark_sqlite_session_database_large_history`
  Measures SQLite session-message insert throughput and full-history load
  latency over a large persisted transcript.
- `client::tests::benchmark_sse_parser_large_stream`
  Measures SSE parser throughput over a synthetic 20,000-event model stream.
- `client::tests::benchmark_responses_request_serialization_large_history`
  Measures typed Responses request serialization for a large mixed conversation
  history.
- `tools::tests::benchmark_read_file_capped_large_file`
  Measures `read_file` latency against a large file while enforcing the capped
  read path.
- `tools::tests::benchmark_read_file_range_large_file`
  Measures targeted range reads against a large file.
- `tools::tests::benchmark_list_dir_large_directory`
  Measures directory listing latency for a large directory while returning a
  capped result set.
- `tools::tests::benchmark_exec_command_large_output`
  Measures command execution over a command that writes far more output than the
  retained-output cap.
- `tools::tests::benchmark_fff_search_current_repo`
  Measures cold FFF index initialization plus warm fuzzy path search and content
  search against the current repository.
- `tools::tests::benchmark_fff_parallel_search_current_repo`
  Measures concurrently issued warm FFF path and content searches against the
  current repository.
- `tools::tests::benchmark_search_text_large_line_output`
  Measures text-search formatting when a matched line is very large.
- `tools::tests::benchmark_apply_patch_many_large_files`
  Measures patch planning and application over many large UTF-8 files.
- `server::tests::benchmark_sse_event_encoding`
  Measures server event JSON plus SSE frame encoding throughput.
- `tests::benchmark_delta_writer_many_small_deltas`
  Measures terminal delta batching for many small streamed chunks.

Run one benchmark by filtering its test name before the ignored-test arguments:

```bash
cargo test --release client::tests::benchmark_sse_parser_large_stream -- --ignored --nocapture
```
