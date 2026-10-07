//! Persisted desktop language preference, shared by native menus and the webview.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Mutex};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LanguagePreference {
    #[default]
    System,
    En,
    Zh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct LanguageSettings {
    pub(crate) preference: LanguagePreference,
    pub(crate) language: Language,
}

pub(crate) struct LanguageState {
    system: Language,
    preference: Mutex<LanguagePreference>,
}

impl Default for LanguageState {
    fn default() -> Self {
        let preference = xrun::client::services::data_dir()
            .ok()
            .and_then(|directory| std::fs::read(directory.join("desktop-language.json")).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            system: Language::detect(),
            preference: Mutex::new(preference),
        }
    }
}

impl LanguageState {
    pub(crate) fn settings(&self) -> LanguageSettings {
        self.snapshot(*self.preference.lock().unwrap())
    }

    fn snapshot(&self, preference: LanguagePreference) -> LanguageSettings {
        LanguageSettings {
            preference,
            language: match preference {
                LanguagePreference::System => self.system,
                LanguagePreference::En => Language::En,
                LanguagePreference::Zh => Language::Zh,
            },
        }
    }

    pub(crate) fn save(&self, preference: LanguagePreference) -> Result<LanguageSettings> {
        self.save_to(&xrun::client::services::data_dir()?, preference)
    }

    fn save_to(
        &self,
        directory: &Path,
        preference: LanguagePreference,
    ) -> Result<LanguageSettings> {
        let mut current = self.preference.lock().unwrap();
        std::fs::create_dir_all(directory)?;
        let temporary = directory.join(".desktop-language.tmp");
        let result = (|| -> Result<()> {
            std::fs::write(&temporary, serde_json::to_vec(&preference)?)?;
            std::fs::rename(&temporary, directory.join("desktop-language.json"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        *current = preference;
        Ok(self.snapshot(preference))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Language {
    #[default]
    En,
    Zh,
}

impl Language {
    pub(crate) fn detect() -> Self {
        Self::from_locale(system_locale().as_deref())
    }

    fn from_locale(locale: Option<&str>) -> Self {
        match locale.and_then(|value| value.split(['-', '_']).next()) {
            Some(value) if value.eq_ignore_ascii_case("zh") => Self::Zh,
            _ => Self::En,
        }
    }

    pub(crate) fn text(self, english: &'static str, chinese: &'static str) -> &'static str {
        match self {
            Self::En => english,
            Self::Zh => chinese,
        }
    }
}

#[cfg(target_os = "macos")]
fn system_locale() -> Option<String> {
    objc2_foundation::NSLocale::preferredLanguages()
        .firstObject()
        .map(|language| language.to_string())
}

#[cfg(windows)]
fn system_locale() -> Option<String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // UI language is independent of the user's date/number format or region.
    const PRIMARY_LANGUAGE_MASK: u16 = 0x03ff;
    const LANG_CHINESE: u16 = 0x0004;
    let language_id = unsafe { GetUserDefaultUILanguage() };
    (language_id != 0).then(|| {
        if language_id & PRIMARY_LANGUAGE_MASK == LANG_CHINESE {
            "zh"
        } else {
            "en"
        }
        .to_string()
    })
}

#[cfg(not(any(target_os = "macos", windows)))]
fn system_locale() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_variants_select_chinese_and_other_languages_default_to_english() {
        for locale in ["zh", "zh-CN", "zh-Hans-CN", "zh-Hant-TW", "ZH_hk"] {
            assert_eq!(Language::from_locale(Some(locale)), Language::Zh);
        }
        for locale in [None, Some(""), Some("en-CN"), Some("ja-JP"), Some("zhx")] {
            assert_eq!(Language::from_locale(locale), Language::En);
        }
        assert_eq!(Language::default().text("Open", "打开"), "Open");
        assert_eq!(Language::Zh.text("Open", "打开"), "打开");
    }

    #[test]
    fn manual_preference_persists_and_system_restores_the_detected_language() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state = LanguageState {
            system: Language::Zh,
            preference: Mutex::new(LanguagePreference::System),
        };
        for (preference, expected) in [
            (LanguagePreference::En, Language::En),
            (LanguagePreference::Zh, Language::Zh),
            (LanguagePreference::System, Language::Zh),
        ] {
            let settings = state.save_to(directory.path(), preference)?;
            assert_eq!(settings.language, expected);
            assert_eq!(state.settings(), settings);
            let saved: LanguagePreference = serde_json::from_slice(&std::fs::read(
                directory.path().join("desktop-language.json"),
            )?)?;
            assert_eq!(saved, preference);
            assert!(!directory.path().join(".desktop-language.tmp").exists());
        }
        let blocker = directory.path().join("file");
        std::fs::write(&blocker, "not a directory")?;
        let before = state.settings();
        assert!(state.save_to(&blocker, LanguagePreference::En).is_err());
        assert_eq!(state.settings(), before);
        Ok(())
    }
}
