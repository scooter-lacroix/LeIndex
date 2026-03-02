import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { PanelType } from '../types';

interface UIState {
  sidebarOpen: boolean;
  activePanel: PanelType;
  theme: 'light' | 'dark';
  selectedCodebaseId: string | null;
  isLoading: boolean;
  
  setSidebarOpen: (open: boolean) => void;
  setActivePanel: (panel: PanelType) => void;
  toggleTheme: () => void;
  setSelectedCodebaseId: (id: string | null) => void;
  setIsLoading: (loading: boolean) => void;
}

export const useUIStore = create<UIState>()(
  persist(
    (set) => ({
      sidebarOpen: true,
      activePanel: 'codebases',
      theme: 'dark',
      selectedCodebaseId: null,
      isLoading: false,
      
      setSidebarOpen: (open) => set({ sidebarOpen: open }),
      setActivePanel: (panel) => set({ activePanel: panel }),
      toggleTheme: () => set((state) => ({ theme: state.theme === 'light' ? 'dark' : 'light' })),
      setSelectedCodebaseId: (id) => set({ selectedCodebaseId: id }),
      setIsLoading: (loading) => set({ isLoading: loading }),
    }),
    {
      name: 'leindex-ui-storage',
      partialize: (state) => ({ theme: state.theme, sidebarOpen: state.sidebarOpen }),
    }
  )
);
