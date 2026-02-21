export interface ApiResponse<T> {
  data: T;
  success: boolean;
  error?: string;
}

export interface SyncReport {
  newlyDiscovered: number;
  updated: number;
  invalidated: number;
  missing: number;
  unchanged: number;
  errors: number;
}

export interface SearchResult {
  rank: number;
  nodeId: string;
  filePath: string;
  symbolName: string;
  language: string;
  score: {
    semantic: number;
    textMatch: number;
    structural: number;
    overall: number;
  };
  context?: string;
  byteRange: [number, number];
}

export interface ServerEvent {
  type: string;
  [key: string]: unknown;
}

export interface IndexingProgressEvent extends ServerEvent {
  type: 'indexing.progress';
  codebaseId: string;
  phase: number;
  percent: number;
  currentFile: string;
}

export interface GraphDeltaEvent extends ServerEvent {
  type: 'graph.delta';
  codebaseId: string;
  delta: import('./graph').GraphDelta;
}