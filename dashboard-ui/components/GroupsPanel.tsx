'use client';

import { useState } from 'react';
import type { TunnelGroup, TunnelGroupMember } from '@/lib/types';
import { relativeTime } from '@/lib/api';
import { Panel } from './Panel';

interface GroupsPanelProps {
  groups: TunnelGroup[];
}

/**
 * Operator view of every active load-balancing group across regions.
 *
 * Renders one card per group with:
 *  - name + protocol + label (subdomain or port) + region.
 *  - `healthy/total` member count, plus a horizontal bar showing the
 *    dispatch share each member has taken since registration.
 *  - Click-to-expand reveals the per-member breakdown (who served how
 *    many requests, current health bit, consecutive failures).
 *
 * Designed to auto-hide when the fleet has no groups: `Dashboard.tsx`
 * only renders the panel when `groups.length > 0`. Solo tunnels keep
 * showing in the Active Tunnels table; this panel is purely the
 * load-balancing surface.
 */
export function GroupsPanel({ groups }: GroupsPanelProps) {
  return (
    <Panel
      title={`Load-Balancing Groups (${groups.length})`}
      actions={<span style={{ fontSize: 11, color: 'var(--muted)' }}>auto-refresh 3s</span>}
    >
      <div
        style={{
          display: 'grid',
          gridTemplateColumns: 'repeat(auto-fill, minmax(360px, 1fr))',
          gap: 14,
          padding: 14,
        }}
      >
        {groups.map((g) => (
          <GroupCard key={`${g.region_id}/${g.protocol}/${g.label}`} group={g} />
        ))}
      </div>
    </Panel>
  );
}

function GroupCard({ group }: { group: TunnelGroup }) {
  const [expanded, setExpanded] = useState(false);

  // Color code on the healthy/total ratio.
  const allHealthy = group.unhealthy_count === 0;
  const someUnhealthy = group.unhealthy_count > 0 && group.healthy_count > 0;
  const allDown = group.healthy_count === 0;

  const healthColor = allDown
    ? '#ff8b6b'
    : someUnhealthy
      ? '#ffb86b'
      : 'var(--accent)';

  // Bar segments — proportion of dispatches per member.
  const totalDispatches = group.members.reduce((s, m) => s + m.request_count, 0);
  const segments = group.members.map((m) => ({
    pct: totalDispatches > 0 ? (m.request_count / totalDispatches) * 100 : 0,
    healthy: m.healthy,
    tunnel_id: m.tunnel_id,
  }));

  return (
    <div
      style={{
        border: '1px solid var(--border)',
        borderRadius: 8,
        background: 'var(--surface2)',
        overflow: 'hidden',
      }}
    >
      {/* Header */}
      <button
        onClick={() => setExpanded((e) => !e)}
        style={{
          width: '100%',
          padding: '12px 14px',
          display: 'flex',
          alignItems: 'center',
          gap: 10,
          background: 'transparent',
          border: 'none',
          borderBottom: expanded ? '1px solid var(--border)' : 'none',
          cursor: 'pointer',
          textAlign: 'left',
        }}
        title={expanded ? 'Hide members' : 'Show members'}
      >
        <span
          style={{
            fontFamily: 'var(--mono)',
            fontSize: 11,
            color: 'var(--accent)',
            textTransform: 'uppercase',
            letterSpacing: '0.04em',
            fontWeight: 700,
            background: 'var(--accent-dim)',
            padding: '2px 6px',
            borderRadius: 4,
          }}
        >
          {group.protocol.toUpperCase()}
        </span>
        <span style={{ flex: 1, fontFamily: 'var(--mono)', fontSize: 13, color: 'var(--text)' }}>
          {group.name}
          <span style={{ color: 'var(--muted)', marginLeft: 6 }}>·</span>
          <span style={{ color: 'var(--muted)', marginLeft: 6 }}>{group.label}</span>
        </span>
        <span
          style={{
            fontFamily: 'var(--mono)',
            fontSize: 11,
            color: 'var(--muted)',
            background: 'var(--surface)',
            padding: '1px 5px',
            borderRadius: 3,
          }}
          title="Region"
        >
          {group.region_id}
        </span>
        <span
          style={{
            fontWeight: 600,
            fontSize: 11,
            padding: '2px 8px',
            borderRadius: 10,
            color: healthColor,
            background: 'rgba(255,255,255,0.04)',
            border: `1px solid ${healthColor}40`,
          }}
          title={
            allHealthy
              ? 'All members healthy and receiving traffic'
              : allDown
                ? 'Group is offline — all members unhealthy. Public traffic returns 502.'
                : `${group.unhealthy_count} of ${group.member_count} members are unhealthy and excluded from dispatch`
          }
        >
          {group.healthy_count}/{group.member_count}
        </span>
        <span style={{ color: 'var(--muted)', fontSize: 11, marginLeft: 4 }}>
          {expanded ? '▾' : '▸'}
        </span>
      </button>

      {/* Dispatch share bar — visual ratio of who's been serving */}
      <div style={{ padding: '0 14px 12px' }}>
        <div
          style={{
            display: 'flex',
            height: 6,
            borderRadius: 3,
            overflow: 'hidden',
            background: 'rgba(255,255,255,0.04)',
            border: '1px solid var(--border)',
          }}
          title={
            totalDispatches > 0
              ? `${totalDispatches.toLocaleString()} total dispatches across ${group.member_count} member(s)`
              : 'No dispatches yet'
          }
        >
          {segments.map((s, i) => (
            <div
              key={s.tunnel_id}
              style={{
                width: `${Math.max(s.pct, totalDispatches > 0 ? 1 : 0)}%`,
                background: s.healthy ? 'var(--accent)' : '#ff8b6b',
                opacity: 0.6 + 0.4 * (1 - i / Math.max(segments.length - 1, 1)),
                transition: 'width 0.25s',
              }}
            />
          ))}
          {totalDispatches === 0 && (
            <div
              style={{
                fontSize: 9,
                color: 'var(--muted)',
                padding: '0 6px',
                lineHeight: '6px',
              }}
            >
              awaiting traffic
            </div>
          )}
        </div>
        <div
          style={{
            display: 'flex',
            justifyContent: 'space-between',
            marginTop: 6,
            fontSize: 10,
            color: 'var(--muted)',
          }}
        >
          <span>{totalDispatches.toLocaleString()} dispatches</span>
          {group.total_health_failures > 0 && (
            <span title="Cumulative TunnelUnhealthy frames received">
              {group.total_health_failures.toLocaleString()} failure
              {group.total_health_failures === 1 ? '' : 's'}
            </span>
          )}
        </div>
      </div>

      {/* Per-member breakdown (expanded) */}
      {expanded && (
        <div style={{ padding: '0 14px 14px', display: 'flex', flexDirection: 'column', gap: 8 }}>
          {group.members.map((m) => (
            <MemberRow key={m.tunnel_id} member={m} totalDispatches={totalDispatches} />
          ))}
        </div>
      )}
    </div>
  );
}

