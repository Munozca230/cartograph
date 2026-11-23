import { expect, test } from "@playwright/test";
import { find } from "../src/actions/catalog";

test("the editor offers the catalog", async () => {
  expect(find("LIST-DIR")).toBeTruthy();
});
