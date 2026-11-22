export const MAX_ITEMS = 3;

export function slugify(text: string): string {
  return normalize(text).replace(/ /g, "-");
}

export function helper(): number {
  return MAX_ITEMS;
}

function normalize(text: string): string {
  return text.trim().toLowerCase();
}
