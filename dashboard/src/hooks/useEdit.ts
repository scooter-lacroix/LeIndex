import { useCallback } from 'react';
import { useEditStore } from '../stores/editStore';
import { useStageEdit, useApplyEdit } from './useEditMutations';

interface UseEditReturn {
  edits: Array<{ id: string; filePath: string; changes: import('../types').Change[] }>;
  stagedEdits: Array<{ id: string; filePath: string; changes: import('../types').Change[] }>;
  hasUnsavedChanges: boolean;
  addEdit: (filePath: string, changes: import('../types').Change[]) => void;
  removeEdit: (id: string) => void;
  clearEdits: () => void;
  stageChanges: () => Promise<void>;
  applyEdits: () => Promise<void>;
  isApplying: boolean;
}

export function useEdit(codebaseId: string, filePath: string): UseEditReturn {
  const { stagedEdits, history, currentIndex, stageEdit, unstageEdit, clearHistory } = useEditStore();
  const stageMutation = useStageEdit();
  const applyMutation = useApplyEdit();

  const edits = history.slice(0, currentIndex + 1);
  const hasUnsavedChanges = edits.length > 0;

  const addEdit = useCallback((_filePath: string, changes: import('../types').Change[]) => {
    stageEdit({
      codebaseId,
      filePath,
      changes,
      status: 'pending' as const,
    });
  }, [codebaseId, stageEdit]);

  const removeEdit = useCallback((id: string) => {
    unstageEdit(id);
  }, [unstageEdit]);

  const clearEdits = useCallback(() => {
    clearHistory();
  }, [clearHistory]);

  const stageChanges = useCallback(async () => {
    if (edits.length === 0) return;
    await stageMutation.mutateAsync({
      codebaseId,
      filePath,
      changes: edits.flatMap(e => e.changes),
    });
    clearHistory();
  }, [codebaseId, filePath, edits, stageMutation, clearHistory]);

  const applyEdits = useCallback(async () => {
    if (stagedEdits.length === 0) return;
    
    for (const edit of stagedEdits) {
      await applyMutation.mutateAsync(edit.id);
    }
    clearHistory();
  }, [stagedEdits, applyMutation, clearHistory]);

  return {
    edits,
    stagedEdits,
    hasUnsavedChanges,
    addEdit,
    removeEdit,
    clearEdits,
    stageChanges,
    applyEdits,
    isApplying: stageMutation.isPending || applyMutation.isPending,
  };
}
