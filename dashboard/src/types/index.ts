export * from './api';
export * from './graph';
export * from './editor';

export interface Codebase {
  id: string;
  uniqueProjectId: string;
  baseName: string;
  pathHash: string;
  instance: number;
  projectPath: string;
  displayName: string;
  projectType: string;
  lastIndexed: string;
  fileCount: number;
  nodeCount: number;
  edgeCount: number;
  isValid: boolean;
  isClone: boolean;
  clonedFrom?: string;
}

export interface CodebaseListResponse {
  codebases: Codebase[];
  total: number;
}

export interface SyncStatus {
  pendingUpdates: number;
  lastSync: string;
  isSyncing: boolean;
}

export type PanelType = 'codebases' | 'graph' | 'search' | 'editor' | 'settings';

export interface EditOperation {
  id: string;
  codebaseId: string;
  filePath: string;
  changes: Change[];
  status: 'pending' | 'staged' | 'applied' | 'failed';
  createdAt: string;
}

export interface Change {
  lineStart: number;
  lineEnd: number;
  replacement: string;
  changeType: 'replace' | 'insert' | 'delete';
}