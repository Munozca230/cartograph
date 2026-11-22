// The explorer: a left drawer (closed by default) with a virtualized ui-sdk Tree over the
// full hierarchy. Its open folders are the plan's open folders: opening or closing one here
// does the same on the plan. Clicking a row selects the node and, if it isn't on the plan,
// opens the folders above it. Files at the repo root sit in one group row.
import { Drawer, DrawerBackdrop, DrawerClose, DrawerContent, DrawerTitle } from "@ui-sdk/react/drawer";
import { Tree, TreeItem, TreeItemIndicator, TreeItemLabel, visibleTreeRows, type TreeItemData } from "@ui-sdk/react/tree";
import { useEffect, useMemo, useRef, useState } from "react";
import type { Hierarchy } from "../model/hierarchy";
import { controller } from "../state/controller";
import { store, useAppState } from "../state/store";
import { formatLoc, formatNumber } from "./format";
import { Icon } from "./icons";

const ROW = 26;
const ROOT_FILES = "group:root-files";

function toItems(h: Hierarchy): TreeItemData[] {
  const order = (ids: string[]) =>
    [...ids].sort((a, b) => {
      const ka = h.nodes.get(a)!;
      const kb = h.nodes.get(b)!;
      if ((ka.kind === "file") !== (kb.kind === "file")) return ka.kind === "file" ? 1 : -1;
      return ka.name.localeCompare(kb.name);
    });
  const build = (id: string): TreeItemData => {
    const n = h.nodes.get(id)!;
    return n.kind === "file" ? { value: id, label: n.name } : { value: id, label: n.name, children: order(n.children).map(build) };
  };
  const top = order(h.nodes.get(h.root)!.children);
  const dirs = top.filter((id) => h.nodes.get(id)!.kind !== "file").map(build);
  const files = top.filter((id) => h.nodes.get(id)!.kind === "file").map(build);
  if (files.length === 0) return dirs;
  return [...dirs, { value: ROOT_FILES, label: `${files.length} archivos en la raíz`, children: files }];
}

export function Explorer() {
  const open = useAppState((s) => s.explorerOpen);
  return (
    <Drawer open={open} onOpenChange={(next) => store.set({ explorerOpen: next })} side="left" modal="trap-focus" closeOnOutsidePress>
      <DrawerBackdrop className="drawer-backdrop light" />
      <DrawerContent className="drawer side">
        <header className="side-head">
          <DrawerTitle className="drawer__title">Explorador</DrawerTitle>
          <span className="small faint">líneas</span>
          <DrawerClose className="icon-btn" aria-label="Cerrar el explorador (Esc)">
            <Icon name="close" />
          </DrawerClose>
        </header>
        <ExplorerTree />
      </DrawerContent>
    </Drawer>
  );
}