function MemberRow({
  member,
  totalDispatches,
}: {
  member: TunnelGroupMember;
  totalDispatches: number;
}) {
  const share =
    totalDispatches > 0 ? Math.round((member.request_count / totalDispatches) * 100) : 0;
  return (
    <div
      style={{
        display: 'grid',
        gridTemplateColumns: 'auto 1fr auto auto',
        gap: 10,
        alignItems: 'center',
        padding: '8px 10px',
        background: 'var(--surface)',
        border: '1px solid var(--border)',
        borderRadius: 6,
        fontSize: 11,
        fontFamily: 'var(--mono)',
      }}
    >
      <span
        style={{
          fontSize: 10,
          fontWeight: 700,
          padding: '1px 6px',
          borderRadius: 8,
          color: member.healthy ? 'var(--accent)' : '#ff8b6b',
          background: member.healthy ? 'var(--accent-dim)' : 'rgba(255, 139, 107, 0.15)',
        }}
        title={
          member.healthy
            ? 'Receiving dispatched connections'
            : `Excluded from dispatch — ${member.consecutive_failures} consecutive probe failures`
        }
      >
        {member.healthy ? 'HEALTHY' : 'UNHEALTHY'}
      </span>
      <span style={{ color: 'var(--muted)', overflow: 'hidden', textOverflow: 'ellipsis' }}>
        <span title={member.tunnel_id}>{member.tunnel_id.slice(0, 8)}</span>
        <span style={{ marginLeft: 6 }}>{member.client_addr || '—'}</span>
        {member.health_check_kind && (
          <span
            style={{
              marginLeft: 8,
              padding: '0 4px',
              borderRadius: 3,
              background: 'var(--surface2)',
              fontSize: 10,
            }}
            title={`Probe: ${member.health_check_kind}`}
          >
            probe:{member.health_check_kind}
          </span>
        )}
        {member.has_alert_webhook && (
          <span
            style={{ marginLeft: 6, fontSize: 11 }}
            title="Per-tenant alert webhook configured — fires on group 0/N transitions"
          >
            🔔
          </span>
        )}
      </span>
      <span style={{ color: 'var(--text)' }} title={`${member.request_count} dispatches`}>
        {member.request_count.toLocaleString()} ({share}%)
      </span>
      <span style={{ color: 'var(--muted)' }} title={member.connected_since}>
        {relativeTime(member.connected_since)}
      </span>
    </div>
  );
}
