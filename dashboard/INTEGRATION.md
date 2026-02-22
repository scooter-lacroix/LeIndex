# Backend Integration Guide

This document outlines the API endpoints, data shapes, and WebSocket event formats required for the LeIndex dashboard to function correctly.

## API Endpoints

### Base URL
- Development: `http://127.0.0.1:47269`
- Configurable via `VITE_API_BASE_URL` environment variable

### 1. Codebases

#### `GET /api/codebases`

List all indexed codebases.

**Response:**
```json
{
  "codebases": [
    {
      "id": "string (UUID)",
      "uniqueProjectId": "owner/repo",
      "displayName": "string",
      "fileCount": 0,
      "nodeCount": 0,
      "edgeCount": 0,
      "lastIndexed": "ISO 8601 timestamp",
      "projectPath": "string",
      "isValid": true,
      "isClone": false,
      "clonedFrom": "string or null"
    }
  ]
}
```

#### `GET /api/codebases/:id`

Get details for a specific codebase.

**Response:**
```json
{
  "codebase": {
    "id": "string",
    "uniqueProjectId": "string",
    "displayName": "string",
    "fileCount": 0,
    "nodeCount": 0,
    "edgeCount": 0,
    "lastIndexed": "string",
    "projectPath": "string",
    "isValid": true,
    "isClone": false,
    "clonedFrom": "string or null"
  }
}
```

#### `POST /api/codebases/refresh`

Trigger a refresh of the codebase list.

**Response:**
```json
{
  "status": "ok",
  "refreshedCount": 0
}
```

### 2. Files

#### `GET /api/codebases/:id/files`

Get the file tree structure for a codebase.

**Query Parameters:**
- `path` (optional): Subdirectory path to list, defaults to root

**Response:**
```json
{
  "tree": [
    {
      "id": "string (full path)",
      "name": "string",
      "type": "file" | "directory",
      "path": "string (relative to codebase root)",
      "children": [] // present only if type: "directory"
    }
  ]
}
```

#### `GET /api/codebases/:id/files/:path`

Get content of a specific file.

**Response:**
```json
{
  "path": "string",
  "content": "string (file contents)",
  "language": "string (e.g., 'typescript', 'python')",
  "size": 0,
  "lastModified": "ISO 8601 timestamp"
}
```

### 3. Graph

#### `GET /api/codebases/:id/graph`

Get the full dependency graph for a codebase.

**Response:**
```json
{
  "nodes": [
    {
      "id": "string (unique identifier)",
      "name": "string",
      "type": "file" | "function" | "class" | "variable",
      "filePath": "string",
      "language": "string",
      "x": 0,
      "y": 0,
      "vx": 0,
      "vy": 0,
      "fx": null | number,
      "fy": null | number
    }
  ],
  "edges": [
    {
      "source": "string (node id)",
      "target": "string (node id)",
      "type": "imports" | "calls" | "uses" | "references"
    }
  ]
}
```

#### `GET /api/codebases/:id/graph/node/:nodeId`

Get a specific node with its immediate neighbors.

**Response:**
```json
{
  "node": {
    "id": "string",
    "name": "string",
    "type": "string",
    "filePath": "string",
    "language": "string",
    "x": 0,
    "y": 0,
    "vx": 0,
    "vy": 0,
    "fx": null,
    "fy": null
  },
  "neighbors": [
    // Array of node objects
  ]
}
```

### 4. Search

#### `GET /api/search`

Search across all indexed codebases.

**Query Parameters:**
- `q` (required): Search query string
- `limit` (optional): Maximum results to return (default: 20)
- `language` (optional): Filter by programming language
- `fileType` (optional): Filter by file extension

**Response:**
```json
{
  "results": [
    {
      "codebaseId": "string",
      "filePath": "string",
      "lineNumber": 0,
      "content": "string (matching snippet)",
      "context": "string (surrounding code)"
    }
  ]
}
```

### 5. Edit Operations

#### `POST /api/codebases/:id/edit/preview`

Preview changes without applying them.

**Request:**
```json
{
  "changes": [
    {
      "filePath": "string",
      "type": "insert" | "delete" | "replace",
      "position": 0,
      "length": 0,
      "content": "string" // for insert/replace
    }
  ]
}
```

**Response:**
```json
{
  "preview": {
    "filePath": "string",
    "original": "string",
    "modified": "string",
    "diff": "string (unified diff format)"
  },
  "issues": [
    {
      "severity": "error" | "warning" | "info",
      "message": "string",
      "line": 0,
      "column": 0
    }
  ]
}
```

