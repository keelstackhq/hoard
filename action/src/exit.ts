// Ends the step's process once main or post is done. A download stopped by the 90 s cap
// (tc.downloadTool takes no AbortSignal) or a tool-cache retry timer can keep Node's event loop
// alive for minutes; the step must not wait for them. Exiting is safe: the hoard child is
// detached and unref'd with its log fd already closed, and @actions/core writes its file
// commands synchronously.
export async function exitWhenDone(
  run: () => Promise<void>,
  exit: (code: number) => void = (c) => process.exit(c),
): Promise<void> {
  await run();
  // stdout/stderr can be asynchronous pipes (macOS, Windows): flush workflow commands first.
  await Promise.all(
    [process.stdout, process.stderr].map((s) => new Promise((r) => s.write("", r))),
  );
  exit(Number(process.exitCode ?? 0));
}
