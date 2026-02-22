# LeIndex Dashboard - Frontend Implementation Plan

## Overview

Bun/React frontend for LeIndex with sophisticated graph visualization, real-time updates, and code editing capabilities.

## Technology Stack

| Category | Technology | Version |
|----------|------------|---------|
| Runtime | Bun | Latest |
| Framework | React | ^18.2.0 |
| Language | TypeScript | ^5.3.0 |
| Styling | TailwindCSS | ^3.4.0 |
| Components | shadcn/ui | Latest |
| State (Server) | TanStack Query | ^5.0.0 |
| State (Client) | Zustand | ^4.4.0 |
| Router | TanStack Router | ^1.0.0 |
| Graph | react-force-graph-2d | ^1.25.0 |
| Editor | @uiw/react-codemirror | ^4.21.0 |
| Diff | codemirror-merge | ^6.0.0 |
| Icons | Lucide React | ^0.300.0 |
| WebSocket | Native WebSocket API | - |

## Project Structure

```
/dashboard/
├── package.json
├── bun.lockb
├── tsconfig.json
├── vite.config.ts
├── index.html
├── public/
│   └── favicon.ico
└── src/
    ├── main.tsx
    ├── App.tsx
    ├── router.tsx
    ├── index.css
    ├── types/
    │   ├── index.ts
    │   ├── graph.ts
    │   ├── edit.ts
    │   └── api.ts
    ├── components/
    │   ├── Layout/
    │   ├── Graph/
    │   ├── Codebase/
    │   ├── Editor/
    │   ├── Search/
    │   └── UI/
    ├── hooks/
    ├── lib/
    ├── stores/
    └── routes/
```

## Phase 1: Foundation (Days 1-3)

### Day 1: Project Setup
- [ ] Initialize Bun project
- [ ] Configure TypeScript with strict mode
- [ ] Setup Vite with Bun plugin
- [ ] Configure TailwindCSS
- [ ] Initialize shadcn/ui
- [ ] Setup TanStack Router
- [ ] Setup TanStack Query
- [ ] Configure Zustand stores
- [ ] Create basic layout components

### Day 2: API Client & Types
- [ ] Define TypeScript interfaces
- [ ] Create API client with fetch
- [ ] Setup TanStack Query hooks
- [ ] Error boundary configuration
- [ ] Loading state components

### Day 3: Core Layout
- [ ] Root layout with sidebar
- [ ] Header with navigation
- [ ] Resizable panels
- [ ] Theme configuration
- [ ] Keyboard shortcuts

## Phase 2: Graph Visualization (Days 4-7)

### Day 4: Graph Component Setup
- [ ] Install react-force-graph-2d
- [ ] Create ForceGraph wrapper
- [ ] GraphView container component
- [ ] Basic node/link rendering

### Day 5: Graph Interactions
- [ ] Node click handling
- [ ] Zoom/pan controls
- [ ] Node selection
- [ ] Multi-select with Shift
- [ ] Context menu

### Day 6: Advanced Features
- [ ] Minimap implementation
- [ ] Graph controls (filter, layout)
- [ ] Node tooltip with CodeMirror preview
- [ ] Search highlighting in graph

### Day 7: Real-time Updates
- [ ] WebSocket connection
- [ ] Graph delta updates
- [ ] Optimistic updates
- [ ] Connection status UI

## Phase 3: Codebase Management (Days 8-10)

### Day 8: Codebase List
- [ ] CodebaseList component
- [ ] CodebaseCard with unique IDs
- [ ] Clone detection indicators
- [ ] Indexing progress display

### Day 9: File Browser
- [ ] File tree component
- [ ] File preview panel
- [ ] Syntax highlighting
- [ ] Breadcrumb navigation

### Day 10: Search Interface
- [ ] Search bar with autocomplete
- [ ] Results list with snippets
- [ ] Filter sidebar
- [ ] Recent searches

## Phase 4: Code Editing (Days 11-15)

### Day 11: Editor Setup
- [ ] CodeMirror 6 integration
- [ ] Theme matching
- [ ] Language modes
- [ ] Line numbers

### Day 12: Diff Viewer
- [ ] Side-by-side diff
- [ ] Inline diff option
- [ ] Diff navigation
- [ ] Accept/reject hunks

### Day 13: Edit Panel
- [ ] Edit preview modal
- [ ] Impact analysis display
- [ ] Validation results
- [ ] Stage/apply controls

### Day 14: Refactoring
- [ ] Refactor menu
- [ ] Rename symbol UI
- [ ] Extract function UI
- [ ] Move to module UI

### Day 15: History
- [ ] Undo/redo buttons
- [ ] History panel
- [ ] Edit comparison
- [ ] Restore points

## Phase 5: Polish (Days 16-18)

### Day 16: Export & Sharing
- [ ] PNG export
- [ ] SVG export
- [ ] JSON export
- [ ] GraphML export

