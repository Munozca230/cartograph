// Fixture A: a realistic ~170-file multi-tenant Next.js SaaS ("tallerpro").
// Core: src/lib/db/schema.ts and src/lib/db/with-tenant.ts, then src/lib/auth.
import { buildRepo, type FileSpec } from "./build.ts";

const specs: FileSpec[] = [];
const add = (path: string, loc: number, imports: string[] = []) => specs.push({ path, loc, imports });

// Root config files.
add("next.config.mjs", 38, ["./src/env"]);
add("drizzle.config.ts", 22, ["drizzle-kit", "./src/env"]);
add("tailwind.config.ts", 64, ["tailwindcss"]);
add("vitest.config.ts", 18, ["vitest/config"]);
add("playwright.config.ts", 41, ["@playwright/test"]);

// --- src/lib/db: the core -------------------------------------------------
add("src/lib/db/schema.ts", 412, ["drizzle-orm/pg-core", "drizzle-orm"]);
add("src/lib/db/client.ts", 46, ["drizzle-orm/postgres-js", "postgres", "@/env", "./schema"]);
add("src/lib/db/with-tenant.ts", 128, ["drizzle-orm", "./client", "./schema", "@/lib/auth/session", "@/lib/utils/errors"]);
add("src/lib/db/index.ts", 12, ["./client", "./schema", "./with-tenant"]);
add("src/lib/db/relations.ts", 96, ["drizzle-orm", "./schema"]);
add("src/lib/db/seed-data.ts", 140, ["./schema"]);
const queries = ["projects", "tasks", "users", "teams", "billing", "messages", "audit", "invoices", "clients", "files"];
for (const q of queries) {
  add(`src/lib/db/queries/${q}.ts`, 70 + q.length * 9, [
    "drizzle-orm",
    "@/lib/db/schema",
    "@/lib/db/with-tenant",
    q === "audit" ? "@/lib/utils/dates" : "@/lib/utils/errors",
  ]);
}
add("src/lib/db/migrations/0001_init.ts", 210, ["drizzle-orm", "../schema"]);
add("src/lib/db/migrations/0002_tenants.ts", 88, ["drizzle-orm", "../schema"]);
add("src/lib/db/migrations/0003_messages.ts", 64, ["drizzle-orm", "../schema"]);

// --- src/lib/auth ------------------------------------------------------------
add("src/lib/auth/options.ts", 118, ["next-auth", "next-auth/providers/google", "@auth/drizzle-adapter", "@/lib/db/client", "@/lib/db/schema", "@/env"]);
add("src/lib/auth/session.ts", 54, ["next-auth", "./options", "react"]);
add("src/lib/auth/permissions.ts", 96, ["@/lib/db/schema", "./session"]);
add("src/lib/auth/tenant.ts", 61, ["@/lib/db/schema", "@/lib/db/client", "./session", "next/headers"]);
add("src/lib/auth/roles.ts", 33, []);
add("src/lib/auth/guards.ts", 47, ["./permissions", "./tenant", "next/navigation"]);

// --- src/lib/billing, utils, validators -------------------------------------
add("src/lib/billing/stripe.ts", 74, ["stripe", "@/env"]);
add("src/lib/billing/plans.ts", 58, ["./stripe"]);
add("src/lib/billing/usage.ts", 82, ["@/lib/db/with-tenant", "@/lib/db/schema", "./plans"]);
add("src/lib/billing/webhooks.ts", 131, ["stripe", "./stripe", "@/lib/db/queries/billing", "@/lib/db/with-tenant"]);
add("src/lib/utils/cn.ts", 9, ["clsx", "tailwind-merge"]);
add("src/lib/utils/dates.ts", 44, ["date-fns", "date-fns/locale"]);
add("src/lib/utils/format.ts", 37, []);
add("src/lib/utils/errors.ts", 52, []);
add("src/lib/utils/ids.ts", 15, ["node:crypto"]);
add("src/lib/utils/analytics.ts", 29, ["!not_found|ningún archivo coincide con src/lib/legacy/segment{.ts,.tsx,/index.ts}|@/lib/legacy/segment"]);
for (const v of ["project", "task", "team", "billing", "message", "user"]) {
  add(`src/lib/validators/${v}.ts`, 30 + v.length * 4, ["zod", "drizzle-zod", "@/lib/db/schema"]);
}

