import { Drawer, DrawerClose, DrawerContent, DrawerDescription, DrawerTitle, DrawerBackdrop } from "@ui-sdk/react/drawer";
import { Tabs, TabsList, TabsPanel, TabsTab } from "@ui-sdk/react/tabs";
import { findingLabel, isProblem, placeOf, problemCount } from "../model/findings";
import { idForIssuePath } from "../model/hierarchy";
import { controller } from "../state/controller";
import { store, useAppState } from "../state/store";
import { formatNumber, reasonLabel } from "./format";
import { Icon } from "./icons";

function goTo(path: string) {
  const h = store.get().hierarchy;
  const id = h ? idForIssuePath(h, path) : `file:${path}`;
  store.set({ issuesOpen: false });
  controller.select(id, { fly: true });
}

function goToNode(id: string) {
  store.set({ issuesOpen: false });
  controller.select(id, { fly: true });
}


export function IssuesDrawer() {
  const open = useAppState((s) => s.issuesOpen);
  const issues = useAppState((s) => s.issues);
  const found = useAppState((s) => s.findings);
  const hierarchy = useAppState((s) => s.hierarchy);
  const unresolved = issues?.unresolved ?? [];
  const skipped = issues?.skipped ?? [];
  const diagnostics = issues?.diagnostics ?? [];
  const byReason = Object.entries(issues?.by_reason ?? {}).sort((a, b) => b[1] - a[1]);
  const findings = found?.findings ?? [];
  // The whole repo's count (the list may be cut at the request's limit).
  const problemFindings = found ? problemCount(found) : 0;

  return (
    <Drawer open={open} onOpenChange={(next) => store.set({ issuesOpen: next })} side="right" modal="trap-focus" closeOnOutsidePress>
      <DrawerBackdrop className="drawer-backdrop" />
      <DrawerContent className="drawer">
        <header className="drawer__header">
          <DrawerTitle className="drawer__title">Problemas</DrawerTitle>
          <DrawerDescription className="drawer__description">
            Lo que huele mal en la arquitectura y lo que cartograph no pudo conectar o leer. Elige uno para verlo en el plano.
          </DrawerDescription>
          <DrawerClose className="icon-btn drawer__close" aria-label="Cerrar problemas">
            <Icon name="close" />
          </DrawerClose>
        </header>
        {byReason.length > 0 && (
          <ul className="reason-chips" aria-label="Sin resolver por motivo">
            {byReason.map(([reason, n]) => (
              <li key={reason}>
                <span>{reasonLabel(reason)}</span> <strong>{formatNumber(n)}</strong>
              </li>
            ))}
          </ul>
        )}
        <Tabs className="tabs drawer__tabs" defaultValue={findings.length > 0 ? "findings" : "unresolved"}>
          <TabsList className="tabs__list" aria-label="Tipos de problema">
            <TabsTab className="tabs__tab" value="findings">
              Hallazgos <span className="tabs__count" data-alert={problemFindings ? "" : undefined}>{formatNumber(problemFindings)}</span>
            </TabsTab>
            <TabsTab className="tabs__tab" value="unresolved">
              Sin resolver <span className="tabs__count" data-alert={unresolved.length ? "" : undefined}>{formatNumber(issues?.total.unresolved ?? 0)}</span>
            </TabsTab>
            <TabsTab className="tabs__tab" value="diagnostics">
              Diagnósticos <span className="tabs__count">{diagnostics.length}</span>
            </TabsTab>
            <TabsTab className="tabs__tab" value="skipped">
              Omitidos <span className="tabs__count">{formatNumber(issues?.total.skipped ?? 0)}</span>
            </TabsTab>
          </TabsList>
          <TabsPanel className="tabs__panel drawer__panel" value="findings">
            {found?.config.error && <p className="note alert small">cartograph.toml no se pudo usar: {found.config.error}</p>}
            {findings.length === 0 ? (
              <p className="detail__empty">Sin hallazgos: no hay ciclos, reglas rotas ni código muerto a la vista.</p>
            ) : (
              <ul className="issue-list">
                {findings.map((f) => {
                  const first = f.nodes[0];
                  const more = f.nodes.length - 1 + (f.nodes_more ?? 0);
                  const body = (
                    <>
                      <span className="issue__head">
                        <span aria-hidden="true" className="issue__mark">
                          {isProblem(f) ? "◆" : "◇"}
                        </span>
                        <span>{findingLabel(f)}</span>
                      </span>
                      {first && (
                        <span className="issue__where">
                          {placeOf(first)}
                          {more > 0 ? ` y ${formatNumber(more)} más` : ""}
                        </span>
                      )}
                      {/* The proof as an import (path:line → path), the same in any language. */}
                      {f.edges[0] && (
                        <span className="issue__detail">
                          {placeOf(f.edges[0].src)}
                          {f.edges[0].line ? `:${f.edges[0].line}` : ""} → {placeOf(f.edges[0].dst)}
                        </span>
                      )}
                    </>
                  );
                  return (
                    <li key={f.id}>
                      {first ? (
                        <button type="button" className="issue issue--button" onClick={() => goToNode(first)}>
                          {body}
                        </button>
                      ) : (
                        <div className="issue">{body}</div>
                      )}
                    </li>
                  );
                })}
              </ul>
            )}
            {found?.truncated && <p className="detail__note">Se muestran los primeros {formatNumber(findings.length)}.</p>}
          </TabsPanel>
          <TabsPanel className="tabs__panel drawer__panel" value="unresolved">
            {unresolved.length === 0 ? (
              <p className="detail__empty">Todos los imports se resolvieron.</p>
            ) : (
              <ul className="issue-list">
                {unresolved.map((u) => (
                  <li key={`${u.file}:${u.line}:${u.specifier}`}>
                    <button type="button" className="issue issue--button" onClick={() => goTo(u.file)}>
                      <span className="issue__head">
                        <span aria-hidden="true" className="issue__mark">◆</span>
                        <code>{u.specifier}</code>
                      </span>
                      <span className="issue__where">
                        {u.file}, línea {u.line}
                      </span>
                      <span className="issue__reason">{reasonLabel(u.reason)}</span>
                      <span className="issue__detail">{u.detail}</span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {issues?.truncated && <p className="detail__note">Se muestran los primeros {formatNumber(unresolved.length)}.</p>}
          </TabsPanel>
          <TabsPanel className="tabs__panel drawer__panel" value="diagnostics">
            {diagnostics.length === 0 ? (
              <p className="detail__empty">Sin diagnósticos.</p>
            ) : (
              <ul className="issue-list">
                {diagnostics.map((d) => (
                  <li key={`${d.path}:${d.detail}`}>
                    {hierarchy?.nodes.has(idForIssuePath(hierarchy, d.path)) ? (
                      <button type="button" className="issue issue--button" onClick={() => goTo(d.path)}>
                        <span className="issue__where">{d.path}</span>
                        <span className="issue__detail">{d.detail}</span>
                      </button>
                    ) : (
                      <div className="issue">
                        <span className="issue__where">{d.path}</span>
                        <span className="issue__detail">{d.detail}</span>
                      </div>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </TabsPanel>
          <TabsPanel className="tabs__panel drawer__panel" value="skipped">
            {skipped.length === 0 ? (
              <p className="detail__empty">No se omitió ningún archivo.</p>
            ) : (
              <ul className="issue-list">
                {skipped.map((s) => {
                  const parent = s.path.includes("/") ? s.path.slice(0, s.path.lastIndexOf("/")) : "";
                  const target = parent && hierarchy?.nodes.has(`dir:${parent}`) ? parent : null;
                  return (
                    <li key={s.path}>
                      {target ? (
                        <button type="button" className="issue issue--button" onClick={() => goTo(target)}>
                          <span className="issue__where">{s.path}</span>
                          <span className="issue__reason">{reasonLabel(s.reason)}</span>
                          <span className="issue__detail">{s.detail}</span>
                        </button>
                      ) : (
                        <div className="issue">
                          <span className="issue__where">{s.path}</span>
                          <span className="issue__reason">{reasonLabel(s.reason)}</span>
                          <span className="issue__detail">{s.detail}</span>
                        </div>
                      )}
                    </li>
                  );
                })}
              </ul>
            )}
          </TabsPanel>
        </Tabs>
      </DrawerContent>
    </Drawer>
  );
}
