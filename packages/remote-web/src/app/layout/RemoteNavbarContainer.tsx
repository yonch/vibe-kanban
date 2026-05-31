import { useCallback, useEffect, useMemo, type ReactNode } from "react";
import { useLocation, useNavigate, useParams } from "@tanstack/react-router";
import {
  MOBILE_TABS,
  Navbar,
  type MobileTabId,
} from "@vibe/ui/components/Navbar";
import { SettingsDialog } from "@/shared/dialogs/settings/SettingsDialog";
import { CommandBarDialog } from "@/shared/dialogs/command-bar/CommandBarDialog";
import { useMobileActiveTab } from "@/shared/stores/useUiPreferencesStore";
import { useMobileWorkspaceTitle } from "@remote/shared/stores/useMobileWorkspaceTitle";
import { useActions } from "@/shared/hooks/useActions";
import { useWorkspaceContext } from "@/shared/hooks/useWorkspaceContext";
import { useActionVisibilityContext } from "@/shared/hooks/useActionVisibilityContext";
import { NavbarActionGroups } from "@/shared/actions";
import { type ActionDefinition } from "@/shared/types/actions";
import {
  filterNavbarItems,
  toNavbarSectionItems,
  MOBILE_NAVBAR_EXCLUDED_ACTION_IDS,
} from "@/shared/lib/navbarItems";

interface RemoteNavbarContainerProps {
  organizationName: string | null;
  mobileMode?: boolean;
  onOpenDrawer?: () => void;
  mobileUserSlot?: ReactNode;
}

export function RemoteNavbarContainer({
  organizationName,
  mobileMode,
  onOpenDrawer,
  mobileUserSlot,
}: RemoteNavbarContainerProps) {
  const location = useLocation();
  const { hostId } = useParams({ strict: false });
  const mobileWorkspaceTitle = useMobileWorkspaceTitle((s) => s.title);
  const { executeAction } = useActions();
  const { workspace: selectedWorkspace } = useWorkspaceContext();
  const actionCtx = useActionVisibilityContext();

  const [mobileActiveTab, setMobileActiveTab] = useMobileActiveTab();

  const remoteMobileTabs = useMemo(
    () =>
      MOBILE_TABS.filter((t) => t.id !== "preview" && t.id !== "workspaces"),
    [],
  );

  const isOnWorkspaceView = /^\/hosts\/[^/]+\/workspaces\/[^/]+/.test(
    location.pathname,
  );
  const isOnWorkspaceList = /^\/hosts\/[^/]+\/workspaces\/?$/.test(
    location.pathname,
  );

  useEffect(() => {
    if (isOnWorkspaceView) {
      setMobileActiveTab("chat");
    }
  }, [isOnWorkspaceView, setMobileActiveTab]);
  const navigate = useNavigate();

  const isOnProjectPage = /^\/projects\/[^/]+/.test(location.pathname);
  const pathSegments = location.pathname.split("/").filter(Boolean);
  const projectSegmentIndex = pathSegments.indexOf("projects");
  const projectId =
    projectSegmentIndex === -1
      ? null
      : (pathSegments[projectSegmentIndex + 1] ?? null);
  const isOnProjectSubRoute =
    isOnProjectPage &&
    (location.pathname.includes("/issues/") ||
      location.pathname.includes("/workspaces/"));

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

  const navbarExcludeIds = mobileMode
    ? MOBILE_NAVBAR_EXCLUDED_ACTION_IDS
    : undefined;

  const rightItems = useMemo(
    () =>
      toNavbarSectionItems(
        filterNavbarItems(
          NavbarActionGroups.right,
          actionCtx,
          navbarExcludeIds,
        ),
        actionCtx,
        handleExecuteAction,
      ),
    [actionCtx, handleExecuteAction, navbarExcludeIds],
  );

  const workspaceTitle = useMemo(() => {
    if (isOnProjectPage) {
      return organizationName ?? "Project";
    }
    if (isOnWorkspaceView) {
      return mobileWorkspaceTitle ?? undefined;
    }
    return undefined;
  }, [
    location.pathname,
    organizationName,
    isOnProjectPage,
    isOnWorkspaceView,
    mobileWorkspaceTitle,
  ]);

  const mobileShowBack = isOnWorkspaceView || isOnWorkspaceList;

  const handleNavigateBack = useCallback(() => {
    if (isOnProjectPage && projectId) {
      navigate({
        to: "/projects/$projectId",
        params: { projectId },
      });
    } else if (isOnWorkspaceView) {
      if (!hostId) {
        navigate({ to: "/" });
        return;
      }
      navigate({ to: "/hosts/$hostId/workspaces", params: { hostId } });
    } else {
      navigate({ to: "/" });
    }
  }, [navigate, hostId, isOnProjectPage, projectId, isOnWorkspaceView]);

  const handleOpenSettings = useCallback(() => {
    SettingsDialog.show();
  }, []);

  const handleOpenCommandBar = useCallback(() => {
    CommandBarDialog.show();
  }, []);

  return (
    <Navbar
      workspaceTitle={workspaceTitle}
      rightItems={isOnProjectPage ? rightItems : undefined}
      mobileMode={mobileMode}
      mobileUserSlot={mobileUserSlot}
      isOnProjectPage={isOnProjectPage}
      isOnProjectSubRoute={isOnProjectSubRoute}
      onNavigateBack={handleNavigateBack}
      mobileShowBack={mobileShowBack}
      onOpenSettings={handleOpenSettings}
      onOpenCommandBar={handleOpenCommandBar}
      onOpenDrawer={isOnProjectPage ? onOpenDrawer : undefined}
      mobileActiveTab={mobileActiveTab as MobileTabId}
      onMobileTabChange={(tab) => setMobileActiveTab(tab)}
      mobileTabs={remoteMobileTabs}
      showMobileTabs={isOnWorkspaceView}
    />
  );
}
