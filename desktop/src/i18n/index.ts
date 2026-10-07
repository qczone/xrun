import { invoke, isTauri } from "@tauri-apps/api/core";
import { useSyncExternalStore } from "react";
import { en } from "./en";
import { zh } from "./zh";

export type Language = "en" | "zh";
export type LanguagePreference = "system" | Language;
export interface LanguageSettings {
  preference: LanguagePreference;
  language: Language;
}
export type MessageKey = keyof typeof en;
let settings: LanguageSettings = { preference: "system", language: "en" };
const listeners = new Set<() => void>();
const storageKey = "xrun.language";

function applySettings(value: LanguageSettings) {
  settings = value;
  document.documentElement.lang = value.language === "zh" ? "zh-CN" : "en";
  for (const listener of listeners) listener();
}

export function useLanguage() {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => settings,
  );
}

/** Use the primary system language; unsupported or missing languages use English. */
export function resolveLanguage(locale: string | null | undefined): Language {
  return locale?.split(/[-_]/, 1)[0].toLowerCase() === "zh" ? "zh" : "en";
}

export function setLanguage(value: Language) {
  applySettings({ preference: "system", language: value });
}

export function formatLocale() {
  return settings.language === "zh" ? "zh-CN" : "en-US";
}

function browserLanguage(preference: LanguagePreference): LanguageSettings {
  return {
    preference,
    language:
      preference === "system"
        ? resolveLanguage(navigator.languages?.[0] || navigator.language)
        : preference,
  };
}

/** Complete before mounting React, so the first frame and native menus agree. */
export async function initializeLanguage() {
  if (isTauri()) {
    try {
      applySettings(await invoke<LanguageSettings>("language_settings"));
    } catch {
      // Native language detection failure follows the English default.
      setLanguage("en");
    }
  } else {
    let preference: LanguagePreference = "system";
    try {
      const saved = window.localStorage.getItem(storageKey);
      if (saved === "en" || saved === "zh") preference = saved;
    } catch {
      // A browser without local storage can still follow its system language.
    }
    applySettings(browserLanguage(preference));
  }
}

/** Persist first, then update the current UI without remounting its pages. */
export async function changeLanguage(preference: LanguagePreference) {
  if (isTauri()) {
    applySettings(
      await invoke<LanguageSettings>("set_language", { preference }),
    );
  } else {
    window.localStorage.setItem(storageKey, preference);
    applySettings(browserLanguage(preference));
  }
}

type Placeholders<Text extends string> =
  Text extends `${string}{${infer Name}}${infer Tail}`
    ? Name | Placeholders<Tail>
    : never;
export type PlainMessageKey = {
  [Key in MessageKey]: [Placeholders<(typeof en)[Key]>] extends [never]
    ? Key
    : never;
}[MessageKey];
type Values<Key extends MessageKey> = [Placeholders<(typeof en)[Key]>] extends [
  never,
]
  ? []
  : [values: Record<Placeholders<(typeof en)[Key]>, string | number>];

/** Values remain plain text; React escapes user names, paths and IDs normally. */
export function t<Key extends MessageKey>(
  key: Key,
  ...args: Values<Key>
): string {
  const message = (settings.language === "zh" ? zh : en)[key];
  const values = args[0] as Record<string, string | number> | undefined;
  return message.replace(/\{(\w+)\}/g, (placeholder, name: string) =>
    values?.[name] === undefined ? placeholder : String(values[name]),
  );
}
