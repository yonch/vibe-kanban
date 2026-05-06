import { createFileRoute } from "@tanstack/react-router";
import { requireAuthenticated } from "@remote/shared/lib/route-auth";
import { Workspaces } from "@/pages/workspaces/Workspaces";
import { RemoteWorkspacesPageShell } from "@remote/pages/RemoteWorkspacesPageShell";
import { workspaceSearchValidator } from "@vibe/web-core/workspace-search";

export const Route = createFileRoute("/hosts/$hostId/workspaces_/$workspaceId")(
  {
    validateSearch: workspaceSearchValidator,
    beforeLoad: async ({ location }) => {
      await requireAuthenticated(location);
    },
    component: WorkspaceRouteComponent,
  },
);

function WorkspaceRouteComponent() {
  return (
    <RemoteWorkspacesPageShell>
      <Workspaces />
    </RemoteWorkspacesPageShell>
  );
}
