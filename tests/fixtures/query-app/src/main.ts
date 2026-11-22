import { start } from "./ui/app";
import { connect } from "./infra/db";

connect();
start();
