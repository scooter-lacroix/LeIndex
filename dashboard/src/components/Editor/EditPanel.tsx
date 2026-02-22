import { useState } from 'react';
import { CodeEditor } from './CodeEditor';
import { DiffViewer } from './DiffViewer';
import { Button } from '../UI/button';
import { Alert, AlertTitle, AlertDescription } from '../UI/alert';
import { Check, X, FileEdit } from 'lucide-react';
import { cn } from '../../lib/utils';

type EditMode = 'edit' | 'preview';

interface EditPanelProps {
  filePath: string;
  originalContent: string;
  language: string;
  onSave?: (content: string) => void;
  onCancel?: () => void;
}

export function EditPanel({
  filePath,
  originalContent,
  language,
  onSave,
  onCancel,
}: EditPanelProps) {
  const [mode, setMode] = useState<EditMode>('edit');
  const [content, setContent] = useState(originalContent);
  const [hasChanges, setHasChanges] = useState(false);

  const handleChange = (newContent: string) => {
    setContent(newContent);
    setHasChanges(newContent !== originalContent);
  };

  const handleSave = () => {
    onSave?.(content);
  };

  return (
    <div className="h-full flex flex-col">
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-2">
          <FileEdit className="h-5 w-5" />
          <h3 className="font-semibold">Editing: {filePath}</h3>
        </div>
        <div className="flex items-center gap-2">
          <div className="flex rounded-lg border p-1">
            <button
              onClick={() => setMode('edit')}
              className={cn(
                'px-3 py-1 rounded text-sm transition-colors',
                mode === 'edit' && 'bg-primary text-primary-foreground'
              )}
            >
              Edit
            </button>
            <button
              onClick={() => setMode('preview')}
              className={cn(
                'px-3 py-1 rounded text-sm transition-colors',
                mode === 'preview' && 'bg-primary text-primary-foreground'
              )}
            >
              Preview Diff
            </button>
          </div>
          <Button variant="outline" size="sm" onClick={onCancel}>
            <X className="mr-2 h-4 w-4" />
            Cancel
          </Button>
          <Button
            size="sm"
            onClick={handleSave}
            disabled={!hasChanges}
          >
            <Check className="mr-2 h-4 w-4" />
            Stage Changes
          </Button>
        </div>
      </div>

      {hasChanges && mode === 'edit' && (
        <Alert className="mb-4">
          <AlertTitle>Unsaved Changes</AlertTitle>
          <AlertDescription>
            You have made changes to this file. Switch to &quot;Preview Diff&quot; to review before saving.
          </AlertDescription>
        </Alert>
      )}

      <div className="flex-1 min-h-0">
        {mode === 'edit' ? (
          <CodeEditor
            content={content}
            language={language}
            path={filePath}
            onChange={handleChange}
          />
        ) : (
          <DiffViewer
            original={originalContent}
            modified={content}
            originalLabel="Original"
            modifiedLabel="Your Changes"
          />
        )}
      </div>
    </div>
  );
}
