# LeIndex Dashboard

A modern React-based dashboard for LeIndex with real-time graph visualization, codebase management, and code editing capabilities.

## Tech Stack

- **Runtime**: Bun
- **Framework**: React 18 + TypeScript
- **Styling**: TailwindCSS + shadcn/ui
- **State**: TanStack Query (server) + Zustand (client)
- **Router**: TanStack Router
- **Graph**: react-force-graph-2d
- **Build**: Vite

## Getting Started

### Prerequisites

- Bun >= 1.0.0
- LeIndex backend server running (default: `http://127.0.0.1:47269`)

### Development

```bash
# Install dependencies
bun install

# Start development server
bun run dev
```

The dev server will start on `http://localhost:5173`

### Build

```bash
# Create production build
bun run build

# Preview production build
bun run preview
```

## Project Structure

```
src/
├── components/
│   ├── Layout/          # Root layout, sidebar, header
│   ├── Graph/           # Graph visualization components
│   ├── Codebase/        # Codebase management UI
│   ├── Editor/          # Code editor components
│   ├── Search/          # Search interface
│   └── UI/              # shadcn/ui components
├── hooks/               # TanStack Query hooks
├── lib/                 # Utilities and API client
├── routes/              # TanStack Router routes
├── stores/              # Zustand state stores
└── types/               # TypeScript type definitions
```

## Features

### Implementation Status: ~65-70% Complete

#### ✅ Completed Features

**Core Infrastructure**
- [x] Bun + React + TypeScript setup
- [x] TailwindCSS + shadcn/ui configuration
- [x] TanStack Router + Query setup
- [x] Zustand state management
- [x] Core layout components with sidebar navigation
- [x] API client with fetch wrapper
- [x] Comprehensive TypeScript type definitions

**Graph Visualization**
- [x] Interactive 2D force-directed graph
- [x] Node/edge rendering with color-coded types
- [x] Zoom, pan, and drag interactions
- [x] Node selection and highlighting
- [x] Graph controls (zoom, fit, save)
- [x] Legend with filtering
- [x] Export to PNG, SVG, JSON
- [x] Real-time WebSocket updates for live graph changes

**Codebase Management**
- [x] Codebase list with unique IDs and statistics
- [x] Clone detection with visual indicators
- [x] File browser with tree structure (mocked data)
- [x] Codebase selection state management
- [x] Refresh functionality
- [x] Navigation cards with direct links to Graph and Editor

**Code Editing**
- [x] CodeMirror 6 integration with syntax highlighting
- [x] Multi-language support (JavaScript, TypeScript, Python, Rust, Go, etc.)
- [x] File content loading via API
- [x] Edit staging and application workflow
- [x] Diff preview of changes
- [x] Validation and conflict detection
- [x] Staged changes tracking

**Routing**
- [x] TanStack Router configuration
- [x] Dynamic routes for graph and editor
- [x] Route-based code splitting
- [x] Navigation between all major views

#### 🔄 In Progress / Backend-Dependent

- [ ] Real file browser data (currently using mock data)
- [ ] Search functionality (UI exists, awaiting backend)
- [ ] Settings page implementation
- [ ] User preferences persistence
- [ ] Authentication/authorization
- [ ] Performance optimizations for large graphs
- [ ] Comprehensive error handling for API failures

## API Integration

The dashboard expects the LeIndex HTTP server to be running on `http://127.0.0.1:47269` (configurable via `VITE_API_BASE_URL` environment variable).

### Required Endpoints

#### Codebases
- `GET /api/codebases` - List all indexed codebases
  ```json
  {
    "codebases": [
      {
        "id": "abc123",
        "uniqueProjectId": "owner/repo",
        "displayName": "My Project",
        "fileCount": 1234,
        "nodeCount": 5678,
        "edgeCount": 9012,
        "lastIndexed": "2025-02-14T10:30:00Z",
        "projectPath": "/path/to/project",
        "isValid": true,
        "isClone": false,
        "clonedFrom": null
      }
    ]
  }
  ```

- `GET /api/codebases/:id` - Get specific codebase details
- `POST /api/codebases/refresh` - Trigger rescan of codebases
  ```json
  { "status": "ok" }
  ```

