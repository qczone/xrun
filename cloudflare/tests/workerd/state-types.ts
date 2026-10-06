import type { Attachment } from "../../src/state";
import type { Route } from "../../src/routes";

// Compile-time contracts: a regression to optional role fields makes these
// expected errors disappear and fails the existing TypeScript check.
export function roleContracts(state: Attachment): void {
  // @ts-expect-error Device identity exists only on control connections.
  void state.device;
  if (state.role === "control") void state.device;
  // @ts-expect-error A pending session cannot omit its one-time session ID.
  const pending: Attachment = {
    id: "id",
    role: "pending",
    network: "probe",
    ip: "ip",
    target: "device",
    generation: "generation",
    anonymous: true,
    deadline: 1,
  };
  // @ts-expect-error Attach routes require both generation and session ID.
  const attach: Route = {
    action: "attach",
    target: "device",
    network: "probe",
    path: "/path",
  };
  void pending;
  void attach;
}
