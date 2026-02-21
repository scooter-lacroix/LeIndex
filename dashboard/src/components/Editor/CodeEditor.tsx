import { useCallback, useState } from 'react';
import CodeMirror from '@uiw/react-codemirror';
import { javascript } from '@codemirror/lang-javascript';
import { rust } from '@codemirror/lang-rust';
import { python } from '@codemirror/lang-python';
import { oneDark } from '@codemirror/theme-one-dark';
import type { Extension } from '@codemirror/state';

interface CodeEditorProps {
  content: string;
  language: string;
  path: string;
  onChange?: (value: string) => void;
  readOnly?: boolean;
}

const languageExtensions: Record<string, Extension> = {
  js: javascript(),
  jsx: javascript({ jsx: true }),
  ts: javascript({ typescript: true }),
  tsx: javascript({ jsx: true, typescript: true }),
  rs: rust(),
  py: python(),
};

export function CodeEditor({
  content,
  language,
  path,
  onChange,
  readOnly = false,
}: CodeEditorProps) {
  const [value, setValue] = useState(content);

  const handleChange = useCallback(
    (newValue: string) => {
      setValue(newValue);
      onChange?.(newValue);
    },
    [onChange]
  );

  const extensions = [languageExtensions[language] || javascript()].filter(Boolean);

  return (
    <div className="h-full flex flex-col border rounded-lg bg-card overflow-hidden">
      <div className="flex items-center justify-between px-4 py-2 border-b bg-muted/50">
        <span className="font-mono text-sm">{path}</span>
        <span className="text-xs text-muted-foreground">{language}</span>
      </div>
      <div className="flex-1 overflow-auto">
        <CodeMirror
          value={value}
          height="100%"
          extensions={extensions}
          theme={oneDark}
          onChange={handleChange}
          editable={!readOnly}
          basicSetup={{
            lineNumbers: true,
            highlightActiveLineGutter: true,
            highlightActiveLine: true,
            foldGutter: true,
          }}
        />
      </div>
    </div>
  );
}
