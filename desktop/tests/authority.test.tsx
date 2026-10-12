import { expect, test } from "bun:test";
import { fireEvent, screen, within } from "@testing-library/react";
import { fixture } from "./fixtures";

const restored = {
  network_id: "network",
  manager_id: "manager",
  authority_generation: 2,
  roster_version: 3,
  transfer_to: null,
};
test("backup uses a chosen local file, requires matching passwords and clears secrets when closed", async () => {
  const { status, poll, page, calls } = await fixture({
    choose_authority_archive: () => "/tmp/owned-backup.xrun-backup",
    export_authority: () => restored,
  });
  status.network!.is_manager = true;
  await poll();
  await page("设置");
  fireEvent.click(screen.getByRole("button", { name: "备份" }));
  const dialog = screen.getByRole("dialog");
  fireEvent.click(within(dialog).getByRole("button", { name: "选择文件" }));
  await screen.findByDisplayValue("/tmp/owned-backup.xrun-backup");
  fireEvent.change(within(dialog).getByLabelText("备份密码"), {
    target: { value: "owned-backup-password" },
  });
  fireEvent.change(within(dialog).getByLabelText("确认密码"), {
    target: { value: "different-password" },
  });
  expect(
    (within(dialog).getByRole("button", { name: "备份" }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  fireEvent.change(within(dialog).getByLabelText("确认密码"), {
    target: { value: "owned-backup-password" },
  });
  fireEvent.submit(dialog.querySelector("form")!);
  await screen.findByText("加密备份已保存");
  expect(
    calls.find((call) => call.command === "export_authority")?.args,
  ).toEqual({
    path: "/tmp/owned-backup.xrun-backup",
    password: "owned-backup-password",
    device: null,
  });
  expect(screen.queryByLabelText("备份密码")).toBeNull();
});
test("ordinary devices can restore only with a stopped service and receive no export control", async () => {
  const { status, poll, page } = await fixture();
  await page("设置");
  expect(screen.queryByRole("button", { name: "备份" })).toBeNull();
  expect(
    (screen.getByRole("button", { name: "恢复" }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  status.local.daemon_running = false;
  await poll();
  expect(
    (screen.getByRole("button", { name: "恢复" }) as HTMLButtonElement)
      .disabled,
  ).toBe(false);
});
