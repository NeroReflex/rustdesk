#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use librustdesk::*;

#[cfg(any(target_os = "android", target_os = "ios", feature = "flutter"))]
fn main() {
    if !common::global_init() {
        eprintln!("Global initialization failed.");
        return;
    }
    common::test_rendezvous_server();
    common::test_nat_type();
    common::global_clean();
}

#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    feature = "flutter"
)))]
fn main() {
    #[cfg(all(windows, not(feature = "inline")))]
    unsafe {
        winapi::um::shellscalingapi::SetProcessDpiAwareness(2);
    }
    if let Some(args) = crate::core_main::core_main().as_mut() {
        // Linux does not load libsciter-gtk.so. The host service started above keeps running.
        #[cfg(target_os = "linux")]
        run_linux_service(args);
        #[cfg(not(target_os = "linux"))]
        ui::start(args);
    }
    common::global_clean();
}

#[cfg(all(target_os = "linux", not(feature = "flutter")))]
fn run_linux_service(args: &[String]) {
    let headless = args.is_empty()
        || args
            .first()
            .is_some_and(|arg| common::is_empty_uni_link(arg));
    if !headless {
        eprintln!(
            "`{}` needs a UI, and this build does not load libsciter-gtk.so.",
            args[0]
        );
        std::process::exit(1);
    }
    std::thread::spawn(ipc::start_pa);
    let id = hbb_common::config::Config::get_id();
    eprintln!("RustDesk host service is running. ID: {id}");
    loop {
        std::thread::park();
    }
}