### Day 17: Performance
- [ ] Virtual scrolling
- [ ] Code splitting
- [ ] Lazy loading
- [ ] Service worker

### Day 18: Testing & QA
- [ ] Component tests
- [ ] E2E tests
- [ ] Performance profiling
- [ ] Accessibility audit

## Component Specifications

### GraphView
```typescript
interface GraphViewProps {
  codebaseId: string;
  initialNodes?: GraphNode[];
  initialEdges?: GraphLink[];
  onNodeSelect?: (node: GraphNode) => void;
  onNodeDoubleClick?: (node: GraphNode) => void;
  height?: number | string;
}

interface GraphNode {
  id: string;
  name: string;
  type: 'function' | 'class' | 'method' | 'variable' | 'module';
  val: number;
  color: string;
  language: string;
  complexity: number;
  x?: number;
  y?: number;
}

interface GraphLink {
  source: string;
  target: string;
  type: EdgeType;
  value: number;
}
```

### CodeEditor
```typescript
interface CodeEditorProps {
  content: string;
  language: string;
  readOnly?: boolean;
  onChange?: (value: string) => void;
  highlights?: Range[];
  decorations?: Decoration[];
}
```

### DiffViewer
```typescript
interface DiffViewerProps {
  original: string;
  modified: string;
  language: string;
  onAccept?: (hunk: Hunk) => void;
  onReject?: (hunk: Hunk) => void;
}
```

## API Integration

### TanStack Query Hooks

```typescript
// useCodebases.ts
export const useCodebases = () => {
  return useQuery({
    queryKey: ['codebases'],
    queryFn: fetchCodebases,
    staleTime: 30000,
  });
};

// useGraph.ts
export const useGraph = (codebaseId: string) => {
  return useQuery({
    queryKey: ['graph', codebaseId],
    queryFn: () => fetchGraph(codebaseId),
    staleTime: Infinity,
  });
};

// useWebSocket.ts
export const useWebSocket = (codebaseId: string) => {
  // WebSocket connection management
};
```

## State Management (Zustand)

```typescript
// stores/uiStore.ts
interface UIState {
  sidebarOpen: boolean;
  activePanel: PanelType;
  theme: 'light' | 'dark';
  setSidebarOpen: (open: boolean) => void;
  setActivePanel: (panel: PanelType) => void;
  toggleTheme: () => void;
}

// stores/graphStore.ts
interface GraphState {
  selectedNodes: Set<string>;
  highlightedNodes: Set<string>;
  zoom: number;
  center: { x: number; y: number };
  selectNode: (id: string) => void;
  deselectNode: (id: string) => void;
  setZoom: (zoom: number) => void;
  setCenter: (x: number, y: number) => void;
}

// stores/editStore.ts
interface EditState {
  stagedEdits: EditOperation[];
  history: EditAction[];
  currentIndex: number;
  stageEdit: (edit: EditOperation) => void;
  unstageEdit: (id: string) => void;
  undo: () => void;
  redo: () => void;
}
```

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| Ctrl/Cmd + K | Open search |
| Ctrl/Cmd + Shift + F | Global search |
| Ctrl/Cmd + Z | Undo |
| Ctrl/Cmd + Shift + Z | Redo |
| Ctrl/Cmd + / | Toggle comment |
| Escape | Close panel/modal |
| G | Focus graph |
| F | Focus file browser |
| Shift + Click | Multi-select |

## Dependencies

```json
{
  "dependencies": {
    "react": "^18.2.0",
    "react-dom": "^18.2.0",
    "@tanstack/react-query": "^5.0.0",
    "@tanstack/react-router": "^1.0.0",
    "zustand": "^4.4.0",
    "react-force-graph-2d": "^1.25.0",
    "@uiw/react-codemirror": "^4.21.0",
    "codemirror": "^6.0.0",
    "@codemirror/lang-javascript": "^6.2.0",
    "@codemirror/lang-rust": "^6.0.0",
    "@codemirror/lang-python": "^6.1.0",
    "@codemirror/merge": "^6.0.0",
    "lucide-react": "^0.300.0",
    "tailwind-merge": "^2.2.0",
    "clsx": "^2.0.0",
    "date-fns": "^3.0.0",
    "zod": "^3.22.0"
  },
  "devDependencies": {
    "@types/react": "^18.2.0",
    "@types/react-dom": "^18.2.0",
    "typescript": "^5.3.0",
    "vite": "^5.0.0",
    "@vitejs/plugin-react": "^4.2.0",
    "tailwindcss": "^3.4.0",
    "autoprefixer": "^10.4.0",
    "postcss": "^8.4.0",
    "eslint": "^8.56.0",
    "@typescript-eslint/eslint-plugin": "^6.0.0",
    "@typescript-eslint/parser": "^6.0.0"
  }
}
```