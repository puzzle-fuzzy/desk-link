export class RateLimiter {
  private readonly buckets = new Map<string, { expiresAt: number; count: number }>();
  private sweep = this.buckets.entries();

  constructor(private readonly maximumKeys = 16_384) {
    if (!Number.isSafeInteger(maximumKeys) || maximumKeys < 1) throw new RangeError("invalid rate limiter capacity");
  }

  get size(): number { return this.buckets.size; }

  allow(key: string, limit: number, windowSeconds: number, now: number): boolean {
    this.prune(now);
    const current = this.buckets.get(key);
    if (!current || now >= current.expiresAt) {
      // Evicting a live bucket would reset its exhausted quota. At capacity,
      // reject new identities until bounded expiry maintenance reclaims room.
      if (!current && this.buckets.size >= this.maximumKeys) return false;
      this.buckets.set(key, { expiresAt: now + windowSeconds, count: 1 });
      return true;
    }
    if (current.count >= limit) return false;
    current.count += 1;
    return true;
  }

  private prune(now: number): void {
    for (let step = 0; step < 16; step++) {
      const entry = this.sweep.next();
      if (entry.done) {
        this.sweep = this.buckets.entries();
        if (this.buckets.size === 0) break;
        continue;
      }
      const [key, bucket] = entry.value;
      if (now >= bucket.expiresAt) this.buckets.delete(key);
    }
  }
}
