import { expect, test } from "bun:test";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { fixture, task, event } from "./fixtures";

test("an unsupported database stops automatic output reads but permits an explicit retry", async () => {
  let reads = 0;
  const { page, poll } = await fixture({
    task_history: () => ({
      db_id: "db-original",
      jobs: [task()],
      next_cursor: null,
    }),
    task_output: () => {
      reads++;
      throw { code: "DB_SCHEMA_MISMATCH", message: "unsupported schema" };
    },
  });
  page("活动记录");
  fireEvent.click(await screen.findByRole("button", { name: /ABC123/ }));
  await screen.findByText("DB_SCHEMA_MISMATCH: unsupported schema");
  expect(reads).toBe(1);
  await poll();
  expect(reads).toBe(1);
  fireEvent.click(screen.getByRole("button", { name: "刷新" }));
  await waitFor(() => expect(reads).toBe(2));
});
test("task output deduplicates sequences, decodes split UTF-8 and renders failures as text", async () => {
  let reads = 0;
  const { calls, page, poll } = await fixture({
    task_history: () => ({
      db_id: "db-original",
      jobs: [task()],
      next_cursor: null,
    }),
    task_output: () =>
      ++reads === 1
        ? {
            job: task(),
            has_more: false,
            events: [
              event(1, "stdout", [0xe4, 0xb8]),
              event(2, "stderr", "<img id=log-injected>\n"),
              event(3, "stdout", [0xad]),
            ],
          }
        : {
            job: task("ABC123", {
              state: "exited",
              exit_code: 7,
              duration_ms: 1234,
              last_seq: 4,
              output_complete: false,
              incomplete_reason: "TRUNCATED",
            }),
            has_more: false,
            events: [
              event(3, "stdout", [0xad]),
              event(4, "stderr", "\u001b[31mfailed\u001b[0m\n"),
            ],
          },
  });
  page("活动记录");
  fireEvent.click(await screen.findByRole("button", { name: /ABC123/ }));
  await waitFor(() =>
    expect(document.getElementById("task-log")?.textContent).toContain("中"),
  );
  expect(document.getElementById("task-log")?.textContent).not.toContain("�");
  expect(document.getElementById("command-injected")).toBeNull();
  expect(document.getElementById("log-injected")).toBeNull();
  await poll();
  expect(
    document.getElementById("task-log")?.textContent?.match(/中/g)?.length,
  ).toBe(1);
  expect(screen.getByText("异常退出")).toBeTruthy();
  expect(screen.getByText("退出码 7")).toBeTruthy();
  expect(document.getElementById("task-warning")?.textContent).toContain(
    "输出已被截断",
  );
  expect(document.getElementById("task-log")?.textContent).not.toContain(
    "\u001b",
  );
  expect(
    calls.filter((call) => call.command === "task_output")[1].args,
  ).toEqual({ dbId: "db-original", job: "ABC123", after: 3 });
  fireEvent.change(screen.getByLabelText("输出类型"), {
    target: { value: "stderr" },
  });
  expect(document.getElementById("task-log")?.dataset.filter).toBe("stderr");
  await poll();
  expect(reads).toBe(2);
});

test("file records show interrupted transfers safely without requesting other devices", async () => {
  const { page, calls } = await fixture({
    file_history: () => ({
      entries: [
        {
          time_ms: Date.now(),
          source_device_id: "remote",
          op: "push",
          path: '<img id="file-injected">',
          size: 2048,
          result: "failed_or_disconnected",
        },
      ],
      next_cursor: null,
    }),
  });
  page("活动记录");
  fireEvent.click(screen.getByRole("button", { name: "文件与截图" }));
  await screen.findByText("失败或中断");
  expect(screen.getByText("接收文件")).toBeTruthy();
  expect(screen.getByText(/2.0 KiB/)).toBeTruthy();
  expect(document.getElementById("file-injected")).toBeNull();
  expect(screen.queryByLabelText("任务状态")).toBeNull();
  expect(calls.find((call) => call.command === "file_history")?.args).toEqual({
    before: null,
  });
});

