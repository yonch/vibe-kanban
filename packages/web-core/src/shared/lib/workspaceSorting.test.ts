import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { sortArchivedWorkspacesByArchiveTime } from './workspaceSorting.ts';

describe('sortArchivedWorkspacesByArchiveTime', () => {
  it('sorts recently archived workspaces before older ones', () => {
    const workspaces = [
      {
        name: 'old workspace',
        createdAt: '2026-01-01T00:00:00Z',
        updatedAt: '2026-09-14T10:00:00Z',
      },
      {
        name: 'new workspace',
        createdAt: '2026-09-01T00:00:00Z',
        updatedAt: '2026-09-14T09:00:00Z',
      },
    ];

    assert.deepEqual(sortArchivedWorkspacesByArchiveTime(workspaces), [
      workspaces[0],
      workspaces[1],
    ]);
  });

  it('puts invalid timestamps last and uses names as a stable tie-breaker', () => {
    const workspaces = [
      { name: 'Zulu', updatedAt: 'invalid' },
      { name: 'Bravo', updatedAt: '2026-09-14T10:00:00Z' },
      { name: 'Alpha', updatedAt: '2026-09-14T10:00:00Z' },
    ];

    assert.deepEqual(
      sortArchivedWorkspacesByArchiveTime(workspaces).map(({ name }) => name),
      ['Alpha', 'Bravo', 'Zulu']
    );
  });
});
