import index from "./index.html";

const port = Number(Bun.env.DASHBOARD_PORT ?? 5173);

Bun.serve({
  port,
  routes: {
    "/": index,
    "/index.html": index,
  },
  development: {
    hmr: true,
    console: true,
  },
});

console.log(`LeIndex dashboard running on http://127.0.0.1:${port}`);
