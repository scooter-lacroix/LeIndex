import { useEffect, useRef, useCallback } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import type { ServerEvent } from '../types/api';

export function useWebSocket(codebaseId: string | null) {
  const ws = useRef<WebSocket | null>(null);
  const queryClient = useQueryClient();
  
  const connect = useCallback(() => {
    if (!codebaseId) return;
    
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const wsUrl = `${protocol}//${window.location.host}/ws/events`;
    
    ws.current = new WebSocket(wsUrl);
    
    ws.current.onopen = () => {
      console.log('WebSocket connected');
      // Subscribe to codebase events
      ws.current?.send(JSON.stringify({
        type: 'subscribe',
        codebaseId,
      }));
    };
    
    ws.current.onmessage = (event) => {
      const data: ServerEvent = JSON.parse(event.data);
      
      switch (data.type) {
        case 'indexing.progress':
          // Update indexing progress
          queryClient.invalidateQueries({ queryKey: ['codebases'] });
          break;
        case 'graph.delta':
          // Update graph with delta
          queryClient.invalidateQueries({ queryKey: ['graph', codebaseId] });
          break;
        case 'edit.applied':
          // Refresh after edit
          queryClient.invalidateQueries({ queryKey: ['codebases'] });
          queryClient.invalidateQueries({ queryKey: ['graph', codebaseId] });
          break;
      }
    };
    
    ws.current.onerror = (error) => {
      console.error('WebSocket error:', error);
    };
    
    ws.current.onclose = () => {
      console.log('WebSocket disconnected');
      // Reconnect after delay
      setTimeout(connect, 3000);
    };
  }, [codebaseId, queryClient]);
  
  useEffect(() => {
    connect();
    
    return () => {
      ws.current?.close();
    };
  }, [connect]);
  
  return ws.current;
}