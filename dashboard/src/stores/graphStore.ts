import { create } from 'zustand';
import type { GraphData } from '../types/graph';

interface GraphState {
  selectedNodes: Set<string>;
  highlightedNodes: Set<string>;
  zoom: number;
  center: { x: number; y: number };
  graphData: GraphData | null;
  
  selectNode: (id: string) => void;
  deselectNode: (id: string) => void;
  toggleNodeSelection: (id: string) => void;
  clearSelection: () => void;
  highlightNode: (id: string) => void;
  clearHighlights: () => void;
  setZoom: (zoom: number) => void;
  setCenter: (x: number, y: number) => void;
  setGraphData: (data: GraphData) => void;
  updateGraphDelta: (delta: import('../types/graph').GraphDelta) => void;
}

export const useGraphStore = create<GraphState>((set) => ({
  selectedNodes: new Set(),
  highlightedNodes: new Set(),
  zoom: 1,
  center: { x: 0, y: 0 },
  graphData: null,
  
  selectNode: (id) => set((state) => ({
    selectedNodes: new Set([...state.selectedNodes, id]),
  })),
  
  deselectNode: (id) => set((state) => {
    const newSet = new Set(state.selectedNodes);
    newSet.delete(id);
    return { selectedNodes: newSet };
  }),
  
  toggleNodeSelection: (id) => set((state) => {
    const newSet = new Set(state.selectedNodes);
    if (newSet.has(id)) {
      newSet.delete(id);
    } else {
      newSet.add(id);
    }
    return { selectedNodes: newSet };
  }),
  
  clearSelection: () => set({ selectedNodes: new Set() }),
  
  highlightNode: (id) => set((state) => ({
    highlightedNodes: new Set([...state.highlightedNodes, id]),
  })),
  
  clearHighlights: () => set({ highlightedNodes: new Set() }),
  
  setZoom: (zoom) => set({ zoom }),
  
  setCenter: (x, y) => set({ center: { x, y } }),
  
  setGraphData: (data) => set({ graphData: data }),
  
  updateGraphDelta: (delta) => set((state) => {
    if (!state.graphData) return state;
    
    const nodes = [...state.graphData.nodes];
    const links = [...state.graphData.links];
    
    // Remove deleted nodes
    const removedSet = new Set(delta.removed);
    const filteredNodes = nodes.filter((n) => !removedSet.has(n.id));
    
    // Add new nodes
    filteredNodes.push(...delta.added);
    
    // Update modified nodes
    delta.modified.forEach((modified) => {
      const idx = filteredNodes.findIndex((n) => n.id === modified.id);
      if (idx !== -1) {
        filteredNodes[idx] = { ...filteredNodes[idx], ...modified };
      }
    });
    
    // Update links
    const filteredLinks = links.filter(
      (l) =>
        !delta.removedLinks.some(
          (rl) => rl.source === l.source && rl.target === l.target
        )
    );
    filteredLinks.push(...delta.addedLinks);
    
    return {
      graphData: {
        nodes: filteredNodes,
        links: filteredLinks,
      },
    };
  }),
}));