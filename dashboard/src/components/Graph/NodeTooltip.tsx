import type { GraphNode } from '../../types/graph';

interface NodeTooltipProps {
  node: GraphNode;
}

export function NodeTooltip({ node }: NodeTooltipProps) {
  return (
    <div className="rounded-lg border border-border bg-popover p-3 shadow-md">
      <h4 className="font-semibold">{node.name}</h4>
      <div className="mt-2 space-y-1 text-sm text-muted-foreground">
        <p>Type: {node.type}</p>
        <p>Language: {node.language}</p>
        <p>Complexity: {node.complexity}</p>
        <p className="truncate max-w-[200px]">{node.filePath}</p>
      </div>
    </div>
  );
}
