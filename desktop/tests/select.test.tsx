import { expect, test } from "bun:test";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { useState } from "react";
import { Select } from "../src/components/Select";
import { selectOption } from "./fixtures";

const shells = ["sh", "bash", "zsh"].map((value) => ({ value, label: value }));

function ShellSelect() {
  const [value, setValue] = useState("sh");
  return (
    <>
      <label htmlFor="shell">Shell</label>
      <Select id="shell" value={value} onChange={setValue} options={shells} />
    </>
  );
}

test("keyboard navigation selects an option and restores focus to the field", async () => {
  render(<ShellSelect />);
  const control = screen.getByRole("combobox", { name: "Shell" });
  control.focus();
  fireEvent.keyDown(control, { key: "ArrowDown" });
  const sh = await screen.findByRole("option", { name: "sh" });
  await waitFor(() => expect(document.activeElement).toBe(sh));
  fireEvent.keyDown(sh, { key: "ArrowDown" });
  const bash = screen.getByRole("option", { name: "bash" });
  await waitFor(() => expect(document.activeElement).toBe(bash));
  fireEvent.keyDown(bash, { key: "Enter" });
  await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
  expect((control as HTMLButtonElement).value).toBe("bash");
  await waitFor(() => expect(document.activeElement).toBe(control));
});

test("popup options remain inside their native dialog and Escape keeps the dialog open", async () => {
  const view = render(
    <dialog open>
      <ShellSelect />
    </dialog>,
  );
  const dialog = view.container.querySelector("dialog")!;
  let escaped = false;
  dialog.addEventListener("keydown", (event) => {
    if (event.key === "Escape") escaped = true;
  });
  fireEvent.click(screen.getByRole("combobox", { name: "Shell" }));
  const menu = await screen.findByRole("listbox");
  expect(menu.closest("dialog")).toBe(dialog);
  fireEvent.keyDown(menu, { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
  expect(escaped).toBe(false);
  expect(dialog.open).toBe(true);
  expect((screen.getByLabelText("Shell") as HTMLButtonElement).value).toBe(
    "sh",
  );
});

test("required account selection still blocks an empty form and submits the selected account", async () => {
  function Accounts() {
    const [value, setValue] = useState("");
    return (
      <form>
        <label htmlFor="account">Account</label>
        <Select
          id="account"
          value={value}
          onChange={setValue}
          required
          options={[
            { value: "", label: "Choose an account" },
            { value: "team", label: "Team" },
          ]}
        />
      </form>
    );
  }
  const view = render(<Accounts />);
  const form = view.container.querySelector("form")!;
  const control = screen.getByLabelText("Account");
  act(() => expect(form.checkValidity()).toBe(false));
  await selectOption(control, "team");
  expect(form.checkValidity()).toBe(true);
  expect((control as HTMLButtonElement).value).toBe("team");
});

test("an empty optional value can be selected and selecting the current value does not save again", async () => {
  const changes: string[] = [];
  function Recent() {
    const [value, setValue] = useState("job");
    return (
      <Select
        id="recent"
        aria-label="Recent"
        value={value}
        onChange={(next) => {
          changes.push(next);
          setValue(next);
        }}
        options={[
          { value: "", label: "New command" },
          { value: "job", label: "Previous command" },
        ]}
      />
    );
  }
  render(<Recent />);
  const control = screen.getByLabelText("Recent");
  await selectOption(control, "job");
  expect(changes).toEqual([]);
  await selectOption(control, "");
  expect(changes).toEqual([""]);
  expect(control.textContent).toBe("New command");
});

test("hiding a page or disabling its control closes an open popup", async () => {
  const field = (hidden: boolean, disabled: boolean) => (
    <section hidden={hidden}>
      <Select
        id="shell"
        aria-label="Shell"
        value="sh"
        onChange={() => {}}
        options={shells}
        disabled={disabled}
      />
    </section>
  );
  const view = render(field(false, false));
  fireEvent.click(screen.getByLabelText("Shell"));
  await screen.findByRole("listbox");
  view.rerender(field(true, false));
  await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
  view.rerender(field(false, false));
  fireEvent.click(screen.getByLabelText("Shell"));
  await screen.findByRole("listbox");
  view.rerender(field(false, true));
  await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
  expect((screen.getByLabelText("Shell") as HTMLButtonElement).disabled).toBe(
    true,
  );
});
