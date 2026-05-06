import { create } from 'zustand';

interface SelectedSessionState {
  /** Last selected session per workspace, keyed by workspace ID. */
  byWorkspace: Record<string, string>;
  setSelected: (workspaceId: string, sessionId: string) => void;
}

export const useSelectedSessionStore = create<SelectedSessionState>((set) => ({
  byWorkspace: {},
  setSelected: (workspaceId, sessionId) =>
    set((state) =>
      state.byWorkspace[workspaceId] === sessionId
        ? state
        : {
            byWorkspace: { ...state.byWorkspace, [workspaceId]: sessionId },
          }
    ),
}));
