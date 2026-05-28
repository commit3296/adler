import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// Dev mode: the SolidJS bundle is served by Vite on :5173 while the
// Rust API runs on :8765. The proxy below makes `/api/*` and the SSE
// stream Just Work from the SPA's perspective.
//
// Production: `npm run build` emits `dist/` which `adler-server`
// embeds via `rust-embed` into the final binary.
export default defineConfig({
    plugins: [solid()],
    server: {
        port: 5173,
        proxy: {
            "/api": {
                target: "http://127.0.0.1:8765",
                changeOrigin: true,
                // SSE needs the connection kept open and chunks
                // forwarded without buffering.
                ws: false,
            },
        },
    },
    build: {
        target: "es2020",
        outDir: "dist",
        sourcemap: false,
        rollupOptions: {
            output: {
                // Single-file-ish bundle for a small SPA. rust-embed is
                // happy either way; this just makes the embedded blob
                // a tad smaller and easier to inspect.
                manualChunks: undefined,
            },
        },
    },
});
