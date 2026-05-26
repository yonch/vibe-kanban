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
const LOG_OVERSCAN = 40;
const NEAR_BOTTOM_THRESHOLD_PX = 24;

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
  const prevLogsLengthRef = useRef(0);
  const prevCurrentMatchRef = useRef<number | undefined>(undefined);
  const scrollFrameRef = useRef<number | null>(null);

  const virtualizer = useVirtualizer({
    count: logs.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ESTIMATED_LOG_ROW_HEIGHT,
    overscan: LOG_OVERSCAN,
    getItemKey: (index) => `log-${index}`,
  });

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

      scrollFrameRef.current = requestAnimationFrame(() => {
        scrollFrameRef.current = null;
        virtualizer.scrollToIndex(index, options);
      });
    },
    [virtualizer]
  );

  useEffect(() => {
    return () => {
      if (scrollFrameRef.current !== null) {
        cancelAnimationFrame(scrollFrameRef.current);
      }
    };
  }, []);

  useLayoutEffect(() => {
    const previousLength = prevLogsLengthRef.current;
    prevLogsLengthRef.current = logs.length;

    if (logs.length === 0) {
      isAtBottomRef.current = true;
      return;
    }

    const isInitialLoad = previousLength === 0;
    const appendedLogs = logs.length > previousLength;
    if (isInitialLoad || (appendedLogs && isAtBottomRef.current)) {
      scheduleScrollToIndex(logs.length - 1, {
        align: 'end',
        behavior: 'auto',
      });
    }
  }, [logs.length, scheduleScrollToIndex]);

  // Scroll to current match when it changes
  useLayoutEffect(() => {
    if (
      matchIndices.length > 0 &&
      currentMatchIndex >= 0 &&
      currentMatchIndex !== prevCurrentMatchRef.current
    ) {
      const logIndex = matchIndices[currentMatchIndex];
      scheduleScrollToIndex(logIndex, {
        align: 'center',
        behavior: 'smooth',
      });
      prevCurrentMatchRef.current = currentMatchIndex;
    }
  }, [currentMatchIndex, matchIndices, scheduleScrollToIndex]);

  const handleScroll = useCallback(() => {
    updateBottomState();
  }, [updateBottomState]);

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
    >
      <div
        className="relative w-full"
        style={{ height: `${virtualizer.getTotalSize()}px` }}
      >
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
