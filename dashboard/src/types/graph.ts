import type { ForceGraphMethods } from 'react-force-graph-2d';

export type NodeType = 'function' | 'class' | 'method' | 'variable' | 'module';
export type EdgeType = 'call' | 'data_dependency' | 'inheritance' | 'import';

export interface GraphNode {
  id: string;
  name: string;
  type: NodeType;
  val: number;
  color: string;
  language: string;
  complexity: number;
  filePath: string;
  byteRange: [number, number];
  x?: number;
  y?: number;
}

export interface GraphLink {
  source: string;
  target: string;
  type: EdgeType;
  value: number;
}

export interface GraphData {
  nodes: GraphNode[];
  links: GraphLink[];
}

export interface GraphDelta {
  added: GraphNode[];
  removed: string[];
  modified: GraphNode[];
  addedLinks: GraphLink[];
  removedLinks: Array<{ source: string; target: string }>;
}

export interface GraphViewHandle {
  zoom: (factor: number, duration?: number) => void;
  fitView: (duration?: number, padding?: number) => void;
  centerAt: (x: number, y: number, duration?: number) => void;
  getGraphRef: () => any;
}
