import { useMemo, useCallback } from "react";
import { useLocation } from "@tanstack/react-router";
import { useWorkspaceContext } from "@/shared/hooks/useWorkspaceContext";
import { useActions } from "@/shared/hooks/useActions";
import { useSyncErrorContext } from "@/shared/hooks/useSyncErrorContext";
import { useUserOrganizations } from "@/shared/hooks/useUserOrganizations";
import { useOrganizationStore } from "@/shared/stores/useOrganizationStore";
import { Navbar } from "@vibe/ui/components/Navbar";
import { NavbarActionGroups } from "@/shared/actions";
import { type ActionDefinition } from "@/shared/types/actions";
import {
  filterNavbarItems,
  toNavbarSectionItems,
} from "@/shared/lib/navbarItems";
import { useActionVisibilityContext } from "@/shared/hooks/useActionVisibilityContext";
import { SettingsDialog } from "@/shared/dialogs/settings/SettingsDialog";
import { CommandBarDialog } from "@/shared/dialogs/command-bar/CommandBarDialog";

/**
 * Desktop navbar for remote workspace and project pages.
 *
 * Mounted on workspace detail routes (/workspaces/:id) and project routes (/projects/:id)
 * where all required providers (ActionsProvider, WorkspaceProvider, etc.) are available.
 *
 * Mobile navbar is handled separately by RemoteNavbarContainer.
 */
export function RemoteDesktopNavbar() {
  const { executeAction } = useActions();
  const { workspace: selectedWorkspace } = useWorkspaceContext();
  const syncErrorContext = useSyncErrorContext();
  const location = useLocation();

  const isOnProjectPage =
    /^\/projects\/[^/]+/.test(location.pathname) ||
    /^\/hosts\/[^/]+\/projects\/[^/]+/.test(location.pathname);

  const { data: orgsData } = useUserOrganizations();
  const selectedOrgId = useOrganizationStore((s) => s.selectedOrgId);
  const orgName =
    orgsData?.organizations.find((o) => o.id === selectedOrgId)?.name ?? "";

  const actionCtx = useActionVisibilityContext();

  const handleExecuteAction = useCallback(
    (action: ActionDefinition) => {
      if (action.requiresTarget && selectedWorkspace?.id) {
        executeAction(action, selectedWorkspace.id);
      } else {
        executeAction(action);
      }
    },
    [executeAction, selectedWorkspace?.id],
  );

  const leftItems = useMemo(
    () =>
      toNavbarSectionItems(
        filterNavbarItems(NavbarActionGroups.left, actionCtx),
        actionCtx,
        handleExecuteAction,
      ),
    [actionCtx, handleExecuteAction],
  );

  const rightItems = useMemo(
    () =>
      toNavbarSectionItems(
        filterNavbarItems(NavbarActionGroups.right, actionCtx),
        actionCtx,
        handleExecuteAction,
      ),
    [actionCtx, handleExecuteAction],
  );

  const handleOpenSettings = useCallback(() => {
    SettingsDialog.show();
  }, []);

  const handleOpenCommandBar = useCallback(() => {
    CommandBarDialog.show();
  }, []);

  const navbarTitle = isOnProjectPage ? orgName : selectedWorkspace?.branch;

  return (
    <Navbar
      workspaceTitle={navbarTitle}
      leftItems={leftItems}
      rightItems={rightItems}
      syncErrors={syncErrorContext?.errors}
      isOnProjectPage={isOnProjectPage}
      onOpenSettings={handleOpenSettings}
      onOpenCommandBar={handleOpenCommandBar}
    />
  );
}
