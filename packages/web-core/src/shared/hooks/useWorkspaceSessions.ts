import { useQuery } from '@tanstack/react-query';
import { useState, useCallback, useEffect, useMemo, useRef } from 'react';
import { sessionsApi } from '@/shared/lib/api';
import { useHostId } from '@/shared/providers/HostIdProvider';
import { workspaceSessionKeys } from '@/shared/hooks/workspaceSessionKeys';
import { useSelectedSessionStore } from '@/shared/stores/useSelectedSessionStore';
import type { Session } from 'shared/types';

interface UseWorkspaceSessionsOptions {
  enabled?: boolean;
}

/** Discriminated union for session selection state */
export type SessionSelection =
  | { mode: 'existing'; sessionId: string }
  | { mode: 'new' };

interface UseWorkspaceSessionsResult {
  sessions: Session[];
  selectedSession: Session | undefined;
  selectedSessionId: string | undefined;
  selectSession: (sessionId: string) => void;
  selectLatestSession: () => void;
  isLoading: boolean;
  /** Whether user is creating a new session */
  isNewSessionMode: boolean;
  /** Enter new session mode */
  startNewSession: () => void;
}

/**
 * Hook for managing sessions within a workspace.
 * Fetches all sessions for a workspace and provides session switching capability.
 * Sessions are ordered by most recently used (latest non-dev server execution first).
 */
export function useWorkspaceSessions(
  workspaceId: string | undefined,
  options: UseWorkspaceSessionsOptions = {}
): UseWorkspaceSessionsResult {
  const hostId = useHostId();
  const { enabled = true } = options;
  const [selection, setSelection] = useState<SessionSelection | undefined>(
    undefined
  );
  const prevWorkspaceIdRef = useRef(workspaceId);
  const setStoredSelection = useSelectedSessionStore(
    (state) => state.setSelected
  );

  const { data: sessions = [], isLoading } = useQuery<Session[]>({
    queryKey: workspaceSessionKeys.byWorkspace(workspaceId, hostId),
    queryFn: () => sessionsApi.getByWorkspace(workspaceId!),
    enabled: enabled && !!workspaceId,
  });

  // Auto-select on mount/workspace change. Prefer the store's last-selected
  // session if it still exists in the fetched list; otherwise fall back to
  // the most recently used session. The chosen ID is written back to the
  // store so the workspace remembers what was last viewed.
  useEffect(() => {
    const workspaceChanged = prevWorkspaceIdRef.current !== workspaceId;
    prevWorkspaceIdRef.current = workspaceId;

    if (sessions.length === 0) {
      setSelection(undefined);
      return;
    }

    setSelection((prev) => {
      if (!workspaceChanged && prev) return prev;

      const stored = workspaceId
        ? useSelectedSessionStore.getState().byWorkspace[workspaceId]
        : undefined;
      const sessionId =
        stored && sessions.some((s) => s.id === stored)
          ? stored
          : sessions[0].id;

      if (workspaceId) setStoredSelection(workspaceId, sessionId);
      return { mode: 'existing', sessionId };
    });
  }, [workspaceId, sessions, setStoredSelection]);

  const isNewSessionMode = selection?.mode === 'new' || sessions.length === 0;
  const selectedSessionId =
    selection?.mode === 'existing' ? selection.sessionId : undefined;

  const selectedSession = useMemo(
    () => sessions.find((s) => s.id === selectedSessionId),
    [sessions, selectedSessionId]
  );

  const selectSession = useCallback(
    (sessionId: string) => {
      if (workspaceId) setStoredSelection(workspaceId, sessionId);
      setSelection({ mode: 'existing', sessionId });
    },
    [workspaceId, setStoredSelection]
  );

  const selectLatestSession = useCallback(() => {
    if (sessions.length > 0) {
      const sessionId = sessions[0].id;
      if (workspaceId) setStoredSelection(workspaceId, sessionId);
      setSelection({ mode: 'existing', sessionId });
    }
  }, [sessions, workspaceId, setStoredSelection]);

  const startNewSession = useCallback(() => {
    setSelection({ mode: 'new' });
  }, []);

  return {
    sessions,
    selectedSession,
    selectedSessionId,
    selectSession,
    selectLatestSession,
    isLoading,
    isNewSessionMode,
    startNewSession,
  };
}
