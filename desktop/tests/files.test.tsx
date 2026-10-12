import { expect, test } from "bun:test";
import { fireEvent, screen, within, waitFor } from "@testing-library/react";
import { fixture, openMemberActions, activityJob } from "./fixtures";
async function openFiles(name: string) {
  openMemberActions(name);
  const menu = screen.getByLabelText(`${name} 的更多操作`).closest("details")!;
  fireEvent.click(within(menu).getByRole("button", { name: "传输文件" }));
  return await screen.findByRole("dialog", { name: `文件 · ${name}` });
}
test("file chooser cancellation does not submit and download intent uses immutable IDs", async () => {
  let choice: string | null = null;
  const { devices, calls } = await fixture({
    choose_transfer_file: () => choice,
    remote_file_download: () => ({
      path: "/tmp/owned.txt",
      remote_path: "/tmp/result.txt",
      size: 3,
      sha256: "a".repeat(64),
      skipped: true,
      resumed_from: 3,
      job: activityJob("pull"),
    }),
  });
  const dialog = await openFiles(devices[0].name);
  fireEvent.click(within(dialog).getByLabelText("下载"));
  fireEvent.change(within(dialog).getByLabelText("远端路径"), {
    target: { value: "/tmp/result.txt" },
  });
  fireEvent.submit(dialog.querySelector("form")!);
  await waitFor(() =>
    expect(calls.some((call) => call.command === "choose_transfer_file")).toBe(
      true,
    ),
  );
  expect(calls.some((call) => call.command === "remote_file_download")).toBe(
    false,
  );
  choice = "/tmp/owned.txt";
  fireEvent.submit(dialog.querySelector("form")!);
  await within(dialog).findByText("内容相同，已跳过");
  expect(
    calls.find((call) => call.command === "remote_file_download")?.args,
  ).toEqual({
    device: devices[0].device_id,
    remote: "/tmp/result.txt",
    local: choice,
    cwd: null,
  });
});
test("pending file recovery resumes the saved request without a new upload", async () => {
  let pending = true;
  const { devices, calls } = await fixture({
    remote_file_receipts: () =>
      pending
        ? [
            {
              request_id: "owned-fixture-request",
              target_device_id: "target",
              kind: "push",
              label: "result.bin",
              expires_at_ms: Date.now() + 86400000,
            },
          ]
        : [],
    remote_file_resume: () => {
      pending = false;
      return {
        path: null,
        remote_path: "result.bin",
        size: 4096,
        sha256: "a".repeat(64),
        skipped: false,
        resumed_from: 2048,
        job: activityJob("push"),
      };
    },
  });
  const dialog = await openFiles(devices[0].name);
  fireEvent.click(await within(dialog).findByRole("button", { name: "继续" }));
  await within(dialog).findByText("传输完成");
  expect(
    calls.find((call) => call.command === "remote_file_resume")?.args,
  ).toEqual({
    device: devices[0].device_id,
    request_id: "owned-fixture-request",
  });
  expect(calls.some((call) => call.command === "remote_file_upload")).toBe(
    false,
  );
  await waitFor(() => expect(within(dialog).queryByText("待续传")).toBeNull());
});
