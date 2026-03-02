const items = [
  { type: 'function', color: '#3b82f6', label: 'Function' },
  { type: 'class', color: '#8b5cf6', label: 'Class' },
  { type: 'method', color: '#06b6d4', label: 'Method' },
  { type: 'variable', color: '#10b981', label: 'Variable' },
  { type: 'module', color: '#6b7280', label: 'Module' },
];

export function GraphLegend() {
  return (
    <div className="absolute top-4 right-4 rounded-lg border border-border bg-card/90 p-3 shadow-md">
      <h4 className="text-xs font-semibold mb-2 uppercase tracking-wider text-muted-foreground">Node Types</h4>
      <div className="space-y-1">
        {items.map((item) => (
          <div key={item.type} className="flex items-center gap-2">
            <div
              className="h-3 w-3 rounded-full"
              style={{ backgroundColor: item.color }}
            />
            <span className="text-xs">{item.label}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
