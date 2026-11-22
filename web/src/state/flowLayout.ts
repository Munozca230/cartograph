// Layout of the flow on screen. Same key, same layout: a re-index that only changes numbers
// moves nothing.
import type { FlowResponse } from "../api/types";
import { cardSizes, flowKey, layoutFlow, type FlowLayout, type FlowMeasures } from "../model/callflow";

let last: FlowLayout | null = null;

export function layoutForFlow(flow: FlowResponse, m: FlowMeasures): FlowLayout {
  if (last && last.key === flowKey(flow, cardSizes(flow, m))) return last;
  last = layoutFlow(flow, m);
  return last;
}
