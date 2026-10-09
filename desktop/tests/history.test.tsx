import { expect, test } from "bun:test";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { fixture, task, event, operation } from "./fixtures";
import type { Attachment } from "../src/api";

const retained: Attachment = {
  id: "capture-one",
  name: "screenshot.png",
  size: 68,
  sha256: "a".repeat(64),
  created_at_ms: Date.now(),
  status: "available",
  expires_at_ms: Date.now() + 30 * 86400000,
};
const image =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a8JkAAAAASUVORK5CYII=";

test("an unsupported database stops automatic output reads but permits an explicit retry", async () => {
  let reads = 0;
  const { page, poll } = await fixture({
    activity_history: () => ({
      db_id: "db-original",
      entries: [task()],
      next_cursor: null,
    }),
    job_output: () => {
      reads++;
      throw { code: "DB_SCHEMA_MISMATCH", message: "unsupported schema" };
    },
  });
  page("活动旅程");
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
    activity_history: () => ({
      db_id: "db-original",
      entries: [task()],
      next_cursor: null,
    }),
    job_output: () =>
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
              state: "failed",
              result: {
                exit_code: 7,
                signal: null,
                duration_ms: 1234,
                input_bytes: null,
                stdout_bytes: null,
                stderr_bytes: null,
              },
              last_log_seq: 4,
              output_complete: false,
              output_loss_reason: "TRUNCATED",
            }),
            has_more: false,
            events: [
              event(3, "stdout", [0xad]),
              event(4, "stderr", "\u001b[31mfailed\u001b[0m\n"),
            ],
          },
  });
  page("活动旅程");
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
  expect(calls.filter((call) => call.command === "job_output")[1].args).toEqual(
    { dbId: "db-original", job: "ABC123", after: 3 },
  );
  fireEvent.change(screen.getByLabelText("输出类型"), {
    target: { value: "stderr" },
  });
  expect(document.getElementById("task-log")?.dataset.filter).toBe("stderr");
  await poll();
  expect(reads).toBe(2);
});

test("file records show interrupted transfers safely without requesting other devices", async () => {
  const { page, calls } = await fixture({
    activity_history: () => ({
      db_id: "db-original",
      entries: [
        operation("push", {
          path: '<img id="file-injected">',
          size: 2048,
          state: "lost",
        }),
      ],
      next_cursor: null,
    }),
  });
  page("活动旅程");
  await screen.findByText("结果丢失");
  expect(screen.getByText("接收文件")).toBeTruthy();
  expect(screen.getByText(/2.0 KiB/)).toBeTruthy();
  expect(document.getElementById("file-injected")).toBeNull();
  expect(screen.getByLabelText("活动状态")).toBeTruthy();
  expect(
    calls.find((call) => call.command === "activity_history")?.args,
  ).toEqual({
    before: null,
    filter: "all",
  });
});

test("late output cannot replace another task and database reset stops automatic reads", async () => {
  let completeOld!: (value: unknown) => void;
  let reads = 0;
  const pending = new Promise((resolve) => {
    completeOld = resolve;
  });
  const { page, poll } = await fixture({
    activity_history: () => ({
      db_id: "db-original",
      entries: [task("OLD"), task("NEW")],
      next_cursor: null,
    }),
    job_output: ({ job }) => {
      reads++;
      if (job === "OLD") return pending;
      throw { code: "DB_RESET", message: "database replaced" };
    },
  });
  page("活动旅程");
  fireEvent.click(await screen.findByRole("button", { name: /OLD/ }));
  fireEvent.click(screen.getByRole("button", { name: "← 返回活动旅程" }));
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
    activity_history: ({ before }) => ({
      db_id: "db-original",
      entries: [task(before ? "PAGE2" : "PAGE1")],
      next_cursor: before ? null : "page-2",
    }),
  });
  page("活动旅程");
  await screen.findByRole("button", { name: /PAGE1/ });
  fireEvent.click(screen.getByRole("button", { name: "下一页" }));
  await screen.findByRole("button", { name: /PAGE2/ });
  expect(
    calls.filter((call) => call.command === "activity_history").at(-1)?.args,
  ).toEqual({ before: "page-2", filter: "all" });
  fireEvent.change(screen.getByLabelText("活动状态"), {
    target: { value: "failed" },
  });
  await waitFor(() =>
    expect(
      calls.filter((call) => call.command === "activity_history").at(-1)?.args,
    ).toEqual({ before: null, filter: "failed" }),
  );
  page("设置");
  const count = calls.filter(
    (call) => call.command === "activity_history",
  ).length;
  await poll();
  expect(
    calls.filter((call) => call.command === "activity_history").length,
  ).toBe(count);
});

