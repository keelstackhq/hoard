import { exitWhenDone } from "./exit.js";
import { runMain } from "./lifecycle.js";

await exitWhenDone(runMain);
