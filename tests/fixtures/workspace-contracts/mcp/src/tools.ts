import { z } from 'zod';
import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';

export const createItemName = 'create_item';
declare const names: { next(): string };

export const createItemSchema = z.object({
  label: z.string().min(1),
  notes: z.string().optional(),
  ownerId: z.string(),
});

export function registerTools(server: McpServer): void {
  server.tool(createItemName, 'Create an item', createItemSchema.shape, async (args) => ({
    content: [{ type: 'text', text: JSON.stringify(args) }],
  }));
  // A name known only at run time: kept without one.
  server.tool(names.next(), 'Archive an item', { id: z.string() }, async () => ({ content: [] }));
  server.tool('list_items', 'List the items of a project', { ownerId: z.string() }, async () => ({
    content: [],
  }));
}
