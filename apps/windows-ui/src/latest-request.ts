export class LatestRequest {
  private generation = 0;

  begin(): number {
    this.generation += 1;
    return this.generation;
  }

  isCurrent(request: number): boolean {
    return request === this.generation;
  }
}
