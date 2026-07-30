import { RecoveryScreen } from "./RecoveryScreen";

export function RelaunchRequiredScreen() {
  return (
    <RecoveryScreen
      testId="relaunch-required"
      title="Restart HireShelby to finish recovery"
      body="Your identity was updated. HireShelby needs to restart so syncing and agents run under it."
    />
  );
}
