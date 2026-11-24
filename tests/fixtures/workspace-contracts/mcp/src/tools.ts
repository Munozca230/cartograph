import { z } from 'zod';
import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';

export const createItemName = 'create_item';

export const createItemSchema = z.object({
  title: z.string().min(1),
  summary: z.string().optional(),
  projectId: z.string(),
});

export function registerTools(server: McpServer): void {
  server.tool(createItemName, 'Create an item', createItemSchema.shape, async (args) => ({
    content: [{ type: 'text', text: JSON.stringify(args) }],
  }));
  server.tool('list_items', 'List the items of a project', { projectId: z.string() }, async () => ({
    content: [],
  }));
}
