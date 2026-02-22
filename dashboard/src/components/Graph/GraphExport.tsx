import React, { useCallback } from 'react';
import { Download, Image, FileJson, FileCode } from 'lucide-react';
import { Button } from '../UI/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '../UI/dropdown-menu';
import type { GraphData } from '../../types/graph';
import type { GraphViewHandle } from '../../types/graph';

interface GraphExportProps {
  graphData: GraphData;
  graphRef: React.RefObject<GraphViewHandle>;
}

export function GraphExport({ graphData, graphRef }: GraphExportProps) {
  const exportToPNG = useCallback(() => {
    const forceGraph = graphRef.current?.getGraphRef();
    const canvas = forceGraph?.canvas;
    if (canvas) {
      const link = document.createElement('a');
      link.download = 'leindex-graph.png';
      link.href = canvas.toDataURL('image/png');
      link.click();
    }
  }, [graphRef]);

  const exportToJSON = useCallback(() => {
    const dataStr = JSON.stringify(graphData, null, 2);
    const blob = new Blob([dataStr], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.download = 'leindex-graph.json';
    link.href = url;
    link.click();
    URL.revokeObjectURL(url);
  }, [graphData]);

  const exportToGraphML = useCallback(() => {
    // Convert to GraphML format
    const nodes = graphData.nodes.map(n => 
      `<node id="${n.id}"><data key="label">${n.name}</data></node>`
    ).join('\n    ');
    
    const edges = graphData.links.map((l, i) => 
      `<edge id="e${i}" source="${l.source}" target="${l.target}"/>`
    ).join('\n    ');

    const graphML = `<?xml version="1.0" encoding="UTF-8"?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
  <graph id="G" edgedefault="directed">
    ${nodes}
    ${edges}
  </graph>
</graphml>`;

    const blob = new Blob([graphML], { type: 'application/xml' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.download = 'leindex-graph.graphml';
    link.href = url;
    link.click();
    URL.revokeObjectURL(url);
  }, [graphData]);

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" size="sm">
          <Download className="mr-2 h-4 w-4" />
          Export
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        <DropdownMenuItem onClick={exportToPNG}>
          <Image className="mr-2 h-4 w-4" />
          Export as PNG
        </DropdownMenuItem>
        <DropdownMenuItem onClick={exportToJSON}>
          <FileJson className="mr-2 h-4 w-4" />
          Export as JSON
        </DropdownMenuItem>
        <DropdownMenuItem onClick={exportToGraphML}>
          <FileCode className="mr-2 h-4 w-4" />
          Export as GraphML
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
