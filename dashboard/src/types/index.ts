export interface HealthResponse {
  status: string;
  service: string;
  version: string;
  active_connections: number;
}

export interface Codebase {
  id: string;
  unique_project_id: string;
  base_name: string;
  path_hash: string;
  instance: number;
  project_path: string;
  display_name: string;
  project_type: string;
  last_indexed: string;
  file_count: number;
  node_count: number;
  edge_count: number;
  is_valid: boolean;
  is_clone: boolean;
  cloned_from?: string;
}

export interface CodebaseListResponse {
  codebases: Codebase[];
  total: number;
}

export interface CodebaseDetailResponse {
  codebase: Codebase;
}

export interface GraphNode {
  id: string;
  name: string;
  type: string;
  val: number;
  color: string;
  language: string;
  complexity: number;
  file_path: string;
  byte_range: [number, number];
}

export interface GraphLink {
  source: string;
  target: string;
  type: string;
  value: number;
}

export interface GraphDataResponse {
  nodes: GraphNode[];
  links: GraphLink[];
}

export interface FileNode {
  name: string;
  path: string;
  type: "file" | "directory";
  size?: number;
  last_modified?: string;
  children?: FileNode[];
}

export interface FileTreeResponse {
  tree: FileNode[];
}

export interface SearchScore {
  semantic: number;
  text_match: number;
  structural: number;
  overall: number;
}

export interface SearchResultResponse {
  rank: number;
  node_id: string;
  file_path: string;
  symbol_name: string;
  language: string;
  score: SearchScore;
  context?: string;
  byte_range: [number, number];
}

export interface SearchResultsResponse {
  results: SearchResultResponse[];
}

export interface DashboardLanguageDistribution {
  language: string;
  count: number;
}

export interface DashboardCodebaseMetrics {
  id: string;
  display_name: string;
  project_path: string;
  file_count: number;
  node_count: number;
  edge_count: number;
  import_edge_count: number;
  external_ref_count: number;
  dependency_link_count: number;
}

export interface DashboardFeatureStatus {
  multi_project_enabled: boolean;
  cache_telemetry_enabled: boolean;
  external_dependency_resolution_enabled: boolean;
  context_aware_editing_enabled: boolean;
  bounded_impact_analysis_enabled: boolean;
}

export interface DashboardCacheOverview {
  analysis_cache_entries: number;
  temperature: string;
  estimated_hit_rate?: number;
}

export interface DashboardExternalDependencies {
  external_refs: number;
  project_dependency_links: number;
  import_edges: number;
}

export interface DashboardOverviewResponse {
  generated_at: number;
  status: string;
  total_codebases: number;
  total_files: number;
  total_nodes: number;
  total_edges: number;
  language_distribution: DashboardLanguageDistribution[];
  feature_status: DashboardFeatureStatus;
  cache: DashboardCacheOverview;
  external_dependencies: DashboardExternalDependencies;
  codebases: DashboardCodebaseMetrics[];
}

export interface WsEvent {
  type: string;
  timestamp?: number;
  codebase_id?: string;
  display_name?: string;
  base_name?: string;
  phase?: number;
  percent?: number;
  current_file?: string;
}
