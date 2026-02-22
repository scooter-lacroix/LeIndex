import { useMemo } from 'react';
import * as diff from 'diff';

interface DiffViewerProps {
  original: string;
  modified: string;
  originalLabel?: string;
  modifiedLabel?: string;
}

export function DiffViewer({
  original,
  modified,
  originalLabel = 'Original',
  modifiedLabel = 'Modified',
}: DiffViewerProps) {
  const diffResult = useMemo(() => {
    return diff.diffLines(original, modified);
  }, [original, modified]);

  return (
    <div className="h-full flex flex-col border rounded-lg bg-card overflow-hidden">
      <div className="flex border-b">
        <div className="flex-1 px-4 py-2 border-r bg-muted/50">
          <span className="font-semibold text-sm">{originalLabel}</span>
        </div>
        <div className="flex-1 px-4 py-2 bg-muted/50">
          <span className="font-semibold text-sm">{modifiedLabel}</span>
        </div>
      </div>
      <div className="flex-1 overflow-auto">
        <div className="flex">
          <div className="flex-1 border-r">
            {diffResult.map((part, i) => (
              <div
                key={`orig-${i}`}
                className={`px-4 py-1 font-mono text-sm whitespace-pre ${
                  part.removed
                    ? 'bg-red-500/20 text-red-700 dark:text-red-300'
                    : part.added
                    ? 'bg-gray-100 dark:bg-gray-800'
                    : ''
                }`}
              >
                {part.removed ? part.value : part.added ? '' : part.value}
              </div>
            ))}
          </div>
          <div className="flex-1">
            {diffResult.map((part, i) => (
              <div
                key={`mod-${i}`}
                className={`px-4 py-1 font-mono text-sm whitespace-pre ${
                  part.added
                    ? 'bg-green-500/20 text-green-700 dark:text-green-300'
                    : part.removed
                    ? 'bg-gray-100 dark:bg-gray-800'
                    : ''
                }`}
              >
                {part.added ? part.value : part.removed ? '' : part.value}
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
