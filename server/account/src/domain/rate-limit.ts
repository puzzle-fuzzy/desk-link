export class RateLimiter {
  private readonly buckets = new Map<string, { startedAt: number; count: number }>();

  allow(key: string, limit: number, windowSeconds: number, now: number): boolean {
    const current = this.buckets.get(key);
    if (!current || now - current.startedAt >= windowSeconds) {
      this.buckets.set(key, { startedAt: now, count: 1 });
      this.prune(now, windowSeconds);
      return true;
    }
    if (current.count >= limit) return false;
    current.count += 1;
    return true;
  }

  private prune(now: number, windowSeconds: number): void {
    if (this.buckets.size < 256) return;
    for (const [key, bucket] of this.buckets) {
      if (now - bucket.startedAt >= windowSeconds) this.buckets.delete(key);
    }
  }
}
