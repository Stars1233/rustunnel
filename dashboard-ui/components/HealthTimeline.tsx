'use client';

import type { HealthEvent } from '@/hooks/useHealthEvents';
import { relativeTime } from '@/lib/api';

interface HealthTimelineProps {
  events: HealthEvent[];
}

/**
 * Compact health-event timeline for a single tunnel.
 *
 * Renders a vertical list of recent state transitions, newest at the top.
 * Only used inside the per-tunnel detail surface — when there are no
 * events (the common case for solo tunnels that didn't opt into health
 * checks), returns `null` so the parent can hide the section entirely.
 */
export function HealthTimeline({ events }: HealthTimelineProps) {
  if (events.length === 0) return null;

  // Newest first.
  const ordered = [...events].reverse();

  return (
    <div style={{ padding: 14, display: 'flex', flexDirection: 'column', gap: 8 }}>
      {ordered.map((e, i) => (
        <div
          key={`${e.at}-${i}`}
          style={{
            display: 'flex',
            alignItems: 'flex-start',
            gap: 10,
            padding: '6px 0',
            borderBottom: i === ordered.length - 1 ? 'none' : '1px solid var(--border)',
            fontSize: 12,
          }}
        >
          <span
            style={{
              flex: '0 0 auto',
              width: 8,
              height: 8,
              borderRadius: '50%',
              marginTop: 6,
              background: e.healthy ? 'var(--accent)' : '#ff8b6b',
              boxShadow: e.healthy
                ? '0 0 0 2px rgba(80, 200, 120, 0.18)'
                : '0 0 0 2px rgba(255, 139, 107, 0.18)',
            }}
          />
          <div style={{ flex: 1, fontFamily: 'var(--mono)' }}>
            <div style={{ display: 'flex', gap: 8, alignItems: 'baseline' }}>
              <span
                style={{
                  fontSize: 10,
                  fontWeight: 700,
                  letterSpacing: '0.04em',
                  color: e.healthy ? 'var(--accent)' : '#ff8b6b',
                }}
              >
                {e.healthy ? 'HEALTHY' : 'UNHEALTHY'}
              </span>
              <span style={{ fontSize: 11, color: 'var(--muted)' }} title={e.at}>
                {relativeTime(e.at)}
              </span>
            </div>
            <div style={{ color: 'var(--muted)', fontSize: 11, marginTop: 2 }}>
              {e.reason}
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}
