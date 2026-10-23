export class RateLimiter {
  private readonly buckets = new Map<string, { expiresAt: number; count: number }>();

  allow(key: string, limit: number, windowSeconds: number, now: number): boolean {
    const current = this.buckets.get(key);
    if (!current || now >= current.expiresAt) {
      this.buckets.set(key, { expiresAt: now + windowSeconds, count: 1 });
      this.prune(now);
      return true;
    }
    if (current.count >= limit) return false;
    current.count += 1;
    return true;
  }

  private prune(now: number): void {
    if (this.buckets.size < 256) return;
    for (const [key, bucket] of this.buckets) {
      if (now >= bucket.expiresAt) this.buckets.delete(key);
    }
  }
}
