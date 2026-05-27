import { useCallback, useEffect, useLayoutEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { useVirtualizer } from '@tanstack/react-virtual';
import { WarningCircleIcon } from '@phosphor-icons/react/dist/ssr';
import RawLogText from '@/shared/components/RawLogText';
import type { PatchType } from 'shared/types';

export type LogEntry = Extract<
  PatchType,
  { type: 'STDOUT' } | { type: 'STDERR' }
>;

export interface VirtualizedProcessLogsProps {
  logs: LogEntry[];
  error: string | null;
  searchQuery: string;
  matchIndices: number[];
  currentMatchIndex: number;
}

const ESTIMATED_LOG_ROW_HEIGHT = 28;
const LOG_OVERSCAN = 12;
const NEAR_BOTTOM_THRESHOLD_PX = 24;
const MATCH_SCROLL_PAUSE_MS = 500;

export function VirtualizedProcessLogs({
  logs,
  error,
  searchQuery,
  matchIndices,
  currentMatchIndex,
}: VirtualizedProcessLogsProps) {
  const { t } = useTranslation('tasks');
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const isAtBottomRef = useRef(true);
  const prevLogsRef = useRef<LogEntry[] | null>(null);
  const prevLogsLengthRef = useRef(0);
  const prevMatchTargetRef = useRef<string | null>(null);
  const lastScrollTopRef = useRef(0);
  const scrollFrameRef = useRef<number | null>(null);
  const isAutoScrollingRef = useRef(false);
  const isMatchScrollInProgressRef = useRef(false);
  const autoScrollReleaseTimerRef = useRef<ReturnType<
    typeof setTimeout
  > | null>(null);
  const matchScrollReleaseTimerRef = useRef<ReturnType<
    typeof setTimeout
  > | null>(null);

  const virtualizer = useVirtualizer({
    count: logs.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ESTIMATED_LOG_ROW_HEIGHT,
    overscan: LOG_OVERSCAN,
    getItemKey: (index) => `log-${index}`,
  });

  const totalSize = virtualizer.getTotalSize();

  const updateBottomState = useCallback(() => {
    const el = scrollRef.current;
    if (!el) {
      isAtBottomRef.current = true;
      return;
    }
    isAtBottomRef.current =
      el.scrollHeight - el.scrollTop - el.clientHeight <=
      NEAR_BOTTOM_THRESHOLD_PX;
  }, []);

  const scheduleScrollToIndex = useCallback(
    (
      index: number,
      options: { align: 'start' | 'center' | 'end'; behavior: ScrollBehavior }
    ) => {
      if (scrollFrameRef.current !== null) {
        cancelAnimationFrame(scrollFrameRef.current);
      }
      if (autoScrollReleaseTimerRef.current !== null) {
        clearTimeout(autoScrollReleaseTimerRef.current);
      }
      isAutoScrollingRef.current = true;

      scrollFrameRef.current = requestAnimationFrame(() => {
        scrollFrameRef.current = null;
        virtualizer.scrollToIndex(index, options);
        autoScrollReleaseTimerRef.current = setTimeout(() => {
          isAutoScrollingRef.current = false;
          updateBottomState();
        }, 150);
      });
    },
    [updateBottomState, virtualizer]
  );

  useEffect(() => {
    return () => {
      if (scrollFrameRef.current !== null) {
        cancelAnimationFrame(scrollFrameRef.current);
      }
      if (autoScrollReleaseTimerRef.current !== null) {
        clearTimeout(autoScrollReleaseTimerRef.current);
      }
      if (matchScrollReleaseTimerRef.current !== null) {
        clearTimeout(matchScrollReleaseTimerRef.current);
      }
    };
  }, []);

  useLayoutEffect(() => {
    const previousLogs = prevLogsRef.current;
    const previousLength = prevLogsLengthRef.current;
    prevLogsRef.current = logs;
    prevLogsLengthRef.current = logs.length;

    if (logs.length === 0) {
      isAtBottomRef.current = true;
      return;
    }

    const isInitialLoad = previousLength === 0;
    const appendedLogs = logs.length > previousLength;
    const replacedLogs =
      previousLogs !== null &&
      previousLogs !== logs &&
      logs.length <= previousLength;
    if (
      !isMatchScrollInProgressRef.current &&
      (isInitialLoad ||
        ((appendedLogs || replacedLogs) && isAtBottomRef.current))
    ) {
      scheduleScrollToIndex(logs.length - 1, {
        align: 'end',
        behavior: 'auto',
      });
    }
  }, [logs, scheduleScrollToIndex]);

  useLayoutEffect(() => {
    if (
      logs.length > 0 &&
      !isMatchScrollInProgressRef.current &&
      (isAtBottomRef.current || isAutoScrollingRef.current)
    ) {
      scheduleScrollToIndex(logs.length - 1, {
        align: 'end',
        behavior: 'auto',
      });
    }
  }, [logs.length, scheduleScrollToIndex, totalSize]);

  // Scroll to current match when it changes
  useLayoutEffect(() => {
    if (matchIndices.length === 0 || currentMatchIndex < 0) {
      prevMatchTargetRef.current = null;
      isMatchScrollInProgressRef.current = false;
      if (matchScrollReleaseTimerRef.current !== null) {
        clearTimeout(matchScrollReleaseTimerRef.current);
        matchScrollReleaseTimerRef.current = null;
      }
      return;
    }

    const logIndex = matchIndices[currentMatchIndex];
    if (logIndex === undefined) {
      prevMatchTargetRef.current = null;
      isMatchScrollInProgressRef.current = false;
      if (matchScrollReleaseTimerRef.current !== null) {
        clearTimeout(matchScrollReleaseTimerRef.current);
        matchScrollReleaseTimerRef.current = null;
      }
      return;
    }

    const target = `${searchQuery}:${logIndex}`;
    if (target === prevMatchTargetRef.current) {
      return;
    }

    isMatchScrollInProgressRef.current = true;
    if (matchScrollReleaseTimerRef.current !== null) {
      clearTimeout(matchScrollReleaseTimerRef.current);
    }
    scheduleScrollToIndex(logIndex, {
      align: 'center',
      behavior: 'smooth',
    });
    matchScrollReleaseTimerRef.current = setTimeout(() => {
      isMatchScrollInProgressRef.current = false;
      matchScrollReleaseTimerRef.current = null;
      updateBottomState();
    }, MATCH_SCROLL_PAUSE_MS);
    prevMatchTargetRef.current = target;
  }, [
    currentMatchIndex,
    matchIndices,
    scheduleScrollToIndex,
    searchQuery,
    updateBottomState,
  ]);

  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    const scrollTop = el?.scrollTop ?? 0;
    const isScrollingUp = scrollTop < lastScrollTopRef.current;
    lastScrollTopRef.current = scrollTop;

    if (isAutoScrollingRef.current && !isScrollingUp) {
      return;
    }
    if (isAutoScrollingRef.current) {
      isAutoScrollingRef.current = false;
      isMatchScrollInProgressRef.current = false;
      if (autoScrollReleaseTimerRef.current !== null) {
        clearTimeout(autoScrollReleaseTimerRef.current);
        autoScrollReleaseTimerRef.current = null;
      }
      if (matchScrollReleaseTimerRef.current !== null) {
        clearTimeout(matchScrollReleaseTimerRef.current);
        matchScrollReleaseTimerRef.current = null;
      }
    }
    updateBottomState();
  }, [updateBottomState]);

  const handleUserScrollIntent = useCallback(() => {
    isAutoScrollingRef.current = false;
    isMatchScrollInProgressRef.current = false;
    if (autoScrollReleaseTimerRef.current !== null) {
      clearTimeout(autoScrollReleaseTimerRef.current);
      autoScrollReleaseTimerRef.current = null;
    }
    if (matchScrollReleaseTimerRef.current !== null) {
      clearTimeout(matchScrollReleaseTimerRef.current);
      matchScrollReleaseTimerRef.current = null;
    }
  }, []);

  if (logs.length === 0 && !error) {
    return (
      <div className="h-full flex items-center justify-center">
        <p className="text-center text-muted-foreground text-sm">
          {t('processes.noLogsAvailable')}
        </p>
      </div>
    );
  }

  if (error && logs.length === 0) {
    return (
      <div className="h-full flex items-center justify-center">
        <p className="text-center text-destructive text-sm">
          <WarningCircleIcon className="size-icon-base inline mr-2" />
          {error}
        </p>
      </div>
    );
  }

  const virtualItems = virtualizer.getVirtualItems();

  return (
    <div
      ref={scrollRef}
      className="h-full overflow-auto"
      onScroll={handleScroll}
      onWheel={handleUserScrollIntent}
      onTouchMove={handleUserScrollIntent}
    >
      <div className="relative w-full" style={{ height: `${totalSize}px` }}>
        {virtualItems.map((virtualItem) => {
          const log = logs[virtualItem.index];
          if (!log) {
            return null;
          }

          const isMatch = matchIndices.includes(virtualItem.index);
          const isCurrentMatch =
            matchIndices[currentMatchIndex] === virtualItem.index;

          return (
            <div
              key={virtualItem.key}
              ref={virtualizer.measureElement}
              data-index={virtualItem.index}
              className="absolute left-0 top-0 w-full"
              style={{
                transform: `translateY(${virtualItem.start}px)`,
              }}
            >
              <RawLogText
                content={log.content}
                channel={log.type === 'STDERR' ? 'stderr' : 'stdout'}
                className="text-sm px-4 py-1"
                linkifyUrls
                searchQuery={isMatch ? searchQuery : undefined}
                isCurrentMatch={isCurrentMatch}
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}