#### `POST /api/codebases/:id/edit/stage`

Stage changes for later application.

**Request:** Same as preview endpoint

**Response:**
```json
{
  "editId": "string (UUID for staged edit)",
  "worktreePath": "string (optional path to temporary worktree)"
}
```

#### `POST /api/edit/staged/:editId/apply`

Apply staged changes permanently.

**Response:**
```json
{
  "status": "applied",
  "appliedAt": "ISO 8601 timestamp"
}
```

#### `POST /api/edit/staged/:editId/discard`

Discard staged changes.

**Response:**
```json
{
  "status": "discarded"
}
```

## WebSocket Events

### Connection
- URL: `ws://127.0.0.1:47269/ws/events`
- No authentication required (development mode)
- Supports multiple concurrent clients

### Event Types

#### `graph_update`

Sent when a codebase's graph changes (e.g., after indexing or file modifications).

**Payload:**
```json
{
  "type": "graph_update",
  "codebaseId": "string",
  "timestamp": "ISO 8601",
  "nodes": [...],  // Full or partial node array
  "edges": [...]   // Full or partial edge array
}
```

#### `indexing_progress`

Sent during codebase indexing to show progress.

**Payload:**
```json
{
  "type": "indexing_progress",
  "codebaseId": "string" or null (for global progress),
  "progress": 0-100,
  "currentFile": "string (optional)",
  "status": "string (e.g., 'scanning', 'analyzing', 'building-graph')"
}
```

#### `edit_applied`

Sent when an edit is successfully applied to the codebase.

**Payload:**
```json
{
  "type": "edit_applied",
  "codebaseId": "string",
  "editId": "string",
  "filePath": "string",
  "timestamp": "ISO 8601"
}
```

#### `error`

Generic error notification.

**Payload:**
```json
{
  "type": "error",
  "codebaseId": "string or null",
  "message": "string",
  "details": "string (optional)",
  "severity": "error" | "warning" | "info"
}
```

## Testing the Integration

### 1. API Tests

Use `curl` or similar to test endpoints:

```bash
# List codebases
curl http://127.0.0.1:47269/api/codebases

# Get graph for specific codebase
curl http://127.0.0.1:47269/api/codebases/ABC123/graph

# Get file tree
curl http://127.0.0.1:47269/api/codebases/ABC123/files

# Search
curl "http://127.0.0.1:47269/api/search?q=function&limit=10"
```

### 2. WebSocket Test

```javascript
// In browser console
const ws = new WebSocket('ws://127.0.0.1:47269/ws/events');
ws.onmessage = (event) => console.log('WS:', JSON.parse(event.data));
ws.onopen = () => console.log('Connected');
```

### 3. Dashboard Connection

1. Start the backend server
2. Run `bun run dev` in the dashboard directory
3. Open `http://localhost:5173`
4. The dashboard will attempt to connect to the API base URL
5. Check browser console for any connection errors
6. If endpoints are missing, the dashboard will show appropriate error messages

### 4. Validation Checklist

- [ ] `GET /api/codebases` returns proper format
- [ ] `GET /api/codebases/:id/graph` returns nodes and edges arrays
- [ ] `GET /api/codebases/:id/files` returns file tree
- [ ] `GET /api/codebases/:id/files/:path` returns file content with language detection
- [ ] WebSocket connection established and sends `graph_update` events
- [ ] Graph visualization renders correctly with >100 nodes
- [ ] Node colors match defined legend (file=blue, function=green, class=yellow, variable=red)
- [ ] Clicking CodebaseCard navigates to `/graph?codebaseId=...`
- [ ] Editor loads file content and shows syntax highlighting
- [ ] Edit staging and apply flow works end-to-end

## Expected Behavior

When all endpoints are implemented:

1. **Codebases Page**: Shows list of indexed codebases with statistics
2. **Graph Page**: Interactive force-directed graph with zoom/pan, node selection shows details
3. **Editor Page**: CodeMirror editor with syntax highlighting, diff preview, staging/apply buttons
4. **Real-time Updates**: Graph updates automatically when backend indexing completes
5. **Navigation**: Seamless routing between pages with TanStack Router

## Mock Mode

The dashboard includes some mock data fallbacks when API endpoints return errors. This allows UI development without a complete backend. However, for full functionality, all endpoints must be implemented.

## Support

For issues or questions about the integration, refer to the main LeIndex project repository.
