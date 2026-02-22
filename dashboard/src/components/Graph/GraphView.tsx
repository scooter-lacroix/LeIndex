import React, { useEffect, useRef, forwardRef, useImperativeHandle } from 'react';
import ForceGraph2D from 'react-force-graph-2d';
import { useGraph } from '../../hooks/useGraph';
import { useGraphStore } from '../../stores/graphStore';
import { useWebSocket } from '../../hooks/useWebSocket';
import { LoadingSpinner } from '../LoadingSpinner';
import type { GraphNode, GraphLink, GraphViewHandle } from '../../types/graph';

interface GraphViewProps {
  codebaseId: string;
  height?: number | string;
  onNodeHover?: (node: GraphNode | null) => void;
}

const NODE_COLORS: Record<string, string> = {
  function: '#3b82f6',    // blue-500
  class: '#8b5cf6',       // violet-500
  method: '#06b6d4',      // cyan-500
  variable: '#10b981',    // emerald-500
  module: '#6b7280',      // gray-500
};

export const GraphView = forwardRef<GraphViewHandle, GraphViewProps>(({ codebaseId, height = '100%', onNodeHover }, ref) => {
  const fgRef = useRef<any>(null);
  const { data, isLoading, error } = useGraph(codebaseId);
  const { 
    graphData, 
    setGraphData, 
    selectedNodes, 
    selectNode, 
    deselectNode,
  } = useGraphStore();

  useWebSocket(codebaseId);

  // Expose methods via ref
  useImperativeHandle(ref, () => ({
    zoom: (factor: number, duration?: number) => {
      if (fgRef.current) {
        fgRef.current.zoom(factor, duration);
      }
    },
    fitView: (duration?: number, padding?: number) => {
      if (fgRef.current) {
        fgRef.current.zoomToFit(duration, padding);
      }
    },
    centerAt: (x: number, y: number, duration?: number) => {
      if (fgRef.current) {
        fgRef.current.centerAt(x, y, duration);
      }
    },
    getGraphRef: () => fgRef.current || null,
  }));

  useEffect(() => {
    if (data) {
      setGraphData(data);
    }
  }, [data, setGraphData]);

  const handleNodeClick = (node: GraphNode) => {
    if (selectedNodes.has(node.id)) {
      deselectNode(node.id);
    } else {
      selectNode(node.id);
    }
  };

  const handleNodeHover = (node: GraphNode | null) => {
    if (onNodeHover) {
      onNodeHover(node);
    }
  };

  const handleBackgroundClick = () => {
    // Clear selection on background click
  };

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center">
        <LoadingSpinner size="lg" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex h-full items-center justify-center text-destructive">
        Error loading graph: {error.message}
      </div>
    );
  }

  if (!graphData || graphData.nodes.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-muted-foreground">
        No graph data available
      </div>
    );
  }

  return (
    <div style={{ height }} className="relative">
       <ForceGraph2D
         ref={fgRef}
         graphData={graphData}
         nodeId="id"
         nodeLabel="name"
         nodeColor={(node: GraphNode) => NODE_COLORS[node.type] || '#6b7280'}
         nodeVal={(node: GraphNode) => node.val}
         linkColor={() => '#374151'}
         linkWidth={(link: GraphLink) => link.value}
         onNodeClick={handleNodeClick}
         onNodeHover={handleNodeHover}
         onBackgroundClick={handleBackgroundClick}
         backgroundColor="transparent"
         width={undefined}
         height={typeof height === 'number' ? height : undefined}
         nodeCanvasObject={(node: GraphNode, ctx, globalScale) => {
          const label = node.name;
          const fontSize = 12 / globalScale;
          ctx.font = `${fontSize}px Sans-Serif`;
          const textWidth = ctx.measureText(label).width;
          const bckgDimensions = [textWidth, fontSize].map((n) => n + fontSize * 0.2);

          ctx.fillStyle = selectedNodes.has(node.id) ? '#fbbf24' : NODE_COLORS[node.type] || '#6b7280';
          ctx.beginPath();
          ctx.arc(node.x || 0, node.y || 0, node.val * 2, 0, 2 * Math.PI);
          ctx.fill();

          ctx.fillStyle = 'rgba(255, 255, 255, 0.8)';
          ctx.fillRect(
            (node.x || 0) - bckgDimensions[0] / 2,
            (node.y || 0) + node.val * 2 + 2,
            bckgDimensions[0],
            bckgDimensions[1]
          );

          ctx.textAlign = 'center';
          ctx.textBaseline = 'middle';
          ctx.fillStyle = '#1f2937';
          ctx.fillText(label, node.x || 0, (node.y || 0) + node.val * 2 + fontSize / 2 + 2);
        }}
      />
    </div>
  );
});

GraphView.displayName = 'GraphView';
