// A small overview in the corner, shown only while the plan is bigger than the stage: the
// boxes and cards as shapes, and the part on screen as a brass frame. Click or drag on it to
// move there.
import { memo, useRef } from "react";
import type { PlanLayout } from "../model/plan";
import type { View } from "./PlanView";
import { minimapFrame, minimapToPlan } from "./planGeometry";

const MAX_W = 200;
const MAX_H = 132;

const Shapes = memo(function Shapes({ layout }: { layout: PlanLayout }) {
  return (
    <>
      {[...layout.boxes.values()].map((r, i) => (
        <rect key={`b${i}`} className="mm-box" x={r.x} y={r.y} width={r.w} height={r.h} rx={8} />
      ))}
      {[...layout.cards.values()].map((r, i) => (
        <rect key={`c${i}`} className="mm-card" x={r.x} y={r.y} width={r.w} height={r.h} />
      ))}
    </>
  );
});

export function Minimap({
  layout,
  view,
  stage,
  right,
  onMove,
  label = "Minimapa: la parte del plano a la vista. Pulsa para ir a otra parte.",
}: {
  layout: PlanLayout;
  view: View;
  stage: { w: number; h: number };
  right: number;
  onMove: (x: number, y: number) => void;
  label?: string;
}) {
  const ref = useRef<SVGSVGElement>(null);
  const dragging = useRef(false);
  const frame = minimapFrame(layout.width, layout.height, MAX_W, MAX_H);
  const { w, h, pad, scale: s } = frame;
  const lw = layout.width + 2 * pad;
  const lh = layout.height + 2 * pad;
  // The visible part of the plan, in plan coordinates.
  const vx = -view.tx / view.k;
  const vy = -view.ty / view.k;
  const vw = stage.w / view.k;
  const vh = stage.h / view.k;
  const to = (e: React.PointerEvent) => {
    const el = ref.current!;
    // The screen → viewBox transform accounts for the border and the `meet` margins.
    const m = el.getScreenCTM();
    if (m) {
      const p = new DOMPoint(e.clientX, e.clientY).matrixTransform(m.inverse());
      onMove(p.x, p.y);
      return;
    }
    const r = el.getBoundingClientRect();
    const p = minimapToPlan(frame, e.clientX - r.left - el.clientLeft, e.clientY - r.top - el.clientTop);
    onMove(p.x, p.y);
  };
  return (
    <svg
      ref={ref}
      className="minimap"
      style={{ right }}
      width={w}
      height={h}
      viewBox={`${-pad} ${-pad} ${lw} ${lh}`}
      role="img"
      aria-label={label}
      onPointerDown={(e) => {
        e.stopPropagation();
        dragging.current = true;
        e.currentTarget.setPointerCapture(e.pointerId);
        to(e);
      }}
      onPointerMove={(e) => {
        e.stopPropagation();
        if (dragging.current) to(e);
      }}
      onPointerUp={(e) => {
        e.stopPropagation();
        dragging.current = false;
      }}
    >
      <Shapes layout={layout} />
      <rect className="mm-view" x={vx} y={vy} width={vw} height={vh} rx={4 / s} style={{ strokeWidth: 1.5 / s }} />
    </svg>
  );
}
