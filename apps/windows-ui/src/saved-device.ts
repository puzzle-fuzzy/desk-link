export function deviceIdsMatch(left: string, right: string): boolean {
  const leftDigits = left.replace(/\D/g, "");
  const rightDigits = right.replace(/\D/g, "");
  return leftDigits.length > 0 && leftDigits === rightDigits;
}

export function formatLastUsed(unixSeconds: number, nowUnixS = Math.floor(Date.now() / 1000)): string {
  const elapsed = Math.max(0, nowUnixS - unixSeconds);
  if (elapsed < 60) {
    return "刚刚";
  }
  if (elapsed < 3_600) {
    return `${Math.floor(elapsed / 60)} 分钟前`;
  }
  if (elapsed < 86_400) {
    return `${Math.floor(elapsed / 3_600)} 小时前`;
  }
  if (elapsed < 604_800) {
    return `${Math.floor(elapsed / 86_400)} 天前`;
  }
  return new Intl.DateTimeFormat("zh-CN", { month: "numeric", day: "numeric" }).format(
    new Date(unixSeconds * 1000),
  );
}
