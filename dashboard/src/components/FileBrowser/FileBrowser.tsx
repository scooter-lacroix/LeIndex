import { useState } from 'react';
import { FileTree } from './FileTree';
import { FilePreview } from './FilePreview';
import type { FileNode, FileContent } from '../../types/editor';

interface FileBrowserProps {
  codebaseId: string;
}

// Mock data - replace with actual API call
const mockFileTree: FileNode[] = [
  {
    id: 'src',
    name: 'src',
    type: 'directory',
    path: 'src',
    children: [
      {
        id: 'src/components',
        name: 'components',
        type: 'directory',
        path: 'src/components',
        children: [
          { id: 'src/components/Button.tsx', name: 'Button.tsx', type: 'file', path: 'src/components/Button.tsx', language: 'tsx' },
          { id: 'src/components/Input.tsx', name: 'Input.tsx', type: 'file', path: 'src/components/Input.tsx', language: 'tsx' },
        ],
      },
      { id: 'src/main.ts', name: 'main.ts', type: 'file', path: 'src/main.ts', language: 'ts' },
      { id: 'src/App.tsx', name: 'App.tsx', type: 'file', path: 'src/App.tsx', language: 'tsx' },
    ],
  },
  { id: 'package.json', name: 'package.json', type: 'file', path: 'package.json', language: 'json' },
  { id: 'README.md', name: 'README.md', type: 'file', path: 'README.md', language: 'markdown' },
];

const mockFileContent: Record<string, FileContent> = {
  'src/main.ts': {
    path: 'src/main.ts',
    content: `import { App } from './App';

function main() {
  const app = new App();
  app.start();
}

main();`,
    language: 'ts',
    size: 123, // mock size in bytes
    lastModified: new Date().toISOString(),
  },
};

// eslint-disable-next-line @typescript-eslint/no-unused-vars
export function FileBrowser(_props: FileBrowserProps) {
  const [selectedNode, setSelectedNode] = useState<FileNode | null>(null);
  const [selectedFile, setSelectedFile] = useState<FileContent | null>(null);

  const handleSelect = (node: FileNode) => {
    setSelectedNode(node);
    if (node.type === 'file') {
      // TODO: Fetch file content from API
      const content = mockFileContent[node.path] || {
        path: node.path,
        content: '// File content not available in preview',
        language: node.language || 'text',
        size: 0,
        lastModified: new Date().toISOString(),
      };
      setSelectedFile(content);
    }
  };

  return (
    <div className="h-full flex gap-4">
      <div className="w-64 flex-shrink-0">
        <h3 className="font-semibold mb-2">Files</h3>
        <FileTree
          nodes={mockFileTree}
          onSelect={handleSelect}
          selectedPath={selectedNode?.path}
        />
      </div>
      <div className="flex-1 min-w-0">
        <FilePreview file={selectedFile} />
      </div>
    </div>
  );
}
