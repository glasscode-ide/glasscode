#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod auto_review;
mod backdrop;
mod browser_ui;
mod center;
mod chat_select;
mod chat_ui;
mod ci_fix;
mod commands;
mod commands2;
mod control_ui;
mod composer;
mod dialogs;
mod editor;
mod file_view;
mod files_ui;
mod github_accounts_ui;
mod github_ui;
mod icons;
mod keymap;
mod kit;
mod glasscode_ui;
mod mirror_ui;
mod pair_ui;
mod palette_ui;
mod platform;
mod pr_status;
mod proofs;
mod prs_view;
mod right_panel;
mod session_start;
mod session_view;
mod terminal;
mod text_area;
mod themes_ui;
mod text_input;
mod tickets_ui;
mod tickets_view;
mod today_cards;
mod today_ui;
mod theme;
mod ui;
mod update_ui;
mod usage_ui;
mod server_ui;
mod settings_ui;
mod sessions_ui;
mod shell;
mod sidebar;
mod notify_ui;
mod switch_ui;
mod window_controls;
mod workspace;
mod worktree_dialog;

use std::borrow::Cow;

use gpui::{
    App, AppContext, Bounds, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowKind, WindowOptions, point, px, size,
};
use workspace::Workspace;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // The `glasscode` command (~/.local/bin/glasscode runs this): it talks to the running app.
    // On Windows the console program glasscode-cli has these modes, as a window program cannot
    // write to a console.
    if args.get(1).map(String::as_str) == Some("cli") {
        std::process::exit(glasscode_core::cli::main(args[2..].to_vec()));
    }
    // The Glasscode Browser's MCP server, which each agent on this Mac starts, and the relay.
    if let Some(code) = args.get(1).and_then(|mode| glasscode_core::cli::run_mode(mode, &args[2..])) {
        std::process::exit(code);
    }
    // Agents started from here reach the browser through this program.
    if let Ok(exe) = std::env::current_exe() {
        let _ = glasscode_core::browser::write_mcp_config(&exe);
    }
    glasscode_core::env::scrub_process_env();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let env = glasscode_core::env::login_shell_env();

    gpui_platform::application().run(move |cx: &mut App| {
        // The fonts Themes offers (all under the SIL Open Font License, next to them in assets).
        macro_rules! fonts {
            ($($file:literal),* $(,)?) => { vec![$(Cow::Borrowed(include_bytes!(concat!("../assets/fonts/", $file)).as_slice())),*] };
        }
        let fonts: Vec<Cow<'static, [u8]>> = fonts![
            "Geist-Regular.ttf", "Geist-Medium.ttf", "Geist-SemiBold.ttf", "Geist-Bold.ttf", "Geist-Italic.ttf",
            "GeistMono-Regular.ttf", "GeistMono-Medium.ttf", "GeistMono-SemiBold.ttf", "GeistMono-Bold.ttf", "GeistMono-Italic.ttf", "GeistMono-BoldItalic.ttf",
            "Inter-Regular.ttf", "Inter-Medium.ttf", "Inter-SemiBold.ttf", "Inter-Bold.ttf", "Inter-Italic.ttf",
            "IBMPlexSans-Regular.ttf", "IBMPlexSans-SemiBold.ttf", "IBMPlexSans-Italic.ttf",
            "IBMPlexMono-Regular.ttf", "IBMPlexMono-Medium.ttf", "IBMPlexMono-Bold.ttf", "IBMPlexMono-Italic.ttf", "IBMPlexMono-BoldItalic.ttf",
            "JetBrainsMono-Regular.ttf", "JetBrainsMono-Medium.ttf", "JetBrainsMono-Bold.ttf", "JetBrainsMono-Italic.ttf", "JetBrainsMono-BoldItalic.ttf",
        ];
        if let Err(err) = cx.text_system().add_fonts(fonts) {
            log::warn!("could not load UI fonts: {err}");
        }
        // The saved theme, fonts and sizes, before the first frame.
        themes_ui::apply_appearance(glasscode_core::store::load().appearance, cx);

        text_input::bind_keys(cx);
        text_area::bind_keys(cx);
        composer::bind_keys(cx);
        chat_ui::bind_keys(cx);
        workspace::bind_keys(cx);
        keymap::bind(cx, Some("Terminal"));
        cx.on_action(|_: &workspace::Quit, cx| cx.quit());
        cx.set_menus([Menu {
            name: "Glasscode".into(),
            items: vec![MenuItem::action("Quit Glasscode", workspace::Quit)],
            disabled: false,
        }]);

        // Developer aid for UI tests: a non-activating panel on top, so it keeps drawing but never
        // takes the keyboard from whatever the developer is typing in. "hidden" shows no window
        // at all, for tests of what runs in the background (sessions, the PR watchers).
        let background = cfg!(debug_assertions) && std::env::var_os("GLASSCODE_DEBUG_BACKGROUND").is_some();
        let hidden = background && std::env::var("GLASSCODE_DEBUG_BACKGROUND").as_deref() == Ok("hidden");
        // Debug: another size for screen checks, e.g. GLASSCODE_DEBUG_SIZE=900x560 (the smallest).
        let (width, height) = std::env::var("GLASSCODE_DEBUG_SIZE")
            .ok()
            .filter(|_| cfg!(debug_assertions))
            .and_then(|v| {
                let (w, h) = v.split_once('x')?;
                Some((w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?))
            })
            .unwrap_or((1440., 900.));
        let mut bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        // Debug: put the window somewhere else (off the screen, for captures that stay out of the way).
        if let Some((x, y)) = std::env::var("GLASSCODE_DEBUG_ORIGIN").ok().filter(|_| background).and_then(|v| {
            let (x, y) = v.split_once(',')?;
            Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
        }) {
            bounds.origin = point(px(x), px(y));
        }
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Glasscode".into()),
                        appears_transparent: true,
                        // Windows has no traffic lights: the title bar draws the window buttons.
                        traffic_light_position: if cfg!(windows) {
                            None
                        } else {
                            Some(point(px(14.), px(15.)))
                        },
                    }),
                    window_min_size: Some(size(px(900.), px(560.))),
                    kind: if background { WindowKind::PopUp } else { WindowKind::Normal },
                    focus: !background,
                    show: !hidden,
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Workspace::new(env, window, cx)),
            )
            .expect("open window");
        // Closing the main window quits; notifications and other small windows come and go.
        let main_window = window.window_id();
        cx.on_window_closed(move |cx, closed| {
            if closed == main_window {
                cx.quit();
            }
        })
        .detach();
        if !background {
            cx.activate(true);
        }
    });
}
