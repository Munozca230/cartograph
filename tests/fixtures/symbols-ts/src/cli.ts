import { program } from "commander";
import { helper } from "./util";

function runBuild(): void {
  helper();
}

program.command("build").action(runBuild);
program.parse();