// --- src/server: tRPC routers ------------------------------------------------
add("src/server/trpc.ts", 72, ["@trpc/server", "superjson", "zod", "@/lib/auth/session", "@/lib/db/with-tenant"]);
add("src/server/context.ts", 39, ["@/lib/auth/session", "@/lib/auth/tenant", "@/lib/db/client"]);
const routers = ["project", "task", "team", "billing", "message", "user", "client", "file"];
for (const r of routers) {
  const query = r === "user" ? "users" : r === "message" ? "messages" : r === "billing" ? "billing" : `${r}s`;
  const validator = ["project", "task", "team", "billing", "message", "user"].includes(r) ? [`@/lib/validators/${r}`] : [];
  add(`src/server/routers/${r}.ts`, 90 + r.length * 11, [
    "zod",
    "../trpc",
    `@/lib/db/queries/${query}`,
    "@/lib/db/with-tenant",
    "@/lib/db/schema",
    "@/lib/auth/permissions",
    "@/lib/auth/session",
    ...validator,
    ...(r === "message" ? ["@/adapters/messaging"] : []),
    ...(r === "file" ? ["@/adapters/storage"] : []),
    ...(r === "billing" ? ["@/lib/billing/usage", "@/lib/billing/plans"] : []),
  ]);
}
add("src/server/root.ts", 28, routers.map((r) => `./routers/${r}`).concat(["./trpc"]));
add("src/server/actions/invite.ts", 58, ["@/lib/db/with-tenant", "@/lib/auth/guards", "@/emails/invite", "@/adapters/messaging/email"]);
add("src/server/actions/upload.ts", 46, ["@/adapters/storage/s3", "@/lib/db/with-tenant"]);

// --- src/adapters ----------------------------------------------------------------
add("src/adapters/messaging/types.ts", 41, []);
add("src/adapters/messaging/index.ts", 36, ["./types", "./twilio", "./whatsapp", "./email", "./slack"]);
add("src/adapters/messaging/twilio.ts", 88, ["twilio", "./types", "@/env", "@/lib/db/queries/messages"]);
add("src/adapters/messaging/whatsapp.ts", 102, ["./types", "@/env", "@/lib/db/queries/messages", "@/lib/db/with-tenant"]);
add("src/adapters/messaging/email.ts", 67, ["resend", "./types", "@/env"]);
add("src/adapters/messaging/slack.ts", 49, ["@slack/web-api", "./types", "@/env"]);
add("src/adapters/messaging/templates.ts", 58, ["./types", "!not_found|ningún archivo coincide con src/adapters/messaging/i18n{.ts,.tsx,/index.ts}|./i18n"]);
add("src/adapters/storage/index.ts", 14, ["./s3"]);
add("src/adapters/storage/s3.ts", 79, ["@aws-sdk/client-s3", "@aws-sdk/s3-request-presigner", "@/env"]);

// --- src/hooks, emails, misc ----------------------------------------------------
for (const h of ["use-session", "use-tenant", "use-debounce", "use-toast", "use-media-query", "use-projects"]) {
  const deps = ["react"];
  if (h === "use-session") deps.push("next-auth/react");
  if (h === "use-tenant") deps.push("@/lib/trpc-client");
  if (h === "use-projects") deps.push("@/lib/trpc-client", "@tanstack/react-query");
  add(`src/hooks/${h}.ts`, 18 + h.length * 2, deps);
}
add("src/lib/trpc-client.ts", 34, ["@trpc/react-query", "@trpc/client", "superjson", "@/server/root"]);
for (const e of ["welcome", "invite", "invoice", "reset-password", "digest"]) {
  add(`src/emails/${e}.tsx`, 60 + e.length * 3, ["@react-email/components", "react", "@/lib/utils/format"]);
}
add("src/env.ts", 48, ["@t3-oss/env-nextjs", "zod"]);
add("src/middleware.ts", 57, ["next/server", "next-auth/middleware", "@/lib/auth/roles"]);

