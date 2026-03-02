import { createFileRoute } from '@tanstack/react-router';
import { useUIStore } from '../stores/uiStore';
import { GraphView, GraphControls, GraphLegend, GraphExport } from '../components/Graph';
import { useWebSocket } from '../hooks/useWebSocket';
import { useEffect, useRef } from 'react';
import { useGraph } from '../hooks/useGraph';
import { useGraphStore } from '../stores/graphStore';
import type { GraphViewHandle } from '../types/graph';

interface GraphSearch {
  codebaseId?: string;
}

export const Route = createFileRoute('/graph')({
  validateSearch: (search: Record<string, unknown>): GraphSearch => ({
    codebaseId: search.codebaseId as string | undefined,
  }),
  component: GraphPage,
});

function GraphPage() {
  const search = Route.useSearch();
  const codebaseId = search.codebaseId || '';
  const { setActivePanel } = useUIStore();
  const graphRef = useRef<GraphViewHandle>(null);
  const { isLoading, error } = useGraph(codebaseId);
  const { graphData } = useGraphStore();
  
  useWebSocket(codebaseId || null);

  useEffect(() => {
    setActivePanel('graph');
  }, [setActivePanel]);

  if (!codebaseId) {
    return (
      <div className="flex h-full items-center justify-center text-muted-foreground">
        <div className="text-center">
          <p className="text-lg font-medium">No Codebase Selected</p>
          <p className="text-sm mt-2">Select a codebase from the Codebases page to view its graph</p>
        </div>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-center">
          <p className="text-lg font-medium">Loading Graph</p>
          <p className="text-sm mt-2 text-muted-foreground">Fetching codebase data...</p>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex h-full items-center justify-center text-destructive">
        <div className="text-center">
          <p className="text-lg font-medium">Error Loading Graph</p>
          <p className="text-sm mt-2">{error.message}</p>
        </div>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col">
      <div className="flex items-center justify-between mb-4">
        <h2 className="text-2xl font-bold">Codebase Graph</h2>
        {graphData && graphData.nodes.length > 0 && (
          <GraphExport graphData={graphData} graphRef={graphRef} />
        )}
      </div>
      <div className="relative flex-1 rounded-lg border border-border bg-card overflow-hidden">
        <GraphView ref={graphRef} codebaseId={codebaseId} height="100%" />
        <GraphControls graphRef={graphRef} />
        <GraphLegend />
      </div>
    </div>
  );
}
