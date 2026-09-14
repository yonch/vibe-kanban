interface ArchivedWorkspaceSortFields {
  name: string;
  updatedAt: string;
}

function toTimestamp(value: string): number | null {
  const timestamp = new Date(value).getTime();
  return Number.isNaN(timestamp) ? null : timestamp;
}

export function sortArchivedWorkspacesByArchiveTime<
  T extends ArchivedWorkspaceSortFields,
>(workspaces: T[]): T[] {
  return [...workspaces].sort((a, b) => {
    const aTimestamp = toTimestamp(a.updatedAt);
    const bTimestamp = toTimestamp(b.updatedAt);

    if (aTimestamp === null && bTimestamp === null) {
      return a.name.localeCompare(b.name);
    }
    if (aTimestamp === null) {
      return 1;
    }
    if (bTimestamp === null) {
      return -1;
    }
    if (aTimestamp === bTimestamp) {
      return a.name.localeCompare(b.name);
    }

    return bTimestamp - aTimestamp;
  });
}