// --- src/components -----------------------------------------------------------
const ui = ["button", "card", "dialog", "input", "select", "table", "tabs", "toast", "badge", "avatar", "dropdown-menu", "sheet", "skeleton", "tooltip", "form", "label", "textarea", "separator", "command", "calendar"];
for (const c of ui) {
  const deps = ["react", "@/lib/utils/cn"];
  if (["dialog", "select", "tabs", "dropdown-menu", "sheet", "tooltip"].includes(c)) deps.push(`@radix-ui/react-${c}`);
  if (c === "form") deps.push("react-hook-form", "./label");
  if (c === "calendar") deps.push("react-day-picker", "./button");
  if (c === "command") deps.push("cmdk", "./dialog");
  if (c === "toast") deps.push("@/hooks/use-toast");
  add(`src/components/ui/${c}.tsx`, 35 + c.length * 6, deps);
}
const feature = (dir: string, names: string[], extra: (n: string) => string[]) => {
  names.forEach((n, i) => {
    add(`src/components/${dir}/${n}.tsx`, 55 + n.length * 7, [
      "react",
      ...(i % 2 === 0 ? ["@/components/ui/button"] : ["@/components/ui/card"]),
      ...extra(n),
    ]);
  });
};
feature("dashboard", ["stats-grid", "activity-feed", "revenue-chart", "quick-actions", "welcome-banner"], (n) =>
  n === "revenue-chart" ? ["recharts", "@/lib/utils/format"] : n === "activity-feed" ? ["@/lib/utils/dates"] : [],
);
feature("projects", ["project-list", "project-card", "project-form", "task-board", "task-card", "task-dialog", "timeline"], (n) =>
  n.includes("form") || n.includes("dialog")
    ? ["@/components/ui/form", "@/components/ui/dialog", "@/lib/validators/project", "@/hooks/use-projects"]
    : n === "task-board"
      ? ["@dnd-kit/core", "./task-card", "@/hooks/use-projects"]
      : ["@/lib/utils/dates"],
);
feature("billing", ["plan-picker", "usage-meter", "invoice-table", "payment-method"], (n) =>
  n === "invoice-table" ? ["@/components/ui/table", "@/lib/utils/format"] : ["@/lib/billing/plans"],
);
feature("team", ["member-list", "invite-dialog", "role-select"], (n) =>
  n === "invite-dialog" ? ["@/components/ui/dialog", "@/server/actions/invite"] : ["@/lib/auth/roles"],
);
feature("inbox", ["conversation-list", "message-thread", "composer", "channel-badge"], (n) =>
  n === "composer" ? ["@/components/ui/textarea", "@/hooks/use-debounce"] : ["@/lib/utils/dates"],
);
feature("marketing", ["hero", "pricing-table", "feature-grid", "footer", "testimonials"], (n) =>
  n === "pricing-table" ? ["@/lib/billing/plans"] : [],
);
feature("layout", ["sidebar", "topbar", "tenant-switcher", "user-menu", "command-menu"], (n) =>
  n === "tenant-switcher" ? ["@/hooks/use-tenant", "@/components/ui/dropdown-menu"] : n === "user-menu" ? ["@/hooks/use-session", "@/components/ui/dropdown-menu"] : n === "command-menu" ? ["@/components/ui/command"] : ["next/link", "lucide-react", "@/lib/auth/roles"],
);
add("src/components/providers.tsx", 44, ["react", "next-auth/react", "@tanstack/react-query", "@/lib/trpc-client", "@/components/ui/toast"]);

// --- src/app ------------------------------------------------------------------------
add("src/app/layout.tsx", 41, ["react", "next/font/google", "@/components/providers"]);
add("src/app/not-found.tsx", 22, ["next/link", "@/components/ui/button"]);
add("src/app/error.tsx", 30, ["react", "@/components/ui/button"]);
add("src/app/(app)/layout.tsx", 48, ["@/lib/auth/guards", "@/components/layout/sidebar", "@/components/layout/topbar"]);
const appPage = (route: string, loc: number, imports: string[]) =>
  add(`src/app/(app)/${route}/page.tsx`, loc, ["@/lib/auth/guards", "@/lib/auth/session", "@/lib/db/with-tenant", ...imports]);
