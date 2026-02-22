import { useState } from 'react';
import { ChevronRight, ChevronDown, File, Folder, FolderOpen } from 'lucide-react';
import type { FileNode } from '../../types/editor';
import { cn } from '../../lib/utils';

interface FileTreeProps {
  nodes: FileNode[];
  onSelect: (node: FileNode) => void;
  selectedPath?: string;
}

function FileTreeNode({ 
  node, 
  onSelect, 
  selectedPath,
  depth = 0 
}: { 
  node: FileNode; 
  onSelect: (node: FileNode) => void;
  selectedPath?: string;
  depth?: number;
}) {
  const [isExpanded, setIsExpanded] = useState(node.isExpanded || false);
  const isSelected = selectedPath === node.path;

  const handleClick = () => {
    if (node.type === 'directory') {
      setIsExpanded(!isExpanded);
    }
    onSelect(node);
  };

  return (
    <div>
      <div
        onClick={handleClick}
        className={cn(
          'flex items-center gap-1 py-1 px-2 cursor-pointer hover:bg-accent',
          isSelected && 'bg-primary/10 text-primary',
          depth > 0 && 'ml-4'
        )}
        style={{ paddingLeft: `${depth * 12 + 8}px` }}
      >
        {node.type === 'directory' ? (
          <>
            {isExpanded ? (
              <ChevronDown className="h-4 w-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="h-4 w-4 text-muted-foreground" />
            )}
            {isExpanded ? (
              <FolderOpen className="h-4 w-4 text-amber-500" />
            ) : (
              <Folder className="h-4 w-4 text-amber-500" />
            )}
          </>
        ) : (
          <>
            <span className="w-4" />
            <File className="h-4 w-4 text-blue-500" />
          </>
        )}
        <span className="text-sm truncate">{node.name}</span>
      </div>
      
      {node.type === 'directory' && isExpanded && node.children && (
        <div>
          {node.children.map((child) => (
            <FileTreeNode
              key={child.id}
              node={child}
              onSelect={onSelect}
              selectedPath={selectedPath}
              depth={depth + 1}
            />
          ))}
        </div>
      )}
    </div>
  );
}

export function FileTree({ nodes, onSelect, selectedPath }: FileTreeProps) {
  return (
    <div className="border rounded-lg bg-card overflow-hidden">
      {nodes.map((node) => (
        <FileTreeNode
          key={node.id}
          node={node}
          onSelect={onSelect}
          selectedPath={selectedPath}
        />
      ))}
    </div>
  );
}
