import { useMemo, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { useWorkspaceContext } from '@/shared/hooks/useWorkspaceContext';
import { useUserContext } from '@/shared/hooks/useUserContext';
import { useActions } from '@/shared/hooks/useActions';
import { useSyncErrorContext } from '@/shared/hooks/useSyncErrorContext';
import { useUserOrganizations } from '@/shared/hooks/useUserOrganizations';
import { useOrganizationStore } from '@/shared/stores/useOrganizationStore';
import {
  Navbar,
  type NavbarBreadcrumbItem,
  type MobileTabId,
} from '@vibe/ui/components/Navbar';
import { useAllOrganizationProjects } from '@/shared/hooks/useAllOrganizationProjects';
import { useShape } from '@/shared/integrations/electric/hooks';
import { PROJECT_ISSUES_SHAPE } from 'shared/remote-types';
import { RemoteIssueLink } from './RemoteIssueLink';
import { AppBarUserPopoverContainer } from './AppBarUserPopoverContainer';
import { useUserSystem } from '@/shared/hooks/useUserSystem';
import { NavbarActionGroups, Actions } from '@/shared/actions';
import { type ActionDefinition } from '@/shared/types/actions';
import {
  filterNavbarItems,
  toNavbarSectionItems,
} from '@/shared/lib/navbarItems';
import { useActionVisibilityContext } from '@/shared/hooks/useActionVisibilityContext';
import { useMobileActiveTab } from '@/shared/stores/useUiPreferencesStore';
import { CommandBarDialog } from '@/shared/dialogs/command-bar/CommandBarDialog';
import { SettingsDialog } from '@/shared/dialogs/settings/SettingsDialog';
import { getProjectDestination } from '@/shared/lib/routes/appNavigation';
import { useAppNavigation } from '@/shared/hooks/useAppNavigation';
import { useCurrentAppDestination } from '@/shared/hooks/useCurrentAppDestination';
import { getRemoteAuthDegradedMessage } from '@/shared/lib/auth/remoteAuthDegraded';

// Actions that don't fit the mobile navbar layout (customContent icons aren't
// rendered in the mobile workspace navbar). They stay globally visible so the
// Command Bar still surfaces them. Trigger on the viewport-driven mobile
// layout switch, not user-agent heuristics, so narrow desktop viewports get
// the same exclusion the mobile chrome already applies.
const MOBILE_NAVBAR_EXCLUDED_ACTION_IDS: ReadonlySet<string> = new Set([
  'open-in-ide',
  'copy-workspace-path',
  'toggle-dev-server',
]);

export function NavbarContainer({
  mobileMode = false,
  onOrgSelect,
  onOpenDrawer,
}: {
  mobileMode?: boolean;
  onOrgSelect?: (orgId: string) => void;
  onOpenDrawer?: () => void;
}) {
  const { t } = useTranslation('common');
  const { executeAction } = useActions();
  const { workspace: selectedWorkspace, isCreateMode } = useWorkspaceContext();
  const { workspaces } = useUserContext();
  const syncErrorContext = useSyncErrorContext();
  const { remoteAuthDegraded } = useUserSystem();
  const appNavigation = useAppNavigation();
  const destination = useCurrentAppDestination();
  const projectDestination = useMemo(
    () => getProjectDestination(destination),
    [destination]
  );
  const isOnProjectPage = projectDestination !== null;
  const projectId = projectDestination?.projectId ?? null;
  const isOnProjectSubRoute =
    projectDestination !== null && projectDestination.kind !== 'project';
  const [mobileActiveTab, setMobileActiveTab] = useMobileActiveTab();

  // Find remote workspace linked to current local workspace
  const linkedRemoteWorkspace = useMemo(() => {
    if (!selectedWorkspace?.id) return null;
    return (
      workspaces.find((w) => w.local_workspace_id === selectedWorkspace.id) ??
      null
    );
  }, [workspaces, selectedWorkspace?.id]);

  const { data: orgsData } = useUserOrganizations();
  const selectedOrgId = useOrganizationStore((s) => s.selectedOrgId);
  const orgName =
    orgsData?.organizations.find((o) => o.id === selectedOrgId)?.name ?? '';

  // Get action visibility context (includes all state for visibility/active/enabled)
  const actionCtx = useActionVisibilityContext();

  // In the viewport-mobile layout (NavbarContainer's mobileMode prop, driven
  // by useIsMobile()), exclude navbar-only actions from rendering here.
  // The actions remain globally visible (Command Bar, kanban shortcuts, etc.).
  const navbarExcludeIds = mobileMode
    ? MOBILE_NAVBAR_EXCLUDED_ACTION_IDS
    : undefined;

  // Action handler - all actions go through the standard executeAction
  const handleExecuteAction = useCallback(
    (action: ActionDefinition) => {
      if (action.requiresTarget && selectedWorkspace?.id) {
        executeAction(action, selectedWorkspace.id);
      } else {
        executeAction(action);
      }
    },
    [executeAction, selectedWorkspace?.id]
  );

  const leftItems = useMemo(
    () =>
      toNavbarSectionItems(
        filterNavbarItems(NavbarActionGroups.left, actionCtx, navbarExcludeIds),
        actionCtx,
        handleExecuteAction
      ),
    [actionCtx, handleExecuteAction, navbarExcludeIds]
  );

  const rightItems = useMemo(
    () =>
      toNavbarSectionItems(
        filterNavbarItems(
          NavbarActionGroups.right,
          actionCtx,
          navbarExcludeIds
        ),
        actionCtx,
        handleExecuteAction
      ),
    [actionCtx, handleExecuteAction, navbarExcludeIds]
  );

  const navbarTitle = isCreateMode
    ? 'Create Workspace'
    : isOnProjectPage
      ? orgName
      : selectedWorkspace?.branch;

  // Breadcrumbs: Project / Issue / Workspace (only on workspace pages with linked project)
  const linkedProjectId = linkedRemoteWorkspace?.project_id ?? null;
  const linkedIssueId = linkedRemoteWorkspace?.issue_id ?? null;
  const shouldResolveBreadcrumbData =
    !isOnProjectPage && !isCreateMode && !!linkedProjectId;
  const shouldResolveIssueBreadcrumb =
    shouldResolveBreadcrumbData && !!linkedIssueId;

  const { data: allProjects, isLoading: isProjectsLoading } =
    useAllOrganizationProjects({
      enabled: shouldResolveBreadcrumbData,
    });
  const { data: projectIssues, isLoading: isProjectIssuesLoading } = useShape(
    PROJECT_ISSUES_SHAPE,
    { project_id: linkedProjectId || '' },
    { enabled: shouldResolveIssueBreadcrumb }
  );
  const linkedProject = allProjects.find((p) => p.id === linkedProjectId);
  const isWaitingForProjectBreadcrumb =
    shouldResolveBreadcrumbData && !linkedProject && isProjectsLoading;
  const isWaitingForIssueBreadcrumb =
    shouldResolveIssueBreadcrumb && isProjectIssuesLoading;
  const isWaitingForBreadcrumbData =
    isWaitingForProjectBreadcrumb || isWaitingForIssueBreadcrumb;

  const breadcrumbs = useMemo((): NavbarBreadcrumbItem[] | undefined => {
    if (
      !shouldResolveBreadcrumbData ||
      !linkedProjectId ||
      isWaitingForBreadcrumbData
    ) {
      return undefined;
    }

    const project = linkedProject;
    if (!project) return undefined;

    const items: NavbarBreadcrumbItem[] = [
      {
        label: project.name,
        onClick: () => appNavigation.goToProject(linkedProjectId),
      },
    ];

    if (linkedIssueId) {
      const issue = projectIssues.find((i) => i.id === linkedIssueId);
      if (issue) {
        items.push({
          label: issue.simple_id,
          onClick: () =>
            appNavigation.goToProjectIssue(linkedProjectId, linkedIssueId),
        });
      }
    }

    const workspaceLabel =
      selectedWorkspace?.name || selectedWorkspace?.branch || '';
    if (workspaceLabel) {
      items.push({ label: workspaceLabel });
    }

    return items.length > 1 ? items : undefined;
  }, [
    shouldResolveBreadcrumbData,
    linkedProjectId,
    linkedIssueId,
    linkedProject,
    isWaitingForBreadcrumbData,
    projectIssues,
    selectedWorkspace?.name,
    selectedWorkspace?.branch,
    appNavigation,
  ]);

  // Mobile-specific callbacks
  const handleOpenCommandBar = useCallback(() => {
    CommandBarDialog.show();
  }, []);

  const handleOpenSettings = useCallback(() => {
    SettingsDialog.show();
  }, []);

  const handleNavigateBack = useCallback(() => {
    if (isOnProjectPage && projectId) {
      // On project sub-route: go back to project root (kanban board)
      appNavigation.goToProject(projectId);
    } else {
      // Non-project page: go to workspaces
      appNavigation.goToWorkspaces();
    }
  }, [isOnProjectPage, projectId, appNavigation]);

  const handleNavigateToBoard = useMemo(() => {
    if (!isOnProjectPage || !projectId) return null;
    return () => {
      appNavigation.goToProject(projectId);
    };
  }, [isOnProjectPage, projectId, appNavigation]);

  // Mobile archive handler - uses the existing ArchiveWorkspace action
  const handleArchive = useCallback(() => {
    handleExecuteAction(Actions.ArchiveWorkspace);
  }, [handleExecuteAction]);

  // Build user popover slot for mobile mode
  const userPopoverSlot = useMemo(() => {
    if (!mobileMode) return undefined;
    return (
      <AppBarUserPopoverContainer
        organizations={orgsData?.organizations ?? []}
        selectedOrgId={selectedOrgId ?? ''}
        onOrgSelect={onOrgSelect ?? (() => {})}
      />
    );
  }, [mobileMode, orgsData?.organizations, selectedOrgId, onOrgSelect]);

  const syncErrors = useMemo(() => {
    const errors = syncErrorContext?.errors ? [...syncErrorContext.errors] : [];

    if (remoteAuthDegraded) {
      errors.push({
        streamId: 'remote-auth-degraded',
        tableName: 'Remote authentication',
        error: {
          message: getRemoteAuthDegradedMessage(remoteAuthDegraded, t),
        },
        retry: () => window.location.reload(),
      });
    }

    return errors;
  }, [remoteAuthDegraded, syncErrorContext?.errors, t]);

  return (
    <Navbar
      workspaceTitle={navbarTitle}
      breadcrumbs={breadcrumbs}
      leftItems={leftItems}
      rightItems={rightItems}
      syncErrors={syncErrors}
      mobileMode={mobileMode}
      mobileUserSlot={userPopoverSlot}
      isOnProjectPage={isOnProjectPage}
      isOnProjectSubRoute={isOnProjectSubRoute}
      onOpenCommandBar={handleOpenCommandBar}
      onOpenSettings={handleOpenSettings}
      onNavigateBack={handleNavigateBack}
      onNavigateToBoard={handleNavigateToBoard}
      onOpenDrawer={onOpenDrawer}
      onArchive={
        mobileMode && selectedWorkspace && !isCreateMode
          ? handleArchive
          : undefined
      }
      mobileActiveTab={mobileActiveTab as MobileTabId}
      onMobileTabChange={(tab) => setMobileActiveTab(tab)}
      leftSlot={
        !breadcrumbs &&
        !isWaitingForBreadcrumbData &&
        linkedRemoteWorkspace?.issue_id ? (
          <RemoteIssueLink
            projectId={linkedRemoteWorkspace.project_id}
            issueId={linkedRemoteWorkspace.issue_id}
          />
        ) : null
      }
    />
  );
}
