import { ZoomIn, ZoomOut, Maximize, Filter } from 'lucide-react';
import { Button } from '../UI/button';
import type { GraphViewHandle } from '../../types/graph';

interface GraphControlsProps {
  graphRef: React.RefObject<GraphViewHandle>;
  onFilter?: () => void;
}

export function GraphControls({ graphRef, onFilter }: GraphControlsProps) {
  return (
    <div className="absolute bottom-4 left-4 flex flex-col gap-2">
      <div className="flex gap-2">
        <Button variant="secondary" size="icon" onClick={() => graphRef.current?.zoom(1.2, 400)}>
          <ZoomIn className="h-4 w-4" />
        </Button>
        <Button variant="secondary" size="icon" onClick={() => graphRef.current?.zoom(1 / 1.2, 400)}>
          <ZoomOut className="h-4 w-4" />
        </Button>
        <Button variant="secondary" size="icon" onClick={() => graphRef.current?.fitView(400, 50)}>
          <Maximize className="h-4 w-4" />
        </Button>
        {onFilter && (
          <Button variant="secondary" size="icon" onClick={onFilter}>
            <Filter className="h-4 w-4" />
          </Button>
        )}
      </div>
    </div>
  );
}
