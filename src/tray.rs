use std::time::Duration;
use tokio::{sync::watch, time::sleep};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

fn clipboard_icon() -> Icon {
    let mut pixels = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32 {
        for x in 0..32 {
            let color = match (x, y) {
                (12..20, 2..4) | (10..22, 4..8) => [255, 255, 255, 255],
                (8..24, 11..13 | 17..19) | (8..18, 23..25) => [255, 255, 255, 255],
                (4..28, 5..30) => [0, 123, 114, 255],
                _ => [0, 0, 0, 0],
            };
            pixels.extend_from_slice(&color);
        }
    }
    Icon::from_rgba(pixels, 32, 32).expect("32 by 32 RGBA icon")
}

pub async fn run(updates: watch::Receiver<&'static str>) -> anyhow::Result<()> {
    TrayIconEvent::set_event_handler(Some(|_| {}));
    let (icon, mut displayed) = loop {
        let tip = *updates.borrow();
        if let Ok(icon) = TrayIconBuilder::new()
            .with_icon(clipboard_icon())
            .with_tooltip(tip)
            .build()
        {
            break (icon, tip);
        }
        sleep(Duration::from_secs(3)).await;
    };
    loop {
        // Dispatch shell notifications on the same thread that created the tray window.
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let tip = *updates.borrow();
        if tip != displayed && icon.set_tooltip(Some(tip)).is_ok() {
            displayed = tip;
        }
        sleep(Duration::from_millis(200)).await;
    }
}
