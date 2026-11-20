use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum Preference {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN")]
    Chinese,
}

static CHINESE: AtomicBool = AtomicBool::new(false);

pub(crate) fn select(preference: Preference) {
    let chinese = match preference {
        Preference::System => (unsafe { GetUserDefaultUILanguage() } & 0x03ff) == 4,
        Preference::English => false,
        Preference::Chinese => true,
    };
    CHINESE.store(chinese, Ordering::Relaxed);
}

pub(crate) fn chinese() -> bool {
    CHINESE.load(Ordering::Relaxed)
}

pub(crate) fn text(english: &'static str, chinese: &'static str) -> &'static str {
    if self::chinese() { chinese } else { english }
}

pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

macro_rules! tr {
    ($english:literal, $chinese:literal) => {
        $crate::i18n::text($english, $chinese)
    };
}

macro_rules! trf {
    ($english:literal, $chinese:literal $(, $argument:expr)* $(,)?) => {{
        if $crate::i18n::chinese() {
            format!($chinese $(, $argument)*)
        } else {
            format!($english $(, $argument)*)
        }
    }};
}
