import { DEVICE, object } from "./auth";
import { WINDOW } from "./limits";
import { PROTOCOL } from "./protocol";
import {
  NETWORK,
  relativeRoute,
  SESSION_TOKEN,
  type ChallengeRoute,
} from "./routes";

export interface ConnectionBase {
  id: string;
  network: string;
  ip: string;
  protocol: number;
}
export type ChallengeState = ConnectionBase &
  ChallengeRoute & {
    role: "auth" | "verifying";
    nonce: string;
    deadline: number;
  };
export interface ControlState extends ConnectionBase {
  role: "control";
  device: string;
  manager: boolean;
  generation: string;
}
export interface SessionBinding {
  target: string;
  generation: string;
  sid: string;
}
export interface PendingState extends ConnectionBase, SessionBinding {
  role: "pending";
  anonymous: boolean;
  source: string | null;
  management: boolean;
  deadline: number;
}
interface TunnelFields extends ConnectionBase, SessionBinding {
  peer: string;
  outstanding: number;
  deadline: number;
}
export interface SourceState extends TunnelFields {
  role: "source";
  anonymous: boolean;
  source: string | null;
  management: boolean;
  cachedSince: number | null;
}
export interface TargetState extends TunnelFields {
  role: "target";
}
export type TunnelState = SourceState | TargetState;
export interface ClosedState extends ConnectionBase {
  role: "closed";
}
export type Attachment =
  ChallengeState | ControlState | PendingState | TunnelState | ClosedState;

const BASE_KEYS = ["id", "role", "network", "ip", "protocol"];
const BINDING_KEYS = ["target", "generation", "sid"];
const TUNNEL_KEYS = [
  ...BASE_KEYS,
  ...BINDING_KEYS,
  "peer",
  "outstanding",
  "deadline",
];

function binding(value: Record<string, unknown>): SessionBinding | undefined {
  if (
    typeof value.target !== "string" ||
    !DEVICE.test(value.target) ||
    typeof value.generation !== "string" ||
    !SESSION_TOKEN.test(value.generation) ||
    typeof value.sid !== "string" ||
    !SESSION_TOKEN.test(value.sid)
  )
    return;
  return { target: value.target, generation: value.generation, sid: value.sid };
}
function deadline(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}
function identifier(value: unknown): value is string {
  return typeof value === "string" && /^[a-f0-9-]{36}$/.test(value);
}

/** Hibernation data is untrusted input: reconstruct only a complete valid role. */
export function attachment(value: unknown): Attachment | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return;
  if (
    !object(value, [
      ...TUNNEL_KEYS,
      "action",
      "path",
      "nonce",
      "device",
      "manager",
      "anonymous",
      "cachedSince",
      "source",
      "management",
    ])
  )
    return;
  if (
    !identifier(value.id) ||
    typeof value.network !== "string" ||
    !NETWORK.test(value.network) ||
    typeof value.ip !== "string"
  )
    return;
  const protocol = value.protocol ?? 1;
  if (
    typeof protocol !== "number" ||
    !Number.isInteger(protocol) ||
    protocol < PROTOCOL.min ||
    protocol > PROTOCOL.max
  )
    return;
  const base: ConnectionBase = {
    id: value.id,
    network: value.network,
    ip: value.ip,
    protocol,
  };
  switch (value.role) {
    case "auth":
    case "verifying": {
      const keys = [
        ...BASE_KEYS,
        "action",
        "path",
        "nonce",
        "target",
        "deadline",
      ];
      if (
        !object(value, keys) ||
        typeof value.path !== "string" ||
        typeof value.nonce !== "string" ||
        !/^[a-z2-7]{26}$/.test(value.nonce) ||
        !deadline(value.deadline)
      )
        return;
      const parsed = relativeRoute(value.path);
      if (
        !parsed ||
        parsed.action === "attach" ||
        parsed.action !== value.action ||
        parsed.network !== base.network
      )
        return;
      if (
        parsed.action === "connect"
          ? parsed.target !== value.target
          : value.target !== undefined
      )
        return;
      return {
        ...base,
        ...parsed,
        role: value.role,
        nonce: value.nonce,
        deadline: value.deadline,
      };
    }
    case "control":
      if (
        !object(value, [...BASE_KEYS, "device", "manager", "generation"]) ||
        typeof value.device !== "string" ||
        !DEVICE.test(value.device) ||
        typeof value.manager !== "boolean" ||
        typeof value.generation !== "string" ||
        !SESSION_TOKEN.test(value.generation)
      )
        return;
      return {
        ...base,
        role: "control",
        device: value.device,
        manager: value.manager,
        generation: value.generation,
      };
    case "pending": {
      const session = binding(value);
      if (
        !object(value, [
          ...BASE_KEYS,
          ...BINDING_KEYS,
          "anonymous",
          "source",
          "management",
          "deadline",
        ]) ||
        !session ||
        typeof value.anonymous !== "boolean" ||
        !deadline(value.deadline) ||
        !admission(value)
      )
        return;
      return {
        ...base,
        ...session,
        role: "pending",
        anonymous: value.anonymous,
        source: (value.source as string | null | undefined) ?? null,
        management: value.management === true,
        deadline: value.deadline,
      };
    }
    case "source":
    case "target": {
      const session = binding(value);
      const keys =
        value.role === "source"
          ? [
            ...TUNNEL_KEYS, "anonymous", "cachedSince", "source", "management",
          ]
          : TUNNEL_KEYS;
      if (
        !object(value, keys) ||
        !session ||
        !identifier(value.peer) ||
        !deadline(value.deadline) ||
        typeof value.outstanding !== "number" ||
        !Number.isSafeInteger(value.outstanding) ||
        value.outstanding < 0 ||
        value.outstanding > WINDOW
      )
        return;
      const tunnel = {
        ...base,
        ...session,
        peer: value.peer,
        deadline: value.deadline,
        outstanding: value.outstanding,
      };
      if (value.role === "target") return { ...tunnel, role: "target" };
      if (typeof value.anonymous !== "boolean" || !admission(value)) return;
      const cachedSince = value.cachedSince ?? null;
      if (
        cachedSince !== null &&
        (!deadline(cachedSince) || protocol < 2 || value.anonymous)
      )
        return;
      return {
        ...tunnel,
        role: "source",
        anonymous: value.anonymous,
        source: (value.source as string | null | undefined) ?? null,
        management: value.management === true,
        cachedSince,
      };
    }
    case "closed":
      if (object(value, BASE_KEYS)) return { ...base, role: "closed" };
  }
}

function admission(value: Record<string, unknown>): boolean {
  return (
    (value.source == null ||
      (typeof value.source === "string" && DEVICE.test(value.source))) &&
    (value.management === undefined || typeof value.management === "boolean") &&
    !(value.anonymous && (value.source != null || value.management === true)) &&
    !(value.management === true && value.source == null)
  );
}

export function base(state: ConnectionBase): ConnectionBase {
  return {
    id: state.id,
    network: state.network,
    ip: state.ip,
    protocol: state.protocol,
  };
}
export function tunnel(state: Attachment): state is TunnelState {
  return state.role === "source" || state.role === "target";
}
export function session(
  state: Attachment,
): state is PendingState | TunnelState {
  return state.role === "pending" || tunnel(state);
}
export function expired(state: Attachment, now: number): boolean {
  return "deadline" in state && state.deadline <= now;
}
