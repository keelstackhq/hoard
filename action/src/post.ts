import { exitWhenDone } from "./exit.js";
import { runPost } from "./lifecycle.js";

await exitWhenDone(runPost);
