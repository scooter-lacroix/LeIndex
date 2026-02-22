import { createFileRoute } from '@tanstack/react-router';
import { useUIStore } from '../stores/uiStore';
import { useEffect, useState } from 'react';
import { Search, Filter, X } from 'lucide-react';
import { useSearch } from '../hooks/useCodebases';
import { Button } from '../components/UI/button';
import { LoadingSpinner } from '../components/LoadingSpinner';

export const Route = createFileRoute('/search')({
  component: SearchPage,
});

function SearchPage() {
  const { setActivePanel } = useUIStore();
  const [query, setQuery] = useState('');
  const [filters, setFilters] = useState({
    language: '',
    minScore: 0,
    fileType: '',
  });
  const { data, isLoading } = useSearch(query);

  useEffect(() => {
    setActivePanel('search');
  }, [setActivePanel]);

  const clearFilters = () => {
    setFilters({ language: '', minScore: 0, fileType: '' });
  };

  const hasFilters = filters.language || filters.minScore > 0 || filters.fileType;

  return (
    <div className="h-full flex flex-col max-w-5xl mx-auto">
      <h2 className="text-2xl font-bold mb-4">Search Code</h2>
      
      <div className="flex gap-4 mb-6">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-5 w-5 text-muted-foreground" />
          <input
            type="text"
            placeholder="Search code... (Ctrl+K)"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="w-full pl-10 pr-4 py-3 rounded-lg border border-border bg-card text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-primary"
            autoFocus
          />
        </div>
      </div>

      <div className="flex items-center gap-4 mb-4">
        <div className="flex items-center gap-2">
          <Filter className="h-4 w-4 text-muted-foreground" />
          <span className="text-sm text-muted-foreground">Filters:</span>
        </div>
        <select
          value={filters.language}
          onChange={(e) => setFilters(f => ({ ...f, language: e.target.value }))}
          className="px-3 py-1 rounded border border-border bg-card text-sm"
        >
          <option value="">All Languages</option>
          <option value="typescript">TypeScript</option>
          <option value="javascript">JavaScript</option>
          <option value="rust">Rust</option>
          <option value="python">Python</option>
        </select>
        <select
          value={filters.fileType}
          onChange={(e) => setFilters(f => ({ ...f, fileType: e.target.value }))}
          className="px-3 py-1 rounded border border-border bg-card text-sm"
        >
          <option value="">All Files</option>
          <option value=".ts">.ts</option>
          <option value=".tsx">.tsx</option>
          <option value=".js">.js</option>
          <option value=".rs">.rs</option>
          <option value=".py">.py</option>
        </select>
        {hasFilters && (
          <Button variant="ghost" size="sm" onClick={clearFilters}>
            <X className="h-4 w-4 mr-1" />
            Clear
          </Button>
        )}
      </div>

      {isLoading && (
        <div className="flex justify-center py-8">
          <LoadingSpinner size="lg" />
        </div>
      )}

      {data && data.results.length > 0 && (
        <div className="space-y-2 overflow-auto">
          <p className="text-sm text-muted-foreground mb-4">
            {data.results.length} result{data.results.length !== 1 ? 's' : ''} found
          </p>
          {data.results.map((result) => (
            <div
              key={result.nodeId}
              className="p-4 rounded-lg border border-border bg-card hover:bg-accent cursor-pointer transition-colors"
            >
              <div className="flex items-center justify-between">
                <h3 className="font-semibold">{result.symbolName}</h3>
                <div className="flex items-center gap-2">
                  <span className="text-xs px-2 py-1 rounded bg-muted">{result.language}</span>
                  <span className="text-xs text-muted-foreground">
                    Score: {result.score.overall.toFixed(2)}
                  </span>
                </div>
              </div>
              <p className="text-sm text-muted-foreground mt-1">
                {result.filePath}:{result.byteRange[0]}
              </p>
              {result.context && (
                <pre className="mt-2 text-xs bg-muted p-2 rounded overflow-x-auto">
                  {result.context}
                </pre>
              )}
            </div>
          ))}
        </div>
      )}

      {query && !isLoading && data?.results.length === 0 && (
        <div className="text-center text-muted-foreground py-8">
          <p>No results found for &quot;{query}&quot;</p>
          <p className="text-sm mt-2">Try adjusting your search or filters</p>
        </div>
      )}
    </div>
  );
}