#### Files
- `GET /api/codebases/:id/files` - Get file tree structure
  ```json
  {
    "tree": [
      {
        "id": "path/to/file",
        "name": "file.ts",
        "type": "file" | "directory",
        "path": "path/to/file",
        "children": [] // only for directories
      }
    ]
  }
  ```
- `GET /api/codebases/:id/files/:path` - Get file content
  ```json
  {
    "path": "src/index.ts",
    "content": "// file content here",
    "language": "typescript",
    "size": 1234,
    "lastModified": "2025-02-14T10:30:00Z"
  }
  ```

#### Graph
- `GET /api/codebases/:id/graph` - Get codebase dependency graph
  ```json
  {
    "nodes": [
      {
        "id": "module/file",
        "name": "file",
        "type": "file" | "function" | "class" | "variable",
        "filePath": "path/to/file",
        "language": "typescript",
        "x": 100,
        "y": 200,
        "vx": 0,
        "vy": 0,
        "fx": null,
        "fy": null
      }
    ],
    "edges": [
      {
        "source": "node1",
        "target": "node2",
        "type": "imports" | "calls" | "uses" | "references"
      }
    ]
  }
  ```
- `GET /api/codebases/:id/graph/node/:nodeId` - Get node with neighbors

#### Search
- `GET /api/search?q=query&limit=20&language=typescript` - Search code
  ```json
  {
    "results": [
      {
        "codebaseId": "abc123",
        "filePath": "src/index.ts",
        "lineNumber": 42,
        "content": "matching content",
        "context": "surrounding code"
      }
    ]
  }
  ```

#### Edit Operations
- `POST /api/codebases/:id/edit/preview` - Preview changes without applying
  ```json
  {
    "preview": {
      "filePath": "src/index.ts",
      "original": "old content",
      "modified": "new content",
      "diff": "@@ -1,3 +1,3 @@..."
    },
    "issues": []
  }
  ```
- `POST /api/codebases/:id/edit/stage` - Stage changes for application
  ```json
  {
    "editId": "stage_abc123"
  }
  ```
- `POST /api/edit/staged/:editId/apply` - Apply staged changes
  ```json
  { "status": "applied" }
  ```
- `POST /api/edit/staged/:editId/discard` - Discard staged changes

#### WebSocket
- `WS /ws/events` - Real-time updates
  - Graph changes: `{ type: "graph_update", codebaseId: "...", nodes: [...], edges: [...] }`
  - Indexing progress: `{ type: "indexing_progress", codebaseId: "...", progress: 75 }`

## Development Workflow

1. Start the LeIndex backend server (`cargo run --bin leindex` or equivalent)
2. Start the dashboard in development mode: `bun run dev`
3. Open `http://localhost:5173` in your browser
4. The dashboard will connect to the backend automatically
5. Select a codebase to view its graph and files

### Configuration

Environment variables (create `.env` file):
- `VITE_API_BASE_URL=http://127.0.0.1:47269` - Backend API URL
- `VITE_WS_BASE_URL=ws://127.0.0.1:47269` - WebSocket URL

## Build Instructions

```bash
# Development build with HMR
bun run dev

# Production build
bun run build

# Preview production build locally
bun run preview
```

Build outputs to `dist/` directory. The built application is static and can be served by any HTTP server.

## Known Limitations

1. **File Browser**: Uses mock data until backend `/api/codebases/:id/files` endpoint is implemented
2. **Search**: UI ready but requires backend `/api/search` endpoint
3. **Settings Page**: Placeholder only, not implemented
4. **Authentication**: None, assumes open backend
5. **Error Handling**: Some API errors may not be user-friendly
6. **Large Graphs**: Performance may degrade with >1000 nodes (optimization pending)

## Testing Locally

1. Start the backend with example data
2. Open dashboard in browser
3. Verify codebase list loads from `/api/codebases`
4. Click on a codebase to view graph at `/graph`
5. Verify WebSocket connections for real-time updates
6. Test file editing by clicking "Editor" on any codebase card

## Contributing

This dashboard is part of the LeIndex project. See main project guidelines for contribution instructions.

## License

Same as LeIndex project
