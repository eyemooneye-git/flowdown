//! UI language: Chinese when Windows' display language is Chinese, English
//! otherwise. Every user-visible string goes through [`tr!`] so the choice is
//! made in one place.

use std::sync::OnceLock;

use windows_sys::Win32::Globalization::{GetSystemDefaultUILanguage, GetUserDefaultUILanguage};

/// `LANG_CHINESE` from `winnt.h`; the low 10 bits of a LANGID are the primary
/// language. Both Simplified and Traditional Chinese map to this id.
const LANG_CHINESE: u16 = 0x04;

static CHINESE: OnceLock<bool> = OnceLock::new();

pub fn chinese() -> bool {
    *CHINESE.get_or_init(|| {
        let langid = unsafe {
            let user = GetUserDefaultUILanguage();
            if user != 0 {
                user
            } else {
                GetSystemDefaultUILanguage()
            }
        };
        is_chinese_langid(langid)
    })
}

fn is_chinese_langid(langid: u16) -> bool {
    langid & 0x03ff == LANG_CHINESE
}

/// Pick the English or Chinese form. Only the chosen expression is evaluated,
/// so both arms may contain `format!` with the same arguments.
#[macro_export]
macro_rules! tr {
    ($en:expr, $zh:expr) => {
        if $crate::lang::chinese() {
            $zh
        } else {
            $en
        }
    };
}

/// Like [`tr!`], but for `format!` templates so arguments are shared between
/// both translations.
#[macro_export]
macro_rules! trf {
    ($en:literal, $zh:literal $(, $arg:expr)* $(,)?) => {
        if $crate::lang::chinese() {
            format!($zh $(, $arg)*)
        } else {
            format!($en $(, $arg)*)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_chinese_languages_are_chinese() {
        // zh-CN, zh-TW, zh-HK, zh-SG
        for langid in [0x0804, 0x0404, 0x0c04, 0x1004] {
            assert!(is_chinese_langid(langid), "{langid:#06x}");
        }
        // en-US, en-GB, ja-JP, de-DE, fr-FR
        for langid in [0x0409, 0x0809, 0x0411, 0x0407, 0x040c] {
            assert!(!is_chinese_langid(langid), "{langid:#06x}");
        }
    }

    #[test]
    fn language_is_stable() {
        assert_eq!(chinese(), chinese());
    }
}
