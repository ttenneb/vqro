// Test-only: the shipped tier-1 Pi plugin with injected faults. Loaded only by tests/pi_pull_e2e.py (`pi -e`);
// the shipped plugin (integrations/pi/vqro-pull.ts) reads no fault setting.
// VQRO_PULL_TEST_FAULT: crash_after_take | drop_report | abort_after_take.
import { createPlugin } from "../../../integrations/pi/vqro-pull.ts";

const FAULT = process.env.VQRO_PULL_TEST_FAULT ?? "";

export default createPlugin({
  // A crash between take and hand-off: the claim stays open for the next consumer's reconcile.
  afterTake: () => { if (FAULT === "crash_after_take") process.exit(42); },
  // A lost progress report (released still goes through, so mail is never stranded by the fault itself).
  skipReport: (stage) => FAULT === "drop_report" && stage !== "released",
  // The user aborts right after a boundary take.
  afterTurnTake: (ctx) => { if (FAULT === "abort_after_take") ctx.abort(); },
});
