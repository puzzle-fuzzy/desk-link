export type DeskLinkView = "controller" | "connection" | "devices" | "pairing" | "fixedAccess" | "settings" | "about";

export const DESKTOP_NAV_ITEMS: ReadonlyArray<{ id: DeskLinkView; label: string }> = [
  { id: "controller", label: "连接设备" },
  { id: "connection", label: "共享此设备" },
  { id: "devices", label: "已批准设备" },
  { id: "settings", label: "设置 / 诊断" },
];

export function navigationViewFor(view: DeskLinkView): DeskLinkView {
  if (view === "pairing") return "connection";
  if (view === "fixedAccess" || view === "about") return "settings";
  return view;
}

export type TabNavigationKey = "ArrowLeft" | "ArrowRight" | "Home" | "End";

export function nextTabIndex(
  currentIndex: number,
  tabCount: number,
  key: string,
): number | null {
  if (tabCount <= 0 || currentIndex < 0 || currentIndex >= tabCount) {
    return null;
  }
  switch (key as TabNavigationKey) {
    case "ArrowRight":
      return (currentIndex + 1) % tabCount;
    case "ArrowLeft":
      return (currentIndex - 1 + tabCount) % tabCount;
    case "Home":
      return 0;
    case "End":
      return tabCount - 1;
    default:
      return null;
  }
}

export type MenuNavigationKey = "ArrowDown" | "ArrowUp" | "Home" | "End";

export function nextMenuIndex(
  currentIndex: number,
  itemCount: number,
  key: string,
): number | null {
  if (itemCount <= 0 || currentIndex < -1 || currentIndex >= itemCount) {
    return null;
  }
  if (currentIndex === -1) {
    if (key === "ArrowDown" || key === "Home") return 0;
    if (key === "ArrowUp" || key === "End") return itemCount - 1;
    return null;
  }
  switch (key as MenuNavigationKey) {
    case "ArrowDown":
      return (currentIndex + 1) % itemCount;
    case "ArrowUp":
      return (currentIndex - 1 + itemCount) % itemCount;
    case "Home":
      return 0;
    case "End":
      return itemCount - 1;
    default:
      return null;
  }
}
