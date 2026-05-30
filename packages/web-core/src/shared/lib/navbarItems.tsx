import type { ReactNode } from 'react';
import { CopyIcon } from '@phosphor-icons/react';
import { type NavbarSectionItem } from '@vibe/ui/components/Navbar';
import {
  NavbarDivider,
  type ActionDefinition,
  type NavbarItem as ActionNavbarItem,
  type ActionVisibilityContext,
  type SpecialIconType,
  isSpecialIcon,
  isActionVisible,
  isActionActive,
  isActionEnabled,
  getActionIcon,
  getActionTooltip,
} from '@/shared/types/actions';
import { IdeIcon } from '@/shared/components/IdeIcon';
import { CopyButton } from '@/shared/components/CopyButton';

export function isNavbarDivider(
  item: ActionNavbarItem
): item is typeof NavbarDivider {
  return 'type' in item && item.type === 'divider';
}

/**
 * Filter navbar items by visibility, keeping dividers but removing them
 * if they would appear at the start, end, or consecutively.
 */
export function filterNavbarItems(
  items: readonly ActionNavbarItem[],
  ctx: ActionVisibilityContext
): ActionNavbarItem[] {
  const filtered = items.filter((item) => {
    if (isNavbarDivider(item)) return true;
    return isActionVisible(item, ctx);
  });

  const result: ActionNavbarItem[] = [];
  for (const item of filtered) {
    if (isNavbarDivider(item)) {
      if (result.length > 0 && !isNavbarDivider(result[result.length - 1])) {
        result.push(item);
      }
    } else {
      result.push(item);
    }
  }

  if (result.length > 0 && isNavbarDivider(result[result.length - 1])) {
    result.pop();
  }

  return result;
}

/**
 * Builds the rendered content for actions whose icon is not a plain Phosphor
 * icon: the editor logo (ide-icon) and the copy-with-feedback button.
 */
function buildSpecialContent(
  iconType: SpecialIconType,
  ctx: ActionVisibilityContext,
  enabled: boolean,
  tooltip: string,
  execute: () => void
): ReactNode {
  if (iconType === 'ide-icon') {
    return (
      <button
        type="button"
        className="flex items-center justify-center rounded-sm text-low hover:text-normal disabled:opacity-40 disabled:cursor-not-allowed"
        aria-label={tooltip}
        onClick={execute}
        disabled={!enabled}
      >
        <IdeIcon
          editorType={ctx.editorType}
          className={
            enabled
              ? 'size-icon-base opacity-60 hover:opacity-100 transition-opacity'
              : 'size-icon-base'
          }
        />
      </button>
    );
  }

  return (
    <CopyButton
      onCopy={execute}
      disabled={!enabled}
      iconSize="size-icon-base"
      icon={CopyIcon}
    />
  );
}

/**
 * Per-icon classes for actions whose navbar glyph needs a state cue. Ports the
 * dev-server spin/color logic from the removed ContextBar (D-001): the glyph is
 * swapped to a spinner during transitions and must animate, and turns red while
 * the server is running.
 */
function getNavbarIconClassName(
  action: ActionDefinition,
  ctx: ActionVisibilityContext
): string | undefined {
  if (action.id === 'toggle-dev-server') {
    if (
      ctx.devServerState === 'starting' ||
      ctx.devServerState === 'stopping'
    ) {
      return 'animate-spin';
    }
    if (ctx.devServerState === 'running') {
      return 'text-error';
    }
  }
  return undefined;
}

export function toNavbarSectionItems(
  items: readonly ActionNavbarItem[],
  ctx: ActionVisibilityContext,
  onExecuteAction: (action: ActionDefinition) => void
): NavbarSectionItem[] {
  return items.reduce<NavbarSectionItem[]>((result, item) => {
    if (isNavbarDivider(item)) {
      result.push({ type: 'divider' });
      return result;
    }

    const icon = getActionIcon(item, ctx);
    const enabled = isActionEnabled(item, ctx);
    const tooltip = getActionTooltip(item, ctx);

    if (isSpecialIcon(icon)) {
      result.push({
        type: 'action',
        id: item.id,
        // CopyButton renders its own tooltip; avoid double-wrapping it.
        tooltip: icon === 'copy-icon' ? undefined : tooltip,
        shortcut: item.shortcut,
        disabled: !enabled,
        customContent: buildSpecialContent(icon, ctx, enabled, tooltip, () =>
          onExecuteAction(item)
        ),
      });
      return result;
    }

    result.push({
      type: 'action',
      id: item.id,
      icon,
      iconClassName: getNavbarIconClassName(item, ctx),
      isActive: isActionActive(item, ctx),
      tooltip,
      shortcut: item.shortcut,
      disabled: !enabled,
      onClick: () => onExecuteAction(item),
    });
    return result;
  }, []);
}
