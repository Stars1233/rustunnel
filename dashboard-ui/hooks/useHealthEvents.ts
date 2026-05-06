'use client';

import { useState, useCallback, useEffect } from 'react';
import type { ApiClient } from '@/lib/types';
import { useInterval } from './useInterval';

export interface HealthEvent {
  /** RFC-3339 UTC timestamp of the transition. */
  at: string;
  healthy: boolean;
  /** Free-form reason from `TunnelUnhealthy`, or `"recovered"` /
   *  `"registered"` for synthetic edges. */
  reason: string;
}

/**
 * Poll `GET /api/tunnels/:id/health-events` for the timeline of recent
 * health-state transitions. Returns oldest → newest. Cleared whenever
 * `tunnelId` changes. Returns 404 → empty list (the tunnel might have
 * just been removed, or is a P2P publisher with no health state).
 *
 * Polls every 5 s — slow enough that the dashboard isn't hammering this
 * endpoint, fast enough that "this backend went down" shows up
 * approximately when it happened.
 */
export function useHealthEvents(api: ApiClient | null, tunnelId: string | null) {
  const [events, setEvents] = useState<HealthEvent[]>([]);
  const [error, setError] = useState<string | null>(null);

  const poll = useCallback(async () => {
    if (!api || !tunnelId) {
      setEvents([]);
      setError(null);
      return;
    }
    try {
      const list = (await api.get(`/api/tunnels/${tunnelId}/health-events`)) as HealthEvent[];
      setEvents(list);
      setError(null);
    } catch (e) {
      const msg = (e as Error).message ?? '';
      // 404 → silent empty list (older edge, deleted tunnel, P2P).
      if (/\b404\b/.test(msg) || /not found/i.test(msg)) {
        setEvents([]);
        setError(null);
      } else {
        setError(msg);
      }
    }
  }, [api, tunnelId]);

  useEffect(() => {
    poll();
  }, [poll]);
  useInterval(poll, 5000);

  return { events, error, refresh: poll };
}
