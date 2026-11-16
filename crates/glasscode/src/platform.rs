//! What people read that differs between macOS and Windows: the words for this computer, its
//! file manager and its secret store, two sentences about the system, the corner where
//! notifications show, and the system fonts that Themes offers. Text in the app takes them from
//! here. macOS keeps the words and fonts it had before the Windows port; a test pins them. Also
//! the switch for the Chat view's status fixes, which macOS does not have yet.

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
    /// Where `notify_ui` puts its notifications on the screen.
    pub notice_corner: &'static str,
    pub ui_font: SystemFont,
    /// The first that the computer has is the one in use; the last one every version has.
    pub code_fonts: &'static [SystemFont],
    /// The Chat view's serif, like the Claude app's replies.
    pub serif_font: &'static str,
    /// See `chat_status_fixes`.
    pub chat_status_fixes: bool,
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
    notice_corner: "the top right",
    ui_font: SystemFont {
        family: ".SystemUIFont",
        label: "Mac (SF Pro)",
    },
    code_fonts: &[SystemFont {
        family: "Menlo",
        label: "Mac (Menlo)",
    }],
    serif_font: "Charter",
    // Off by the owner's rule: macOS behaves as before the Windows port until the owner turns
    // these on here.
    chat_status_fixes: false,
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
    // Above the taskbar, where Windows shows its own notifications.
    notice_corner: "the bottom right",
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
    chat_status_fixes: true,
};

/// The platform the app runs on.
pub const THIS: &Platform = if cfg!(windows) { &WINDOWS } else { &MAC };

// CI builds the app on macOS but does not run its tests: this stops that build if the switch
// turns on for the Mac without the owner's decision.
const _: () = assert!(!MAC.chat_status_fixes, "the chat status fixes stay off on macOS until the owner turns them on");

/// What the Chat view and the session status tell about an agent that the screen check of the
/// Windows port found missing: a Codex session's activity from its rollout, a Codex turn that
/// ended without a reply, and a Claude session that waits on a question before it starts. On
/// for Windows (`cfg!(windows)`); off on macOS, by the owner's rule, until the owner turns it on.
pub fn chat_status_fixes() -> bool {
    THIS.chat_status_fixes
}

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

/// Where a notification shows, inside a sentence: "the top right", "the bottom right".
pub fn notice_corner() -> &'static str {
    THIS.notice_corner
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

    fn words(p: &Platform) -> [&'static str; 9] {
        [
            p.this_computer,
            p.this_computer_label,
            p.computer,
            p.file_manager,
            p.secret_store,
            p.os_name,
            p.firewall_note,
            p.install_example,
            p.notice_corner,
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
                "the top right",
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
        assert!(!MAC.chat_status_fixes, "the chat status fixes are off on macOS");
        assert_eq!(chat_status_fixes(), cfg!(windows));
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

    /// `notify_ui::popup_bounds` puts the notifications at the bottom on Windows, at the top on the Mac.
    #[test]
    fn the_notice_corner_is_where_notifications_show() {
        assert_eq!(WINDOWS.notice_corner, "the bottom right");
        assert_eq!(
            notice_corner(),
            if cfg!(windows) {
                "the bottom right"
            } else {
                "the top right"
            }
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
            notice_corner(),
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
