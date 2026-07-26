import type {
  CodebaseDetailResponse,
  CodebaseListResponse,
  DashboardOverviewResponse,
  FileTreeResponse,
  GraphDataResponse,
  HealthResponse,
  SearchResultsResponse,
} from "../types";

const API_BASE = "http://127.0.0.1:47269";

async function request<T>(path: string): Promise<T> {
  const response = await fetch(`${API_BASE}${path}`);
  if (!response.ok) {
    throw new Error(`API request failed (${response.status}): ${path}`);
  }
  return response.json() as Promise<T>;
}

const search = (query: string, limit = 20) =>
  request<SearchResultsResponse>(
    `/api/search?q=${encodeURIComponent(query)}&limit=${limit}`,
  );

export const api = {
  health: () => request<HealthResponse>("/api/health"),
  dashboardOverview: () => request<DashboardOverviewResponse>("/api/dashboard/overview"),
  listCodebases: () => request<CodebaseListResponse>("/api/codebases"),
  getCodebase: (id: string) =>
    request<CodebaseDetailResponse>(`/api/codebases/${encodeURIComponent(id)}`),
  getGraph: (id: string) =>
    request<GraphDataResponse>(`/api/codebases/${encodeURIComponent(id)}/graph`),
  getFileTree: (id: string) =>
    request<FileTreeResponse>(`/api/codebases/${encodeURIComponent(id)}/files`),
  search,
  connectEvents: () => new WebSocket("ws://127.0.0.1:47269/ws/events"),
};

export default api;
