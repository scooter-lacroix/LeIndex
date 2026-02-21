import { useQuery } from '@tanstack/react-query';
import api from '../lib/api';

export function useGraph(codebaseId: string) {
  return useQuery({
    queryKey: ['graph', codebaseId],
    queryFn: () => api.graph.get(codebaseId),
    staleTime: Infinity,
    enabled: !!codebaseId,
  });
}

export function useGraphNode(codebaseId: string, nodeId: string) {
  return useQuery({
    queryKey: ['graph', codebaseId, 'node', nodeId],
    queryFn: () => api.graph.getNode(codebaseId, nodeId),
    enabled: !!codebaseId && !!nodeId,
  });
}