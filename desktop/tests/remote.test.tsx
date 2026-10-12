import { expect, test } from "bun:test";
import {
  act,
  fireEvent,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import {
  commandJob,
  event,
  fixture,
  openMemberActions,
  selectOption,
} from "./fixtures";
import type { CommandJob } from "../src/api";

const openCommand = async (name: string) => {
  openMemberActions(name);
  const menu = screen.getByLabelText(`${name} 的更多操作`).closest("details")!;
  fireEvent.click(within(menu).getByRole("button", { name: "执行命令" }));
  return await screen.findByRole("dialog", { name: `在 ${name} 执行命令` });
};

test("remote commands target immutable IDs, deduplicate clicks, and render output as text", async () => {
  let complete!: () => void;
  const pending = new Promise<void>((resolve) => {
    complete = resolve;
  });
  const finished = commandJob("ABC123", {
    state: "succeeded",
    result: {
      exit_code: 0,
      signal: null,
      duration_ms: 10,
      input_bytes: 0,
      stdout_bytes: 10,
      stderr_bytes: 0,
    },
    last_log_seq: 1,
  });
  const { calls, devices } = await fixture({
    remote_submit: async () => {
      await pending;
      return finished;
    },
    remote_output: () => ({
      job: finished,
      events: [event(1, "stdout", '<img id="remote-injected">\n')],
      has_more: false,
    }),
  });
  const dialog = await openCommand(devices[0].name);
  fireEvent.change(within(dialog).getByLabelText("命令"), {
    target: { value: "Get-Date" },
  });
  const form = dialog.querySelector("form")!;
  fireEvent.submit(form);
  fireEvent.submit(form);
  expect(calls.filter((call) => call.command === "remote_submit")).toHaveLength(
    1,
  );
  const submitted = calls.find((call) => call.command === "remote_submit")!;
  expect(submitted.args.device).toBe(devices[0].device_id);
  expect(submitted.args.command).toMatchObject({
    script: "Get-Date",
    shell: "powershell",
    cwd: null,
    timeout: 1800,
  });
  expect((submitted.args.command as { request_id: string }).request_id).toMatch(
    /^[0-9a-f-]{36}$/,
  );
  await act(async () => complete());
  await screen.findByText("退出码 0");
  await waitFor(() =>
    expect(within(dialog).getByLabelText("命令输出").textContent).toContain(
      '<img id="remote-injected">',
    ),
  );
  expect(document.getElementById("remote-injected")).toBeNull();
  expect(calls.some((call) => call.command === "permission")).toBe(false);
});

test("an unconfirmed submission is queried without resubmitting, including after reopening", async () => {
  let requestId = "";
  let recent = false;
  const job = commandJob("ABC123", { state: "succeeded", last_log_seq: 0 });
  const { calls, devices } = await fixture({
    remote_submit: ({ command }) => {
      requestId = (command as { request_id: string }).request_id;
      recent = true;
      throw { code: "UNCONFIRMED", message: "acknowledgement lost" };
    },
    remote_recent: () =>
      recent
        ? [
            {
              request_id: requestId,
              db_id: job.db_id,
              job_id: null,
              status: "unconfirmed",
              created_at_ms: 1000,
            },
          ]
        : [],
    remote_query: () => job,
    remote_output: () => ({ job, events: [], has_more: false }),
  });
  let dialog = await openCommand(devices[0].name);
  fireEvent.change(within(dialog).getByLabelText("命令"), {
    target: { value: "Get-Date" },
  });
  await act(async () => fireEvent.submit(dialog.querySelector("form")!));
  await screen.findByText(
    "提交可能已经执行，请先查询结果。查询不会重复执行命令。",
  );
  await act(async () =>
    fireEvent.click(within(dialog).getByRole("button", { name: "查询结果" })),
  );
  expect(calls.filter((call) => call.command === "remote_submit")).toHaveLength(
    1,
  );
  expect(
    calls.find((call) => call.command === "remote_query")?.args.requestId,
  ).toBe(requestId);
  fireEvent.click(screen.getByRole("button", { name: "关闭远程命令" }));
  dialog = await openCommand(devices[0].name);
  const history = await within(dialog).findByLabelText("最近提交");
  await selectOption(history, requestId);
  await waitFor(() =>
    expect(within(dialog).getByRole("status").textContent).toContain(
      "ABC123 · 已完成",
    ),
  );
  expect(calls.filter((call) => call.command === "remote_submit")).toHaveLength(
    1,
  );
  expect(calls.filter((call) => call.command === "remote_query")).toHaveLength(
    2,
  );
});

test("remote cancellation requires confirmation and retains the acknowledged state", async () => {
  let job: CommandJob = commandJob("ABC123", { last_log_seq: 0 });
  const { calls, devices } = await fixture({
    remote_submit: () => job,
    remote_output: () => ({ job, events: [], has_more: false }),
    remote_cancel: () => {
      job = { ...job, state: "canceled" };
      return job;
    },
  });
  const dialog = await openCommand(devices[0].name);
  fireEvent.change(within(dialog).getByLabelText("命令"), {
    target: { value: "Start-Sleep 30" },
  });
  await act(async () => fireEvent.submit(dialog.querySelector("form")!));
  await act(async () =>
    fireEvent.click(within(dialog).getByRole("button", { name: "取消任务" })),
  );
  const confirm = await screen.findByRole("dialog", { name: "取消远程任务？" });
  expect(calls.some((call) => call.command === "remote_cancel")).toBe(false);
  await act(async () => (confirm as HTMLDialogElement).close("ok"));
  await waitFor(() =>
    expect(
      calls.filter((call) => call.command === "remote_cancel"),
    ).toHaveLength(1),
  );
  expect(calls.find((call) => call.command === "remote_cancel")?.args).toEqual({
    device: devices[0].device_id,
    dbId: job.db_id,
    jobId: job.job_id,
  });
  await within(dialog).findByText("ABC123 · 已取消");
});

test("offline and revoked devices cannot initiate remote commands", async () => {
  const { devices, poll } = await fixture();
  devices[0].online = false;
  await poll();
  openMemberActions(devices[0].name);
  expect(
    (
      within(
        screen
          .getByLabelText(`${devices[0].name} 的更多操作`)
          .closest("details")!,
      ).getByRole("button", { name: "执行命令" }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  devices[0].online = true;
  devices[0].revoked = true;
  await poll();
  openMemberActions(devices[0].name);
  expect(
    (
      within(
        screen
          .getByLabelText(`${devices[0].name} 的更多操作`)
          .closest("details")!,
      ).getByRole("button", { name: "执行命令" }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
});

test("a refused command explains where to grant access and remains editable", async () => {
  const { calls, devices } = await fixture({
    remote_submit: () => {
      throw { code: "SOURCE_NOT_ALLOWED", message: "not allowed" };
    },
  });
  const dialog = await openCommand(devices[0].name);
  fireEvent.change(within(dialog).getByLabelText("命令"), {
    target: { value: "Get-Date" },
  });
  await act(async () => fireEvent.submit(dialog.querySelector("form")!));
  await within(dialog).findByText("对方未允许本机访问，请在目标设备授权。");
  expect(
    (within(dialog).getByLabelText("命令") as HTMLTextAreaElement).value,
  ).toBe("Get-Date");
  expect(within(dialog).queryByRole("button", { name: "查询结果" })).toBeNull();
  expect(calls.filter((call) => call.command === "remote_submit")).toHaveLength(
    1,
  );
});
