import { expect, test } from "bun:test";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { fixture } from "./fixtures";

test("the default onboarding accepts a manually typed relay and code with explicit consent", async () => {
  const { calls } = await fixture({}, false);
  fireEvent.change(screen.getByLabelText("中转地址"), {
    target: { value: "relay.example" },
  });
  fireEvent.change(screen.getByLabelText("接入码"), {
    target: { value: "k7m4-p9q2" },
  });
  fireEvent.change(screen.getByLabelText("本机名称"), {
    target: { value: "mac2" },
  });
  fireEvent.click(screen.getByLabelText("允许管理设备访问本机"));
  await act(async () =>
    fireEvent.submit(document.getElementById("connect-form")!),
  );
  expect(calls.find((call) => call.command === "connect")?.args).toEqual({
    link: "xrun://relay.example#K7M4-P9Q2",
    name: "mac2",
    allowManager: false,
  });
  await waitFor(() => expect(screen.queryByLabelText("接入码")).toBeNull());
});
test("pasting a short invitation fills the two code fields without carrying a private route", async () => {
  await fixture({}, false);
  fireEvent.change(screen.getByLabelText("中转地址"), {
    target: { value: "xrun://relay.example:9528#K7M4-P9Q2" },
  });
  expect((screen.getByLabelText("中转地址") as HTMLInputElement).value).toBe(
    "relay.example:9528",
  );
  expect((screen.getByLabelText("接入码") as HTMLInputElement).value).toBe(
    "K7M4-P9Q2",
  );
  expect(screen.queryByLabelText("接入链接")).toBeNull();
});
test("an exhausted code explains regeneration and keeps the name and address editable", async () => {
  await fixture(
    {
      connect: () => {
        throw { code: "CODE_LOCKED", message: "budget exhausted" };
      },
    },
    false,
  );
  fireEvent.change(screen.getByLabelText("中转地址"), {
    target: { value: "relay.example" },
  });
  fireEvent.change(screen.getByLabelText("接入码"), {
    target: { value: "K7M4-P9Q2" },
  });
  await act(async () =>
    fireEvent.submit(document.getElementById("connect-form")!),
  );
  await screen.findByText("接入码验证次数已用完，请重新生成。");
  expect((screen.getByLabelText("中转地址") as HTMLInputElement).value).toBe(
    "relay.example",
  );
});
test("manager codes copy only on demand and replacing a code revokes its previous identity", async () => {
  let sequence = 0;
  const { calls, status, poll } = await fixture({
    invite: ({ allow }) => ({
      link: "xrun://relay.example#K7M4-P9Q2",
      address: "relay.example",
      code: "K7M4-P9Q2",
      invitation_id: String(++sequence).repeat(32),
      allow,
      expires_in: 600,
    }),
  });
  status.network!.is_manager = true;
  await poll();
  fireEvent.click(screen.getByRole("button", { name: "邀请设备" }));
  fireEvent.click(screen.getByRole("button", { name: "生成接入码" }));
  await screen.findByLabelText("接入码");
  expect(calls.some((call) => call.command.startsWith("copy_"))).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "复制命令" }));
  await screen.findByText("接入命令已复制");
  expect(
    calls.find((call) => call.command === "copy_invitation_command")?.args,
  ).toEqual({ address: "relay.example", code: "K7M4-P9Q2" });
  fireEvent.click(screen.getByRole("button", { name: "重新生成" }));
  await waitFor(() =>
    expect(calls.filter((call) => call.command === "invite").length).toBe(2),
  );
  expect(calls.filter((call) => call.command === "invite")[1].args).toEqual({
    allow: false,
    replace: "1".repeat(32),
  });
});
