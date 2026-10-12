import * as SelectPrimitive from "@radix-ui/react-select";
import { useEffect, useRef, useState } from "react";
import { getNonce } from "get-nonce";
import { Icon } from "./Icon";

interface SelectOption<Value extends string> {
  value: Value;
  label: string;
  lang?: string;
  disabled?: boolean;
}
interface Props<Value extends string> {
  id: string;
  value: Value;
  options: readonly SelectOption<Value>[];
  onChange: (value: Value) => void | Promise<void>;
  disabled?: boolean;
  required?: boolean;
  compact?: boolean;
  fullWidth?: boolean;
  "aria-label"?: string;
  "aria-describedby"?: string;
}
const optionValue = (value: string) => `option:${value}`;

export function Select<Value extends string>({
  id,
  value,
  options,
  onChange,
  disabled = false,
  required = false,
  compact = false,
  fullWidth = false,
  "aria-label": label,
  "aria-describedby": description,
}: Props<Value>) {
  const trigger = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [container, setContainer] = useState<HTMLElement>();
  const selected = options.find((option) => option.value === value);
  const changeOpen = (next: boolean) => {
    if (next && disabled) return;
    if (next) setContainer(trigger.current?.closest("dialog") ?? undefined);
    setOpen(next);
  };
  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);
  useEffect(() => {
    if (!open) return;
    // Page navigation can hide a mounted field, including native tray shortcuts.
    const observer = new MutationObserver(() => {
      if (trigger.current?.closest("[hidden]")) setOpen(false);
    });
    observer.observe(document.body, {
      subtree: true,
      attributes: true,
      attributeFilter: ["hidden"],
    });
    return () => observer.disconnect();
  }, [open]);
  return (
    <span
      className={`select-field ${compact ? "compact" : ""} ${fullWidth ? "full-width" : ""}`}
      onInvalid={(event) => {
        event.preventDefault();
        trigger.current?.focus();
        changeOpen(true);
      }}
    >
      <SelectPrimitive.Root
        value={required && value === "" ? "" : optionValue(value)}
        open={open}
        disabled={disabled}
        required={required}
        onOpenChange={changeOpen}
        onValueChange={(next) => {
          const option = options.find(
            (item) => optionValue(item.value) === next,
          );
          if (option && !option.disabled && !disabled && option.value !== value)
            void onChange(option.value);
        }}
      >
        <SelectPrimitive.Trigger
          ref={trigger}
          id={id}
          value={value}
          className="select-trigger"
          aria-label={label}
          aria-describedby={description}
          title={selected?.label}
        >
          <span className="select-value" lang={selected?.lang}>
            <SelectPrimitive.Value placeholder={selected?.label} />
          </span>
          <SelectPrimitive.Icon className="select-chevron">
            <Icon name="chevron-down" />
          </SelectPrimitive.Icon>
        </SelectPrimitive.Trigger>
        <SelectPrimitive.Portal container={container}>
          <SelectPrimitive.Content
            className="select-menu"
            position="popper"
            sideOffset={6}
            collisionPadding={8}
            onEscapeKeyDown={(event) => event.stopPropagation()}
          >
            <SelectPrimitive.ScrollUpButton className="select-scroll-button up">
              <Icon name="chevron-down" />
            </SelectPrimitive.ScrollUpButton>
            <SelectPrimitive.Viewport
              className="select-options"
              nonce={getNonce()}
            >
              {options
                .filter((option) => !required || option.value !== "")
                .map((option) => (
                  <SelectPrimitive.Item
                    key={option.value}
                    value={optionValue(option.value)}
                    data-value={option.value}
                    lang={option.lang}
                    disabled={option.disabled}
                    className="select-option"
                  >
                    <SelectPrimitive.ItemText>
                      {option.label}
                    </SelectPrimitive.ItemText>
                    <SelectPrimitive.ItemIndicator className="select-check">
                      <Icon name="check" />
                    </SelectPrimitive.ItemIndicator>
                  </SelectPrimitive.Item>
                ))}
            </SelectPrimitive.Viewport>
            <SelectPrimitive.ScrollDownButton className="select-scroll-button">
              <Icon name="chevron-down" />
            </SelectPrimitive.ScrollDownButton>
          </SelectPrimitive.Content>
        </SelectPrimitive.Portal>
      </SelectPrimitive.Root>
    </span>
  );
}
