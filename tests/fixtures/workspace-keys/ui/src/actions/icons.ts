/** Icon of each action. */
export const ACTION_ICONS: Record<string, string> = {
  "READ-FILE": "file",
  "WRITE-FILE": "edit",
  "COPY-FILE": "copy",
  "MOVE-FILE": "move",
  "DELETE-FILE": "trash",
  "RENAME-FILE": "label",
  "TOUCH-FILE": "clock",
  "LIST-DIR": "folder",
  "MAKE-DIR": "folder-plus",
  "REMOVE-DIR": "folder-minus",
  "ZIP-DIR": "archive",
  "UNZIP-DIR": "unarchive",
};

/** Two colours only: too few keys to compare with the others. */
export const ACCENTS: Record<string, string> = {
  "READ-FILE": "blue",
  "WRITE-FILE": "green",
};
