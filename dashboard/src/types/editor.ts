export interface FileNode {
  id: string;
  name: string;
  type: 'file' | 'directory';
  path: string;
  language?: string;
  size?: number;
  lastModified?: string;
  children?: FileNode[];
  isExpanded?: boolean;
}

export interface FileContent {
  path: string;
  content: string;
  language: string;
  size: number;
  lastModified: string;
}

export interface Change {
  lineStart: number;
  lineEnd: number;
  replacement: string;
  changeType: 'replace' | 'insert' | 'delete';
}

export interface EditPreview {
  original: string;
  modified: string;
  diff: string;
  impact: {
    affectedNodes: string[];
    affectedFiles: string[];
  };
}

export interface ValidationIssue {
  type: 'error' | 'warning' | 'info';
  message: string;
  line?: number;
  column?: number;
}
