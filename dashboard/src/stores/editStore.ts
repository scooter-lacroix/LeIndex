import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { EditOperation } from '../types';

interface EditState {
  stagedEdits: EditOperation[];
  history: EditOperation[];
  currentIndex: number;
  canUndo: boolean;
  canRedo: boolean;

  stageEdit: (edit: Omit<EditOperation, 'id' | 'createdAt'>) => string;
  unstageEdit: (id: string) => void;
  undo: () => void;
  redo: () => void;
  clearHistory: () => void;
}

export const useEditStore = create<EditState>()(
  persist(
    (set, get) => ({
      stagedEdits: [],
      history: [],
      currentIndex: -1,
      canUndo: false,
      canRedo: false,

      stageEdit: (edit) => {
        const id = `edit-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
        const fullEdit: EditOperation = {
          ...edit,
          id,
          createdAt: new Date().toISOString(),
        };

        set((state) => ({
          stagedEdits: [...state.stagedEdits, fullEdit],
          history: [...state.history.slice(0, state.currentIndex + 1), fullEdit],
          currentIndex: state.currentIndex + 1,
          canUndo: true,
          canRedo: false,
        }));

        return id;
      },

      unstageEdit: (id) => {
        set((state) => ({
          stagedEdits: state.stagedEdits.filter((e) => e.id !== id),
        }));
      },

      undo: () => {
        const { currentIndex } = get();
        if (currentIndex >= 0) {
          set((state) => ({
            currentIndex: state.currentIndex - 1,
            canUndo: state.currentIndex - 1 >= 0,
            canRedo: true,
          }));
        }
      },

      redo: () => {
        const { currentIndex, history } = get();
        if (currentIndex < history.length - 1) {
          set((state) => ({
            currentIndex: state.currentIndex + 1,
            canUndo: true,
            canRedo: state.currentIndex + 1 < history.length - 1,
          }));
        }
      },

      clearHistory: () => {
        set({
          stagedEdits: [],
          history: [],
          currentIndex: -1,
          canUndo: false,
          canRedo: false,
        });
      },
    }),
    {
      name: 'leindex-edit-storage',
      partialize: (state) => ({ history: state.history, currentIndex: state.currentIndex }),
    }
  )
);