test("scrolling up pauses automatic scrolling while new output continues to arrive", async () => {
  let reads = 0;
  const { page, poll } = await fixture({
    activity_history: () => ({
      db_id: "db-original",
      entries: [task()],
      next_cursor: null,
    }),
    job_output: () => ({
      job: task(),
      has_more: false,
      events: [event(++reads, "stdout", `output ${reads}\n`)],
    }),
  });
  page("活动旅程");
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
    activity_history: ({ before }) => ({
      db_id: "db-original",
      entries: [task(before ? "PAGE2" : "PAGE1")],
      next_cursor: before ? null : "page-2",
    }),
    job_output: () => ({ job: task("PAGE2"), has_more: false, events: [] }),
  });
  page("活动旅程");
  await screen.findByRole("button", { name: /PAGE1/ });
  fireEvent.click(screen.getByRole("button", { name: "下一页" }));
  const row = await screen.findByRole("button", { name: /PAGE2/ });
  const list = document.querySelector(".history-record-panel")!;
  list.scrollTop = 120;
  fireEvent.click(row);
  fireEvent.click(
    await screen.findByRole("button", { name: "← 返回活动旅程" }),
  );
  await screen.findByRole("button", { name: /PAGE2/ });
  expect(list.scrollTop).toBe(120);
  expect(screen.getByText("第 2 页")).toBeTruthy();
  expect(
    calls.filter((call) => call.command === "activity_history").at(-1)?.args,
  ).toEqual({ before: "page-2", filter: "all" });
});

test("one journey mixes commands, transfers and screenshots and previews and saves retained attachments", async () => {
  const now = Date.now();
  const { page, calls } = await fixture({
    activity_history: () => ({
      db_id: "db-original",
      next_cursor: null,
      entries: [
        operation("screenshot", { attachment: retained, created_at_ms: now }),
        task("MIXED", { created_at_ms: now - 60000 }),
        operation("pull", {
          path: "/tmp/report.txt",
          created_at_ms: now - 86400000,
        }),
      ],
    }),
    activity_attachment: () => ({ attachment: retained, image, text: null }),
    save_activity_attachment: () => "/Users/test/Downloads/screenshot.png",
  });
  page("活动旅程");
  await screen.findByRole("button", { name: /MIXED/ });
  const rows = [...document.querySelectorAll(".journey-record")];
  expect(rows).toHaveLength(3);
  expect(rows[0].textContent).toContain("截图");
  expect(rows[1].textContent).toContain("执行命令");
  expect(rows[2].textContent).toContain("发送文件");
  expect(screen.getByText("今天")).toBeTruthy();
  expect(screen.getByText("昨天")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "文件与截图" })).toBeNull();
  fireEvent.click(rows[0]);
  const preview = await screen.findByAltText("附件预览：screenshot.png");
  expect(preview.getAttribute("src")).toBe(image);
  fireEvent.click(screen.getByRole("button", { name: "另存为…" }));
  await screen.findByText("附件已保存至 /Users/test/Downloads/screenshot.png");
  expect(
    calls.find((call) => call.command === "save_activity_attachment")?.args,
  ).toEqual({ id: retained.id });
});

