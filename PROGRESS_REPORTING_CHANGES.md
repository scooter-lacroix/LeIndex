# Progress Reporting and SSE Implementation

**Date:** 2026-02-13
**Status:** ✅ Complete and Building

## Summary

Successfully implemented Server-Sent Events (SSE) support for streaming indexing progress, eliminating the need for arbitrary timeouts during project indexing. The implementation includes:

1. **ProgressEvent struct** - Defined in `protocol.rs` with helper methods for progress, completion, and error events
2. **SSE endpoint** - `/mcp/index/stream` that accepts POST requests and streams progress updates
3. **Storage close()** - WAL checkpoint to release SQLite file locks before switching projects
4. **Integration** - Both handlers and server.rs now call `close()` before switching projects

## Files Modified

### 1. `crates/lestockage/src/schema.rs`
- **Added:** `close()` method to force WAL checkpoint
- **Purpose:** Release SQLite file locks cleanly

### 2. `crates/lepasserelle/src/leindex.rs`
- **Added:** `close()` method that delegates to storage
- **Purpose:** Expose storage cleanup functionality

### 3. `crates/lepasserelle/src/mcp/protocol.rs`
- **Added:** `ProgressEvent` struct with `progress()`, `complete()`, and `error()` helpers
- **Purpose:** Define structured progress events for SSE streaming

### 4. `crates/lepasserelle/src/mcp/server.rs`
- **Added:** `index_stream_handler()` function for SSE streaming
- **Added:** `index_with_progress()` helper for background indexing
- **Modified:** Router now includes `/mcp/index/stream` route
- **Purpose:** Enable SSE streaming endpoint

### 5. `crates/lepasserelle/src/mcp/handlers.rs`
- **Modified:** `IndexHandler::execute()` now calls `close()` before switching projects
- **Purpose:** Ensure clean resource management

### 6. `crates/lepasserelle/src/mcp/sse.rs`
- **Status:** Module exists but SSE functionality has been integrated directly into server.rs
- **Note:** The separate SSE module is kept for reference but not actively used

## SSE Endpoint Usage

### URL: `POST /mcp/index/stream`

### Request Body:
```json
{
  "project_path": "/absolute/path/to/project",
  "force_reindex": false
}
```

### Progress Event Format:
```json
{
  "type": "progress" | "complete" | "error",
  "stage": "starting" | "collecting" | "switching_projects" | "loading_storage" | "indexing",
  "current": 0,
  "total": 0,
  "message": "Human-readable status message",
  "timestamp_ms": 1234567890
}
```

### Event Flow During Indexing:
1. **Starting Event** - "Starting indexing for: {project_path}"
2. **Collecting Event** - "Collecting source files..."
3. **Switching Projects Event** - When switching from project A to B (if needed)
4. **Loading Storage Event** - "Loading indexed data from storage..."
5. **Completion Event** - "Done: {files_parsed} files"
6. **Error Event** - "Error: {error_message}" (if indexing fails)

## Architecture

```
Client Request
       |
       v
+-------------------+
| /mcp/index/stream|
+-------------------+
       |
       v
+-------------------+       +-------------------+
| index_stream      |------>| mpsc::channel    |
| _handler          |       | (100 buffer)     |
+-------------------+       +-------------------+
                                    |
                                    v
                         +-------------------+
                         | tokio::spawn      |
                         | (background task)|
                         +-------------------+
                                    |
        +---------------------------+---------------------------+
        |                           |                           |
        v                           v                           v
+-------------+           +-------------+           +-------------+
| Check if    |           | Spawn       |           | Load from   |
| indexed      |           | blocking     |           | storage     |
+-------------+           | task to      |           +-------------+
        |                   | index        |                   |
        v                   +-------------+                   |
+-------------+                      |                      |
| Send events |                      v                      |
| via channel |               +-------------+                |
+-------------+               | Progress     |                |
        |                   | Events       |                |
        v                   +-------------+                |
+-------------------+                    |                   |
| ReceiverStream    |<-------------------+                   |
| wraps channel  |                                        |
+-------------------+                                        |
        |                                                   |
        v                                                   |
+-------------------+                                    |
| SSE Stream       |                                    |
| Sse::new(stream) |                                    |
+-------------------+                                    |
        |                                                   |
        v                                                   |
    Client Response (SSE stream)
```

## Compilation Status

✅ **Build succeeds** - `cargo build` completes without errors
✅ **Tests pass** - All unit tests pass
✅ **Clean build** - No warnings (unused imports removed)

## Key Design Decisions

1. **SSE in server.rs** - The SSE handler was integrated directly into server.rs rather than keeping a separate sse.rs module to avoid complexity with module imports and shared state access.

2. **Channel size of 100** - The mpsc channel has a buffer of 100 events. If the client can't keep up, events will be buffered. This is reasonable for typical indexing operations.

3. **15-second keep-alive** - SSE connections send a "keep-alive" comment every 15 seconds to prevent proxies from closing idle connections.

4. **Infallible error type** - The stream uses `Infallible` as the error type since the closure should never produce errors - errors are sent through the channel as `ProgressEvent::error()` instead.

## Future Improvements

1. **Granular progress callbacks** - Currently, progress events are only sent at key points (starting, collecting, switching, loading, complete). The actual `index_project()` method doesn't have progress callback support for file-by-file progress.

2. **Connection pooling** - Consider implementing connection pooling at Storage layer for better resource management.

3. **Project-specific tokens** - Add token budget parameter to SSE handler to limit indexing resources.

4. **Cancelation support** - Allow clients to cancel in-flight indexing operations via a separate endpoint.

## Testing the SSE Endpoint

```bash
# Using curl
curl -N -X POST http://localhost:3000/mcp/index/stream \
  -H "Content-Type: application/json" \
  -d '{"project_path": "/path/to/project", "force_reindex": false}'

# Using JavaScript
const response = await fetch('/mcp/index/stream', {
  method: 'POST',
  headers: {'Content-Type': 'application/json'},
  body: JSON.stringify({
    project_path: '/path/to/project',
    force_reindex: false
  })
});

const reader = response.body.getReader();
const decoder = new TextDecoder();

while (true) {
  const { done, value } = await reader.read();
  if (done) break;
  const chunk = decoder.decode(value);
  const lines = chunk.split('\n');
  for (const line of lines) {
    if (line.startsWith('data: ')) {
      const event = JSON.parse(line.slice(6));
      console.log('Progress:', event);
    }
  }
}
```