function ExplorerTree() {
  const hierarchy = useAppState((s) => s.hierarchy);
  const openFolders = useAppState((s) => s.expanded);
  const selected = useAppState((s) => s.selected);
  const [rootOpen, setRootOpen] = useState(false);
  const items = useMemo(() => (hierarchy ? toItems(hierarchy) : []), [hierarchy]);
  const selectedRootFile = Boolean(selected && hierarchy?.nodes.get(selected)?.parent === hierarchy?.root && hierarchy?.nodes.get(selected)?.kind === "file");
  // Selecting a root file opens its group once; after that the user can close it again.
  const forceFor = selectedRootFile ? selected : null;
  const [forced, setForced] = useState<string | null>(null);
  if (forced !== forceFor) {
    setForced(forceFor);
    if (forceFor) setRootOpen(true);
  }
  const expanded = useMemo(() => (rootOpen ? [...openFolders, ROOT_FILES] : openFolders), [openFolders, rootOpen]);
  const rows = useMemo(() => visibleTreeRows(items, expanded), [items, expanded]);
  const scroller = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(600);

  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Keep the selected row in view when the selection comes from the plan or the palette.
  useEffect(() => {
    const el = scroller.current;
    if (!el || !selected) return;
    const index = rows.findIndex((r) => r.item.value === selected);
    if (index < 0) return;
    const top = index * ROW;
    if (top < el.scrollTop || top + ROW > el.scrollTop + el.clientHeight) {
      el.scrollTop = Math.max(0, top - el.clientHeight / 2);
      setScrollTop(el.scrollTop);
    }
  }, [selected, rows]);

  const onExpandedChange = (next: string[]) => {
    const now = new Set(expanded);
    const after = new Set(next);
    if (now.has(ROOT_FILES) !== after.has(ROOT_FILES)) setRootOpen(after.has(ROOT_FILES));
    const opened = next.find((id) => id !== ROOT_FILES && !now.has(id));
    const closed = [...now].find((id) => id !== ROOT_FILES && !after.has(id));
    if (opened) void controller.openFolder(opened);
    else if (closed) void controller.closeFolder(closed);
  };

  const start = Math.max(0, Math.floor(scrollTop / ROW) - 6);
  const end = Math.min(rows.length, Math.ceil((scrollTop + height) / ROW) + 6);

  return (
    <>
      <div className="tree-scroll" ref={scroller} onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}>
        {hierarchy && (
          <Tree
            aria-label="Archivos del repositorio"
            className="tree"
            items={items}
            expanded={expanded}
            onExpandedChange={onExpandedChange}
            value={selected && hierarchy.nodes.has(selected) ? selected : null}
            onValueChange={(value) => {
              if (typeof value !== "string") return;
              if (value === ROOT_FILES) setRootOpen((o) => !o);
              else controller.select(value, { fly: true });
            }}
            activation="manual"
            onHighlight={(value, { index }) => {
              controller.hover(value);
              const el = scroller.current;
              if (!el) return;
              const top = index * ROW;
              if (top < el.scrollTop) el.scrollTop = top;
              else if (top + ROW > el.scrollTop + el.clientHeight) el.scrollTop = top + ROW - el.clientHeight;
              setScrollTop(el.scrollTop);
            }}
            style={{ height: rows.length * ROW }}
          >
            {rows.slice(start, end).map(({ item, index, level }) => {
              const group = item.value === ROOT_FILES;
              const n = group ? null : hierarchy.nodes.get(item.value)!;
              const branch = Boolean(item.children);
              const loc = group ? (item.children ?? []).reduce((s, c) => s + (hierarchy.nodes.get(c.value)?.loc ?? 0), 0) : n!.loc;
              return (
                <TreeItem key={item.value} value={item.value} label={item.label} className="tree-item" style={{ top: index * ROW }}>
                  <TreeItemLabel
                    className={`tree-row${group ? " dim" : ""}`}
                    style={{ paddingLeft: 12 + (level - 1) * 16 }}
                    onPointerEnter={() => controller.hover(group ? null : item.value)}
                    onPointerLeave={() => controller.hover(null)}
                  >
                    {branch ? (
                      <TreeItemIndicator className="chev">
                        <svg viewBox="0 0 10 10" width="10" height="10" aria-hidden="true">
                          <path d="M3.5 2l3 3-3 3" />
                        </svg>
                      </TreeItemIndicator>
                    ) : (
                      <span className="chev" aria-hidden="true" />
                    )}
                    {n?.kind === "file" && <span className="ld" style={{ background: `var(--lang-${n.lang ?? "other"}, var(--lang-other))` }} aria-hidden="true" />}
                    <span className="name">{item.label}</span>
                    {n && n.unresolved > 0 && (
                      <span className="tree-alert" title={`${formatNumber(n.unresolved)} imports sin resolver`}>
                        <span aria-hidden="true">◆</span>
                        <span className="visually-hidden">{n.unresolved} sin resolver</span>
                      </span>
                    )}
                    <span className="num" aria-hidden="true">
                      {formatLoc(loc)}
                    </span>
                  </TreeItemLabel>
                </TreeItem>
              );
            })}
          </Tree>
        )}
      </div>
    </>
  );
}
