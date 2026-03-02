import { FormEvent, useEffect, useMemo, useState } from "react";
import { api } from "./lib/api";
import type {
  Codebase,
  DashboardCodebaseMetrics,
  DashboardOverviewResponse,
  FileNode,
  GraphDataResponse,
  HealthResponse,
  SearchResultResponse,
  WsEvent,
} from "./types";

type LoadState = "loading" | "ready" | "error";
type SocketState = "connecting" | "live" | "offline";

function formatCount(value: number): string {
  return new Intl.NumberFormat("en-US", {
    notation: value >= 1000 ? "compact" : "standard",
    maximumFractionDigits: 1,
  }).format(value);
}

function formatPercent(value?: number): string {
  if (typeof value !== "number" || Number.isNaN(value)) {
    return "n/a";
  }
  return `${Math.round(value * 100)}%`;
}

function fromUnixTimestamp(unixTs: number): string {
  return new Date(unixTs * 1000).toLocaleString();
}

function flattenFileCount(nodes: FileNode[]): number {
  let total = 0;
  for (const node of nodes) {
    if (node.type === "file") {
      total += 1;
    }
    if (node.children && node.children.length > 0) {
      total += flattenFileCount(node.children);
    }
  }
  return total;
}

export default function App() {
  const [loadState, setLoadState] = useState<LoadState>("loading");
  const [socketState, setSocketState] = useState<SocketState>("connecting");
  const [error, setError] = useState<string>("");
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [overview, setOverview] = useState<DashboardOverviewResponse | null>(null);
  const [codebases, setCodebases] = useState<Codebase[]>([]);
  const [selectedCodebaseId, setSelectedCodebaseId] = useState<string>("");
  const [selectedCodebase, setSelectedCodebase] = useState<Codebase | null>(null);
  const [selectedGraph, setSelectedGraph] = useState<GraphDataResponse | null>(null);
  const [selectedFileCount, setSelectedFileCount] = useState<number>(0);
  const [searchQuery, setSearchQuery] = useState<string>("auth");
  const [searchResults, setSearchResults] = useState<SearchResultResponse[]>([]);
  const [searching, setSearching] = useState<boolean>(false);
  const [events, setEvents] = useState<WsEvent[]>([]);

  const selectedOverviewMetrics = useMemo<DashboardCodebaseMetrics | null>(() => {
    if (!overview || !selectedCodebaseId) {
      return null;
    }
    return (
      overview.codebases.find((cb) => cb.id === selectedCodebaseId) ?? null
    );
  }, [overview, selectedCodebaseId]);

  const languagePeak = useMemo<number>(() => {
    if (!overview || overview.language_distribution.length === 0) {
      return 1;
    }
    return Math.max(...overview.language_distribution.map((lang) => lang.count), 1);
  }, [overview]);

  const loadDashboard = async () => {
    try {
      setLoadState("loading");
      setError("");
      const [healthRes, overviewRes, listRes] = await Promise.all([
        api.health(),
        api.dashboardOverview(),
        api.listCodebases(),
      ]);
      setHealth(healthRes);
      setOverview(overviewRes);
      setCodebases(listRes.codebases);
      setLoadState("ready");

      const defaultSelection =
        selectedCodebaseId ||
        overviewRes.codebases[0]?.id ||
        listRes.codebases[0]?.id ||
        "";
      if (defaultSelection) {
        setSelectedCodebaseId(defaultSelection);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : "Failed to load dashboard";
      setError(message);
      setLoadState("error");
    }
  };

  const runSearch = async (event?: FormEvent) => {
    event?.preventDefault();
    const trimmed = searchQuery.trim();
    if (!trimmed) {
      setSearchResults([]);
      return;
    }

    try {
      setSearching(true);
      const response = await api.search(trimmed, 12);
      setSearchResults(response.results);
    } catch (err) {
      const message = err instanceof Error ? err.message : "Search failed";
      setError(message);
    } finally {
      setSearching(false);
    }
  };

  useEffect(() => {
    void loadDashboard();
  }, []);

  useEffect(() => {
    if (!selectedCodebaseId) {
      return;
    }

    let cancelled = false;
    const loadSelected = async () => {
      try {
        const [detail, graph, files] = await Promise.all([
          api.getCodebase(selectedCodebaseId),
          api.getGraph(selectedCodebaseId),
          api.getFileTree(selectedCodebaseId),
        ]);

        if (cancelled) {
          return;
        }
        setSelectedCodebase(detail.codebase);
        setSelectedGraph(graph);
        setSelectedFileCount(flattenFileCount(files.tree));
      } catch (err) {
        if (!cancelled) {
          const message =
            err instanceof Error ? err.message : "Failed to load selected codebase";
          setError(message);
        }
      }
    };

    void loadSelected();
    return () => {
      cancelled = true;
    };
  }, [selectedCodebaseId]);

  useEffect(() => {
    const ws = api.connectEvents();
    setSocketState("connecting");

    ws.onopen = () => setSocketState("live");
    ws.onerror = () => setSocketState("offline");
    ws.onclose = () => setSocketState("offline");
    ws.onmessage = (message) => {
      try {
        const parsed = JSON.parse(message.data as string) as WsEvent;
        setEvents((previous) => [parsed, ...previous].slice(0, 24));
      } catch {
        // Ignore malformed events so dashboard remains stable.
      }
    };

    return () => ws.close();
  }, []);

  return (
    <main className="app-shell">
      <section className="hero-panel">
        <div className="hero-mesh" />
        <div>
          <p className="eyebrow">LeIndex Control Room</p>
          <h1 className="hero-title">Operational Graph Intelligence Dashboard</h1>
          <p className="hero-subtitle">
            Multi-codebase visibility, dependency analytics, cache thermals, and
            live event telemetry in one surface.
          </p>
        </div>
        <div className="hero-status">
          <span className={`dot dot-${socketState}`} />
          <span className="status-label">Socket: {socketState}</span>
          <span className="status-item">
            Service: {health?.service ?? "unknown"} {health?.version ?? ""}
          </span>
          <span className="status-item">
            Snapshot: {overview ? fromUnixTimestamp(overview.generated_at) : "n/a"}
          </span>
        </div>
      </section>

      {error && <div className="error-banner">{error}</div>}

      <section className="metric-grid">
        <article className="metric-card">
          <p>Total Codebases</p>
          <h2>{formatCount(overview?.total_codebases ?? 0)}</h2>
        </article>
        <article className="metric-card">
          <p>Indexed Files</p>
          <h2>{formatCount(overview?.total_files ?? 0)}</h2>
        </article>
        <article className="metric-card">
          <p>Indexed Nodes</p>
          <h2>{formatCount(overview?.total_nodes ?? 0)}</h2>
        </article>
        <article className="metric-card">
          <p>Graph Edges</p>
          <h2>{formatCount(overview?.total_edges ?? 0)}</h2>
        </article>
      </section>

      <section className="panel-grid">
        <article className="panel">
          <header className="panel-header">
            <h3>Codebases</h3>
            <button className="button" onClick={() => void loadDashboard()}>
              Refresh
            </button>
          </header>
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Project</th>
                  <th>Files</th>
                  <th>Nodes</th>
                  <th>Edges</th>
                </tr>
              </thead>
              <tbody>
                {codebases.map((codebase) => (
                  <tr
                    key={codebase.id}
                    className={
                      selectedCodebaseId === codebase.id ? "row-selected" : ""
                    }
                    onClick={() => setSelectedCodebaseId(codebase.id)}
                  >
                    <td>{codebase.display_name}</td>
                    <td>{formatCount(codebase.file_count)}</td>
                    <td>{formatCount(codebase.node_count)}</td>
                    <td>{formatCount(codebase.edge_count)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </article>

        <article className="panel">
          <header className="panel-header">
            <h3>Selected Codebase</h3>
            <span>{selectedCodebase?.display_name ?? "None selected"}</span>
          </header>
          <div className="codebase-detail">
            <p>
              <strong>Path</strong>
              <span>{selectedCodebase?.project_path ?? "n/a"}</span>
            </p>
            <p>
              <strong>Graph Nodes</strong>
              <span>{formatCount(selectedGraph?.nodes.length ?? 0)}</span>
            </p>
            <p>
              <strong>Graph Edges</strong>
              <span>{formatCount(selectedGraph?.links.length ?? 0)}</span>
            </p>
            <p>
              <strong>Indexed Files</strong>
              <span>{formatCount(selectedFileCount)}</span>
            </p>
            <p>
              <strong>Import Edges</strong>
              <span>
                {formatCount(selectedOverviewMetrics?.import_edge_count ?? 0)}
              </span>
            </p>
            <p>
              <strong>External Refs</strong>
              <span>
                {formatCount(selectedOverviewMetrics?.external_ref_count ?? 0)}
              </span>
            </p>
          </div>
        </article>
      </section>

      <section className="panel-grid">
        <article className="panel">
          <header className="panel-header">
            <h3>Cache + Dependency Telemetry</h3>
          </header>
          <div className="mini-metrics">
            <div>
              <p>Cache Entries</p>
              <h4>{formatCount(overview?.cache.analysis_cache_entries ?? 0)}</h4>
            </div>
            <div>
              <p>Cache Hit Rate</p>
              <h4>{formatPercent(overview?.cache.estimated_hit_rate)}</h4>
            </div>
            <div>
              <p>Cache Temperature</p>
              <h4 className={`temp-${overview?.cache.temperature ?? "cold"}`}>
                {overview?.cache.temperature ?? "cold"}
              </h4>
            </div>
            <div>
              <p>External Refs</p>
              <h4>
                {formatCount(overview?.external_dependencies.external_refs ?? 0)}
              </h4>
            </div>
            <div>
              <p>Project Links</p>
              <h4>
                {formatCount(
                  overview?.external_dependencies.project_dependency_links ?? 0,
                )}
              </h4>
            </div>
            <div>
              <p>Import Edges</p>
              <h4>
                {formatCount(overview?.external_dependencies.import_edges ?? 0)}
              </h4>
            </div>
          </div>
          <div className="feature-flags">
            {Object.entries(overview?.feature_status ?? {}).map(([name, enabled]) => (
              <span key={name} className={enabled ? "flag-on" : "flag-off"}>
                {name.split("_").join(" ")}
              </span>
            ))}
          </div>
        </article>

        <article className="panel">
          <header className="panel-header">
            <h3>Language Distribution</h3>
          </header>
          <div className="bars">
            {(overview?.language_distribution ?? []).map((lang) => (
              <div key={lang.language} className="bar-row">
                <span>{lang.language}</span>
                <div className="bar-track">
                  <div
                    className="bar-fill"
                    style={{
                      width: `${Math.max(
                        (lang.count / languagePeak) * 100,
                        5,
                      ).toFixed(2)}%`,
                    }}
                  />
                </div>
                <strong>{formatCount(lang.count)}</strong>
              </div>
            ))}
          </div>
        </article>
      </section>

      <section className="panel-grid">
        <article className="panel">
          <header className="panel-header">
            <h3>Semantic Search</h3>
          </header>
          <form className="search-form" onSubmit={(event) => void runSearch(event)}>
            <input
              value={searchQuery}
              onChange={(event) => setSearchQuery(event.target.value)}
              placeholder="Try: authentication, cache, context, index"
            />
            <button className="button" type="submit" disabled={searching}>
              {searching ? "Searching..." : "Search"}
            </button>
          </form>
          <div className="search-results">
            {searchResults.map((result) => (
              <article key={`${result.node_id}-${result.rank}`} className="search-result">
                <header>
                  <strong>{result.symbol_name}</strong>
                  <span>{result.language}</span>
                </header>
                <p>{result.file_path}</p>
                <div className="score-row">
                  <span>overall {result.score.overall.toFixed(3)}</span>
                  <span>semantic {result.score.semantic.toFixed(3)}</span>
                  <span>text {result.score.text_match.toFixed(3)}</span>
                  <span>structural {result.score.structural.toFixed(3)}</span>
                </div>
              </article>
            ))}
            {searchResults.length === 0 && (
              <p className="empty-state">Run a query to inspect ranked graph symbols.</p>
            )}
          </div>
        </article>

        <article className="panel">
          <header className="panel-header">
            <h3>Live Events</h3>
          </header>
          <div className="events-feed">
            {events.map((evt, index) => (
              <div className="event-row" key={`${evt.type}-${evt.timestamp ?? index}`}>
                <span>{evt.type}</span>
                <span>
                  {evt.timestamp
                    ? new Date(evt.timestamp).toLocaleTimeString()
                    : "now"}
                </span>
              </div>
            ))}
            {events.length === 0 && (
              <p className="empty-state">
                Waiting for websocket events from indexing or project changes.
              </p>
            )}
          </div>
        </article>
      </section>

      {loadState === "loading" && (
        <div className="loading-overlay">Loading dashboard snapshot...</div>
      )}
    </main>
  );
}