test("expiry is checked when opening an attachment and the summary remains visible", async () => {
  const { page } = await fixture({
    activity_history: () => ({
      db_id: "db-original",
      next_cursor: null,
      entries: [
        operation("push", {
          path: "/repo/report.txt",
          attachment: retained,
        }),
      ],
    }),
    activity_attachment: () => ({
      attachment: { ...retained, status: "expired" },
      image: null,
      text: null,
    }),
  });
  page("活动旅程");
  fireEvent.click(await screen.findByRole("button", { name: /接收文件/ }));
  await screen.findByText("附件已到保留期限，活动摘要仍可查看。");
  expect(screen.queryByRole("button", { name: "另存为…" })).toBeNull();
  expect(document.getElementById("job-detail")?.textContent).toContain(
    "/repo/report.txt",
  );
});

test("late attachment previews cannot replace another operation and UTF-8 files remain plain text", async () => {
  let resolveOld!: (value: unknown) => void;
  const pending = new Promise((resolve) => {
    resolveOld = resolve;
  });
  const textAttachment = { ...retained, id: "text-two", name: "report.txt" };
  const { page } = await fixture({
    activity_history: () => ({
      db_id: "db-original",
      next_cursor: null,
      entries: [
        operation("screenshot", { attachment: retained }),
        operation("pull", {
          attachment: textAttachment,
          path: "report.txt",
        }),
      ],
    }),
    activity_attachment: ({ id }) =>
      id === retained.id
        ? pending
        : {
            attachment: textAttachment,
            image: null,
            text: '<img id="attachment-injected"> file content',
          },
  });
  page("活动旅程");
  fireEvent.click(await screen.findByRole("button", { name: /截图/ }));
  fireEvent.click(screen.getByRole("button", { name: "← 返回活动旅程" }));
  fireEvent.click(await screen.findByRole("button", { name: /发送文件/ }));
  await screen.findByText('<img id="attachment-injected"> file content');
  await act(async () => {
    resolveOld({ attachment: retained, image, text: null });
  });
  expect(screen.queryByAltText("附件预览：screenshot.png")).toBeNull();
  expect(document.getElementById("attachment-injected")).toBeNull();
});

test("a running file job updates its original detail and reveals the retained attachment", async () => {
  let job = operation("push", { state: "running", finished_at_ms: null });
  const { page, poll } = await fixture({
    activity_history: () => ({
      db_id: job.db_id,
      entries: [job],
      next_cursor: null,
    }),
    activity_job: () => structuredClone(job),
    activity_attachment: () => ({
      attachment: retained,
      image: null,
      text: "finished file",
    }),
  });
  page("活动旅程");
  fireEvent.click(await screen.findByRole("button", { name: /接收文件/ }));
  await waitFor(() =>
    expect(document.getElementById("job-detail")?.textContent).toContain(
      "运行中",
    ),
  );
  expect(screen.queryByText("finished file")).toBeNull();
  const id = job.job_id;
  job = operation("push", { job_id: id, attachment: retained });
  await poll();
  await screen.findByText("finished file");
  expect(screen.getByText("已完成")).toBeTruthy();
  expect(document.getElementById("job-detail")?.textContent).toContain(
    "/repo/report.txt",
  );
});

test("a database replacement stops automatic file detail reads and explicit retry remains available", async () => {
  let reads = 0;
  const job = operation("forward", { state: "running", finished_at_ms: null });
  const { page, poll } = await fixture({
    activity_history: () => ({
      db_id: job.db_id,
      entries: [job],
      next_cursor: null,
    }),
    activity_job: () => {
      reads++;
      throw { code: "DB_RESET", message: "database replaced" };
    },
  });
  page("活动旅程");
  fireEvent.click(await screen.findByRole("button", { name: /端口转发/ }));
  await screen.findByText("DB_RESET: database replaced");
  expect(reads).toBe(1);
  await poll();
  expect(reads).toBe(1);
  fireEvent.click(screen.getByRole("button", { name: "刷新" }));
  await waitFor(() => expect(reads).toBe(2));
});
