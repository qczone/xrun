import type { Job } from "../../api";
import { taskState } from "../../format";
export function StateChip({ job }: { job: Job }) {
  const [label, kind] = taskState(job);
  return <span className={`task-state ${kind}`}>{label}</span>;
}
