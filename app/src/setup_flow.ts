/**
 * Pure setup-flow policy for the settings UI.
 *
 * This module decides the next user-visible setup step. It does not request OS
 * permissions or own engine lifecycle; those effects remain in the Tauri
 * composition root so the frontend cannot mistake a prompt for a grant.
 */

export type PermissionState = "available" | "required" | "not-required" | "checked-on-use";

export type SetupReport = {
  engine: { status?: string };
  permissions: {
    accessibility?: PermissionState;
    input_monitoring?: PermissionState;
    microphone?: PermissionState;
    screen_capture?: PermissionState;
  };
  offline_ready: boolean;
};

export type SetupStep =
  | "download"
  | "engine-starting"
  | "accessibility"
  | "input-monitoring"
  | "complete";

export function permissionReady(state: PermissionState | undefined): boolean {
  return state === "available" || state === "not-required";
}

export function nextSetupStep(report: SetupReport): SetupStep {
  if (!report.offline_ready || report.engine.status === "not-installed") return "download";
  if (report.engine.status !== "ok") return "engine-starting";
  if (!permissionReady(report.permissions.accessibility)) return "accessibility";
  if (!permissionReady(report.permissions.input_monitoring)) return "input-monitoring";
  return "complete";
}
