# Diagnostic retention

Receipt time remains the retention clock: events and batch deduplication markers
with `received_at_unix_s < now - 14 days` are eligible for deletion. The exact
boundary is retained. Ingestion only commits its batch and events; maintenance
failure cannot roll back an otherwise valid receipt. An event write failure still
rolls back the entire batch, and retries remain deduplicated.

`events_by_received_at` and `batches_by_received_at` are created for new and
existing databases at startup. Startup index construction on a large existing
database needs a maintenance window; it is not an online migration claim.

Maintenance runs independently of requests. It performs at most 1,000 event
deletions per transaction and, after all expired events have drained, at most
1,000 batch deletions. Every event receives its batch's receipt timestamp, so
batch markers survive while any expired event remains. The scheduler yields for
one second between backlog slices and one minute when caught up. It retries
later on contention/failure; it never waits on a writer lock. Graceful SIGINT or
SIGTERM stops maintenance, drains the HTTP server, then closes SQLite.

`GET /health` includes a fixed-field `retention` object with maintenance state,
failure count, last slice time and deletion counts, backlog flag, database bytes,
WAL bytes and reusable database bytes. It contains no database paths or raw
errors. `starting` lasts until the first maintenance tick. Monitor `degraded`, a
sustained `more`, increasing slice time, WAL size and available filesystem space.
The per-slice limit bounds each deletion transaction, not total database size.
Catch-up capacity is at most 1,000 events/second (batch cleanup then follows);
ingress above that rate or a long outage needs more maintenance capacity or
ingress controls. The 14-day policy has eventual cleanup, with up to one idle
interval of normal lag and longer lag while draining a backlog.

SQLite reuses free pages. A PASSIVE WAL checkpoint follows each slice, outside
the deletion transaction; long-lived readers can prevent checkpoint completion.
PASSIVE does not truncate the physical WAL file, which can retain a reusable
high-water allocation.
Deleting rows does **not** immediately return main-file space to the OS. For OS
space reclamation, stop ingestion and maintenance, take and verify a backup,
provide enough temporary disk space, run an explicitly scheduled offline
`VACUUM`, then restart. This change does not run automatic full-file VACUUM or
claim an absolute disk cap. Filesystem free-space alerting remains an operator
responsibility.

## Local synthetic verification

Run with Bun 1.3.14 from this directory:

```powershell
npm exec --yes --package bun@1.3.14 -- bun install --frozen-lockfile
npm exec --yes --package bun@1.3.14 -- bun test
npm exec --yes --package bun@1.3.14 -- bun run typecheck
$env:DESKLINK_DIAGNOSTICS_BENCH_DIR = 'E:\desk-link\.worktrees\dl2-targets\dl2-services\diagnostics-bench'
npm exec --yes --package bun@1.3.14 -- bun run benchmark:retention
```

The benchmark creates fresh synthetic files under the chosen scratch directory,
closes them, and removes only its own generated files. It never reads service
configuration or opens a deployed database. It compares the prior unindexed
full-delete transaction with the indexed implementation, using 250,000 retained
events / 50,000 batches, 180 timed five-event receipts after warmup, and two
additional 250,000-event expired-history cycles. It emits populated-database
query plans, receipt call p50/p95/max, every purge transaction call distribution,
and main-file/WAL/reusable-byte high-water observations. Purge call wall time is
a conservative upper bound on the writer lock duration including commit;
checkpoint work is included in the production maintenance duration and excluded
from the benchmark's purge timing. Receipt call measurements exclude HTTP/signature
validation. The fixed-workload disk observations do not prove a production disk
cap, a real two-machine latency result or a long-duration production soak.

Measured on 2026-10-26 with Bun 1.3.14 / Windows / i7-13700KF;
full output is in `retention-benchmark-evidence.json`:

| Measurement | Prior full delete | Indexed bounded maintenance |
| --- | ---: | ---: |
| Five-event store receipt p95, 180 samples | 341.436 ms | 0.331 ms |
| Expired-history purge transaction, cycle 1 | 2727.607 ms, one transaction | p95 29.236 ms, max 112.478 ms, 301 slices |
| Expired-history purge transaction, cycle 2 | 2555.675 ms, one transaction | p95 34.205 ms, max 137.687 ms, 301 slices |
| Main-file high-water after cycle 2 | 337,580,032 bytes | 346,497,024 bytes |
| Reusable main-file pages after cycle 2 | 158,920,704 bytes | 163,401,728 bytes |
| Physical WAL high-water | 176,303,072 bytes | 7,815,672 bytes |

Both runs retained exactly 251,000 events / 50,200 batches, including 1,000
events / 200 batches from the receipt samples and warmup. Each expired-history
cycle removed all 250,000 events / 50,000 batches. The populated new query plans
use covering receipt-time indexes; prior plans scan both tables. The second
identical growth cycle increased each main file by only one 4 KiB page, showing
page reuse for this workload. Indexes add about 8.9 MB at this high-water. Purge
call maxima still include occasional SQLite commit/checkpoint or filesystem
stalls; the evidence does not promise a hard millisecond deadline. These are
one-run local measurements on a synthetic workload, not production percentiles.
