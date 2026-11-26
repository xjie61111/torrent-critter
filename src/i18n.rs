//! Two-language UI (Russian / English). Strings are written inline as
//! `t("русский", "English")` or `tf!("формат {x}", "format {x}")`, so both
//! versions stay next to each other in the code.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "ru")]
    Russian,
    #[serde(rename = "en")]
    English,
}

impl Language {
    pub const ALL: [Language; 2] = [Language::Russian, Language::English];

    /// Name of the language in that language, for the picker.
    pub fn native_name(self) -> &'static str {
        match self {
            Language::Russian => "Русский",
            Language::English => "English",
        }
    }

    /// Russian on a Russian-language Windows, English everywhere else.
    pub fn system_default() -> Self {
        #[cfg(windows)]
        {
            const LANG_RUSSIAN: u16 = 0x19;
            let lang_id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
            if lang_id & 0x3ff == LANG_RUSSIAN { return Language::Russian; }
        }
        Language::English
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set_language(language: Language) {
    CURRENT.store(if language == Language::English { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn is_ru() -> bool {
    CURRENT.load(Ordering::Relaxed) == 0
}

/// Picks the string for the current language.
pub fn t<'a>(ru: &'a str, en: &'a str) -> &'a str {
    if is_ru() { ru } else { en }
}

pub fn app_name() -> &'static str {
    t("Торрент-зверёк", "Torrent Critter")
}

/// `format!` for the current language; both format strings take the same arguments.
macro_rules! tf {
    ($ru:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if crate::i18n::is_ru() { format!($ru $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}
pub(crate) use tf;
