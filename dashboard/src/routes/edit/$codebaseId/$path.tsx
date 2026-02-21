import { createFileRoute, useParams } from '@tanstack/react-router';
import { useState, useEffect } from 'react';
import { useUIStore } from '../../../stores/uiStore';
import { CodeEditor } from '../../../components/Editor/CodeEditor';
import { EditPanel } from '../../../components/Editor/EditPanel';
import { useEdit } from '../../../hooks/useEdit';
import { api } from '../../../lib/api';
import type { FileNode } from '../../../types/editor';
import { Loader2 } from 'lucide-react';

export const Route = createFileRoute('/edit/$codebaseId/$path')({
  component: EditPage,
});

function EditPage() {
  const { codebaseId, path } = useParams({ from: '/edit/$codebaseId/$path' });
  const { setActivePanel } = useUIStore();
  const [fileNode, setFileNode] = useState<FileNode | null>(null);
  const [content, setContent] = useState<string>('');
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  
   const {
     edits,
     stagedEdits,
     applyEdits,
     stageChanges,
     isApplying,
   } = useEdit(codebaseId, path);

  useEffect(() => {
    setActivePanel('editor');
  }, [setActivePanel]);

   useEffect(() => {
     if (!codebaseId || !path) return;

     const loadFile = async () => {
       setIsLoading(true);
       setError(null);
       try {
         // Get file content using the files API
         const fileContent = await api.files.getContent(codebaseId, decodeURIComponent(path));
         setFileNode({
           id: fileContent.path,
           name: fileContent.path.split('/').pop() || fileContent.path,
           type: 'file',
           path: fileContent.path,
           language: fileContent.language,
         } as FileNode);
         setContent(fileContent.content);
       } catch (err) {
         setError(err instanceof Error ? err.message : 'Failed to load file');
       } finally {
         setIsLoading(false);
       }
     };

     loadFile();
   }, [codebaseId, path]);

  const handleStageChanges = async () => {
    await stageChanges();
  };

  const handleApplyChanges = async () => {
    await applyEdits();
  };

  const handleCancel = () => {
    // Reset content to original or clear edits
  };

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex h-full items-center justify-center text-destructive">
        {error}
      </div>
    );
  }

  if (!fileNode) {
    return (
      <div className="flex h-full items-center justify-center text-muted-foreground">
        File not found
      </div>
    );
  }

  if (fileNode.type === 'directory') {
    return (
      <div className="h-full flex flex-col">
        <div className="p-4 border-b">
          <h1 className="text-2xl font-bold">{fileNode.name}</h1>
          <p className="text-sm text-muted-foreground">{fileNode.path}</p>
        </div>
        <div className="flex-1 p-4">
          <p className="text-muted-foreground">Directory view not implemented</p>
        </div>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col">
      <div className="p-4 border-b flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold">{fileNode.name}</h1>
          <p className="text-sm text-muted-foreground">{fileNode.path}</p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleStageChanges}
            disabled={edits.length === 0 || isApplying}
            className="px-4 py-2 bg-primary text-primary-foreground rounded hover:bg-primary/90 disabled:opacity-50"
          >
            {isApplying ? 'Staging...' : 'Stage Changes'}
          </button>
          <button
            onClick={handleApplyChanges}
            disabled={stagedEdits.length === 0 || isApplying}
            className="px-4 py-2 bg-green-600 text-white rounded hover:bg-green-700 disabled:opacity-50"
          >
            Apply All
          </button>
        </div>
      </div>
      
      <div className="flex-1 flex overflow-hidden">
        <div className="flex-1 overflow-auto">
          <CodeEditor
            content={content}
            language={fileNode.language || ''}
            path={fileNode.path}
            onChange={setContent}
          />
        </div>
        
        <div className="w-96 border-l overflow-auto">
          <EditPanel
            filePath={fileNode.path}
            originalContent={content}
            language={fileNode.language || ''}
            onCancel={handleCancel}
          />
          {stagedEdits.length > 0 && (
            <div className="p-4 border-t">
              <h3 className="font-semibold mb-2">Staged Changes</h3>
              <div className="space-y-2">
                {stagedEdits.map((edit) => (
                  <div key={edit.id} className="text-sm p-2 bg-muted rounded">
                    <p className="font-mono text-xs">{edit.filePath}</p>
                    <p>{edit.changes.length} change(s)</p>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
