'use client';

import { useState, useCallback, useEffect } from 'react';
import type { TunnelGroup } from '@/lib/types';
import { useInterval } from './useInterval';
import type { RegionApi } from './useTunnels';

/**
 * Poll `/api/groups` on all supplied regional API clients in parallel.
 * Results are flat-merged across regions. Older edges (< 0.7.0) that
 * don't have the endpoint return 404 — we silently treat that as "no
 * groups in this region" so the dashboard works during a partial rollout.
 *
 * Mirrors `useTunnels` so the wiring in `Dashboard.tsx` stays uniform.
 */
export function useGroups(regionApis: RegionApi[], enabled: boolean) {
  const [groups, setGroups] = useState<TunnelGroup[]>([]);
  const [errors, setErrors] = useState<string[]>([]);

  const poll = useCallback(async () => {
    if (!enabled || regionApis.length === 0) return;

    const results = await Promise.allSettled(
      regionApis.map(({ api }) => api.get('/api/groups') as Promise<TunnelGroup[]>)
    );

    const all: TunnelGroup[] = [];
    const errs: string[] = [];
    results.forEach((r, i) => {
      if (r.status === 'fulfilled') {
        all.push(...r.value);
      } else {
        const msg = (r.reason as Error).message ?? '';
        // 404 from older regions is expected during a rollout; not an error.
        if (!/\b404\b/.test(msg) && !/not found/i.test(msg)) {
          errs.push(`${regionApis[i].regionId}: ${msg}`);
        }
      }
    });

    setGroups(all);
    setErrors(errs);
  }, [regionApis, enabled]);

  useEffect(() => {
    poll();
  }, [poll]);
  useInterval(poll, 3000);

  const error = errors.length > 0 ? errors[0] : null;

  return { groups, error, errors, refresh: poll };
}
