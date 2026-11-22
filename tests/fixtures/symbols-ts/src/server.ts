import express from "express";
import { run } from "./app";

const app = express();

function getUser(req: unknown, res: { send(x: string): void }): void {
  res.send(run("user"));
}

app.get("/users/:id", getUser);
app.listen(3000);
