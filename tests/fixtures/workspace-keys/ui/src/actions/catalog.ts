/** The actions the editor offers, by category (moving a file is not offered yet). */
export interface ActionEntry {
  name: string;
  label: string;
}

export const ACTION_CATEGORIES = [
  {
    id: "files",
    actions: [
      { name: "READ-FILE", label: "Read a file" },
      { name: "WRITE-FILE", label: "Write a file" },
      { name: "COPY-FILE", label: "Copy a file" },
      { name: "DELETE-FILE", label: "Delete a file" },
      { name: "RENAME-FILE", label: "Rename a file" },
      { name: "TOUCH-FILE", label: "Touch a file" },
    ],
  },
  {
    id: "folders",
    actions: [
      { name: "LIST-DIR", label: "List a folder" },
      { name: "MAKE-DIR", label: "Make a folder" },
      { name: "REMOVE-DIR", label: "Remove a folder" },
      { name: "ZIP-DIR", label: "Zip a folder" },
      { name: "UNZIP-DIR", label: "Unzip a folder" },
    ],
  },
];

export function find(name: string): ActionEntry | undefined {
  return ACTION_CATEGORIES.flatMap((c) => c.actions).find((a) => a.name === name);
}
