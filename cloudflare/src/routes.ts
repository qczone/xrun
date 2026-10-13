import { DEVICE } from "./auth";

export const NETWORK = /^(net_[a-z2-7]{52}|probe)$/;
/** One durable admission and routing authority per deployed relay. */
export const RELAY_OBJECT = "relay";
export const SESSION_TOKEN = /^[a-f0-9]{32}$/;
interface RouteBase {
  network: string;
  path: string;
}
export type ChallengeRoute = RouteBase &
  (
    | { action: "status" }
    | { action: "traffic" }
    | { action: "control" }
    | { action: "claim" }
    | { action: "connect"; target: string }
    | { action: "standby"; target: string; generation: string }
  );
export type AttachRoute = RouteBase & {
  action: "attach";
  target: string;
  generation: string;
  sid: string;
};
export type Route = ChallengeRoute | AttachRoute;

/** Parse the operation path used by both routing and signed challenges. */
export function route(path: string): Route | undefined {
  if (!path.startsWith("/")) return;
  const parts = path.split("/").slice(1);
  const [resource, network, action, target, generation, sid] = parts;
  if (resource !== "networks" || !NETWORK.test(network)) return;
  if (
    (action === "status" ||
      action === "control" ||
      action === "traffic" ||
      action === "claim") &&
    parts.length === 3
  ) {
    return { network, action, path };
  }
  if (action === "connect" && parts.length === 4 && DEVICE.test(target)) {
    return { network, action, target, path };
  }
  if (
    action === "standby" &&
    parts.length === 5 &&
    DEVICE.test(target) &&
    SESSION_TOKEN.test(generation)
  )
    return { network, action, target, generation, path };
  const attach =
    action === "attach" &&
    parts.length === 6 &&
    DEVICE.test(target) &&
    SESSION_TOKEN.test(generation) &&
    SESSION_TOKEN.test(sid);
  if (attach) return { network, action, target, generation, sid, path };
}