test("late output cannot replace another task and database reset stops automatic reads", async () => {
  let completeOld!: (value: unknown) => void;
  let reads = 0;
  const pending = new Promise((resolve) => {
    completeOld = resolve;
  });
  const { page, poll } = await fixture({
    task_history: () => ({
      db_id: "db-original",
      jobs: [task("OLD"), task("NEW")],
      next_cursor: null,
    }),
    task_output: ({ job }) => {
      reads++;
      if (job === "OLD") return pending;
      throw { code: "DB_RESET", message: "database replaced" };
    },
  });
  page("活动记录");
  fireEvent.click(await screen.findByRole("button", { name: /OLD/ }));
  fireEvent.click(screen.getByRole("button", { name: "← 返回任务列表" }));
  fireEvent.click(await screen.findByRole("button", { name: /NEW/ }));
  await screen.findByText("DB_RESET: database replaced");
  await act(async () => {
    completeOld({
      job: task("OLD"),
      events: [event(1, "stdout", "old output")],
      has_more: false,
    });
  });
  expect(screen.getByText("任务 NEW · 查看详情")).toBeTruthy();
  expect(document.getElementById("task-log")?.textContent).not.toContain(
    "old output",
  );
  const previous = reads;
  await poll();
  expect(reads).toBe(previous);
});

test("history pages and filters retain server cursors and stop polling while hidden", async () => {
  const { page, calls, poll } = await fixture({
    task_history: ({ before }) => ({
      db_id: "db-original",
      jobs: [task(before ? "PAGE2" : "PAGE1")],
      next_cursor: before ? null : 100,
    }),
  });
  page("活动记录");
  await screen.findByRole("button", { name: /PAGE1/ });
  fireEvent.click(screen.getByRole("button", { name: "下一页" }));
  await screen.findByRole("button", { name: /PAGE2/ });
  expect(
    calls.filter((call) => call.command === "task_history").at(-1)?.args,
  ).toEqual({ before: 100, filter: "all" });
  fireEvent.change(screen.getByLabelText("任务状态"), {
    target: { value: "failed" },
  });
  await waitFor(() =>
    expect(
      calls.filter((call) => call.command === "task_history").at(-1)?.args,
    ).toEqual({ before: null, filter: "failed" }),
  );
  page("设置");
  const count = calls.filter((call) => call.command === "task_history").length;
  await poll();
  expect(calls.filter((call) => call.command === "task_history").length).toBe(
    count,
  );
});

test("scrolling up pauses automatic scrolling while new output continues to arrive", async () => {
  let reads = 0;
  const { page, poll } = await fixture({
    task_history: () => ({
      db_id: "db-original",
      jobs: [task()],
      next_cursor: null,
    }),
    task_output: () => ({
      job: task(),
      has_more: false,
      events: [event(++reads, "stdout", `output ${reads}\n`)],
    }),
  });
  page("活动记录");
  fireEvent.click(await screen.findByRole("button", { name: /ABC123/ }));
  await waitFor(() =>
    expect(document.getElementById("task-log")?.textContent).toContain(
      "output 1",
    ),
  );
  const log = document.getElementById("task-log")!;
  Object.defineProperty(log, "scrollHeight", {
    configurable: true,
    value: 1000,
  });
  Object.defineProperty(log, "clientHeight", {
    configurable: true,
    value: 200,
  });
  log.scrollTop = 800;
  fireEvent.scroll(log);
  log.scrollTop = 500;
  fireEvent.scroll(log);
  const follow = screen.getByLabelText("自动滚动到底部") as HTMLInputElement;
  expect(follow.checked).toBe(false);
  await poll();
  expect(log.textContent).toContain("output 2");
  expect(log.scrollTop).toBe(500);
  fireEvent.click(screen.getByRole("button", { name: "回到最新输出" }));
  expect(follow.checked).toBe(true);
  expect(log.scrollTop).toBeGreaterThanOrEqual(800);
});

test("returning from task details retains the current page and list scroll position", async () => {
  const { calls, page } = await fixture({
    task_history: ({ before }) => ({
      db_id: "db-original",
      jobs: [task(before ? "PAGE2" : "PAGE1")],
      next_cursor: before ? null : 100,
    }),
    task_output: () => ({ job: task("PAGE2"), has_more: false, events: [] }),
  });
  page("活动记录");
  await screen.findByRole("button", { name: /PAGE1/ });
  fireEvent.click(screen.getByRole("button", { name: "下一页" }));
  const row = await screen.findByRole("button", { name: /PAGE2/ });
  const list = document.querySelector(".history-record-panel")!;
  list.scrollTop = 120;
  fireEvent.click(row);
  fireEvent.click(
    await screen.findByRole("button", { name: "← 返回任务列表" }),
  );
  await screen.findByRole("button", { name: /PAGE2/ });
  expect(list.scrollTop).toBe(120);
  expect(screen.getByText("第 2 页")).toBeTruthy();
  expect(
    calls.filter((call) => call.command === "task_history").at(-1)?.args,
  ).toEqual({ before: 100, filter: "all" });
});
