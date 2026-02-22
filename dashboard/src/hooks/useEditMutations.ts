import { useMutation, useQueryClient } from '@tanstack/react-query';
import api from '../lib/api';
import { useEditStore } from '../stores/editStore';

export function useStageEdit() {
  const { stageEdit } = useEditStore();
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ codebaseId, filePath, changes }: { 
      codebaseId: string; 
      filePath: string; 
      changes: import('../types').Change[] 
    }) => {
      const result = await api.edit.stage(codebaseId, changes);
      stageEdit({
        codebaseId,
        filePath,
        changes,
        status: 'staged',
      });
      return result;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['edits'] });
    },
  });
}

export function useApplyEdit() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: (editId: string) => api.edit.apply(editId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['codebases'] });
      queryClient.invalidateQueries({ queryKey: ['graph'] });
    },
  });
}
