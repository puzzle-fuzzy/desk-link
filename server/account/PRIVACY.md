# Account discovery timing and resource budget

`register` performs one Argon2id hash even for an existing address. `login`
performs one verify against the user's hash or a fixed public dummy hash with
the same parameters. Forgot-password and verification resend perform one dummy
verify on either branch; mail remains asynchronous through the durable outbox.
Responses share an absolute deadline chosen before looking up account state:
100 ms plus a cryptographically random 0–24 ms. Waiting does not hold an Argon2
slot. If real work exceeds the deadline, the response waits for that work;
this is a mitigation, not a constant-time cryptographic claim.

Unauthenticated resource limits apply before this extra work:

- Global 120 sensitive POST requests per minute, before reading the body,
  independent of client IP or account existence; per-peer and email limits remain.
- Request body 16 KiB and total body-read timeout 2 seconds. Bun also applies
  the body size limit at the socket boundary.
- Process-wide maximum two Argon2 operations and sixteen waiting operations;
  overflow is 429. With the current `m=65536` parameters, the two native hashing
  jobs have roughly 128 MiB of Argon2 working memory, plus runtime overhead.

The global HTTP budget is per running service instance. These limits do not
constitute a distributed rate limit across multiple deployments. Wrong passwords
and nonexistent accounts have the same 401 response. An authenticated correct
password may still lead to the intended verification-required or login result.

## Local synthetic evidence, 2026-10-26

Run with Bun 1.3.14:

```sh
npm exec --yes --package bun@1.3.14 -- bun run src/privacy-benchmark.ts
npm exec --yes --package bun@1.3.14 -- bun test src/privacy.test.ts
```

The benchmark invokes the actual service and Argon2 against an in-memory synthetic
database; it never calls a provider. Each pair has 100 samples per branch after
ten warmup pairs, with alternating branch order. Direct service invocation
intentionally bypasses HTTP admission; the separate regression tests exercise
the production admission, bounded worker queue, body size and timeout paths.

| Pair | Known p50 | Absent/new p50 | Orientation-independent rank AUC |
| --- | ---: | ---: | ---: |
| Wrong-password login / absent account | 117.005 ms | 117.219 ms | 0.5164 |
| Duplicate registration / new registration | 117.062 ms | 118.163 ms | 0.5794 |
| Known / absent forgot-password | 118.326 ms | 117.810 ms | 0.5664 |

Raw final results: `privacy-benchmark-evidence.json`. The initial equal-hash-only
run is retained in `privacy-before-floor-evidence.json`: register AUC 0.6518 and
forgot AUC 0.5981 exposed residual database-path cost, which motivated the response
deadline. Rank AUC 0.5 is no ordering signal; values above it in a finite sample
do not by themselves prove an exploitable classifier or prove privacy.

This is one local Windows run, without HTTP, a network adversary or deployment
load. It verifies removal of the large early-return/Argon2/provider timing gap;
it does not establish universal timing indistinguishability. Production-like
network measurements under idle and saturated budgets remain an explicit
acceptance step, particularly when CPU pressure makes work exceed the deadline.
