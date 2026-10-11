import type { Job } from "../../api";
import { jobState } from "../../format";
export function ActivityState({ job }: { job: Job }) {
  const [label, kind] = jobState(job);
  return <span className={`activity-state ${kind}`}>{label}</span>;
}
