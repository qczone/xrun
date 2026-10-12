import { DEVICE, object } from "./auth";
import { WINDOW } from "./limits";
import { PROTOCOL } from "./protocol";
import { trafficCounters, type TrafficCounter } from "./traffic-state";
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
  pairing?: string;
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
  authorityGeneration: number;
}
export interface TrafficState extends ConnectionBase {
  role: "traffic";
  device: string;
  manager: boolean;
  deadline: number;
  authorityGeneration: number;
}
export interface SessionBinding {
  target: string;
  generation: string;
  sid: string;
  authorityGeneration: number;
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
  traffic: TrafficCounter[];
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
  | ChallengeState
  | ControlState
  | TrafficState
  | PendingState
  | TunnelState
  | ClosedState;

const BASE_KEYS = ["id", "role", "network", "ip", "protocol", "pairing"];
const BINDING_KEYS = ["target", "generation", "sid", "authorityGeneration"];
const TUNNEL_KEYS = [
  ...BASE_KEYS,
  ...BINDING_KEYS,
  "peer",
  "outstanding",
  "deadline",
  "traffic",
];

function binding(value: Record<string, unknown>): SessionBinding | undefined {
  if (
    typeof value.target !== "string" ||
    !DEVICE.test(value.target) ||
    typeof value.generation !== "string" ||
    !SESSION_TOKEN.test(value.generation) ||
    typeof value.sid !== "string" ||
    !SESSION_TOKEN.test(value.sid) ||
    typeof value.authorityGeneration !== "number" ||
    !Number.isSafeInteger(value.authorityGeneration) ||
    value.authorityGeneration <= 0
  )
    return;
  return {
    target: value.target,
    generation: value.generation,
    sid: value.sid,
    authorityGeneration: value.authorityGeneration,
  };
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
      "authorityGeneration",
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
  const protocol = value.protocol;
  if (
    typeof protocol !== "number" ||
    !Number.isInteger(protocol) ||
    protocol < PROTOCOL.min ||
    protocol > PROTOCOL.max
  )
    return;
  if (
    value.pairing !== undefined &&
    (typeof value.pairing !== "string" ||
      !SESSION_TOKEN.test(value.pairing) ||
      !["auth", "verifying", "pending", "source", "target"].includes(
        String(value.role),
      ))
  )
    return;
  const base: ConnectionBase = {
    id: value.id,
    network: value.network,
    ip: value.ip,
    protocol,
    ...(value.pairing === undefined
      ? {}
      : { pairing: value.pairing as string }),
  };
  switch (value.role) {
    case "traffic":
      if (
        !object(value, [
          ...BASE_KEYS,
          "device",
          "manager",
          "deadline",
          "authorityGeneration",
        ]) ||
        typeof value.device !== "string" ||
        !DEVICE.test(value.device) ||
        typeof value.manager !== "boolean" ||
        !deadline(value.deadline) ||
        !deadline(value.authorityGeneration)
      )
        return;
      return {
        ...base,
        role: "traffic",
        device: value.device,
        manager: value.manager,
        deadline: value.deadline,
        authorityGeneration: value.authorityGeneration,
      };
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
        !object(value, [
          ...BASE_KEYS,
          "device",
          "manager",
          "generation",
          "authorityGeneration",
        ]) ||
        typeof value.device !== "string" ||
        !DEVICE.test(value.device) ||
        typeof value.manager !== "boolean" ||
        typeof value.generation !== "string" ||
        !SESSION_TOKEN.test(value.generation) ||
        !deadline(value.authorityGeneration)
      )
        return;
      return {
        ...base,
        role: "control",
        device: value.device,
        manager: value.manager,
        generation: value.generation,
        authorityGeneration: value.authorityGeneration,
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
        source: value.source as string | null,
        management: value.management === true,
        deadline: value.deadline,
      };
    }
    case "source":
    case "target": {
      const session = binding(value);
      const traffic = trafficCounters(value.traffic);
      const keys =
        value.role === "source"
          ? [...TUNNEL_KEYS, "anonymous", "cachedSince", "source", "management"]
          : TUNNEL_KEYS;
      if (
        !object(value, keys) ||
        !session ||
        !traffic ||
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
        traffic,
      };
      if (value.role === "target") return { ...tunnel, role: "target" };
      if (typeof value.anonymous !== "boolean" || !admission(value)) return;
      const cachedSince = value.cachedSince;
      if (cachedSince !== null && (!deadline(cachedSince) || value.anonymous))
        return;
      return {
        ...tunnel,
        role: "source",
        anonymous: value.anonymous,
        source: value.source as string | null,
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
    (value.source === null ||
      (typeof value.source === "string" && DEVICE.test(value.source))) &&
    typeof value.management === "boolean" &&
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
    ...(state.pairing ? { pairing: state.pairing } : {}),
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
