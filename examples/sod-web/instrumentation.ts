// Runs once at server boot (Next instrumentation hook). Without this the
// replica — and crucially the sync serve loop — would initialize lazily
// on the first page/API hit, so a freshly restarted hub would be deaf to
// peers until a browser happened to visit it.
export async function register() {
  if (process.env.NEXT_RUNTIME === "nodejs") {
    const { getSod } = await import("./lib/sod");
    getSod();
  }
}
