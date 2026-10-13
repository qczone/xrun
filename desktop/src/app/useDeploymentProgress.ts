import { useCallback, useEffect, useRef, useState } from "react";
import type { DeploymentProgress } from "../api";

export function useDeploymentProgress() {
  const [events, setEvents] = useState<DeploymentProgress[]>([]);
  const [seconds, setSeconds] = useState(0);
  const started = useRef(0);
  const start = useCallback(() => {
    started.current = Date.now();
    setEvents([]);
    setSeconds(0);
  }, []);
  const receive = useCallback((event: DeploymentProgress) => {
    setEvents((previous) => [...previous, event].slice(-80));
  }, []);
  const phases = new Map<string, DeploymentProgress>();
  for (const event of events) {
    const key = `${event.operation_id}:${event.stage}`;
    const previous = phases.get(key);
    if (previous && previous.sequence > event.sequence) continue;
    phases.set(key, event);
  }
  // Terminal events also retire earlier starts that arrived out of order.
  for (const event of [...phases.values()]) {
    if (event.stage === "authorization_received") {
      phases.delete(`${event.operation_id}:authorization`);
    }
    if (event.stage === "complete") {
      for (const [phase, value] of phases) {
        if (value.operation_id === event.operation_id) phases.delete(phase);
      }
    }
  }
  const active = [...phases.values()].filter(
    (event) => event.status === "running" || event.status === "waiting",
  );
  const current =
    active.find((event) => event.stage !== "authorization_cleanup") ||
    active.at(-1);
  useEffect(() => {
    if (!current) return;
    const timer = setInterval(
      () => setSeconds(Math.floor((Date.now() - started.current) / 1000)),
      1000,
    );
    return () => clearInterval(timer);
  }, [current]);
  return { start, receive, current, events, seconds };
}
