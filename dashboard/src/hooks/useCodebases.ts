import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import api from '../lib/api';

export function useCodebases() {
  return useQuery({
    queryKey: ['codebases'],
    queryFn: () => api.codebases.list(),
    staleTime: 30000,
  });
}

export function useSearch(query: string) {
  return useQuery({
    queryKey: ['search', query],
    queryFn: () => api.search.query(query),
    enabled: query.length > 0,
    staleTime: 60000,
  });
}

export function useRefreshCodebases() {
  const queryClient = useQueryClient();
  
  return useMutation({
    mutationFn: async () => {
      // Refetch all codebases data instead of calling refresh endpoint
      await queryClient.invalidateQueries({ queryKey: ['codebases'] });
    },
    onSuccess: () => {
      // Already invalidated
    },
  });
}
