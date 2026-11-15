//! What people read that differs between macOS and Windows: the words for this computer, its
//! file manager and its secret store, two sentences about the system, and the system fonts that
//! Themes offers. Text in the app takes them from here. macOS keeps the words and fonts it had
//! before the Windows port; a test pins them.

/// A font of the system that Themes offers: its family, and the name of the option.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SystemFont {
    pub family: &'static str,
    pub label: &'static str,
}

/// The words and the system fonts of one platform.
#[derive(Debug)]
pub struct Platform {
    pub this_computer: &'static str,
    pub this_computer_label: &'static str,
    pub computer: &'static str,
    pub file_manager: &'static str,
    pub secret_store: &'static str,
    pub os_name: &'static str,
    pub firewall_note: &'static str,
    pub install_example: &'static str,
    pub ui_font: SystemFont,
    /// The first that the computer has is the one in use; the last one every version has.
    pub code_fonts: &'static [SystemFont],
    /// The Chat view's serif, like the Claude app's replies.
    pub serif_font: &'static str,
}

pub const MAC: Platform = Platform {
    this_computer: "this Mac",
    this_computer_label: "This Mac",
    computer: "Mac",
    file_manager: "Finder",
    secret_store: "the macOS Keychain",
    os_name: "macOS",
    firewall_note: "If macOS asks whether Glasscode may accept incoming connections, choose Allow.",
    install_example: "brew install ripgrep",
    ui_font: SystemFont {
        family: ".SystemUIFont",
        label: "Mac (SF Pro)",
    },
    code_fonts: &[SystemFont {
        family: "Menlo",
        label: "Mac (Menlo)",
    }],
    serif_font: "Charter",
};

pub const WINDOWS: Platform = Platform {
    this_computer: "this PC",
    this_computer_label: "This PC",
    computer: "PC",
    file_manager: "File Explorer",
    secret_store: "Windows Credential Manager",
    os_name: "Windows",
    firewall_note: "If Windows Firewall asks whether Glasscode may communicate on networks, choose Allow access.",
    install_example: "winget install BurntSushi.ripgrep.MSVC",
    ui_font: SystemFont {
        family: "Segoe UI",
        label: "Windows (Segoe UI)",
    },
    // Windows 11 can come with Cascadia Mono; Windows 10 has Consolas only.
    code_fonts: &[
        SystemFont {
            family: "Cascadia Mono",
            label: "Windows (Cascadia Mono)",
        },
        SystemFont {
            family: "Consolas",
            label: "Windows (Consolas)",
        },
    ],
    serif_font: "Georgia",
};

/// The platform the app runs on.
pub const THIS: &Platform = if cfg!(windows) { &WINDOWS } else { &MAC };

/// Where local sessions run, inside a sentence: "this Mac", "this PC".
pub fn this_computer() -> &'static str {
    THIS.this_computer
}

/// The same at the start of a sentence or as a label: "This Mac", "This PC".
pub fn this_computer_label() -> &'static str {
    THIS.this_computer_label
}

/// The kind of computer alone, as in "the Mac's microphone": "Mac", "PC".
pub fn computer() -> &'static str {
    THIS.computer
}

/// The program that shows files and folders: "Finder", "File Explorer".
pub fn file_manager() -> &'static str {
    THIS.file_manager
}

/// Where Glasscode keeps the Linear key: "the macOS Keychain", "Windows Credential Manager".
pub fn secret_store() -> &'static str {
    THIS.secret_store
}

/// "macOS", "Windows".
pub fn os_name() -> &'static str {
    THIS.os_name
}

/// What to answer when the system asks about the port the phone pairs on.
pub fn firewall_note() -> &'static str {
    THIS.firewall_note
}

/// An install command, as the example in the field for one.
pub fn install_example() -> &'static str {
    THIS.install_example
}

/// A text made with the platform words, made once and then kept: for the places that take a
/// `&'static str`. Only for texts whose parts do not change while the app runs.
macro_rules! fixed_text {
    ($($format:tt)*) => {{
        static TEXT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!($($format)*));
        TEXT.as_str()
    }};
}
pub(crate) use fixed_text;

#[cfg(test)]
mod tests {
    use super::*;

    fn words(p: &Platform) -> [&'static str; 8] {
        [
            p.this_computer,
            p.this_computer_label,
            p.computer,
            p.file_manager,
            p.secret_store,
            p.os_name,
            p.firewall_note,
            p.install_example,
        ]
    }

    /// What macOS showed before the Windows port, word for word.
    #[test]
    fn mac_keeps_its_words_and_fonts() {
        assert_eq!(
            words(&MAC),
            [
                "this Mac",
                "This Mac",
                "Mac",
                "Finder",
                "the macOS Keychain",
                "macOS",
                "If macOS asks whether Glasscode may accept incoming connections, choose Allow.",
                "brew install ripgrep",
            ]
        );
        assert_eq!(
            MAC.ui_font,
            SystemFont {
                family: ".SystemUIFont",
                label: "Mac (SF Pro)"
            }
        );
        assert_eq!(
            MAC.code_fonts,
            &[SystemFont {
                family: "Menlo",
                label: "Mac (Menlo)"
            }]
        );
        assert_eq!(MAC.serif_font, "Charter");
        if !cfg!(windows) {
            assert_eq!(words(THIS), words(&MAC));
        }
    }

    #[test]
    fn windows_words_do_not_name_the_mac() {
        let fonts = WINDOWS.code_fonts.iter().flat_map(|f| [f.family, f.label]);
        let all = words(&WINDOWS)
            .into_iter()
            .chain([
                WINDOWS.ui_font.family,
                WINDOWS.ui_font.label,
                WINDOWS.serif_font,
            ])
            .chain(fonts);
        for text in all {
            assert!(!text.is_empty());
            for mac in [
                "Mac", "Finder", "Keychain", "brew ", "Menlo", "Charter", "SF Pro",
            ] {
                assert!(!text.contains(mac), "{text:?} has {mac:?}");
            }
        }
        assert_eq!(
            WINDOWS.code_fonts.last().map(|f| f.family),
            Some("Consolas")
        );
    }

    #[test]
    fn each_function_gives_a_text() {
        let given = [
            this_computer(),
            this_computer_label(),
            computer(),
            file_manager(),
            secret_store(),
            os_name(),
            firewall_note(),
            install_example(),
        ];
        for text in given {
            assert!(!text.is_empty());
            if cfg!(windows) {
                assert!(!text.contains("Mac"), "{text:?}");
            }
        }
        assert_eq!(given, words(THIS));
        let text: &'static str = fixed_text!("Runs on {}", this_computer());
        assert_eq!(text, format!("Runs on {}", this_computer()));
    }
}
