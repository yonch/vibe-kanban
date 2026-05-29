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
  execute: () => void
): ReactNode {
  if (iconType === 'ide-icon') {
    return (
      <button
        type="button"
        className="flex items-center justify-center rounded-sm text-low hover:text-normal disabled:opacity-40 disabled:cursor-not-allowed"
        onClick={execute}
        disabled={!enabled}
      >
        <IdeIcon editorType={ctx.editorType} className="size-icon-base" />
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
        customContent: buildSpecialContent(icon, ctx, enabled, () =>
          onExecuteAction(item)
        ),
      });
      return result;
    }

    result.push({
      type: 'action',
      id: item.id,
      icon,
      isActive: isActionActive(item, ctx),
      tooltip,
      shortcut: item.shortcut,
      disabled: !enabled,
      onClick: () => onExecuteAction(item),
    });
    return result;
  }, []);
}
