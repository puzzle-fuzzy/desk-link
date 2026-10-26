# Diagnostic ingestion budgets

A signature proves that an installation ID and its public key belong together.
It is not an admission allowlist: anyone can generate another key. Ingestion
therefore applies independent socket-IP and global budgets before reading a body,
hashing, parsing or verifying a signature. Installation budgets are an additional
post-verification constraint.

The default source address is Bun's `server.requestIP(request).address`. Forwarded
headers are ignored. The shipped systemd unit explicitly trusts `127.0.0.1,::1`
because the shipped nginx snippet proxies to loopback and **overwrites**
`X-Real-IP` with `$remote_addr`. Only a socket matching that configured set can
supply one valid IP in `X-Real-IP`; missing, scoped, comma-separated or malformed
values are rejected. X-Forwarded-For is never used. Bind ingestion to loopback in
this deployment. Do not trust a public proxy address until its header-overwrite
behavior and network boundary have been verified. Loopback trust assumes local
processes are trusted; it is not authentication between mutually untrusted local
users. A direct deployment leaves `DESKLINK_DIAGNOSTICS_TRUSTED_PROXIES` unset.

## Default budgets

These are fixed one-minute windows for `POST /v1/batches`, per server process:

| Scope | Requests | Body bytes | Verified event attempts |
| --- | ---: | ---: | ---: |
| Global | 120 | 4 MiB | 6,000 |
| Socket / trusted-proxy client IP | 12 | 768 KiB | 1,200 |
| Verified installation | 12 | 768 KiB | 1,200 |

Before a body read, the server reserves one request, the full 64 KiB maximum
body size and one concurrent slot against both global and IP limits. It refunds
unused body bytes only after a complete bounded read; it does not trust the
Content-Length for billing. Invalid signatures still consume actual bytes and a
request. Interrupted, malformed or timed-out bodies retain their maximum-byte
reservation. Reads must exactly match a valid Content-Length, never copy beyond
that length, and have a 5-second deadline. Maximum concurrent reads are 16 global
and 4 per IP; all paths release their concurrent slot. Old-window leases cannot
refund credits into a newer window. Events are charged after signature validation
and before storage, including attempts later rejected by the durable quota.
Fixed windows permit a burst around an adjacent-window boundary.

IP state holds at most 2,048 keys and installation state at most 4,096 keys. A
new key checks/reclaims one expired oldest entry in O(1); there is no sweep.
Full active tables reject new keys, and entries with an in-flight read stay
pinned. Rotating keys cannot evict active counters and reset their budgets.
Health includes only key counts and total concurrent slots, never IPs or keys.
The health route is a liveness/metrics read and is outside ingestion budgets.

Global per-process limits can be overridden with positive integer environment
settings: `DESKLINK_DIAGNOSTICS_GLOBAL_REQUESTS_PER_MINUTE`,
`DESKLINK_DIAGNOSTICS_GLOBAL_BYTES_PER_MINUTE`, and
`DESKLINK_DIAGNOSTICS_GLOBAL_EVENTS_PER_MINUTE`. A byte budget must have room for
one complete 64 KiB reservation to admit a request. Multiple server processes
have separate rate budgets; the shipped service runs one process.

## Durable retained-content quota

SQLite maintains one persisted usage row and enforces, by default, all three:

- 1,000,000 retained events;
- 250,000 retained batch deduplication markers, including zero-new-event batches;
- 256 MiB of retained UTF-8 TEXT bytes across **all** batch and event columns,
  including replicated app metadata and payloads.

`DESKLINK_DIAGNOSTICS_MAX_STORED_EVENTS`, `..._MAX_STORED_BATCHES` and
`..._MAX_STORED_BYTES` override these positive integer limits. All writers sharing
a database must use the same configuration. Row caps also bound record/index
overhead, and app versions are limited to 96 characters. These are retained-data
quotas, not a claim that the physical SQLite/WAL files fit in 256 MiB. Indexes,
free pages and readers pinning WAL frames need filesystem capacity monitoring and
an operator-managed filesystem quota if a hard disk-space limit is required.

Insert/update/delete triggers atomically update actual UTF-8 byte and row counts.
Explicit `RAISE(ABORT)` rejects an over-budget statement even under outer
`INSERT OR IGNORE`; the receiving transaction then rolls back the entire batch,
its events and its counters. Duplicate rows do not get billed again. Authenticated
duplicate batches still receive 202 at capacity. A new over-budget batch receives
503 `storage_quota` without partial writes. Retention deletes return capacity.
Counters survive restart and are shared by connections. Rate-limit state resets
on process restart; retained-content quotas do not.

Existing databases have a one-time usage backfill in the same writer transaction
that installs triggers. Plan a maintenance window for a large legacy database.
An existing database above a newly lowered quota keeps its historical rows and
may drain them through retention; further growth is rejected. Do not edit or
drop the usage row/triggers independently of an intentional migration.

## Verification

Run `bun test` and `bun run typecheck` using Bun 1.3.14. Tests cover 10,000-key
IP/installation floods with bounded state, fresh signing keys sharing pre-body
budgets, actual loopback HTTP sockets and spoofed headers, pre-read rejection,
global/IP bytes and events, concurrent reads and deadlines, 200 fresh keys/IPs
stopping at a durable eight-row cap, all-or-nothing quota failures, UTF-8 metadata,
zero-new-event batch growth, full-quota deduplication, two file connections,
restart, legacy backfill and retention reclaim. These are local synthetic tests;
nginx/systemd deployment and distributed ingress load have not been exercised.
