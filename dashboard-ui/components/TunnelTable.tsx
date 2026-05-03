'use client';

import { useState } from 'react';
import type { Tunnel } from '@/lib/types';
import { relativeTime, copyToClipboard } from '@/lib/api';
import { Badge } from './ui/Badge';

const PROTOCOLS = ['all', 'http', 'tcp', 'udp', 'p2p'] as const;

interface TunnelTableProps {
  tunnels: Tunnel[];
  selected: Tunnel | null;
  onSelect: (t: Tunnel | null) => void;
  onClose: (t: Tunnel) => void;
}

export function TunnelTable({ tunnels, selected, onSelect, onClose }: TunnelTableProps) {
  const [protocolFilter, setProtocolFilter] = useState<string>('all');

  const filtered = protocolFilter === 'all'
    ? tunnels
    : tunnels.filter((t) => t.protocol === protocolFilter);

  if (tunnels.length === 0) {
    return (
      <div
        style={{
          padding: '60px 20px',
          textAlign: 'center',
          color: 'var(--muted)',
          fontSize: 13,
        }}
      >
        <div style={{ fontSize: 32, marginBottom: 10 }}>⟳</div>
        No active tunnels — connect a client to get started.
      </div>
    );
  }

  return (
    <div style={{ overflowX: 'auto' }}>
      {/* Protocol filter */}
      <div style={{ padding: '8px 14px', display: 'flex', gap: 6, alignItems: 'center' }}>
        <span style={{ fontSize: 11, color: 'var(--muted)', marginRight: 4 }}>Filter:</span>
        {PROTOCOLS.map((p) => (
          <button
            key={p}
            onClick={() => setProtocolFilter(p)}
            style={{
              padding: '2px 8px',
              fontSize: 11,
              borderRadius: 4,
              border: '1px solid var(--border)',
              background: protocolFilter === p ? 'var(--accent-dim)' : 'transparent',
              color: protocolFilter === p ? 'var(--accent)' : 'var(--muted)',
              cursor: 'pointer',
              textTransform: 'uppercase',
              fontWeight: protocolFilter === p ? 600 : 400,
            }}
          >
            {p}
          </button>
        ))}
      </div>
      <table
        style={{
          width: '100%',
          borderCollapse: 'collapse',
          fontSize: 12,
          fontFamily: 'var(--mono)',
        }}
      >
        <thead>
          <tr style={{ borderBottom: '1px solid var(--border)', color: 'var(--muted)' }}>
            {['Protocol', 'Public URL', 'Group', 'Region', 'Client', 'Connected', 'Requests', ''].map((h) => (
              <th
                key={h}
                style={{
                  padding: '8px 14px',
                  textAlign: 'left',
                  fontWeight: 500,
                  fontSize: 11,
                  textTransform: 'uppercase',
                  letterSpacing: '0.05em',
                  whiteSpace: 'nowrap',
                }}
              >
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {filtered.map((t) => {
            const isSelected = selected?.tunnel_id === t.tunnel_id;
            return (
              <tr
                key={t.tunnel_id}
                onClick={() => onSelect(isSelected ? null : t)}
                style={{
                  borderBottom: '1px solid var(--border)',
                  cursor: 'pointer',
                  background: isSelected ? 'var(--accent-dim)' : 'transparent',
                  transition: 'background 0.1s',
                }}
                onMouseEnter={(e) => {
                  if (!isSelected) e.currentTarget.style.background = 'var(--surface2)';
                }}
                onMouseLeave={(e) => {
                  if (!isSelected) e.currentTarget.style.background = 'transparent';
                }}
              >
                <td style={{ padding: '10px 14px', display: 'flex', alignItems: 'center', gap: 6 }}>
                  <Badge
                    label={t.protocol.toUpperCase()}
                    color={t.protocol === 'http' ? 'var(--accent)' : 'var(--purple)'}
                  />
                  {t.protocol === 'p2p' && t.nat_type && (
                    <span
                      style={{
                        fontSize: 10,
                        color: 'var(--muted)',
                        background: 'var(--surface2)',
                        padding: '1px 4px',
                        borderRadius: 3,
                      }}
                      title={t.mapped_addrs?.join(', ') || ''}
                    >
                      {t.nat_type}
                    </span>
                  )}
                  {/*
                    Health pill — only shown for grouped members. Solo
                    tunnels are always healthy by definition; rendering the
                    pill on every row would just be noise. (TUNNEL-8 Phase 5)
                  */}
                  {t.group && (
                    <span
                      style={{
                        fontSize: 10,
                        fontWeight: 600,
                        padding: '1px 6px',
                        borderRadius: 8,
                        color: t.healthy ? 'var(--accent)' : '#ff8b6b',
                        background: t.healthy ? 'var(--accent-dim)' : 'rgba(255, 139, 107, 0.15)',
                      }}
                      title={
                        t.healthy
                          ? 'Receiving dispatched connections'
                          : `Excluded from dispatch — ${t.consecutive_failures} consecutive probe failures`
                      }
                    >
                      {t.healthy ? 'HEALTHY' : 'UNHEALTHY'}
                    </span>
                  )}
                </td>
                <td style={{ padding: '10px 14px', color: 'var(--accent)', maxWidth: 260 }}>
                  <span
                    style={{ cursor: 'pointer' }}
                    title={t.public_url}
                    onClick={(e) => {
                      e.stopPropagation();
                      copyToClipboard(t.public_url);
                    }}
                  >
                    {t.public_url}
                  </span>
                </td>
                <td style={{ padding: '10px 14px' }}>
                  {t.group ? (
                    <span
                      style={{
                        fontFamily: 'var(--mono)',
                        fontSize: 11,
                        color: 'var(--accent)',
                        background: 'var(--accent-dim)',
                        padding: '1px 5px',
                        borderRadius: 3,
                      }}
                      title={`Group "${t.group.name}" — ${t.group.healthy_count}/${t.group.member_count} healthy member(s) (key ${t.group.key_hash_short})`}
                    >
                      {t.group.name} · {t.group.healthy_count}/{t.group.member_count}
                    </span>
                  ) : (
                    <span style={{ color: 'var(--muted)' }}>—</span>
                  )}
                </td>
                <td style={{ padding: '10px 14px' }}>
                  <span
                    style={{
                      fontFamily: 'var(--mono)',
                      fontSize: 11,
                      color: 'var(--muted)',
                      background: 'var(--surface2)',
                      padding: '1px 5px',
                      borderRadius: 3,
                    }}
                  >
                    {t.region_id || '—'}
                  </span>
                </td>
                <td style={{ padding: '10px 14px', color: 'var(--muted)' }}>
                  {t.client_addr ?? '—'}
                </td>
                <td style={{ padding: '10px 14px', color: 'var(--muted)', whiteSpace: 'nowrap' }}>
                  {relativeTime(t.connected_since)}
                </td>
                <td style={{ padding: '10px 14px' }}>
                  {t.request_count > 0 ? (
                    <span style={{ color: 'var(--text)' }}>{t.request_count.toLocaleString()}</span>
                  ) : (
                    <span style={{ color: 'var(--muted)' }}>0</span>
                  )}
                </td>
                <td style={{ padding: '10px 14px' }}>
                  <button
                    className="danger"
                    style={{ padding: '3px 8px', fontSize: 11 }}
                    onClick={(e) => {
                      e.stopPropagation();
                      if (confirm(`Force close tunnel ${t.label}?`)) onClose(t);
                    }}
                  >
                    ✕ Close
                  </button>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
