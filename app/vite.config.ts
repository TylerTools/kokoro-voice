import { defineConfig } from "vite";
import { resolve } from "path";

// Two entry points: the settings window and the floating player. Without the
// explicit rollup input the player is not emitted into dist/ and the window
// loads a 404 in the packaged app.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        player: resolve(__dirname, "player.html"),
      },
    },
  },
});
