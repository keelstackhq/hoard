import { defineConfig } from "vite-plus";

// Tasks defined here are cached by Vite Task (node_modules/.vite/task-cache).

export default defineConfig({
  run: {
    tasks: {
      hello: {
        command: "node hello.js",
      },
    },
  },
});
