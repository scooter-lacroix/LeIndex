import { CodebaseCard } from './CodebaseCard';
import { useCodebases, useRefreshCodebases } from '../../hooks/useCodebases';
import { useUIStore } from '../../stores/uiStore';
import { LoadingSpinner } from '../LoadingSpinner';
import { Alert, AlertTitle, AlertDescription } from '../UI/alert';
import { AlertCircle, RefreshCw } from 'lucide-react';
import { Button } from '../UI/button';

export function CodebaseList() {
  const { data, isLoading, error, refetch } = useCodebases();
  const { selectedCodebaseId, setSelectedCodebaseId } = useUIStore();
  const refreshMutation = useRefreshCodebases();

  if (isLoading) {
    return (
      <div className="flex h-64 items-center justify-center">
        <LoadingSpinner size="lg" />
      </div>
    );
  }

  if (error) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Error loading codebases</AlertTitle>
        <AlertDescription className="flex items-center gap-4">
          {error.message}
          <Button variant="outline" size="sm" onClick={() => refetch()}>
            Retry
          </Button>
        </AlertDescription>
      </Alert>
    );
  }

  const validCodebases = data?.codebases.filter(c => c.isValid) || [];
  const invalidCodebases = data?.codebases.filter(c => !c.isValid) || [];
  const clones = validCodebases.filter(c => c.isClone);

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-2xl font-bold">Indexed Codebases</h2>
          <p className="text-muted-foreground">
            {validCodebases.length} active, {invalidCodebases.length} invalid, {clones.length} clones
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => refreshMutation.mutate()}
            disabled={refreshMutation.isPending}
          >
            <RefreshCw className={`mr-2 h-4 w-4 ${refreshMutation.isPending ? 'animate-spin' : ''}`} />
            Refresh
          </Button>
        </div>
      </div>

      {clones.length > 0 && (
        <div className="rounded-lg border border-amber-500/20 bg-amber-500/10 p-4">
          <p className="text-sm text-amber-700 dark:text-amber-300">
            <strong>Clone Detection:</strong> {clones.length} project{clones.length !== 1 ? 's' : ''} detected as clones. 
            These share significant content with other indexed projects.
          </p>
        </div>
      )}

      <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
        {validCodebases.map((codebase) => (
          <CodebaseCard
            key={codebase.id}
            codebase={codebase}
            isSelected={selectedCodebaseId === codebase.id}
          />
        ))}
      </div>

      {invalidCodebases.length > 0 && (
        <div className="space-y-4">
          <h3 className="text-lg font-semibold text-destructive">Invalid Codebases</h3>
          <p className="text-sm text-muted-foreground">
            These codebases could not be validated. They may have been moved or deleted.
          </p>
          <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
            {invalidCodebases.map((codebase) => (
              <CodebaseCard
                key={codebase.id}
                codebase={codebase}
                isSelected={selectedCodebaseId === codebase.id}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
