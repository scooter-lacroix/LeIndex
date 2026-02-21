import { Database, FolderGit2, GitBranch, Clock, AlertCircle, GitGraph, FileEdit } from 'lucide-react';
import type { Codebase } from '../../types';
import { cn } from '../../lib/utils';
import { Link } from '@tanstack/react-router';

interface CodebaseCardProps {
  codebase: Codebase;
  isSelected?: boolean;
}

export function CodebaseCard({ codebase, isSelected }: CodebaseCardProps) {
  return (
    <Link
      to="/graph"
      search={{ codebaseId: codebase.id }}
      className={cn(
        'rounded-lg border bg-card p-6 transition-all block no-underline',
        isSelected 
          ? 'border-primary ring-2 ring-primary ring-offset-2' 
          : 'border-border hover:border-primary/50 hover:shadow-md',
        !codebase.isValid && 'opacity-60'
      )}
    >
      <div className="flex items-start justify-between">
        <div className="flex items-center gap-3">
          <div className="rounded-lg bg-primary/10 p-2">
            <Database className="h-5 w-5 text-primary" />
          </div>
          <div className="min-w-0">
            <h3 className="font-semibold truncate">{codebase.displayName}</h3>
            <p className="text-xs text-muted-foreground font-mono">{codebase.uniqueProjectId}</p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {codebase.isClone && (
            <div 
              className="flex items-center gap-1 text-xs text-amber-500 bg-amber-500/10 px-2 py-1 rounded-full"
              title={`Clone of ${codebase.clonedFrom || 'another project'}`}
            >
              <GitBranch className="h-3 w-3" />
              <span>Clone</span>
            </div>
          )}
          {!codebase.isValid && (
            <div className="flex items-center gap-1 text-xs text-destructive bg-destructive/10 px-2 py-1 rounded-full">
              <AlertCircle className="h-3 w-3" />
              <span>Invalid</span>
            </div>
          )}
        </div>
      </div>

      <div className="mt-4 grid grid-cols-3 gap-4 text-sm">
        <div className="text-center p-2 rounded bg-muted/50">
          <p className="text-muted-foreground text-xs">Files</p>
          <p className="font-semibold text-lg">{codebase.fileCount.toLocaleString()}</p>
        </div>
        <div className="text-center p-2 rounded bg-muted/50">
          <p className="text-muted-foreground text-xs">Nodes</p>
          <p className="font-semibold text-lg">{codebase.nodeCount.toLocaleString()}</p>
        </div>
        <div className="text-center p-2 rounded bg-muted/50">
          <p className="text-muted-foreground text-xs">Edges</p>
          <p className="font-semibold text-lg">{codebase.edgeCount.toLocaleString()}</p>
        </div>
      </div>

      <div className="mt-4 flex items-center gap-4 text-xs text-muted-foreground">
        <div className="flex items-center gap-1 truncate flex-1">
          <FolderGit2 className="h-3 w-3 flex-shrink-0" />
          <span className="truncate">{codebase.projectPath}</span>
        </div>
        <div className="flex items-center gap-1 flex-shrink-0">
          <Clock className="h-3 w-3" />
          <span>{new Date(codebase.lastIndexed).toLocaleDateString()}</span>
        </div>
      </div>

      <div className="mt-4 pt-4 border-t flex gap-2">
        <div
          className="flex-1 flex items-center justify-center gap-2 px-3 py-2 text-sm bg-secondary text-secondary-foreground rounded"
        >
          <GitGraph className="h-4 w-4" />
          View Graph
        </div>
        <Link
          to="/edit/$codebaseId/$path"
          params={{ codebaseId: codebase.id, path: encodeURIComponent('') }}
          onClick={(e) => e.stopPropagation()}
          className="flex-1 flex items-center justify-center gap-2 px-3 py-2 text-sm bg-secondary text-secondary-foreground rounded hover:bg-secondary/80"
        >
          <FileEdit className="h-4 w-4" />
          Editor
        </Link>
      </div>
    </Link>
  );
}