appPage("dashboard", 66, ["@/components/dashboard/stats-grid", "@/components/dashboard/activity-feed", "@/components/dashboard/revenue-chart", "@/lib/db/queries/projects"]);
appPage("projects", 52, ["@/components/projects/project-list", "@/lib/db/queries/projects"]);
appPage("projects/[id]", 88, ["@/components/projects/task-board", "@/components/projects/timeline", "@/lib/db/queries/tasks", "next/navigation"]);
appPage("projects/[id]/settings", 61, ["@/components/projects/project-form", "@/lib/db/queries/projects"]);
appPage("clients", 57, ["@/components/ui/table", "@/lib/db/queries/clients"]);
appPage("clients/[id]", 63, ["@/lib/db/queries/clients", "@/components/ui/card"]);
appPage("inbox", 74, ["@/components/inbox/conversation-list", "@/components/inbox/message-thread", "@/components/inbox/composer", "@/lib/db/queries/messages"]);
appPage("billing", 59, ["@/components/billing/plan-picker", "@/components/billing/usage-meter", "@/components/billing/invoice-table", "@/lib/billing/usage"]);
appPage("team", 49, ["@/components/team/member-list", "@/components/team/invite-dialog", "@/lib/db/queries/teams"]);
appPage("settings", 44, ["@/components/ui/form", "@/lib/db/queries/users"]);
appPage("settings/integrations", 71, ["@/adapters/messaging", "@/components/ui/card"]);
add("src/app/(app)/projects/[id]/loading.tsx", 14, ["@/components/ui/skeleton"]);
add("src/app/(auth)/layout.tsx", 26, ["react"]);
for (const p of ["login", "signup", "reset-password", "verify"]) {
  add(`src/app/(auth)/${p}/page.tsx`, 48 + p.length * 3, ["next-auth/react", "@/lib/auth/options", "@/lib/auth/roles", "@/components/ui/button", "@/components/ui/input", "@/components/ui/form"]);
}
add("src/app/(marketing)/layout.tsx", 31, ["@/components/marketing/footer"]);
add("src/app/(marketing)/page.tsx", 58, ["@/components/marketing/hero", "@/components/marketing/feature-grid", "@/components/marketing/testimonials"]);
add("src/app/(marketing)/pricing/page.tsx", 36, ["@/components/marketing/pricing-table"]);
add("src/app/(marketing)/blog/page.tsx", 41, ["contentlayer/generated", "next/link"]);
add("src/app/(marketing)/blog/[slug]/page.tsx", 52, ["contentlayer/generated", "~@/lib/mdx"]);
add("src/app/api/trpc/[trpc]/route.ts", 24, ["@trpc/server/adapters/fetch", "@/server/root", "@/server/context"]);
add("src/app/api/auth/[...nextauth]/route.ts", 8, ["next-auth", "@/lib/auth/options"]);
add("src/app/api/webhooks/stripe/route.ts", 44, ["next/server", "@/lib/auth/tenant", "@/lib/billing/webhooks", "@/lib/billing/stripe"]);
add("src/app/api/webhooks/twilio/route.ts", 51, ["next/server", "@/lib/auth/tenant", "@/adapters/messaging/twilio", "@/lib/db/with-tenant"]);
add("src/app/api/webhooks/whatsapp/route.ts", 58, ["next/server", "@/lib/auth/tenant", "@/adapters/messaging/whatsapp", "@/lib/db/with-tenant"]);
add("src/app/api/cron/digest/route.ts", 47, ["next/server", "@/lib/auth/guards", "@/lib/db/queries/messages", "@/emails/digest", "@/adapters/messaging/email"]);
add("src/app/api/cron/usage/route.ts", 39, ["next/server", "@/lib/auth/guards", "@/lib/billing/usage"]);
add("src/app/api/files/[id]/route.ts", 36, ["next/server", "@/adapters/storage", "@/lib/auth/session", "@/lib/db/queries/files"]);
add("src/app/api/health/route.ts", 12, ["next/server", "@/lib/db/client"]);
add("src/app/sitemap.ts", 21, ["@/lib/db/queries/projects"]);
add("src/app/robots.ts", 10, []);
add("src/lib/mdx.ts", 33, ["next-mdx-remote/rsc", "!dynamic|import() con plantilla: `./mdx/${name}`|./mdx/${name}"]);

// --- scripts & tests -----------------------------------------------------------------
add("scripts/seed.ts", 84, ["../src/lib/db/client", "../src/lib/db/seed-data", "../src/lib/db/schema"]);
add("scripts/migrate.ts", 31, ["drizzle-orm/postgres-js/migrator", "../src/lib/db/client"]);
add("scripts/backfill-tenants.ts", 66, ["../src/lib/db/with-tenant", "../src/lib/db/schema", "!not_found|ningún archivo coincide con scripts/legacy-map{.ts,.tsx,/index.ts}|./legacy-map"]);
add("tests/unit/with-tenant.test.ts", 112, ["vitest", "@/lib/db/with-tenant", "@/lib/db/schema"]);
add("tests/unit/permissions.test.ts", 74, ["vitest", "@/lib/auth/permissions"]);
add("tests/unit/usage.test.ts", 58, ["vitest", "@/lib/billing/usage"]);
add("tests/unit/messaging.test.ts", 81, ["vitest", "@/adapters/messaging/whatsapp", "@/adapters/messaging/twilio"]);
add("tests/e2e/onboarding.spec.ts", 66, ["@playwright/test"]);
add("tests/e2e/billing.spec.ts", 52, ["@playwright/test"]);
add("tests/e2e/inbox.spec.ts", 47, ["@playwright/test"]);
add("tests/fixtures/tenants.ts", 39, ["../../src/lib/db/schema"]);

export function saasFixture() {
  return buildRepo(specs, {
    name: "tallerpro",
    root: "D:/projects/demo/tallerpro",
    alias: { "@/": "src", "~@/": "src" },
    packages: [
      { id: "pkg:package.json", name: "tallerpro", kind: "npm", dir: "repo:", manifest: "file:package.json", notes: null },
    ],
    skipped: [
      { path: "public/vendor/chart.min.js", reason: "too_large", detail: "1.8 MB, el límite es 1 MB" },
      { path: "src/lib/db/migrations/0004_draft.ts", reason: "parse_error", detail: "línea 12: token inesperado" },
    ],
    diagnostics: [{ path: "package.json", detail: "campo \"workspaces\" ignorado: no hay paquetes que coincidan" }],
  });
}

export const SAAS_FILE_COUNT = specs.length;
