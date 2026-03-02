import { useMemo } from 'react';
import CodeMirror from '@uiw/react-codemirror';
import { javascript } from '@codemirror/lang-javascript';
import { rust } from '@codemirror/lang-rust';
import { python } from '@codemirror/lang-python';
import { oneDark } from '@codemirror/theme-one-dark';
import type { Extension } from '@codemirror/state';
import type { FileContent } from '../../types/editor';

interface FilePreviewProps {
  file: FileContent | null;
}

const languageExtensions: Record<string, Extension> = {
  js: javascript(),
  jsx: javascript({ jsx: true }),
  ts: javascript({ typescript: true }),
  tsx: javascript({ jsx: true, typescript: true }),
  rs: rust(),
  py: python(),
};

export function FilePreview({ file }: FilePreviewProps) {
  const extensions = useMemo(() => {
    if (!file) return [];
    const ext = file.language || file.path.split('.').pop() || '';
    return [languageExtensions[ext] || javascript()].filter(Boolean);
  }, [file]);

  if (!file) {
    return (
      <div className="h-full flex items-center justify-center text-muted-foreground border rounded-lg bg-card">
        <p>Select a file to preview</p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col border rounded-lg bg-card overflow-hidden">
      <div className="flex items-center justify-between px-4 py-2 border-b bg-muted/50">
        <span className="font-mono text-sm truncate">{file.path}</span>
        <span className="text-xs text-muted-foreground">{file.language}</span>
      </div>
      <div className="flex-1 overflow-auto">
        <CodeMirror
          value={file.content}
          height="100%"
          extensions={extensions}
          theme={oneDark}
          editable={false}
          basicSetup={{
            lineNumbers: true,
            highlightActiveLineGutter: false,
            highlightActiveLine: false,
          }}
        />
      </div>
    </div>
  );
}
